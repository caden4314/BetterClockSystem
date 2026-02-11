import os
import sys
from time import sleep

import betterclock_time as time

CLIENT_NAME = "Raspberry-PI-Group-1"
INSTANCE_ID = "CS_Group1"
PORT = 8099

# Host Pi hotspot defaults are usually 10.42.0.1. Keep localhost as last local fallback.
MANUAL_FALLBACK_HOSTS = ["10.42.0.1", "127.0.0.1"]
RECONNECT_ATTEMPTS = 5
FRAME_SLEEP_SECONDS = 0.025


def clear() -> None:
    os.system("cls" if os.name == "nt" else "clear")


def connect_with_fallback():
    last_error: Exception | None = None

    try:
        client = time.connect_auto(
            name=CLIENT_NAME,
            instance_id=INSTANCE_ID,
            discovery_timeout_seconds=1.2,
            discovery_retries=10,
            mdns_first=True,
            local_first=False,
            use_cache=False,
            timeout_seconds=1.0,
        )
        report = time.scan_report(
            timeout_seconds=0.8,
            retries=5,
            full_scan=False,
            local_first=False,
            use_cache=False,
            mdns_first=True,
            subnet_sweep=True
        )
        return client, "Auto", report
    except Exception as exc:
        last_error = exc

    for host in MANUAL_FALLBACK_HOSTS:
        try:
            client = time.connect(
                host=host,
                port=PORT,
                name=CLIENT_NAME,
                instance_id=INSTANCE_ID,
                timeout_seconds=1.0,
            )
            report = time.scan_report(
                timeout_seconds=0.6,
                retries=1,
                full_scan=False,
                local_first=False,
                use_cache=False,
                mdns_first=True,
                subnet_sweep=True,
            )
            return client, f"Manual({host})", report
        except Exception as exc:
            last_error = exc

    raise RuntimeError(f"failed to connect to BetterClock server: {last_error}")


def main() -> int:
    try:
        client, connection_type, report = connect_with_fallback()
    except Exception as exc:
        print(f"Failed to connect to BetterClock Time Server: {exc}")
        return 1

    ip_info = client.get_device_ip_info()
    conn = client.get_connection_info()

    try:
        while True:
            try:
                corrected_time = client.get_corrected_time()
            except Exception as exc:
                clear()
                print(f"Connection lost ({type(exc).__name__}: {exc})")
                print("Attempting reconnect...")
                recovered = False
                for attempt in range(1, RECONNECT_ATTEMPTS + 1):
                    print(f"Reconnect attempt {attempt}/{RECONNECT_ATTEMPTS}...")
                    try:
                        client, connection_type, report = connect_with_fallback()
                        conn = client.get_connection_info()
                        recovered = True
                        print("Reconnected.")
                        sleep(0.25)
                        break
                    except Exception as reconnect_exc:
                        print(f"  failed: {reconnect_exc}")
                        sleep(min(2.0, 0.35 * attempt))
                if not recovered:
                    print("Unable to reconnect. Retrying in 2s...")
                    sleep(2.0)
                continue

            clear()
            print(
                f"IP Local: {ip_info.resolved_local_ip}, Public: {ip_info.public_ip}, Loopback: {ip_info.loopback_ip}"
            )
            print(
                f"Connected IP: {conn.connection_ip}  Port: {conn.port} Connection Type: {connection_type}"
            )
            print(f"Corrected Unix MS: {corrected_time.corrected_unix_ms}")
            print(f"Corrected Date: {corrected_time.date_text}")
            print(f"Corrected Time: {corrected_time.time_12h}")
            print(f"Offset: {corrected_time.offset_ms:.2f} ms")
            print(f"RTT: {corrected_time.rtt_ms:.2f} ms")
            print(f"Desync: {corrected_time.desync_ms:.2f} ms")
            print("_________________________________")
            print(time.format_scan_report(report))
            sleep(FRAME_SLEEP_SECONDS)
    except KeyboardInterrupt:
        print("Exiting...")
        try:
            result = client.disconnect()
            print(
                f"Disconnected: {result.disconnected}, Client ID: {result.client_id}, Instance ID: {result.instance_id}"
            )
        except Exception as exc:
            print(f"Disconnect call failed: {exc}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

