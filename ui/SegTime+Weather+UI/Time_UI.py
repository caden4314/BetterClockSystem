from __future__ import annotations

from datetime import datetime
import json
import os
from pathlib import Path
import threading
import time
import urllib.error
import urllib.request
import uuid

import betterclock_time as bct
from betterclock_time import BetterClockTimeClient
import webview

CLIENT_NAME = "ClockUI"
INSTANCE_ID = uuid.uuid4().hex[:8]
PORT = 8099
MANUAL_FALLBACK_HOSTS = ["10.42.0.1", "127.0.0.1"]
LONE_GROVE_LAT = 34.1734
LONE_GROVE_LON = -97.4159
NWS_CACHE_TTL_SECONDS = 900
NWS_USER_AGENT = "BetterClockDSEG/1.0 (local display app)"
FONT_OVERRIDE_CSS = "app_example_font_override.css"
REQUIRED_FONT_FILES = [
    Path("DSEG7-Modern/DSEG7Modern-Italic.woff"),
    Path("DSEG14-Modern/DSEG14Modern-Italic.woff"),
    Path("DSEG7-Modern/DSEG7Modern-BoldItalic.woff"),
    Path("DSEGWeather/DSEGWeather.woff"),
]


class BetterClockBackend:
    def __init__(self) -> None:
        self._lock = threading.Lock()
        self._stop = threading.Event()
        self._offset_ms = 0.0
        self._client: BetterClockTimeClient | None = None

        self.auto_host = os.getenv("BETTERCLOCK_HOST", "").strip()
        self.auto_port = self._safe_port(os.getenv("BETTERCLOCK_PORT", str(PORT)))

        self._thread = threading.Thread(target=self._worker, daemon=True)
        self._thread.start()

    def _safe_port(self, value: str) -> int:
        try:
            port = int(value)
        except ValueError:
            return PORT
        if 1 <= port <= 65535:
            return port
        return PORT

    def _manual_hosts(self) -> list[str]:
        hosts: list[str] = []
        if self.auto_host:
            hosts.append(self.auto_host)
        for host in MANUAL_FALLBACK_HOSTS:
            if host not in hosts:
                hosts.append(host)
        return hosts

    def _connect_with_fallback(self) -> BetterClockTimeClient:
        try:
            return bct.connect_auto(
                port=self.auto_port,
                name=CLIENT_NAME,
                instance_id=INSTANCE_ID,
                discovery_timeout_seconds=1.0,
                discovery_retries=6,
                mdns_first=True,
                local_first=False,
                use_cache=False,
                timeout_seconds=1.0,
            )
        except Exception:
            pass

        for host in self._manual_hosts():
            try:
                return bct.connect(
                    host=host,
                    port=self.auto_port,
                    name=CLIENT_NAME,
                    instance_id=INSTANCE_ID,
                    timeout_seconds=1.0,
                )
            except Exception:
                continue
        raise RuntimeError("no BetterClock server reachable")

    def _worker(self) -> None:
        retry_sleep = 0.4
        while not self._stop.is_set():
            if self._client is None:
                try:
                    self._client = self._connect_with_fallback()
                    retry_sleep = 0.4
                except Exception:
                    self._stop.wait(retry_sleep)
                    retry_sleep = min(2.0, retry_sleep + 0.25)
                    continue

            try:
                snapshot = self._client.get_corrected_time()
                now_ms = time.time() * 1000.0
                offset_ms = float(snapshot.corrected_unix_ms) - now_ms
                with self._lock:
                    self._offset_ms = offset_ms
                self._stop.wait(0.25)
            except Exception:
                if self._client is not None:
                    try:
                        self._client.disconnect()
                    except Exception:
                        pass
                self._client = None
                self._stop.wait(0.5)

    def get_offset_ms(self) -> float:
        with self._lock:
            return self._offset_ms

    def close(self) -> None:
        self._stop.set()
        self._thread.join(timeout=1.0)
        if self._client is not None:
            try:
                self._client.disconnect()
            except Exception:
                pass
            self._client = None


