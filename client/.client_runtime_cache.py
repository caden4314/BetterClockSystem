#!/usr/bin/env python3
import argparse
from collections import deque
import hashlib
import json
import math
import os
import socket
import sys
import time
import tkinter as tk
import urllib.parse
import urllib.request
import uuid
from datetime import datetime


NORMAL_BG = "#061226"
NORMAL_PANEL = "#0a1a33"
WARNING_BG_SOFT = "#4a2a08"
WARNING_BG_PULSE = "#8d3e00"
DISCONNECTED_BG = "#351a1"
TEXT_MAIN = "#dce6f2"
TEXT_ACCENT = "#6ee0d2"
TEXT_WARN = "#ffbe73"
TEXT_ALERT = "#ff8e8e"
UPDATING_BG = "#0b2236"
UPDATING_PANEL = "#13324d"
LATENCY_SAMPLE_WINDOW = 24
LOW_RTT_SAMPLE_FLOOR = 5
LOW_RTT_HEADROOM_MS = 8.0
OFFSET_SLEW_RATE_MS_PER_SEC = 240.0
OFFSET_DESYNC_GAIN_FAST = 0.35
OFFSET_DESYNC_GAIN_SLOW = 0.16
MAX_REASONABLE_RTT_MS = 60_000.0
MAX_REASONABLE_OFFSET_MS = 60_000.0
DEFAULT_RUNTIME_UPDATE_CHECK_MS = 4_000


