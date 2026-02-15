BetterClock Build Outputs

windows/
- server/betterclock.exe
- server/alarms.json
- server/openapi.yaml
- python/ (betterclock_time package + scripts)

linux/raspi5/
- server/betterclock (best-effort aarch64 Linux prebuilt)
- server/alarms.json
- server/openapi.yaml
- python/ (betterclock_time package + scripts)
- unpacked/server_rebuild/ (Rust server source for native rebuild on Pi)

raspi5/
- Legacy compatibility mirror of linux/raspi5

If the prebuilt Linux binary fails on your Pi:
1) cd linux/raspi5/unpacked/server_rebuild
2) cargo build --release
3) use target/release/betterclock