class NwsForecastService:
    def __init__(self, *, lat: float, lon: float) -> None:
        self.lat = lat
        self.lon = lon
        self._lock = threading.Lock()
        self._forecast_url: str | None = None
        self._forecast_hourly_url: str | None = None
        self._cached_bundle: dict[str, list[dict[str, object]]] = {
            "weekly": [],
            "hourly_today": [],
        }
        self._expires_at = 0.0

    def _get_json(self, url: str) -> dict:
        request = urllib.request.Request(
            url,
            headers={
                "User-Agent": NWS_USER_AGENT,
                "Accept": "application/geo+json, application/json",
            },
            method="GET",
        )
        with urllib.request.urlopen(request, timeout=6.0) as response:
            payload = response.read().decode("utf-8")
        data = json.loads(payload)
        if not isinstance(data, dict):
            raise RuntimeError("unexpected NWS payload")
        return data

    def _resolve_forecast_urls(self) -> tuple[str, str | None]:
        if self._forecast_url:
            return self._forecast_url, self._forecast_hourly_url
        points_url = f"https://api.weather.gov/points/{self.lat:.4f},{self.lon:.4f}"
        data = self._get_json(points_url)
        properties = data.get("properties", {})
        forecast = properties.get("forecast")
        forecast_hourly = properties.get("forecastHourly")
        if not isinstance(forecast, str) or not forecast:
            raise RuntimeError("NWS forecast URL not found")
        self._forecast_url = forecast
        if isinstance(forecast_hourly, str) and forecast_hourly:
            self._forecast_hourly_url = forecast_hourly
        return self._forecast_url, self._forecast_hourly_url

    def _weather_icon_code(self, short_forecast: str) -> str:
        text = short_forecast.lower()
        if "thunder" in text and ("rain" in text or "storm" in text or "showers" in text):
            return "6"
        if "thunder" in text:
            return "8"
        if any(token in text for token in ("snow", "flurr", "sleet", "blizzard", "ice")):
            return "5"
        if "heavy" in text and any(token in text for token in ("rain", "showers", "drizzle")):
            return "4"
        if any(token in text for token in ("rain", "showers", "drizzle")):
            return "3"
        if any(
            token in text
            for token in ("partly", "mostly sunny", "mostly clear", "few clouds", "sun and clouds")
        ):
            return "9"
        if any(token in text for token in ("cloudy", "overcast")):
            return "2"
        if any(token in text for token in ("sunny", "clear", "fair")):
            return "1"
        return "2"

    def _current_hourly_temp(self, hourly_periods: list[dict]) -> tuple[str | None, str]:
        for period in hourly_periods:
            temp = period.get("temperature")
            if not isinstance(temp, (int, float)):
                continue
            unit = str(period.get("temperatureUnit", "F")).strip() or "F"
            return str(int(round(float(temp)))), unit
        return None, "F"

    def _select_daily(
        self, periods: list[dict], *, days: int, current_temp: str | None, current_unit: str
    ) -> list[dict[str, object]]:
        buckets: dict[str, list[tuple[datetime, dict]]] = {}
        for period in periods:
            start_raw = period.get("startTime")
            if not isinstance(start_raw, str):
                continue
            try:
                start_dt = datetime.fromisoformat(start_raw.replace("Z", "+00:00"))
            except ValueError:
                continue
            day_key = start_dt.date().isoformat()
            buckets.setdefault(day_key, []).append((start_dt, period))

        out: list[dict[str, object]] = []
        for index, day_key in enumerate(sorted(buckets.keys())[:days]):
            entries = sorted(buckets[day_key], key=lambda item: item[0])
            start_dt = entries[0][0]
            rep_period = entries[0][1]
            for _dt, candidate in entries:
                if bool(candidate.get("isDaytime")):
                    rep_period = candidate
                    break

            short = str(rep_period.get("shortForecast", "")).strip()
            unit = str(rep_period.get("temperatureUnit", "F")).strip() or "F"
            temps = [
                int(round(float(p.get("temperature"))))
                for _dt, p in entries
                if isinstance(p.get("temperature"), (int, float))
            ]
            if temps:
                low_text = str(min(temps))
                high_text = str(max(temps))
            else:
                low_text = "--"
                high_text = "--"

            rep_temp = rep_period.get("temperature")
            rep_current = str(int(round(float(rep_temp)))) if isinstance(rep_temp, (int, float)) else "--"
            is_today = index == 0
            current_text = current_temp if (is_today and current_temp is not None) else rep_current
            display_unit = current_unit if is_today and current_temp is not None else unit
            label = "TODAY" if index == 0 else start_dt.strftime("%a").upper()
            out.append(
                {
                    "day": label,
                    "low": low_text,
                    "current": current_text if is_today else "",
                    "high": high_text,
                    "unit": display_unit,
                    "icon": self._weather_icon_code(short),
                    "is_today": is_today,
                }
            )
        return out

    def _select_hourly_today(self, hourly_periods: list[dict], *, hours: int) -> list[dict[str, object]]:
        now = datetime.now().astimezone()
        parsed_periods: list[tuple[datetime, dict]] = []
        for period in hourly_periods:
            start_raw = period.get("startTime")
            if not isinstance(start_raw, str):
                continue
            try:
                start_dt = datetime.fromisoformat(start_raw.replace("Z", "+00:00"))
            except ValueError:
                continue
            parsed_periods.append((start_dt, period))

        out: list[dict[str, object]] = []
        for start_dt, period in sorted(parsed_periods, key=lambda item: item[0]):
            if start_dt < now and (now - start_dt).total_seconds() > 3600:
                continue
            temp = period.get("temperature")
            temp_text = str(int(round(float(temp)))) if isinstance(temp, (int, float)) else "--"
            unit = str(period.get("temperatureUnit", "F")).strip() or "F"
            short = str(period.get("shortForecast", "")).strip()
            if len(out) == 0:
                label = "NOW"
            else:
                label = start_dt.strftime("%I%p").lstrip("0")
            out.append(
                {
                    "hour": label,
                    "temp": temp_text,
                    "unit": unit,
                    "icon": self._weather_icon_code(short),
                }
            )
            if len(out) >= hours:
                break
        return out

    def get_weather_bundle(self) -> dict[str, list[dict[str, object]]]:
        now = time.monotonic()
        with self._lock:
            if (
                self._cached_bundle["weekly"]
                and now < self._expires_at
            ):
                return {
                    "weekly": [dict(item) for item in self._cached_bundle["weekly"]],
                    "hourly_today": [dict(item) for item in self._cached_bundle["hourly_today"]],
                }

        try:
            forecast_url, hourly_url = self._resolve_forecast_urls()
            data = self._get_json(forecast_url)
            periods = data.get("properties", {}).get("periods", [])
            if not isinstance(periods, list):
                raise RuntimeError("unexpected periods format")

            current_temp: str | None = None
            current_unit = "F"
            hourly_periods: list[dict] = []
            if hourly_url:
                hourly_data = self._get_json(hourly_url)
                hourly_periods = hourly_data.get("properties", {}).get("periods", [])
                if isinstance(hourly_periods, list):
                    current_temp, current_unit = self._current_hourly_temp(hourly_periods)
                else:
                    hourly_periods = []

            weekly = self._select_daily(
                periods, days=7, current_temp=current_temp, current_unit=current_unit
            )
            hourly_today = self._select_hourly_today(hourly_periods, hours=7)
            bundle: dict[str, list[dict[str, object]]] = {
                "weekly": weekly,
                "hourly_today": hourly_today,
            }
            with self._lock:
                self._cached_bundle = bundle
                self._expires_at = now + NWS_CACHE_TTL_SECONDS
            return {
                "weekly": [dict(item) for item in bundle["weekly"]],
                "hourly_today": [dict(item) for item in bundle["hourly_today"]],
            }
        except (urllib.error.URLError, RuntimeError, json.JSONDecodeError, TimeoutError):
            with self._lock:
                return {
                    "weekly": [dict(item) for item in self._cached_bundle["weekly"]],
                    "hourly_today": [dict(item) for item in self._cached_bundle["hourly_today"]],
                }

    def get_forecast(self) -> list[dict[str, object]]:
        return self.get_weather_bundle()["weekly"]


