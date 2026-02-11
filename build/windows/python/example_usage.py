#!/usr/bin/env python3
from __future__ import annotations

import argparse
import time

import betterclock_time as bct


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Example usage for betterclock-time")
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--port", type=int, default=8099)
    parser.add_argument("--local", action="store_true")
    parser.add_argument("--auto", action="store_true")
    parser.add_argument("--client-name", default="example-client")
    parser.add_argument("--poll-ms", type=int, default=250)
    parser.add_argument("--once", action="store_true")
    parser.add_argument("--disconnect-on-exit", action="store_true")
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    if args.auto:
        client = bct.connect_auto(port=args.port, name=args.client_name)
    elif args.local:
        client = bct.connect_local(port=args.port, client_name=args.client_name)
    else:
        client = bct.connect(args.host, args.port, client_name=args.client_name)

    if args.once:
        snap = client.get_corrected_time()
        print(
            f"{snap.time_12h} | warning={snap.state.runtime.warning_active_count} "
            f"pulse={int(snap.state.runtime.warning_pulse_on)} "
            f"offset={snap.offset_ms:+.2f}ms rtt={snap.rtt_ms:.2f}ms"
        )
        if args.disconnect_on_exit:
            result = client.disconnect()
            print(f"disconnect={int(result.disconnected)} id={result.client_id} inst={result.instance_id}")
        return 0

    poll_seconds = max(0.05, args.poll_ms / 1000.0)
    try:
        while True:
            snap = client.get_corrected_time()
            print(
                f"{snap.time_12h} | {snap.date_text} | "
                f"warn={snap.state.runtime.warning_active_count:03d} "
                f"pulse={int(snap.state.runtime.warning_pulse_on)} | "
                f"offset={snap.offset_ms:+07.2f}ms desync={snap.desync_ms:+07.2f}ms "
                f"rtt={snap.rtt_ms:06.2f}ms"
            )
            time.sleep(poll_seconds)
    finally:
        if args.disconnect_on_exit:
            result = client.disconnect()
            print(f"disconnect={int(result.disconnected)} id={result.client_id} inst={result.instance_id}")


if __name__ == "__main__":
    raise SystemExit(main())