class BetterClockClient:
    def __init__(
        self,
        root: tk.Tk,
        server_url: str,
        poll_ms: int,
        client_id: str,
        client_instance: str,
    ) -> None:
        self.root = root
        self.server_url = server_url
        self.poll_ms = max(50, poll_ms)
        self.client_id = client_id
        self.client_instance = client_instance
        self.connected = False
        self.client_debug_mode = False
        self.debug_panel_visible = False
        self.rtt_ewma_ms = 0.0
        self.offset_ewma_ms = 0.0
        self.offset_display_ms = 0.0
        self.offset_desync_ms = 0.0
        self.offset_initialized = False
        self.last_offset_update_mono = time.perf_counter()
        self.latency_samples: deque[tuple[float, float]] = deque(
            maxlen=LATENCY_SAMPLE_WINDOW
        )
        self.runtime_endpoint_url = str(
            globals().get("__runtime_endpoint_url__", "")
        ) or derive_runtime_endpoint_url(server_url)
        cache_hint = str(globals().get("__runtime_cache_path__", "")).strip()
        if not cache_hint:
            file_hint = str(globals().get("__file__", "")).strip()
            if file_hint and not file_hint.startswith("<"):
                cache_hint = file_hint
        self.runtime_cache_path = cache_hint
        runtime_source = globals().get("__runtime_source__", "")
        if isinstance(runtime_source, str):
            self.runtime_hash = sha256_hex(runtime_source)
        else:
            self.runtime_hash = ""
        if not self.runtime_hash and self.runtime_cache_path:
            try:
                with open(self.runtime_cache_path, "r", encoding="utf-8") as handle:
                    self.runtime_hash = sha256_hex(handle.read())
            except Exception:
                self.runtime_hash = ""
        raw_runtime_args = globals().get("__runtime_args__", sys.argv[1:])
        if isinstance(raw_runtime_args, list):
            self.runtime_args = [str(arg) for arg in raw_runtime_args]
        else:
            self.runtime_args = list(sys.argv[1:])
        self.runtime_update_check_ms = max(
            1000,
            int(globals().get("__runtime_update_check_ms__", DEFAULT_RUNTIME_UPDATE_CHECK_MS)),
        )
        self.last_runtime_update_check_mono = time.perf_counter()
        self.runtime_update_in_progress = False
        self.runtime_update_status = ""

        root.title("BetterClock Production Client")
        root.geometry("1280x720")
        root.minsize(900, 520)
        root.configure(bg=NORMAL_BG)

        self.container = tk.Frame(root, bg=NORMAL_BG)
        self.container.pack(fill="both", expand=True, padx=18, pady=18)

        self.header_panel = tk.Frame(
            self.container, bg=NORMAL_PANEL, bd=0, highlightthickness=1
        )
        self.header_panel.pack(fill="x", pady=(0, 14))
        self.header_panel.configure(
            highlightbackground="#1f3a5b", highlightcolor="#1f3a5b"
        )

        self.title_label = tk.Label(
            self.header_panel,
            text="BetterClock Hall Display",
            font=("Segoe UI", 24, "bold"),
            bg=NORMAL_PANEL,
            fg=TEXT_ACCENT,
            anchor="w",
        )
        self.title_label.pack(side="left", padx=14, pady=10)

        self.mode_badge = tk.Label(
            self.header_panel,
            text="PRODUCTION",
            font=("Consolas", 12, "bold"),
            bg="#14443e",
            fg="#b9f9ea",
            padx=10,
            pady=4,
        )
        self.mode_badge.pack(side="right", padx=(8, 14), pady=10)

        self.warning_badge = tk.Label(
            self.header_panel,
            text="NORMAL",
            font=("Consolas", 12, "bold"),
            bg="#1f3652",
            fg="#b7d3f2",
            padx=10,
            pady=4,
        )
        self.warning_badge.pack(side="right", padx=8, pady=10)

        self.connection_badge = tk.Label(
            self.header_panel,
            text="CONNECTING",
            font=("Consolas", 12, "bold"),
            bg="#3f2f18",
            fg=TEXT_WARN,
            padx=10,
            pady=4,
        )
        self.connection_badge.pack(side="right", padx=8, pady=10)

        self.clock_stage = tk.Frame(
            self.container, bg=NORMAL_BG, bd=0, highlightthickness=1
        )
        self.clock_stage.pack(fill="both", expand=True, pady=(0, 14))
        self.clock_stage.configure(
            highlightbackground="#123150", highlightcolor="#123150"
        )

        self.time_label = tk.Label(
            self.clock_stage,
            text="12:00:00 AM",
            font=("Consolas", 124, "bold"),
            bg=NORMAL_BG,
            fg=TEXT_MAIN,
            anchor="center",
        )
        self.time_label.pack(fill="x", padx=12, pady=(34, 8))

        self.date_label = tk.Label(
            self.clock_stage,
            text="Monday, January 01 2001",
            font=("Segoe UI", 32, "bold"),
            bg=NORMAL_BG,
            fg="#b9cde4",
            anchor="center",
        )
        self.date_label.pack(fill="x", padx=12, pady=(0, 12))

        self.subline_label = tk.Label(
            self.clock_stage,
            text="Source: -- | Clients: 000 | Armed: 0000 | Triggered: 0000",
            font=("Consolas", 20, "bold"),
            bg=NORMAL_BG,
            fg="#7fb8d3",
            anchor="center",
        )
        self.subline_label.pack(fill="x", padx=12, pady=(0, 30))

        self.status_banner = tk.Frame(
            self.container, bg=NORMAL_PANEL, bd=0, highlightthickness=1
        )
        self.status_banner.pack(fill="x")
        self.status_banner.configure(
            highlightbackground="#1f3a5b", highlightcolor="#1f3a5b"
        )

        self.status_label = tk.Label(
            self.status_banner,
            text="Connecting...",
            font=("Segoe UI", 28, "bold"),
            bg=NORMAL_PANEL,
            fg=TEXT_WARN,
            anchor="center",
        )
        self.status_label.pack(fill="x", padx=10, pady=(12, 2))

        self.detail_label = tk.Label(
            self.status_banner,
            text="",
            font=("Consolas", 15, "bold"),
            bg=NORMAL_PANEL,
            fg=TEXT_MAIN,
            anchor="center",
            justify="center",
        )
        self.detail_label.pack(fill="x", padx=10, pady=(2, 12))

        self.footer_label = tk.Label(
            self.container,
            text=f"API: {self.server_url} | ID={self.client_id} | INST={self.client_instance}",
            font=("Consolas", 10),
            bg=NORMAL_BG,
            fg="#8fa9c6",
            anchor="w",
        )
        self.footer_label.pack(fill="x", pady=(10, 0))

        self.debug_panel = tk.Frame(
            self.container, bg=NORMAL_PANEL, bd=0, highlightthickness=1
        )
        self.debug_panel.configure(
            highlightbackground="#3d5b78", highlightcolor="#3d5b78"
        )
        self.debug_label = tk.Label(
            self.debug_panel,
            text="",
            font=("Consolas", 11),
            bg=NORMAL_PANEL,
            fg="#a7c2de",
            anchor="w",
            justify="left",
        )
        self.debug_label.pack(fill="x", padx=10, pady=8)
        self.set_debug_panel_visible(False)

        self.schedule_poll(initial=True)

    def schedule_poll(self, initial: bool = False) -> None:
        delay = 40 if initial else self.poll_ms
        self.root.after(delay, self.poll_once)

    def poll_once(self) -> None:
        try:
            payload, rtt_ms, client_send_ms, client_recv_ms = self.fetch_state()
            self.connected = True
            self.update_latency_model(payload, rtt_ms, client_send_ms, client_recv_ms)
            self.render_from_payload(payload, rtt_ms)
            self.maybe_check_runtime_update()
        except Exception as exc:
            self.connected = False
            self.render_disconnected(str(exc))
        finally:
            if not self.runtime_update_in_progress:
                self.schedule_poll()

    def fetch_state(self) -> tuple[dict, float, int, int]:
        reported_rtt_ms = self.rtt_ewma_ms if self.offset_initialized else None
        reported_offset_ms = self.offset_display_ms if self.offset_initialized else None
        reported_desync_ms = self.offset_desync_ms if self.offset_initialized else None
        return fetch_state_once(
            self.server_url,
            self.client_id,
            self.client_instance,
            timeout_seconds=1.0,
            reported_rtt_ms=reported_rtt_ms,
            reported_offset_ms=reported_offset_ms,
            reported_desync_ms=reported_desync_ms,
        )

    def maybe_check_runtime_update(self) -> None:
        if self.runtime_update_in_progress:
            return
        if not self.runtime_endpoint_url:
            return

        now = time.perf_counter()
        interval_seconds = self.runtime_update_check_ms / 1000.0
        if (now - self.last_runtime_update_check_mono) < interval_seconds:
            return
        self.last_runtime_update_check_mono = now

        try:
            latest_runtime = fetch_runtime_code_text(
                self.runtime_endpoint_url,
                timeout_seconds=1.5,
            )
        except Exception:
            return

        latest_hash = sha256_hex(latest_runtime)
        if not self.runtime_hash:
            self.runtime_hash = latest_hash
            return
        if latest_hash == self.runtime_hash:
            return

        self.apply_runtime_update(latest_runtime, latest_hash)

    def apply_runtime_update(self, runtime_code: str, runtime_hash: str) -> None:
        self.runtime_update_in_progress = True
        self.runtime_update_status = "Runtime update detected. Downloading update..."
        self.render_runtime_updating(self.runtime_update_status)
        self.root.update_idletasks()

        if not self.runtime_cache_path:
            self.runtime_update_in_progress = False
            self.runtime_update_status = "Update available but cache is disabled."
            self.render_runtime_updating(self.runtime_update_status)
            return

        try:
            with open(self.runtime_cache_path, "w", encoding="utf-8", newline="\n") as handle:
                handle.write(runtime_code)
        except Exception as exc:
            self.runtime_update_in_progress = False
            self.runtime_update_status = f"Runtime update failed: {exc}"
            self.render_runtime_updating(self.runtime_update_status)
            return

        self.runtime_hash = runtime_hash
        self.runtime_update_status = "Runtime updated. Restarting client..."
        self.render_runtime_updating(self.runtime_update_status)
        self.root.update_idletasks()
        self.root.after(220, self.restart_runtime_from_cache)

    def restart_runtime_from_cache(self) -> None:
        try:
            exec_args = [sys.executable, self.runtime_cache_path, *self.runtime_args]
            os.execv(sys.executable, exec_args)
        except Exception as exc:
            self.runtime_update_in_progress = False
            self.runtime_update_status = f"Runtime restart failed: {exc}"
            self.render_runtime_updating(self.runtime_update_status)
            self.schedule_poll()

    def render_runtime_updating(self, message: str) -> None:
        self.connected = True
        self.connection_badge.config(text="UPDATING", bg="#2f4e71", fg="#d2e9ff")
        self.warning_badge.config(text="RUNTIME", bg="#2b425d", fg="#bbdcff")
        self.mode_badge.config(
            text="DEBUG" if self.client_debug_mode else "PRODUCTION",
            bg="#6a2b2b" if self.client_debug_mode else "#14443e",
            fg="#ffd2d2" if self.client_debug_mode else "#b9f9ea",
        )
        self.time_label.config(text="--:--:-- --")
        self.date_label.config(text="Applying Runtime Update")
        self.subline_label.config(
            text="Client is updating runtime code from server. Please wait."
        )
        self.status_banner.configure(bg=UPDATING_PANEL)
        self.status_banner.configure(
            highlightbackground="#2e5e87", highlightcolor="#2e5e87"
        )
        self.status_label.config(text="RUNTIME UPDATING", fg="#bfe3ff")
        self.detail_label.config(text=message)
        self.set_debug_panel_visible(self.client_debug_mode)
        if self.client_debug_mode:
            self.debug_label.config(
                text=(
                    f"Endpoint={self.runtime_endpoint_url}\n"
                    f"Cache={self.runtime_cache_path or '<disabled>'}\n"
                    f"Check interval={self.runtime_update_check_ms:04d} ms"
                )
            )
        self.apply_background(UPDATING_BG)

    def update_latency_model(
        self, payload: dict, rtt_ms: float, client_send_ms: int, client_recv_ms: int
    ) -> None:
        corrected_rtt_ms, offset_sample_ms = compute_network_sample(
            payload,
            fallback_rtt_ms=rtt_ms,
            client_send_ms=client_send_ms,
            client_recv_ms=client_recv_ms,
        )
        self.latency_samples.append((corrected_rtt_ms, offset_sample_ms))
        target_rtt_ms, target_offset_ms = estimate_low_jitter_target(
            list(self.latency_samples)
        )

        if not self.offset_initialized:
            self.offset_ewma_ms = target_offset_ms
            self.offset_display_ms = target_offset_ms
            self.rtt_ewma_ms = target_rtt_ms
            self.offset_initialized = True
            self.last_offset_update_mono = time.perf_counter()
            return

        best_rtt_ms = min(sample[0] for sample in self.latency_samples)
        alpha_offset = 0.34 if corrected_rtt_ms <= best_rtt_ms + 3.0 else 0.16
        alpha_rtt = 0.25
        self.offset_ewma_ms = (1.0 - alpha_offset) * self.offset_ewma_ms + alpha_offset * target_offset_ms
        self.rtt_ewma_ms = (1.0 - alpha_rtt) * self.rtt_ewma_ms + alpha_rtt * target_rtt_ms

        now_mono = time.perf_counter()
        delta_seconds = max(0.001, now_mono - self.last_offset_update_mono)
        self.last_offset_update_mono = now_mono
        max_step_ms = OFFSET_SLEW_RATE_MS_PER_SEC * delta_seconds
        desync_ms = target_offset_ms - self.offset_display_ms
        self.offset_desync_ms = desync_ms
        desync_gain = (
            OFFSET_DESYNC_GAIN_FAST
            if corrected_rtt_ms <= best_rtt_ms + 3.0
            else OFFSET_DESYNC_GAIN_SLOW
        )
        step_ms = desync_ms * desync_gain
        if abs(step_ms) > max_step_ms:
            step_ms = max_step_ms if step_ms > 0 else -max_step_ms
        self.offset_display_ms += step_ms

    def render_from_payload(self, payload: dict, rtt_ms: float) -> None:
        runtime = payload.get("runtime", {})

        estimated_server_ms = int(time.time() * 1000 + self.offset_display_ms)
        server_dt = datetime.fromtimestamp(estimated_server_ms / 1000.0)
        hour = server_dt.hour
        minute = server_dt.minute
        second = server_dt.second
        hour12 = hour % 12
        if hour12 == 0:
            hour12 = 12
        meridiem = "PM" if hour >= 12 else "AM"
        self.time_label.config(text=f"{hour12:02}:{minute:02}:{second:02} {meridiem}")
        self.date_label.config(text=server_dt.strftime("%A, %B %d %Y"))

        warning_enabled = bool(runtime.get("warning_enabled", False))
        warning_active_count = int(runtime.get("warning_active_count", 0))
        warning_pulse_on = bool(runtime.get("warning_pulse_on", False))
        self.client_debug_mode = bool(payload.get("client_debug_mode", False))

        source = runtime.get("source_label", "unknown")
        triggered = int(runtime.get("triggered_count", 0))
        armed = int(runtime.get("armed_count", 0))
        clients_seen = int(payload.get("clients_seen", 0))

        self.mode_badge.config(
            text="DEBUG" if self.client_debug_mode else "PRODUCTION",
            bg="#6a2b2b" if self.client_debug_mode else "#14443e",
            fg="#ffd2d2" if self.client_debug_mode else "#b9f9ea",
        )
        self.connection_badge.config(
            text=f"RTT {self.rtt_ewma_ms:05.1f} ms",
            bg="#133d32",
            fg="#9ce8cb",
        )
        if warning_enabled and warning_active_count > 0:
            status = f"BELL WARNING ACTIVE ({warning_active_count:03d})"
            status_color = "#ffd8a8"
            bg = WARNING_BG_PULSE if warning_pulse_on else WARNING_BG_SOFT
            self.warning_badge.config(
                text="WARNING PULSE" if warning_pulse_on else "WARNING",
                bg="#8f3e08" if warning_pulse_on else "#65421f",
                fg="#ffd8a8",
            )
            banner_bg = "#7e3b06" if warning_pulse_on else "#5f3a1a"
            banner_border = "#f49b55" if warning_pulse_on else "#bd844a"
            detail_text = "Prepare for bell trigger"
        elif triggered > 0:
            status = f"BELL TRIGGERED ({triggered:03d})"
            status_color = "#ffe6e6"
            bg = "#3a1520"
            self.warning_badge.config(text="TRIGGERED", bg="#71253c", fg="#ffd1dd")
            banner_bg = "#5f2035"
            banner_border = "#cc5f85"
            detail_text = "Active bell event"
        else:
            status = "SCHEDULE NORMAL"
            status_color = "#86e3a2"
            bg = NORMAL_BG
            self.warning_badge.config(text="NORMAL", bg="#1f3652", fg="#b7d3f2")
            banner_bg = NORMAL_PANEL
            banner_border = "#1f3a5b"
            detail_text = "No active warning"

        self.subline_label.config(
            text=(
                f"Source: {source}  |  Clients: {clients_seen:03d}  |  "
                f"Armed: {armed:04d}  |  Triggered: {triggered:04d}"
            )
        )
        self.status_banner.configure(bg=banner_bg)
        self.status_banner.configure(
            highlightbackground=banner_border, highlightcolor=banner_border
        )
        self.status_label.config(text=status, fg=status_color)
        self.detail_label.config(
            text=(
                f"{detail_text}  |  Warning lead {int(runtime.get('warning_lead_time_ms', 0)):06d} ms"
            )
        )
        self.set_debug_panel_visible(self.client_debug_mode)
        if self.client_debug_mode:
            self.debug_label.config(
                text=(
                    f"Offset={self.offset_display_ms:+09.2f} ms  "
                    f"Desync={self.offset_desync_ms:+09.2f} ms  "
                    f"RTT={self.rtt_ewma_ms:07.2f} ms  "
                    f"Poll={self.poll_ms:04d} ms\n"
                    f"WarningEnabled={1 if warning_enabled else 0}  "
                    f"WarningActive={warning_active_count:03d}  "
                    f"Pulse={1 if warning_pulse_on else 0}  "
                    f"Updated={int(runtime.get('updated_unix_ms', 0)):013d}"
                )
            )
        self.apply_background(bg)

    def render_disconnected(self, error_text: str) -> None:
        self.connection_badge.config(text="DISCONNECTED", bg="#622a2a", fg="#ffd0d0")
        self.warning_badge.config(text="OFFLINE", bg="#4f2b2f", fg="#ffd0d0")
        self.mode_badge.config(
            text="DEBUG" if self.client_debug_mode else "PRODUCTION",
            bg="#6a2b2b" if self.client_debug_mode else "#14443e",
            fg="#ffd2d2" if self.client_debug_mode else "#b9f9ea",
        )
        self.time_label.config(text="--:--:-- --")
        self.date_label.config(text="Local Time Server Disconnected")
        self.status_banner.configure(bg="#451f26")
        self.status_banner.configure(
            highlightbackground="#8e4c5b", highlightcolor="#8e4c5b"
        )
        self.subline_label.config(
            text="Waiting for server reconnect..."
        )
        self.status_label.config(text="CONNECTION LOST", fg=TEXT_ALERT)
        self.detail_label.config(
            text=(
                "Could not reach local time server.\n"
                "Check server status and local network link."
            )
        )
        if self.client_debug_mode:
            self.set_debug_panel_visible(True)
            self.debug_label.config(text=f"Last error: {error_text}")
        else:
            self.set_debug_panel_visible(False)
        self.apply_background(DISCONNECTED_BG)

    def set_debug_panel_visible(self, visible: bool) -> None:
        if visible and not self.debug_panel_visible:
            self.debug_panel.pack(fill="x", pady=(8, 0), before=self.footer_label)
            self.debug_panel_visible = True
        elif not visible and self.debug_panel_visible:
            self.debug_panel.pack_forget()
            self.debug_panel_visible = False

    def apply_background(self, bg: str) -> None:
        panel_bg = NORMAL_PANEL if self.connected else "#2c1418"
        self.root.configure(bg=bg)
        self.container.configure(bg=bg)
        self.header_panel.configure(bg=panel_bg)
        self.title_label.configure(bg=panel_bg)
        self.clock_stage.configure(bg=bg)
        self.time_label.configure(bg=bg)
        self.date_label.configure(bg=bg)
        self.subline_label.configure(bg=bg)
        self.footer_label.configure(bg=bg)
        self.status_label.configure(bg=self.status_banner.cget("bg"))
        self.detail_label.configure(bg=self.status_banner.cget("bg"))
        self.debug_panel.configure(bg=panel_bg)
        self.debug_label.configure(bg=panel_bg)