class JsApi:
    def __init__(self, backend: BetterClockBackend, forecast: NwsForecastService) -> None:
        self._backend = backend
        self._forecast = forecast
        self._window: webview.Window | None = None

    def get_offset_ms(self) -> float:
        return self._backend.get_offset_ms()

    def get_nws_forecast(self) -> list[dict[str, object]]:
        return self._forecast.get_forecast()

    def get_nws_weather(self) -> dict[str, list[dict[str, object]]]:
        return self._forecast.get_weather_bundle()

    def get_instance_id(self) -> str:
        return INSTANCE_ID

    def set_window(self, window: webview.Window) -> None:
        self._window = window

    def toggle_fullscreen(self) -> bool:
        if self._window is None:
            return False
        self._window.toggle_fullscreen()
        return True


def _asset_uri(filename: str) -> str:
    return (Path(__file__).resolve().parent / filename).resolve().as_uri()


def _looks_like_font_pack(path: Path) -> bool:
    return path.is_dir() and all((path / rel).exists() for rel in REQUIRED_FONT_FILES)


def _find_font_pack() -> Path:
    script_dir = Path(__file__).resolve().parent
    env_candidates = [
        os.getenv("FONT_PACK_PATH", "").strip(),
        os.getenv("DSEG_FONT_PACK", "").strip(),
    ]
    candidates: list[Path] = []
    for raw in env_candidates:
        if raw:
            candidates.append(Path(raw))
    candidates.extend(
        [
            script_dir / "Font_Pack",
            Path.cwd() / "Font_Pack",
            Path.home() / ".python_project" / "Font_Pack",
            Path(r"G:\.python_project\Font_Pack"),
        ]
    )

    for candidate in candidates:
        if _looks_like_font_pack(candidate):
            return candidate.resolve()

    searched = ", ".join(str(p) for p in candidates)
    raise FileNotFoundError(f"Unable to locate Font_Pack. Searched: {searched}")


