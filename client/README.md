## BetterClock Client

Python bootstrap client for the BetterClock server API.

The local `client.py` file is now a loader only. It downloads the current runtime code from the server (`/v1/client/code`) and executes it.

### Run

```powershell
python client.py --bootstrap-base http://127.0.0.1:8099
```

### Same-machine test

Use built-in self-test mode (no GUI):

```powershell
python client.py --bootstrap-base http://127.0.0.1:8099 --self-test --samples 20
```

### Features

- Displays current server time (`HH:MM:SS`).
- Polls local API without keys.
- Background changes when warning mode becomes active.
- Pulse effect follows server `warning_pulse_on`.
- Latency-aware clock display using RTT/offset smoothing (better over Wi-Fi).

### API Docs / Discovery

- OpenAPI YAML: `http://<server-ip>:8099/openapi.yaml`
- API index links: `http://<server-ip>:8099/v1`

### LAN usage

If your server is on another machine in your local network:

```powershell
python client.py --bootstrap-base http://192.168.1.50:8099
```

### Passing runtime args

Any non-bootstrap flags are forwarded to the downloaded runtime:

```powershell
python client.py --bootstrap-base http://192.168.1.50:8099 --poll-ms 80 --client-id kiosk-1
```

### Simple Python Time-Network Client (corrected time + warnings)

Use `base_time_ui_client.py` to join the time network and read corrected time:

```powershell
python base_time_ui_client.py --base-url http://127.0.0.1:8099
```

Fetch once (JSON):

```powershell
python base_time_ui_client.py --base-url http://127.0.0.1:8099 --once --json
```

Full usage and field docs:

- `client/SIMPLE_TIME_CLIENT_GUIDE.md`
