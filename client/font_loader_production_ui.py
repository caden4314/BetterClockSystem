#!/usr/bin/env python3
from __future__ import annotations

import argparse
import os
import sys
import time
import tkinter as tk
from datetime import datetime
from pathlib import Path
from tkinter import font as tkfont


PROJECT_ROOT = Path(__file__).resolve().parents[1]
if str(PROJECT_ROOT) not in sys.path:
    sys.path.insert(0, str(PROJECT_ROOT))

try:
    from base_time_ui_client import SimpleTimeNetworkClient, TimeNetworkSnapshot
except ModuleNotFoundError:
    from client.base_time_ui_client import SimpleTimeNetworkClient, TimeNetworkSnapshot

from ui.font_loader import SegmentedFontLoader


DEFAULT_FONT_NAME = "DSEG14Classic-Regular"
DEFAULT_FONT_PATH = r"G:\BetterClock\ui\fonts\fonts-DSEG_v046\DSEG14-Classic\DSEG14Classic-Regular.ttf"
UI_BG = "#13181d"
TIME_FG = "#d0deea"
DATE_FG = "#a6b7c8"
STATE_CONNECTING_FG = "#d8c393"
STATE_CONNECTED_FG = "#a6d7b0"
STATE_ERROR_FG = "#e4aaaa"
STATS_BG = "#0f1318"
STATS_FG = "#a8bbce"
CLOCK_RENDER_MS = 50
MAX_SLEW_STEP_MS = 28.0