def sha256_hex(payload: str) -> str:
    return hashlib.sha256(payload.encode("utf-8")).hexdigest()


def derive_runtime_endpoint_url(server_state_url: str) -> str:
    parsed = urllib.parse.urlsplit(server_state_url)
    path = parsed.path or ""
    if path.endswith("/v1/state"):
        path = f"{path[:-9]}/v1/client/code"
    elif path.endswith("/state"):
        path = f"{path[:-6]}/client/code"
    else:
        base = path.rsplit("/", 1)[0] if "/" in path else ""
        path = f"{base}/v1/client/code"
    return urllib.parse.urlunsplit((parsed.scheme, parsed.netloc, path, "", ""))


def fetch_runtime_code_text(runtime_endpoint_url: str, timeout_seconds: float) -> str:
    request = urllib.request.Request(
        runtime_endpoint_url,
        headers={"Accept": "text/x-python, text/plain"},
        method="GET",
    )
    with urllib.request.urlopen(request, timeout=max(0.1, timeout_seconds)) as response:
        raw = response.read().decode("utf-8")
    if not raw.strip():
        raise RuntimeError("server returned empty runtime payload")
    return raw


def build_default_client_id() -> str:
    return f"{socket.gethostname()}-{os_getpid()}"


def build_default_client_instance() -> str:
    return f"inst-{uuid.uuid4().hex[:10]}"


