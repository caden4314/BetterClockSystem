# Simple Time Network Client Guide

This guide is for users who just want to join the BetterClock time network and read corrected time.

## 1) Quick start

Run from the `client` folder:

```powershell
python base_time_ui_client.py --base-url http://127.0.0.1:8099
```

For a server on your LAN:

```powershell
python base_time_ui_client.py --base-url http://192.168.1.50:8099
```

One snapshot only:

```powershell
python base_time_ui_client.py --base-url http://192.168.1.50:8099 --once
```

JSON output:

```powershell
python base_time_ui_client.py --base-url http://192.168.1.50:8099 --once --json
```

## 2) What "corrected time" means

The script does not trust raw server timestamps directly. It calculates:

- network RTT
- estimated clock offset
- smoothed offset correction

This gives a corrected local display time that is more stable over Wi-Fi than naive polling.

## 3) Fields you can use in your own UI

`TimeNetworkSnapshot` (returned by `SimpleTimeNetworkClient.poll()`) includes:

- `time_12h`: display-ready time string.
- `date_text`: display-ready date string.
- `corrected_unix_ms`: corrected epoch time in milliseconds.
- `corrected_iso_local`: corrected local ISO timestamp.
- `warning_enabled`: warning system enabled/disabled.
- `warning_active_count`: number of active warning windows.
- `warning_pulse_on`: pulse state for warning visuals.
- `warning_lead_time_ms`: configured warning lead time.
- `warning_pulse_time_ms`: configured pulse cadence.
- `armed_count`: scheduled events waiting.
- `triggered_count`: active triggered events.
- `rtt_ms`: smoothed round-trip time.
- `offset_ms`: applied clock correction.
- `desync_ms`: remaining correction error.

## 4) Build your own UI

Minimal pattern:

```python
from base_time_ui_client import SimpleTimeNetworkClient

client = SimpleTimeNetworkClient("http://192.168.1.50:8099", client_id="hall-display-1")
snapshot = client.poll()
print(snapshot.time_12h, snapshot.warning_pulse_on)
```

Call `poll()` at your own refresh rate (for example 4 to 10 times per second) and update UI colors based on:

- `warning_active_count > 0`
- `warning_pulse_on`

## 5) API docs

- Discovery: `http://<server-ip>:8099/v1`
- OpenAPI: `http://<server-ip>:8099/openapi.yaml`

## 6) Importable Python library

There is now a reusable import library in `python/`:

```powershell
python -m pip install -e ./python
```

Then in your own project:

```python
import betterclock_time as bct

client = bct.connect("192.168.1.50", 8099)
snap = client.get_corrected_time()
print(snap.time_12h, snap.state.runtime.warning_active_count)
```
