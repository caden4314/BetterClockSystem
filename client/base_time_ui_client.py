#!/usr/bin/env python3
"""
Simple BetterClock time-network client.

Goal:
- Let a non-expert join the local time network with one command.
- Return corrected time (RTT/offset aware), not just raw server time.
- Expose warning/bell fields in an easy snapshot object.
"""

from __future__ import annotations

import argparse
import json
import math
import time
import urllib.parse
import urllib.request
import uuid
from collections import deque
from dataclasses import dataclass
from datetime import datetime

LATENCY_SAMPLE_WINDOW = 24
LOW_RTT_SAMPLE_FLOOR = 5
LOW_RTT_HEADROOM_MS = 8.0
OFFSET_SLEW_RATE_MS_PER_SEC = 240.0
OFFSET_DESYNC_GAIN_FAST = 0.35
OFFSET_DESYNC_GAIN_SLOW = 0.16
MAX_REASONABLE_RTT_MS = 60_000.0
MAX_REASONABLE_OFFSET_MS = 60_000.0


def _resolve_state_url(base_url: str) -> str:
    base = base_url.strip().rstrip("/")
    if base.endswith("/v1/state") or base.endswith("/state"):
        return base
    if base.endswith("/v1"):
        return f"{base}/state"
    return f"{base}/v1/state"


@dataclass
class TimeNetworkSnapshot:
    corrected_unix_ms: int
    corrected_iso_local: str
    time_12h: str
    date_text: str
    source_label: str
    warning_enabled: bool
    warning_active_count: int
    warning_pulse_on: bool
    warning_lead_time_ms: int
    warning_pulse_time_ms: int
    armed_count: int
    triggered_count: int
    clients_seen: int
    total_requests: int
    rtt_ms: float
    offset_ms: float
    desync_ms: float


class SimpleTimeNetworkClient:
    """
    Minimal, dependency-free API client.

    Usage:
        client = SimpleTimeNetworkClient("http://127.0.0.1:8099")
        snapshot = client.poll()
        print(snapshot.time_12h, snapshot.warning_active_count)
    """

    def __init__(
        self,
        base_url: str,
        *,
        client_id: str = "simple-time-ui",
        instance_id: str | None = None,
        timeout_seconds: float = 1.0,
    ) -> None:
        self.state_url = _resolve_state_url(base_url)
        self.timeout_seconds = max(0.1, timeout_seconds)
        self.client_id = client_id
        self.instance_id = instance_id or f"py-{uuid.uuid4().hex[:10]}"
        self.offset_initialized = False
        self.offset_display_ms = 0.0
        self.offset_desync_ms = 0.0
        self.rtt_ewma_ms = 0.0
        self.last_offset_update_mono = time.perf_counter()
        self.latency_samples: deque[tuple[float, float]] = deque(
            maxlen=LATENCY_SAMPLE_WINDOW
        )
        self.session_start_unix_ms = int(time.time() * 1000)
        self.last_in_unix_ms = 0
        self.last_out_unix_ms = 0
        self.last_request_bytes = 0
        self.last_response_bytes = 0
        self.total_out_bytes = 0
        self.total_in_bytes = 0
        self.poll_count = 0

    def poll(self) -> TimeNetworkSnapshot:
        payload, raw_rtt_ms, send_ms, recv_ms = self._fetch_state_once()
        corrected_rtt_ms, offset_sample_ms = _compute_network_sample(
            payload=payload,
            fallback_rtt_ms=raw_rtt_ms,
            client_send_ms=send_ms,
            client_recv_ms=recv_ms,
        )
        self._update_offset_model(corrected_rtt_ms, offset_sample_ms)

        corrected_unix_ms = int(time.time() * 1000 + self.offset_display_ms)
        corrected_dt = datetime.fromtimestamp(corrected_unix_ms / 1000.0)
        runtime = payload.get("runtime", {})
        hour12 = corrected_dt.hour % 12
        if hour12 == 0:
            hour12 = 12
        meridiem = "PM" if corrected_dt.hour >= 12 else "AM"

        return TimeNetworkSnapshot(
            corrected_unix_ms=corrected_unix_ms,
            corrected_iso_local=corrected_dt.isoformat(timespec="milliseconds"),
            time_12h=f"{hour12:02}:{corrected_dt.minute:02}:{corrected_dt.second:02} {meridiem}",
            date_text=corrected_dt.strftime("%A, %B %d %Y"),
            source_label=str(runtime.get("source_label", "")),
            warning_enabled=bool(runtime.get("warning_enabled", False)),
            warning_active_count=int(runtime.get("warning_active_count", 0)),
            warning_pulse_on=bool(runtime.get("warning_pulse_on", False)),
            warning_lead_time_ms=int(runtime.get("warning_lead_time_ms", 0)),
            warning_pulse_time_ms=int(runtime.get("warning_pulse_time_ms", 0)),
            armed_count=int(runtime.get("armed_count", 0)),
            triggered_count=int(runtime.get("triggered_count", 0)),
            clients_seen=int(payload.get("clients_seen", 0)),
            total_requests=int(payload.get("total_requests", 0)),
            rtt_ms=self.rtt_ewma_ms,
            offset_ms=self.offset_display_ms,
            desync_ms=self.offset_desync_ms,
        )

    def _fetch_state_once(self) -> tuple[dict, float, int, int]:
        query = urllib.parse.urlencode(
            {
                "client_id": self.client_id,
                "instance_id": self.instance_id,
            }
        )
        separator = "&" if "?" in self.state_url else "?"
        url = f"{self.state_url}{separator}{query}"

        headers = {
            "Accept": "application/json",
            "X-Client-Id": self.client_id,
            "X-Client-Instance": self.instance_id,
        }
        if self.offset_initialized:
            headers["X-Client-Rtt-Ms"] = f"{self.rtt_ewma_ms:.3f}"
            headers["X-Client-Offset-Ms"] = f"{self.offset_display_ms:.3f}"
            headers["X-Client-Desync-Ms"] = f"{self.offset_desync_ms:.3f}"

        request = urllib.request.Request(url, headers=headers, method="GET")
        request_bytes = _estimate_http_request_bytes(url, headers)
        send_ms = int(time.time() * 1000)
        self.last_request_bytes = request_bytes
        self.total_out_bytes += request_bytes
        self.last_out_unix_ms = send_ms
        start = time.perf_counter()
        with urllib.request.urlopen(request, timeout=self.timeout_seconds) as response:
            raw = response.read()
            payload = json.loads(raw.decode("utf-8"))
        end = time.perf_counter()
        recv_ms = int(time.time() * 1000)
        response_bytes = len(raw)
        self.last_response_bytes = response_bytes
        self.total_in_bytes += response_bytes
        self.last_in_unix_ms = recv_ms
        self.poll_count += 1
        raw_rtt_ms = (end - start) * 1000.0
        return payload, raw_rtt_ms, send_ms, recv_ms

    def _update_offset_model(self, corrected_rtt_ms: float, offset_sample_ms: float) -> None:
        self.latency_samples.append((corrected_rtt_ms, offset_sample_ms))
        target_rtt_ms, target_offset_ms = _estimate_low_jitter_target(
            list(self.latency_samples)
        )

        if not self.offset_initialized:
            self.offset_display_ms = target_offset_ms
            self.rtt_ewma_ms = target_rtt_ms
            self.offset_desync_ms = 0.0
            self.offset_initialized = True
            self.last_offset_update_mono = time.perf_counter()
            return

        best_rtt_ms = min(sample[0] for sample in self.latency_samples)
        alpha_rtt = 0.25
        self.rtt_ewma_ms = (
            (1.0 - alpha_rtt) * self.rtt_ewma_ms + alpha_rtt * target_rtt_ms
        )

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