class BasicDebugUI:
    def __init__(
        self,
        root: tk.Tk,
        base_url: str,
        poll_ms: int,
        client_id: str,
        fullscreen: bool,
        font_name: str,
        font_path: str,
    ) -> None:
        self.root = root
        self.base_url = base_url
        self.poll_ms = max(1, int(poll_ms))
        self.client = SimpleTimeNetworkClient(base_url=base_url, client_id=client_id)
        self.font_loader = SegmentedFontLoader(
            fonts_root=PROJECT_ROOT / "ui" / "fonts",
            auto_extract=False,
        )
        self.time_font_family, self.time_font_source = self._load_time_font(
            font_name=font_name,
            font_path=font_path,
        )
        self._clock_target_ms = int(time.time() * 1000)
        self._clock_anchor_ms = float(self._clock_target_ms)
        self._clock_anchor_mono = time.perf_counter()

        root.title("BetterClock Debug UI")
        root.geometry("1200x700")
        root.minsize(900, 520)
        if fullscreen:
            root.attributes("-fullscreen", True)
            root.bind("<Escape>", lambda _e: root.attributes("-fullscreen", False))
        root.configure(bg=UI_BG)

        self.container = tk.Frame(root, bg=UI_BG)
        self.container.pack(fill="both", expand=True, padx=16, pady=16)

        self.time_label = tk.Label(
            self.container,
            text="--:--:--",
            font=(self.time_font_family, 82, "normal"),
            bg=UI_BG,
            fg=TIME_FG,
            anchor="center",
        )
        self.time_label.pack(fill="x", pady=(8, 4))

        self.ms_label = tk.Label(
            self.container,
            text="--  ms ---",
            font=("Consolas", 22, "bold"),
            bg=UI_BG,
            fg="#b8c8d8",
            anchor="center",
        )
        self.ms_label.pack(fill="x", pady=(0, 6))

        self.date_label = tk.Label(
            self.container,
            text="Waiting for data...",
            font=("Consolas", 24, "bold"),
            bg=UI_BG,
            fg=DATE_FG,
            anchor="center",
        )
        self.date_label.pack(fill="x", pady=(0, 12))

        self.state_label = tk.Label(
            self.container,
            text="CONNECTING",
            font=("Consolas", 18, "bold"),
            bg=UI_BG,
            fg=STATE_CONNECTING_FG,
            anchor="center",
        )
        self.state_label.pack(fill="x", pady=(0, 12))

        self.stats = tk.Text(
            self.container,
            height=16,
            wrap=tk.WORD,
            font=("Consolas", 13),
            bg=STATS_BG,
            fg=STATS_FG,
            relief=tk.FLAT,
        )
        self.stats.pack(fill="both", expand=True)
        self.stats.configure(state=tk.DISABLED)

        self.root.after(20, self.poll_once)
        self.root.after(CLOCK_RENDER_MS, self.render_clock_tick)

    def poll_once(self) -> None:
        try:
            snapshot = self.client.poll()
            self.render_snapshot(snapshot)
        except Exception as exc:
            self.render_error(str(exc))
        finally:
            self.root.after(self.poll_ms, self.poll_once)

    def render_snapshot(self, snap: TimeNetworkSnapshot) -> None:
        self._set_clock_target_ms(snap.corrected_unix_ms)
        self.date_label.config(text=snap.date_text)
        self.state_label.config(text="CONNECTED", fg=STATE_CONNECTED_FG)

        synced_ms = snap.corrected_unix_ms % 1000
        now_ms = int(time.time() * 1000)
        session_seconds = max(0.001, (now_ms - self.client.session_start_unix_ms) / 1000.0)
        out_rate_bps = self.client.total_out_bytes / session_seconds
        in_rate_bps = self.client.total_in_bytes / session_seconds

        lines = [
            f"Server: {self.base_url}",
            f"Client ID: {self.client.client_id}",
            f"Instance ID: {self.client.instance_id}",
            f"Poll: {self.poll_ms} ms ({1000.0 / self.poll_ms:.2f} Hz)",
            f"Clock Font: {self.time_font_family}",
            f"Font Source: {self.time_font_source}",
            "",
            f"Session Start: {format_datetime_ms(self.client.session_start_unix_ms)}",
            f"Last OUT Date: {format_datetime_ms(self.client.last_out_unix_ms)}",
            f"Last IN Date: {format_datetime_ms(self.client.last_in_unix_ms)}",
            "",
            f"Data OUT Last: {format_bytes(self.client.last_request_bytes)}",
            f"Data OUT Total: {format_bytes(self.client.total_out_bytes)}",
            f"Data OUT Rate: {format_bytes(out_rate_bps)}/s",
            "",
            f"Data IN Last: {format_bytes(self.client.last_response_bytes)}",
            f"Data IN Total: {format_bytes(self.client.total_in_bytes)}",
            f"Data IN Rate: {format_bytes(in_rate_bps)}/s",
            "",
            f"Source: {snap.source_label}",
            f"Clients Seen: {snap.clients_seen}",
            f"Total Requests: {snap.total_requests}",
            "",
            f"RTT: {snap.rtt_ms:.2f} ms",
            f"Offset: {snap.offset_ms:+.2f} ms",
            f"Desync: {snap.desync_ms:+.2f} ms",
            f"Synced MS: {synced_ms:03d}",
            "",
            f"Corrected Unix MS: {snap.corrected_unix_ms}",
            f"Corrected ISO Local: {snap.corrected_iso_local}",
        ]
        self.set_stats("\n".join(lines))

    def render_error(self, error_text: str) -> None:
        self.time_label.config(text="--:--:--")
        self.ms_label.config(text="--  ms ---")
        self.date_label.config(text="Disconnected")
        self.state_label.config(text="ERROR", fg=STATE_ERROR_FG)
        self.set_stats(
            "\n".join(
                [
                    f"Server: {self.base_url}",
                    f"Client ID: {self.client.client_id}",
                    f"Instance ID: {self.client.instance_id}",
                    f"Poll: {self.poll_ms} ms ({1000.0 / self.poll_ms:.2f} Hz)",
                    f"Clock Font: {self.time_font_family}",
                    f"Font Source: {self.time_font_source}",
                    "",
                    f"Session Start: {format_datetime_ms(self.client.session_start_unix_ms)}",
                    f"Last OUT Date: {format_datetime_ms(self.client.last_out_unix_ms)}",
                    f"Last IN Date: {format_datetime_ms(self.client.last_in_unix_ms)}",
                    f"Data OUT Total: {format_bytes(self.client.total_out_bytes)}",
                    f"Data IN Total: {format_bytes(self.client.total_in_bytes)}",
                    "",
                    f"Last Error: {error_text}",
                ]
            )
        )

    def set_stats(self, text: str) -> None:
        self.stats.configure(state=tk.NORMAL)
        self.stats.delete("1.0", tk.END)
        self.stats.insert("1.0", text)
        self.stats.configure(state=tk.DISABLED)

    def render_clock_tick(self) -> None:
        now_mono = time.perf_counter()
        predicted_ms = self._clock_anchor_ms + ((now_mono - self._clock_anchor_mono) * 1000.0)
        dt = datetime.fromtimestamp(predicted_ms / 1000.0)
        hour12 = dt.hour % 12
        if hour12 == 0:
            hour12 = 12
        meridiem = "PM" if dt.hour >= 12 else "AM"
        synced_ms = dt.microsecond // 1000
        self.time_label.config(text=f"{hour12:02}:{dt.minute:02}:{dt.second:02}")
        self.ms_label.config(text=f"{meridiem}  ms {synced_ms:03d}")
        self.root.after(CLOCK_RENDER_MS, self.render_clock_tick)

    def _set_clock_target_ms(self, target_ms: int) -> None:
        now_mono = time.perf_counter()
        predicted_ms = self._clock_anchor_ms + ((now_mono - self._clock_anchor_mono) * 1000.0)
        error_ms = float(target_ms) - float(predicted_ms)
        if abs(error_ms) > 2500.0:
            self._clock_anchor_ms = float(target_ms)
            self._clock_anchor_mono = now_mono
            self._clock_target_ms = int(target_ms)
            return

        step_ms = max(-MAX_SLEW_STEP_MS, min(MAX_SLEW_STEP_MS, error_ms * 0.35))
        self._clock_anchor_ms = float(predicted_ms + step_ms)
        self._clock_anchor_mono = now_mono
        self._clock_target_ms = int(target_ms)

    def _load_time_font(self, font_name: str, font_path: str) -> tuple[str, str]:
        path = self._resolve_font_path(font_name=font_name, font_path=font_path)
        if path is None:
            return "Consolas", "fallback:no-path"

        if os.name == "nt":
            try:
                self.font_loader.register_windows_font_by_path(path)
            except Exception:
                pass

        family = self._detect_dseg_family()
        if family:
            return family, str(path)
        return "Consolas", f"fallback:family-not-found ({path})"

    def _resolve_font_path(self, font_name: str, font_path: str) -> Path | None:
        explicit = str(font_path).strip()
        if explicit:
            p = Path(explicit)
            if p.is_file():
                return p
        try:
            resolved = self.font_loader.get_font_path(
                font_name,
                extensions=self.font_loader.RUNTIME_EXTENSIONS,
                strict=False,
                refresh=True,
            )
            return resolved if resolved.is_file() else None
        except Exception:
            return None

    def _detect_dseg_family(self) -> str | None:
        try:
            families = [str(f) for f in tkfont.families(self.root)]
        except Exception:
            return None
        preferred = [
            "DSEG14 Classic",
            "DSEG14 Classic Mini",
            "DSEG14 Classic MINI",
            "DSEG14",
        ]
        for target in preferred:
            for family in families:
                if family.lower() == target.lower():
                    return family
        for family in families:
            lowered = family.lower()
            if "dseg14" in lowered and "mini" in lowered:
                return family
        for family in families:
            if "dseg" in family.lower():
                return family
        return None


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Basic BetterClock debug UI")
    parser.add_argument("--base-url", default="http://127.0.0.1:8099")
    parser.add_argument("--poll-ms", type=int, default=120)
    parser.add_argument("--client-id", default="basic-debug-ui")
    parser.add_argument("--fullscreen", action="store_true")
    parser.add_argument("--font-name", default=DEFAULT_FONT_NAME)
    parser.add_argument("--font-path", default=DEFAULT_FONT_PATH)
    return parser.parse_args(argv)


def main(argv: list[str]) -> int:
    args = parse_args(argv)
    root = tk.Tk()
    BasicDebugUI(
        root=root,
        base_url=args.base_url,
        poll_ms=args.poll_ms,
        client_id=args.client_id,
        fullscreen=args.fullscreen,
        font_name=args.font_name,
        font_path=args.font_path,
    )
    root.mainloop()
    return 0


def format_bytes(value: float | int) -> str:
    size = float(value)
    if size < 0:
        size = 0.0
    units = ["B", "KB", "MB", "GB", "TB"]
    idx = 0
    while size >= 1024.0 and idx < len(units) - 1:
        size /= 1024.0
        idx += 1
    if idx == 0:
        return f"{int(size)} {units[idx]}"
    return f"{size:.2f} {units[idx]}"


def format_datetime_ms(unix_ms: int) -> str:
    if not unix_ms:
        return "--"
    dt = datetime.fromtimestamp(unix_ms / 1000.0)
    return dt.strftime("%Y-%m-%d %H:%M:%S.%f")[:-3]


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
