# betterclock-time

Simple Python import library for BetterClock local-network API.

## Install (local repo)

From repo root:

```powershell
python -m pip install -e ./python
```

Or from inside `python/`:

```powershell
python install.py
```

## Quick start

```python
import betterclock_time as bct

client = bct.connect("192.168.1.50", 8099, name="hallway-display-01")
corrected = client.get_corrected_time()

print(corrected.time_12h)
print(corrected.state.runtime.warning_active_count)
print(corrected.offset_ms, corrected.desync_ms, corrected.rtt_ms)
```

Local server shortcut:

```python
import betterclock_time as bct

client = bct.connect_local()
# or:
client = bct.connect(local=True)
# or:
client = bct.init_client(local=True, name="local-test-screen")
```

Hardcoded IP/port init:

```python
import betterclock_time as bct

client = bct.init_client(ip="192.168.1.50", port=8099, name="gym-clock")
```

Automatic LAN discovery (no IP needed):

```python
import betterclock_time as bct

client = bct.connect_auto(name="hall-display-auto")
# or classmethod:
client = bct.BetterClockTimeClient.auto(name="hall-display-auto")

# Wider subnet sweep examples:
client = bct.connect_auto(name="hall-display-auto", sweep_prefix=23)  # scans /23
client = bct.connect_auto(name="hall-display-auto", sweep_cidr="10.0.0.0/16")
```

Discovery diagnostics report:

```python
import betterclock_time as bct

report = bct.scan_report()
print(bct.format_scan_report(report))

# Optional: run every discovery stage even after first hit
full = bct.scan_report(full_scan=True)
print(bct.format_scan_report(full))
```

Discovery order in `connect_auto()`:
1. local health check (`127.0.0.1`)
2. mDNS/Bonjour (`_betterclock._tcp.local.`) if `zeroconf` is installed
3. UDP discovery fallback (`BETTERCLOCK_DISCOVER_V1`)
4. Subnet sweep fallback (parallel `/24` health checks on port `8099`)

It also caches the last successful server in:

- `~/.betterclock_time/discovery_cache.json`

So reconnect is fast even when broadcast/mDNS is flaky (common on hotspots).

Optional mDNS dependency:

```powershell
python -m pip install zeroconf
```

Hotspot / no port-forward note:

- No port forwarding is required for local LAN/hotspot operation.
- If hotspot/AP client isolation is enabled, devices may be blocked from talking to each other; auto-discovery cannot bypass that.

## Main API

- `bct.connect(host, port, ...)` -> `BetterClockTimeClient`
- `bct.connect_auto(...)` -> auto-discover BetterClock on local network
- `bct.discover_server(...)` -> discovery-only, returns discovered base URL and IP
- `bct.scan_report(...)` -> detailed scan report across local/cache/mDNS/UDP/subnet-sweep probes
- `bct.format_scan_report(report)` -> readable text report for logs/console
- `bct.connect(local=True, ...)` -> local `127.0.0.1` initializer
- `bct.connect_local(...)` -> explicit local initializer
- `bct.init_client(ip=..., port=..., local=...)` -> user-friendly init wrapper
- `name="..."` or `client_name="..."` -> sets client ID shown on server
- `client.get_corrected_time()` -> corrected time + full state
- `client.get_state()` -> raw `/v1/state`
- `client.get_clients()` -> `/v1/clients`
- `client.get_api_index()` -> `/v1`
- `client.healthz()` -> `/healthz`
- `client.get_runtime_code()` -> `/v1/client/code`
- `client.get_openapi_yaml()` -> `/openapi.yaml`
- `client.get_debug_html()` -> `/debug`
- `client.disconnect()` -> remove this client session from server active list
- `client.reconnect()` -> reopen session after `disconnect()`
- `client.get_device_ip_info()` -> loopback + LAN + optional public IP
- `client.get_public_ip()` -> public WAN IP only
- `client.get_connection_ip()` / `client.connection_ip` -> current server connection IP
- `client.get_connection_info()` -> host/port/base_url + resolved connection IP

Subnet sweep tuning:

- `sweep_prefix` -> CIDR prefix used from local LAN IP (default `24`, min `8`, max `30`)
- `sweep_cidr` -> explicit CIDR override (example `"10.0.0.0/16"`); takes priority over `sweep_prefix`
- `sweep_max_hosts` -> max hosts to probe from that subnet (default `254`)
- `sweep_workers` -> parallel probe workers (default `48`)

## Data objects (vars)

Everything is returned as dataclasses, so users can access values as vars:

```python
snap = client.get_corrected_time()

time_text = snap.time_12h
date_text = snap.date_text
source = snap.state.runtime.source_label
warning_on = snap.state.runtime.warning_active_count > 0
pulse_on = snap.state.runtime.warning_pulse_on
lead_ms = snap.state.runtime.warning_lead_time_ms
pulse_ms = snap.state.runtime.warning_pulse_time_ms
armed = snap.state.runtime.armed_count
triggered = snap.state.runtime.triggered_count

# graceful disconnect when your app closes
result = client.disconnect()
print(result.disconnected, result.client_id, result.instance_id)

# device/network/public IP info
ip_info = client.get_device_ip_info()
print(ip_info.loopback_ip, ip_info.lan_ip, ip_info.public_ip)

# current server connection target IP
print(client.connection_ip)
conn = client.get_connection_info()
print(conn.host, conn.port, conn.connection_ip)

# discovery details if you only want to find server without connecting
found = bct.discover_server()
if found:
    print(found.base_url, found.ip, found.via)
```