def os_getpid() -> int:
    try:
        import os

        return os.getpid()
    except Exception:
        return 0


def fetch_state_once(
    server_url: str,
    client_id: str,
    client_instance: str,
    timeout_seconds: float,
    reported_rtt_ms: float | None = None,
    reported_offset_ms: float | None = None,
    reported_desync_ms: float | None = None,
) -> tuple[dict, float, int, int]:
    separator = "&" if "?" in server_url else "?"
    url = (
        f"{server_url}{separator}client_id={urllib.parse.quote(client_id)}"
        f"&instance_id={urllib.parse.quote(client_instance)}"
    )
    headers = {
        "X-Client-Id": client_id,
        "X-Client-Instance": client_instance,
        "Accept": "application/json",
    }
    if reported_rtt_ms is not None:
        headers["X-Client-Rtt-Ms"] = f"{reported_rtt_ms:.3f}"
    if reported_offset_ms is not None:
        headers["X-Client-Offset-Ms"] = f"{reported_offset_ms:.3f}"
    if reported_desync_ms is not None:
        headers["X-Client-Desync-Ms"] = f"{reported_desync_ms:.3f}"
    req = urllib.request.Request(
        url,
        headers=headers,
        method="GET",
    )
    send_ms = int(time.time() * 1000)
    start = time.perf_counter()
    with urllib.request.urlopen(req, timeout=timeout_seconds) as response:
        raw = response.read().decode("utf-8")
        payload = json.loads(raw)
    end = time.perf_counter()
    recv_ms = int(time.time() * 1000)
    rtt_ms = (end - start) * 1000.0
    return payload, rtt_ms, send_ms, recv_ms


