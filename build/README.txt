BetterClock Build Outputs

windows/
- server/betterclock.exe
- server/alarms.json
- server/openapi.yaml
- python/ (betterclock_time package + scripts)

linux/raspi5/
- server/betterclock (aarch64 Linux binary)
- server/alarms.json
- server/openapi.yaml
- python/ (betterclock_time package + scripts)
- unpacked/server_rebuild/ (Rust server source for native rebuild on Pi)

raspi5/
- Legacy compatibility mirror of linux/raspi5
