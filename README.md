# BetterClock System

BetterClock is a local-network time server + client system for synchronized clocks, warnings, and bell/timer displays.

## Repo Layout

- `server/` Rust BetterClock server (UI + local API)
- `python/` Python `betterclock_time` client library + examples
- `client/` additional client scripts/docs
- `build/windows/` packaged Windows server + Python bundle
- `build/linux/raspi5/` packaged Raspberry Pi 5 Linux server + Python bundle + unpacked rebuild source

## Quick Start

### 1) Run server (Windows)

```powershell
cd server
cargo run --release -- --api-bind 0.0.0.0 --api-port 8099 --mdns-enabled true
```

### 2) Install Python client lib

```powershell
cd python
python install.py
```

### 3) Run Python test client

```powershell
python test.py
```

## Build Outputs

Current artifacts are organized by platform:

- Windows: `build/windows/server/betterclock.exe`
- Linux (Raspberry Pi 5 aarch64): `build/linux/raspi5/server/betterclock`

Bundled Python package/examples are included in both platform folders.

## API

OpenAPI spec:

- `server/openapi.yaml`

## Release Packaging

Release bundles are produced under:

- `build/releases/`

Expected bundles:

- `betterclock-windows-x86_64-v<version>.zip`
- `betterclock-linux-raspi5-aarch64-v<version>.tar.gz`
- `betterclock-source-v<version>.zip`

## Notes

- Default API port is `8099`.
- Discovery supports local check, cache, mDNS, UDP broadcast, and subnet sweep fallback.
- For restrictive networks (guest Wi-Fi), hotspot mode and manual fallback hosts are supported in the Python test client.