def parse_server_timestamps_ms(payload: dict) -> tuple[float | None, float | None]:
    runtime = payload.get("runtime", {})

    def parse_numeric(value: object) -> float | None:
        try:
            parsed = float(value)
        except (TypeError, ValueError):
            return None
        if not math.isfinite(parsed):
            return None
        return parsed

    request_received_ms = parse_numeric(payload.get("request_received_unix_ms"))
    response_send_ms = parse_numeric(payload.get("response_send_unix_ms"))
    if response_send_ms is None:
        response_send_ms = parse_numeric(payload.get("response_unix_ms"))
    if response_send_ms is None:
        response_send_ms = parse_numeric(runtime.get("updated_unix_ms"))
    return request_received_ms, response_send_ms


def compute_network_sample(
    payload: dict, fallback_rtt_ms: float, client_send_ms: int, client_recv_ms: int
) -> tuple[float, float]:
    request_received_ms, response_send_ms = parse_server_timestamps_ms(payload)
    t1 = float(client_send_ms)
    t4 = float(client_recv_ms)

    offset_ms: float
    rtt_ms: float
    if request_received_ms is not None and response_send_ms is not None:
        t2 = request_received_ms
        t3 = response_send_ms
        rtt_ms = (t4 - t1) - (t3 - t2)
        offset_ms = ((t2 - t1) + (t3 - t4)) / 2.0
        if not math.isfinite(rtt_ms) or rtt_ms < 0:
            rtt_ms = fallback_rtt_ms
    else:
        midpoint_ms = (t1 + t4) / 2.0
        server_ms = response_send_ms if response_send_ms is not None else midpoint_ms
        rtt_ms = fallback_rtt_ms
        offset_ms = server_ms - midpoint_ms

    if not math.isfinite(rtt_ms):
        rtt_ms = fallback_rtt_ms
    if not math.isfinite(offset_ms):
        offset_ms = 0.0

    rtt_ms = max(0.0, min(rtt_ms, MAX_REASONABLE_RTT_MS))
    offset_ms = max(-MAX_REASONABLE_OFFSET_MS, min(offset_ms, MAX_REASONABLE_OFFSET_MS))
    return rtt_ms, offset_ms