def _parse_server_timestamps_ms(payload: dict) -> tuple[float | None, float | None]:
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


def _estimate_http_request_bytes(url: str, headers: dict[str, str]) -> int:
    parsed = urllib.parse.urlsplit(url)
    path_query = parsed.path or "/"
    if parsed.query:
        path_query = f"{path_query}?{parsed.query}"
    request_line = f"GET {path_query} HTTP/1.1\r\n"
    host_line = f"Host: {parsed.netloc}\r\n" if parsed.netloc else ""
    header_lines = "".join(f"{key}: {value}\r\n" for key, value in headers.items())
    return len((request_line + host_line + header_lines + "\r\n").encode("utf-8"))


def _compute_network_sample(
    payload: dict, fallback_rtt_ms: float, client_send_ms: int, client_recv_ms: int
) -> tuple[float, float]:
    request_received_ms, response_send_ms = _parse_server_timestamps_ms(payload)
    t1 = float(client_send_ms)
    t4 = float(client_recv_ms)

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


def _estimate_low_jitter_target(samples: list[tuple[float, float]]) -> tuple[float, float]:
    if not samples:
        return 0.0, 0.0

    sorted_by_rtt = sorted(samples, key=lambda sample: sample[0])
    best_rtt_ms = sorted_by_rtt[0][0]
    selected = [
        sample for sample in samples if sample[0] <= best_rtt_ms + LOW_RTT_HEADROOM_MS
    ]
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


def format_snapshot_line(snapshot: TimeNetworkSnapshot) -> str:
    warning_label = (
        f"WARNING({snapshot.warning_active_count:03d})"
        if snapshot.warning_enabled and snapshot.warning_active_count > 0
        else "NORMAL"
    )
    pulse_label = "PULSE" if snapshot.warning_pulse_on else "steady"
    return (
        f"{snapshot.time_12h} | {snapshot.date_text} | {warning_label} {pulse_label} | "
        f"offset={snapshot.offset_ms:+08.2f}ms desync={snapshot.desync_ms:+08.2f}ms "
        f"rtt={snapshot.rtt_ms:06.2f}ms | source={snapshot.source_label}"
    )


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Simple BetterClock client for corrected network time + warning status."
    )
    parser.add_argument(
        "--base-url",
        default="http://127.0.0.1:8099",
        help="Server URL (base or /v1/state endpoint).",
    )
    parser.add_argument(
        "--poll-ms",
        type=int,
        default=250,
        help="Polling interval in milliseconds.",
    )
    parser.add_argument(
        "--client-id",
        default="simple-time-ui",
        help="Client ID reported to the server.",
    )
    parser.add_argument(
        "--once",
        action="store_true",
        help="Fetch one corrected snapshot and exit.",
    )
    parser.add_argument(
        "--json",
        action="store_true",
        help="Print snapshot as JSON instead of formatted text.",
    )
    return parser.parse_args()


def _print_snapshot(snapshot: TimeNetworkSnapshot, as_json: bool) -> None:
    if as_json:
        print(json.dumps(snapshot.__dict__, indent=2))
        return
    print(format_snapshot_line(snapshot))


def main() -> int:
    args = parse_args()
    client = SimpleTimeNetworkClient(base_url=args.base_url, client_id=args.client_id)
    interval_seconds = max(50, args.poll_ms) / 1000.0

    if args.once:
        _print_snapshot(client.poll(), args.json)
        return 0

    print(f"Connected to {_resolve_state_url(args.base_url)}")
    print("Streaming corrected time snapshots. Press Ctrl+C to stop.")
    try:
        while True:
            _print_snapshot(client.poll(), args.json)
            time.sleep(interval_seconds)
    except KeyboardInterrupt:
        return 0


if __name__ == "__main__":
    raise SystemExit(main())