def _write_font_override_css(font_pack: Path) -> None:
    script_dir = Path(__file__).resolve().parent
    out_path = script_dir / FONT_OVERRIDE_CSS
    d7mi = (font_pack / "DSEG7-Modern/DSEG7Modern-Italic.woff").resolve().as_uri()
    d14mi = (font_pack / "DSEG14-Modern/DSEG14Modern-Italic.woff").resolve().as_uri()
    d7mbi = (font_pack / "DSEG7-Modern/DSEG7Modern-BoldItalic.woff").resolve().as_uri()
    dweather = (font_pack / "DSEGWeather/DSEGWeather.woff").resolve().as_uri()
    css = (
        "@font-face { font-family: \"D7MI\"; src: url(\""
        + d7mi
        + "\") format(\"woff\"); }\n"
        "@font-face { font-family: \"D14MI\"; src: url(\""
        + d14mi
        + "\") format(\"woff\"); }\n"
        "@font-face { font-family: \"D7MBI\"; src: url(\""
        + d7mbi
        + "\") format(\"woff\"); }\n"
        "@font-face { font-family: \"DWEATHER\"; src: url(\""
        + dweather
        + "\") format(\"woff\"); }\n"
    )
    out_path.write_text(css, encoding="utf-8")


def main() -> None:
    font_pack = _find_font_pack()
    _write_font_override_css(font_pack)

    backend = BetterClockBackend()
    forecast = NwsForecastService(lat=LONE_GROVE_LAT, lon=LONE_GROVE_LON)
    api = JsApi(backend, forecast)
    app_html = Path(__file__).resolve().parent / "app_example.html"
    if not app_html.exists():
        raise FileNotFoundError(f"Missing file: {app_html}")

    try:
        window = webview.create_window(
            "DSEG Application Example",
            url=_asset_uri("app_example.html"),
            js_api=api,
            width=640,
            height=420,
            min_size=(340, 220),
            resizable=True,
        )
        api.set_window(window)
        try:
            webview.start(gui="edgechromium")
        except Exception:
            webview.start()
    finally:
        backend.close()


if __name__ == "__main__":
    main()