def estimate_low_jitter_target(samples: list[tuple[float, float]]) -> tuple[float, float]:
    if not samples:
        return 0.0, 0.0

    sorted_by_rtt = sorted(samples, key=lambda sample: sample[0])
    best_rtt_ms = sorted_by_rtt[0][0]
    selected = [sample for sample in samples if sample[0] <= best_rtt_ms + LOW_RTT_HEADROOM_MS]
    if len(selected) < LOW_RTT_SAMPLE_FLOOR:
        selected = sorted_by_rtt[: min(len(sorted_by_rtt), LOW_RTT_SAMPLE_FLOOR)]

    weight_sum = 0.0
    weighted_rtt = 0.0
    weighted_offset = 0.0
    for rtt_ms, offset_ms in selected:
        weight = 1.0 / ((1.0 + rtt_ms) * (1.0 + rtt_ms))
        weighted_rtt += rtt_ms * weight
        weighted_offset += offset_ms * weight
        weight_sum += weight

    if weight_sum <= 0.0:
        return sorted_by_rtt[0][0], sorted_by_rtt[0][1]
    return weighted_rtt / weight_sum, weighted_offset / weight_sum


def run_self_test(server_url: str, client_id: str, samples: int, timeout_seconds: float) -> int:
    instance_id = f"{build_default_client_instance()}-selftest"
    rtts = []
    offsets = []
    total_samples = max(1, samples)
    index_width = len(str(total_samples))
    for index in range(total_samples):
        payload, rtt_ms, send_ms, recv_ms = fetch_state_once(
            server_url=server_url,
            client_id=client_id,
            client_instance=instance_id,
            timeout_seconds=timeout_seconds,
        )
        corrected_rtt_ms, offset_ms = compute_network_sample(
            payload=payload,
            fallback_rtt_ms=rtt_ms,
            client_send_ms=send_ms,
            client_recv_ms=recv_ms,
        )
        runtime = payload.get("runtime", {})
        rtts.append(corrected_rtt_ms)
        offsets.append(offset_ms)
        print(
            f"[{index + 1:0{index_width}d}/{total_samples:0{index_width}d}] "
            f"RTT={corrected_rtt_ms:07.2f} ms  Offset={offset_ms:+08.2f} ms  "
            f"WarningActive={int(runtime.get('warning_active_count', 0)):03d}"
        )
        time.sleep(0.05)

    avg_rtt = sum(rtts) / len(rtts)
    max_rtt = max(rtts)
    avg_offset = sum(offsets) / len(offsets)
    print("--- Self-test summary ---")
    print(f"Server URL: {server_url}")
    print(f"Samples: {len(rtts):03d}")
    print(f"Average RTT: {avg_rtt:07.2f} ms")
    print(f"Max RTT: {max_rtt:07.2f} ms")
    print(f"Average Clock Offset: {avg_offset:+08.2f} ms")
    print("Self-test PASS")
    return 0


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="BetterClock local-network client",
    )
    parser.add_argument(
        "--server",
        default="http://127.0.0.1:8099/v1/state",
        help="Server state endpoint URL",
    )
    parser.add_argument(
        "--poll-ms",
        type=int,
        default=120,
        help="Poll interval in milliseconds",
    )
    parser.add_argument(
        "--client-id",
        default=build_default_client_id(),
        help="Client identifier shown in server /v1/clients",
    )
    parser.add_argument(
        "--client-instance",
        default=build_default_client_instance(),
        help="Unique client session instance id for multi-client tracking",
    )
    parser.add_argument(
        "--self-test",
        action="store_true",
        help="Run connectivity/latency test only (no GUI). Useful for same-machine and Wi-Fi validation.",
    )
    parser.add_argument(
        "--samples",
        type=int,
        default=12,
        help="Number of samples for --self-test",
    )
    parser.add_argument(
        "--timeout-ms",
        type=int,
        default=1000,
        help="HTTP timeout in milliseconds (used in --self-test)",
    )
    parser.add_argument(
        "--runtime-update-check-ms",
        type=int,
        default=DEFAULT_RUNTIME_UPDATE_CHECK_MS,
        help="Runtime auto-update check interval in milliseconds",
    )
    parser.add_argument(
        "--runtime-endpoint",
        default="",
        help="Optional explicit runtime code endpoint URL",
    )
    return parser.parse_args(argv)


def main(argv: list[str]) -> int:
    args = parse_args(argv)
    if args.self_test:
        return run_self_test(
            server_url=args.server,
            client_id=args.client_id,
            samples=args.samples,
            timeout_seconds=max(0.1, args.timeout_ms / 1000.0),
        )

    globals()["__runtime_update_check_ms__"] = int(max(1000, args.runtime_update_check_ms))
    if args.runtime_endpoint:
        globals()["__runtime_endpoint_url__"] = str(args.runtime_endpoint)

    root = tk.Tk()
    BetterClockClient(
        root=root,
        server_url=args.server,
        poll_ms=args.poll_ms,
        client_id=args.client_id,
        client_instance=args.client_instance,
    )
    root.mainloop()
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
