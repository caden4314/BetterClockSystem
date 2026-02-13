from __future__ import annotations

import ipaddress
import json
import math
import os
import socket
import time
import urllib.parse
import urllib.request
import uuid
from collections import deque
from concurrent.futures import ThreadPoolExecutor, as_completed
from datetime import datetime

from .models import (
    ApiIndexResponse,
    ClientsResponse,
    ConnectionInfo,
    CorrectedTimeSnapshot,
    DeviceIpInfo,
    DiscoveryResult,
    DisconnectResponse,
    PublicClient,
    RuntimeSnapshot,
    ScanReport,
    ScanStep,
    StateResponse,
)

LATENCY_SAMPLE_WINDOW = 24
LOW_RTT_SAMPLE_FLOOR = 5
LOW_RTT_HEADROOM_MS = 8.0
OFFSET_SLEW_RATE_MS_PER_SEC = 240.0
OFFSET_DESYNC_GAIN_FAST = 0.35
OFFSET_DESYNC_GAIN_SLOW = 0.16
MAX_REASONABLE_RTT_MS = 60_000.0
MAX_REASONABLE_OFFSET_MS = 60_000.0
LOCALHOST_IP = "127.0.0.1"
PUBLIC_IP_SERVICES = (
    "https://api.ipify.org",
    "https://ifconfig.me/ip",
    "https://ident.me",
)
DISCOVERY_PROBE_TOKEN = "BETTERCLOCK_DISCOVER_V1"
DISCOVERY_SERVICE_NAME = "betterclock"
MDNS_SERVICE_TYPE = "_betterclock._tcp.local."
DISCOVERY_CACHE_DIR = ".betterclock_time"
DISCOVERY_CACHE_FILE = "discovery_cache.json"


def format_bytes_auto(value: float | int) -> str:
    size = float(value)
    if size < 0:
        size = 0.0
    units = ("B", "KB", "MB", "GB", "TB", "PB")
    unit_index = 0
    while size >= 1024.0 and unit_index < len(units) - 1:
        size /= 1024.0
        unit_index += 1
    if unit_index == 0:
        return f"{int(size)} {units[unit_index]}"
    return f"{size:.2f} {units[unit_index]}"


def format_unix_ms_local(unix_ms: int) -> str:
    if not unix_ms:
        return "--"
    dt = datetime.fromtimestamp(unix_ms / 1000.0)
    return dt.strftime("%Y-%m-%d %H:%M:%S.%f")[:-3]


def _to_int(value: object, default: int = 0) -> int:
    try:
        return int(value)
    except (TypeError, ValueError):
        return default


def _to_float_or_none(value: object) -> float | None:
    try:
        parsed = float(value)
    except (TypeError, ValueError):
        return None
    if not math.isfinite(parsed):
        return None
    return parsed


def _resolve_base_url(host: str, port: int, https: bool) -> str:
    scheme = "https" if https else "http"
    return f"{scheme}://{host}:{port}"


def _connection_host_port_from_url(base_url: str) -> tuple[str, int | None]:
    parsed = urllib.parse.urlsplit(base_url)
    host = parsed.hostname or ""
    return host, parsed.port


def _is_valid_ip(value: str) -> bool:
    try:
        ipaddress.ip_address(value)
    except ValueError:
        return False
    return True


def _detect_lan_ip() -> str | None:
    # This does not send actual traffic; it asks the OS which interface would route outward.
    try:
        with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as sock:
            sock.connect(("8.8.8.8", 80))
            candidate = sock.getsockname()[0]
            if candidate and _is_valid_ip(candidate):
                return candidate
    except OSError:
        pass
    return None


def _resolve_hostname_ip(hostname: str) -> str | None:
    try:
        candidate = socket.gethostbyname(hostname)
    except OSError:
        return None
    if not candidate or not _is_valid_ip(candidate):
        return None
    return candidate


def _lookup_public_ip(timeout_seconds: float) -> str | None:
    for url in PUBLIC_IP_SERVICES:
        try:
            request = urllib.request.Request(
                url,
                headers={
                    "Accept": "text/plain",
                    "User-Agent": "betterclock-time/0.1",
                },
                method="GET",
            )
            with urllib.request.urlopen(request, timeout=timeout_seconds) as response:
                candidate = response.read().decode("utf-8").strip()
            if candidate and _is_valid_ip(candidate):
                return candidate
        except Exception:
            continue
    return None


def _try_healthz(base_url: str, timeout_seconds: float) -> bool:
    request = urllib.request.Request(
        f"{base_url.rstrip('/')}/healthz",
        headers={"Accept": "text/plain"},
        method="GET",
    )
    try:
        with urllib.request.urlopen(request, timeout=max(0.1, timeout_seconds)) as response:
            payload = response.read().decode("utf-8").strip().lower()
        return payload == "ok"
    except Exception:
        return False


def _default_discovery_cache_path() -> str:
    home = os.path.expanduser("~")
    cache_dir = os.path.join(home, DISCOVERY_CACHE_DIR)
    return os.path.join(cache_dir, DISCOVERY_CACHE_FILE)


def _load_cached_discovery(cache_path: str) -> DiscoveryResult | None:
    try:
        with open(cache_path, "r", encoding="utf-8") as handle:
            payload = json.load(handle)
    except Exception:
        return None

    if not isinstance(payload, dict):
        return None
    try:
        base_url = str(payload["base_url"]).strip()
        ip = str(payload["ip"]).strip()
        port = int(payload["port"])
    except Exception:
        return None
    if not base_url or not ip or port <= 0:
        return None

    service = str(payload.get("service", DISCOVERY_SERVICE_NAME))
    version = _to_int(payload.get("version", 1), default=1)
    via = str(payload.get("via", "cache"))
    return DiscoveryResult(
        base_url=base_url.rstrip("/"),
        ip=ip,
        port=port,
        service=service,
        version=version,
        via=via,
    )


def _save_cached_discovery(discovery: DiscoveryResult, cache_path: str) -> None:
    try:
        cache_dir = os.path.dirname(cache_path)
        if cache_dir:
            os.makedirs(cache_dir, exist_ok=True)
        payload = {
            "base_url": discovery.base_url,
            "ip": discovery.ip,
            "port": discovery.port,
            "service": discovery.service,
            "version": discovery.version,
            "via": discovery.via,
            "updated_unix_ms": int(time.time() * 1000),
        }
        with open(cache_path, "w", encoding="utf-8") as handle:
            json.dump(payload, handle)
    except Exception:
        # Cache failures must never break connectivity.
        return


def _is_discovery_payload(payload: dict) -> bool:
    service = str(payload.get("service", "")).strip().lower()
    if service != DISCOVERY_SERVICE_NAME:
        return False
    try:
        int(payload.get("api_port", 0))
        int(payload.get("version", 0))
    except (TypeError, ValueError):
        return False
    return True


def _try_parse_mdns_version(properties: object) -> int:
    if not isinstance(properties, dict):
        return 1
    for key in ("version", b"version"):
        if key in properties:
            value = properties[key]
            if isinstance(value, bytes):
                try:
                    return int(value.decode("utf-8", errors="ignore").strip() or "1")
                except ValueError:
                    return 1
            try:
                return int(value)
            except (TypeError, ValueError):
                return 1
    return 1


def _discover_server_mdns(timeout_seconds: float) -> DiscoveryResult | None:
    try:
        from zeroconf import ServiceBrowser, Zeroconf
    except Exception:
        return None

    timeout_seconds = max(0.1, timeout_seconds)
    result: dict[str, object] = {}

    class Listener:
        def add_service(self, zc: object, service_type: str, name: str) -> None:
            if result:
                return
            try:
                info = zc.get_service_info(
                    service_type,
                    name,
                    timeout=int(timeout_seconds * 1000),
                )
            except Exception:
                return
            if info is None:
                return

            addresses: list[str] = []
            try:
                parsed = info.parsed_addresses()
                if parsed:
                    addresses = [str(item) for item in parsed if item]
            except Exception:
                addresses = []

            if not addresses:
                raw_addresses = getattr(info, "addresses", None) or []
                for raw in raw_addresses:
                    if isinstance(raw, (bytes, bytearray)) and len(raw) == 4:
                        try:
                            addresses.append(socket.inet_ntoa(raw))
                        except OSError:
                            continue

            if not addresses:
                return

            discovered_port = _to_int(getattr(info, "port", 0), default=0)
            if discovered_port <= 0:
                return

            version = _try_parse_mdns_version(getattr(info, "properties", None))
            result["ip"] = addresses[0]
            result["port"] = discovered_port
            result["version"] = version

        def update_service(self, zc: object, service_type: str, name: str) -> None:
            self.add_service(zc, service_type, name)

        def remove_service(self, zc: object, service_type: str, name: str) -> None:
            return

    zeroconf = Zeroconf()
    try:
        ServiceBrowser(zeroconf, MDNS_SERVICE_TYPE, Listener())
        deadline = time.monotonic() + timeout_seconds
        while time.monotonic() < deadline and not result:
            time.sleep(0.05)
    finally:
        zeroconf.close()

    if not result:
        return None

    discovered_ip = str(result["ip"])
    discovered_port = _to_int(result.get("port", 0), default=0)
    if discovered_port <= 0:
        return None
    discovered_version = _to_int(result.get("version", 1), default=1)
    return DiscoveryResult(
        base_url=_resolve_base_url(discovered_ip, discovered_port, https=False),
        ip=discovered_ip,
        port=discovered_port,
        service=DISCOVERY_SERVICE_NAME,
        version=discovered_version,
        via="mdns",
    )


def _is_zeroconf_available() -> bool:
    try:
        from zeroconf import Zeroconf  # noqa: F401
    except Exception:
        return False
    return True


def _build_subnet_candidates(
    lan_ip: str,
    max_hosts: int,
    *,
    sweep_prefix: int = 24,
    sweep_cidr: str | None = None,
) -> tuple[list[str], str | None]:
    try:
        local_addr = ipaddress.ip_address(lan_ip)
    except ValueError:
        return [], None
    if not isinstance(local_addr, ipaddress.IPv4Address):
        return [], None

    if sweep_cidr and sweep_cidr.strip():
        try:
            network = ipaddress.ip_network(sweep_cidr.strip(), strict=False)
        except ValueError:
            return [], None
    else:
        sweep_prefix = max(8, min(30, int(sweep_prefix)))
        network = ipaddress.ip_network(f"{lan_ip}/{sweep_prefix}", strict=False)
    if not isinstance(network, ipaddress.IPv4Network):
        return [], None

    local_text = str(local_addr)
    same_24 = ipaddress.ip_network(f"{lan_ip}/24", strict=False)
    primary: list[str] = []
    secondary: list[str] = []
    for host in network.hosts():
        host_text = str(host)
        if host in same_24:
            primary.append(host_text)
        else:
            secondary.append(host_text)

    candidates = primary + secondary
    if not candidates:
        return [], str(network)

    # Prefer self + common gateway first so same-machine server/client can resolve immediately.
    gateway = f"{local_text.rsplit('.', 1)[0]}.1"
    prioritized: list[str] = []
    if local_text in candidates:
        prioritized.append(local_text)
        candidates.remove(local_text)
    if gateway in candidates:
        prioritized.append(gateway)
        candidates.remove(gateway)
    prioritized.extend(candidates)

    max_hosts = max(1, max_hosts)
    return prioritized[:max_hosts], str(network)


def _discover_server_subnet_sweep(
    *,
    port: int,
    timeout_seconds: float,
    max_hosts: int,
    workers: int,
    sweep_prefix: int = 24,
    sweep_cidr: str | None = None,
) -> tuple[DiscoveryResult | None, str]:
    lan_ip = _detect_lan_ip()
    if not lan_ip:
        return None, "no LAN IP detected for subnet sweep"

    candidates, target_network = _build_subnet_candidates(
        lan_ip,
        max_hosts=max_hosts,
        sweep_prefix=sweep_prefix,
        sweep_cidr=sweep_cidr,
    )
    if not candidates:
        if sweep_cidr and sweep_cidr.strip():
            return None, f"invalid or empty sweep CIDR: {sweep_cidr}"
        return None, f"could not derive subnet candidates from LAN IP {lan_ip}"

    per_host_timeout = min(0.25, max(0.08, timeout_seconds * 0.35))
    worker_count = max(4, min(workers, len(candidates)))
    scanned = 0

    with ThreadPoolExecutor(max_workers=worker_count) as pool:
        futures = {
            pool.submit(
                _try_healthz,
                _resolve_base_url(candidate_ip, port, https=False),
                per_host_timeout,
            ): candidate_ip
            for candidate_ip in candidates
        }
        for future in as_completed(futures):
            scanned += 1
            candidate_ip = futures[future]
            try:
                ok = bool(future.result())
            except Exception:
                ok = False
            if not ok:
                continue

            return (
                DiscoveryResult(
                    base_url=_resolve_base_url(candidate_ip, port, https=False),
                    ip=candidate_ip,
                    port=port,
                    service=DISCOVERY_SERVICE_NAME,
                    version=1,
                    via="subnet-sweep",
                ),
                (
                    f"found server after scanning {scanned}/{len(candidates)} hosts "
                    f"on {target_network or 'target network'}"
                ),
            )

    return None, f"no host responded on {target_network or 'target network'} ({len(candidates)} hosts scanned)"


def _build_scan_step(
    *,
    step: str,
    status: str,
    started_mono: float,
    message: str,
    discovery: DiscoveryResult | None = None,
) -> ScanStep:
    elapsed_ms = int(max(0.0, (time.monotonic() - started_mono) * 1000.0))
    if discovery is None:
        return ScanStep(
            step=step,
            status=status,
            elapsed_ms=elapsed_ms,
            message=message,
            via=None,
            base_url=None,
            ip=None,
            port=None,
        )
    return ScanStep(
        step=step,
        status=status,
        elapsed_ms=elapsed_ms,
        message=message,
        via=discovery.via,
        base_url=discovery.base_url,
        ip=discovery.ip,
        port=discovery.port,
    )


def _discover_server_internal(
    *,
    port: int = 8099,
    timeout_seconds: float = 0.8,
    retries: int = 3,
    broadcast_address: str = "255.255.255.255",
    local_first: bool = True,
    mdns_first: bool = True,
    use_cache: bool = True,
    cache_path: str | None = None,
    subnet_sweep: bool = True,
    sweep_prefix: int = 24,
    sweep_cidr: str | None = None,
    sweep_max_hosts: int = 254,
    sweep_workers: int = 48,
    stop_on_first: bool = True,
    collect_steps: bool = False,
) -> tuple[DiscoveryResult | None, list[ScanStep], str]:
    timeout_seconds = max(0.1, timeout_seconds)
    retries = max(1, retries)
    cache_file = cache_path or _default_discovery_cache_path()
    selected: DiscoveryResult | None = None
    steps: list[ScanStep] = []

    if local_first:
        local_started = time.monotonic()
        local_base_url = _resolve_base_url(LOCALHOST_IP, port, https=False)
        if _try_healthz(local_base_url, timeout_seconds=min(0.35, timeout_seconds)):
            local_result = DiscoveryResult(
                base_url=local_base_url,
                ip=LOCALHOST_IP,
                port=port,
                service=DISCOVERY_SERVICE_NAME,
                version=1,
                via="local-healthz",
            )
            if use_cache:
                _save_cached_discovery(local_result, cache_file)
            if collect_steps:
                steps.append(
                    _build_scan_step(
                        step="local-healthz",
                        status="ok",
                        started_mono=local_started,
                        message="local server is reachable",
                        discovery=local_result,
                    )
                )
            selected = local_result
            if stop_on_first:
                return selected, steps, cache_file
        elif collect_steps:
            steps.append(
                _build_scan_step(
                    step="local-healthz",
                    status="fail",
                    started_mono=local_started,
                    message="local server not reachable on localhost",
                )
            )
    elif collect_steps:
        skipped_started = time.monotonic()
        steps.append(
            _build_scan_step(
                step="local-healthz",
                status="skipped",
                started_mono=skipped_started,
                message="local-first probe disabled",
            )
        )

    if use_cache:
        cache_started = time.monotonic()
        cached = _load_cached_discovery(cache_file)
        if cached is None:
            if collect_steps:
                steps.append(
                    _build_scan_step(
                        step="cache-healthz",
                        status="fail",
                        started_mono=cache_started,
                        message=f"no cache entry at {cache_file}",
                    )
                )
        elif _try_healthz(cached.base_url, timeout_seconds=min(0.35, timeout_seconds)):
            cache_result = DiscoveryResult(
                base_url=cached.base_url,
                ip=cached.ip,
                port=cached.port,
                service=cached.service,
                version=cached.version,
                via="cache-healthz",
            )
            if collect_steps:
                steps.append(
                    _build_scan_step(
                        step="cache-healthz",
                        status="ok",
                        started_mono=cache_started,
                        message="cached server is reachable",
                        discovery=cache_result,
                    )
                )
            if selected is None:
                selected = cache_result
                if stop_on_first:
                    return selected, steps, cache_file
        elif collect_steps:
            steps.append(
                _build_scan_step(
                    step="cache-healthz",
                    status="fail",
                    started_mono=cache_started,
                    message=f"cached server is stale/unreachable: {cached.base_url}",
                )
            )
    elif collect_steps:
        skipped_started = time.monotonic()
        steps.append(
            _build_scan_step(
                step="cache-healthz",
                status="skipped",
                started_mono=skipped_started,
                message="cache lookup disabled",
            )
        )

    if mdns_first:
        mdns_started = time.monotonic()
        mdns_result = _discover_server_mdns(timeout_seconds=timeout_seconds)
        if mdns_result is not None:
            if use_cache:
                _save_cached_discovery(mdns_result, cache_file)
            if collect_steps:
                steps.append(
                    _build_scan_step(
                        step="mdns",
                        status="ok",
                        started_mono=mdns_started,
                        message="mDNS service discovered",
                        discovery=mdns_result,
                    )
                )
            if selected is None:
                selected = mdns_result
                if stop_on_first:
                    return selected, steps, cache_file
        elif collect_steps:
            if _is_zeroconf_available():
                mdns_message = (
                    "no mDNS response from server (check server --mdns-enabled and UDP 5353 "
                    "multicast/firewall rules)"
                )
            else:
                mdns_message = "no mDNS response (install 'zeroconf' for mDNS support)"
            steps.append(
                _build_scan_step(
                    step="mdns",
                    status="fail",
                    started_mono=mdns_started,
                    message=mdns_message,
                )
            )
    elif collect_steps:
        skipped_started = time.monotonic()
        steps.append(
            _build_scan_step(
                step="mdns",
                status="skipped",
                started_mono=skipped_started,
                message="mDNS scan disabled",
            )
        )

    udp_started = time.monotonic()
    probe_bytes = DISCOVERY_PROBE_TOKEN.encode("utf-8")
    udp_result: DiscoveryResult | None = None
    hit_attempt = 0
    last_udp_error = ""
    for attempt in range(1, retries + 1):
        try:
            with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as sock:
                sock.setsockopt(socket.SOL_SOCKET, socket.SO_BROADCAST, 1)
                sock.settimeout(timeout_seconds)

                try:
                    sock.sendto(probe_bytes, (broadcast_address, port))
                except OSError:
                    pass
                try:
                    sock.sendto(probe_bytes, (LOCALHOST_IP, port))
                except OSError:
                    pass

                start = time.monotonic()
                while (time.monotonic() - start) < timeout_seconds:
                    try:
                        raw, source = sock.recvfrom(2048)
                    except socket.timeout:
                        break
                    except OSError as exc:
                        last_udp_error = str(exc)
                        break

                    try:
                        payload = json.loads(raw.decode("utf-8"))
                    except Exception:
                        continue

                    if not isinstance(payload, dict) or not _is_discovery_payload(payload):
                        continue

                    source_ip = str(source[0])
                    discovered_port = _to_int(payload.get("api_port", port), default=port)
                    version = _to_int(payload.get("version", 1), default=1)
                    udp_result = DiscoveryResult(
                        base_url=_resolve_base_url(source_ip, discovered_port, https=False),
                        ip=source_ip,
                        port=discovered_port,
                        service=str(payload.get("service", DISCOVERY_SERVICE_NAME)),
                        version=version,
                        via="udp-broadcast",
                    )
                    hit_attempt = attempt
                    break
                if udp_result is not None:
                    break
        except OSError as exc:
            last_udp_error = str(exc)
            continue

    if udp_result is not None:
        if use_cache:
            _save_cached_discovery(udp_result, cache_file)
        if collect_steps:
            steps.append(
                _build_scan_step(
                    step="udp-broadcast",
                    status="ok",
                    started_mono=udp_started,
                    message=f"discovered over UDP on attempt {hit_attempt}/{retries}",
                    discovery=udp_result,
                )
            )
        if selected is None:
            selected = udp_result
            if stop_on_first:
                return selected, steps, cache_file
    elif collect_steps:
        message = (
            f"no UDP discovery response on port {port}"
            if not last_udp_error
            else f"UDP discovery failed: {last_udp_error}"
        )
        steps.append(
            _build_scan_step(
                step="udp-broadcast",
                status="fail",
                started_mono=udp_started,
                message=message,
            )
        )

    if subnet_sweep:
        sweep_started = time.monotonic()
        sweep_result, sweep_message = _discover_server_subnet_sweep(
            port=port,
            timeout_seconds=timeout_seconds,
            max_hosts=sweep_max_hosts,
            workers=sweep_workers,
            sweep_prefix=sweep_prefix,
            sweep_cidr=sweep_cidr,
        )
        if sweep_result is not None:
            if use_cache:
                _save_cached_discovery(sweep_result, cache_file)
            if collect_steps:
                steps.append(
                    _build_scan_step(
                        step="subnet-sweep",
                        status="ok",
                        started_mono=sweep_started,
                        message=sweep_message,
                        discovery=sweep_result,
                    )
                )
            if selected is None:
                selected = sweep_result
                if stop_on_first:
                    return selected, steps, cache_file
        elif collect_steps:
            steps.append(
                _build_scan_step(
                    step="subnet-sweep",
                    status="fail",
                    started_mono=sweep_started,
                    message=sweep_message,
                )
            )
    elif collect_steps:
        skipped_started = time.monotonic()
        steps.append(
            _build_scan_step(
                step="subnet-sweep",
                status="skipped",
                started_mono=skipped_started,
                message="subnet sweep disabled",
            )
        )

    return selected, steps, cache_file


def discover_server(
    *,
    port: int = 8099,
    timeout_seconds: float = 0.8,
    retries: int = 3,
    broadcast_address: str = "255.255.255.255",
    local_first: bool = True,
    mdns_first: bool = True,
    use_cache: bool = True,
    cache_path: str | None = None,
    subnet_sweep: bool = True,
    sweep_prefix: int = 24,
    sweep_cidr: str | None = None,
    sweep_max_hosts: int = 254,
    sweep_workers: int = 48,
) -> DiscoveryResult | None:
    selected, _, _ = _discover_server_internal(
        port=port,
        timeout_seconds=timeout_seconds,
        retries=retries,
        broadcast_address=broadcast_address,
        local_first=local_first,
        mdns_first=mdns_first,
        use_cache=use_cache,
        cache_path=cache_path,
        subnet_sweep=subnet_sweep,
        sweep_prefix=sweep_prefix,
        sweep_cidr=sweep_cidr,
        sweep_max_hosts=sweep_max_hosts,
        sweep_workers=sweep_workers,
        stop_on_first=True,
        collect_steps=False,
    )
    return selected


def scan_report(
    *,
    port: int = 8099,
    timeout_seconds: float = 0.8,
    retries: int = 3,
    broadcast_address: str = "255.255.255.255",
    local_first: bool = True,
    mdns_first: bool = True,
    use_cache: bool = True,
    cache_path: str | None = None,
    subnet_sweep: bool = True,
    sweep_prefix: int = 24,
    sweep_cidr: str | None = None,
    sweep_max_hosts: int = 254,
    sweep_workers: int = 48,
    full_scan: bool = False,
) -> ScanReport:
    started_unix_ms = int(time.time() * 1000)
    started_mono = time.monotonic()
    selected, steps, cache_file = _discover_server_internal(
        port=port,
        timeout_seconds=timeout_seconds,
        retries=retries,
        broadcast_address=broadcast_address,
        local_first=local_first,
        mdns_first=mdns_first,
        use_cache=use_cache,
        cache_path=cache_path,
        subnet_sweep=subnet_sweep,
        sweep_prefix=sweep_prefix,
        sweep_cidr=sweep_cidr,
        sweep_max_hosts=sweep_max_hosts,
        sweep_workers=sweep_workers,
        stop_on_first=not full_scan,
        collect_steps=True,
    )
    finished_unix_ms = int(time.time() * 1000)
    elapsed_ms = int(max(0.0, (time.monotonic() - started_mono) * 1000.0))
    return ScanReport(
        started_unix_ms=started_unix_ms,
        finished_unix_ms=finished_unix_ms,
        elapsed_ms=elapsed_ms,
        selected=selected,
        steps=steps,
        cache_path=cache_file,
        local_first=local_first,
        mdns_first=mdns_first,
        use_cache=use_cache,
        subnet_sweep=subnet_sweep,
        sweep_prefix=max(8, min(30, sweep_prefix)),
        sweep_cidr=sweep_cidr.strip() if sweep_cidr else None,
        sweep_max_hosts=max(1, sweep_max_hosts),
        sweep_workers=max(1, sweep_workers),
        retries=max(1, retries),
        timeout_seconds=max(0.1, timeout_seconds),
        broadcast_address=broadcast_address,
    )


def format_scan_report(report: ScanReport) -> str:
    lines = [
        "BetterClock Discovery Scan Report",
        (
            f"elapsed={report.elapsed_ms}ms retries={report.retries} "
            f"timeout={report.timeout_seconds:.2f}s cache={'on' if report.use_cache else 'off'} "
            f"mdns={'on' if report.mdns_first else 'off'} local-first={'on' if report.local_first else 'off'} "
            f"sweep={'on' if report.subnet_sweep else 'off'} "
            f"prefix=/{report.sweep_prefix} cidr={report.sweep_cidr or '-'} "
            f"hosts={report.sweep_max_hosts} workers={report.sweep_workers}"
        ),
    ]
    for step in report.steps:
        line = (
            f"- {step.step:<13} {step.status.upper():<7} {step.elapsed_ms:>4}ms | {step.message}"
        )
        if step.base_url:
            line += f" | {step.base_url}"
        lines.append(line)

    if report.selected is None:
        lines.append("Selected: none")
    else:
        lines.append(
            f"Selected: {report.selected.base_url} via {report.selected.via} "
            f"(ip={report.selected.ip}, port={report.selected.port})"
        )
    lines.append(f"Cache path: {report.cache_path}")
    return "\n".join(lines)


class BetterClockTimeClient:
    """
    Importable BetterClock API client.

    Example:
        client = BetterClockTimeClient(host="192.168.1.50", port=8099)
        state = client.get_state()
        corrected = client.get_corrected_time()
    """

    def __init__(
        self,
        *,
        host: str | None = None,
        port: int = 8099,
        https: bool = False,
        local: bool = False,
        base_url: str | None = None,
        client_id: str = "python-time-lib",
        client_name: str | None = None,
        name: str | None = None,
        instance_id: str | None = None,
        timeout_seconds: float = 1.0,
    ) -> None:
        if base_url:
            self.base_url = base_url.strip().rstrip("/")
        else:
            resolved_host = LOCALHOST_IP if local else (host or LOCALHOST_IP)
            self.base_url = _resolve_base_url(host=resolved_host, port=port, https=https)
        self.connection_host, self.connection_port = _connection_host_port_from_url(self.base_url)
        if not self.connection_host:
            self.connection_host = LOCALHOST_IP if local else (host or LOCALHOST_IP)
        if self.connection_port is None:
            self.connection_port = port
        self.local = local
        self.state_url = f"{self.base_url}/v1/state"
        self.clients_url = f"{self.base_url}/v1/clients"
        self.index_url = f"{self.base_url}/v1"
        self.health_url = f"{self.base_url}/healthz"
        self.runtime_code_url = f"{self.base_url}/v1/client/code"
        self.debug_url = f"{self.base_url}/debug"
        self.openapi_url = f"{self.base_url}/openapi.yaml"
        self.timeout_seconds = max(0.1, timeout_seconds)
        selected_name = name if name is not None else client_name
        self.client_id = selected_name.strip() if selected_name and selected_name.strip() else client_id
        self.instance_id = instance_id or f"py-{uuid.uuid4().hex[:10]}"
        self.disconnected = False

        self.offset_initialized = False
        self.offset_display_ms = 0.0
        self.offset_desync_ms = 0.0
        self.rtt_ewma_ms = 0.0
        self.last_offset_update_mono = time.perf_counter()
        self.latency_samples: deque[tuple[float, float]] = deque(
            maxlen=LATENCY_SAMPLE_WINDOW
        )

    @classmethod
    def local(
        cls,
        *,
        port: int = 8099,
        https: bool = False,
        client_id: str = "python-time-lib",
        client_name: str | None = None,
        name: str | None = None,
        instance_id: str | None = None,
        timeout_seconds: float = 1.0,
    ) -> "BetterClockTimeClient":
        return cls(
            host=LOCALHOST_IP,
            port=port,
            https=https,
            local=True,
            client_id=client_id,
            client_name=client_name,
            name=name,
            instance_id=instance_id,
            timeout_seconds=timeout_seconds,
        )

    @classmethod
    def remote(
        cls,
        host: str,
        *,
        port: int = 8099,
        https: bool = False,
        client_id: str = "python-time-lib",
        client_name: str | None = None,
        name: str | None = None,
        instance_id: str | None = None,
        timeout_seconds: float = 1.0,
    ) -> "BetterClockTimeClient":
        return cls(
            host=host,
            port=port,
            https=https,
            local=False,
            client_id=client_id,
            client_name=client_name,
            name=name,
            instance_id=instance_id,
            timeout_seconds=timeout_seconds,
        )

    @classmethod
    def auto(
        cls,
        *,
        port: int = 8099,
        client_id: str = "python-time-lib",
        client_name: str | None = None,
        name: str | None = None,
        instance_id: str | None = None,
        timeout_seconds: float = 1.0,
        discovery_timeout_seconds: float = 0.8,
        discovery_retries: int = 3,
        subnet_sweep: bool = True,
        sweep_prefix: int = 24,
        sweep_cidr: str | None = None,
        sweep_max_hosts: int = 254,
        sweep_workers: int = 48,
    ) -> "BetterClockTimeClient":
        return connect_auto(
            port=port,
            client_id=client_id,
            client_name=client_name,
            name=name,
            instance_id=instance_id,
            timeout_seconds=timeout_seconds,
            discovery_timeout_seconds=discovery_timeout_seconds,
            discovery_retries=discovery_retries,
            subnet_sweep=subnet_sweep,
            sweep_prefix=sweep_prefix,
            sweep_cidr=sweep_cidr,
            sweep_max_hosts=sweep_max_hosts,
            sweep_workers=sweep_workers,
        )

    def _get(
        self,
        url: str,
        *,
        accept: str,
        send_client_headers: bool = True,
        query: dict[str, str] | None = None,
    ) -> tuple[bytes, float, int, int]:
        if self.disconnected:
            raise RuntimeError("client session is disconnected; call reconnect() first")
        full_url = url
        if query:
            encoded = urllib.parse.urlencode(query)
            separator = "&" if "?" in full_url else "?"
            full_url = f"{full_url}{separator}{encoded}"

        headers: dict[str, str] = {"Accept": accept}
        if send_client_headers:
            headers["X-Client-Id"] = self.client_id
            headers["X-Client-Instance"] = self.instance_id
            if self.offset_initialized:
                headers["X-Client-Rtt-Ms"] = f"{self.rtt_ewma_ms:.3f}"
                headers["X-Client-Offset-Ms"] = f"{self.offset_display_ms:.3f}"
                headers["X-Client-Desync-Ms"] = f"{self.offset_desync_ms:.3f}"

        request = urllib.request.Request(full_url, headers=headers, method="GET")
        send_ms = int(time.time() * 1000)
        start = time.perf_counter()
        with urllib.request.urlopen(request, timeout=self.timeout_seconds) as response:
            payload = response.read()
        end = time.perf_counter()
        recv_ms = int(time.time() * 1000)
        rtt_ms = (end - start) * 1000.0
        return payload, rtt_ms, send_ms, recv_ms

    def set_client_id(self, client_name: str) -> None:
        cleaned = client_name.strip()
        if not cleaned:
            raise ValueError("client_name cannot be empty")
        self.client_id = cleaned

    def reconnect(self, *, new_instance: bool = True) -> None:
        if new_instance:
            self.instance_id = f"py-{uuid.uuid4().hex[:10]}"
        self.disconnected = False
        self.offset_initialized = False
        self.offset_display_ms = 0.0
        self.offset_desync_ms = 0.0
        self.rtt_ewma_ms = 0.0
        self.last_offset_update_mono = time.perf_counter()
        self.latency_samples.clear()

    def _state_from_payload(self, payload: dict) -> StateResponse:
        runtime_raw = payload.get("runtime", {})
        runtime = RuntimeSnapshot(
            iso_local=str(runtime_raw.get("iso_local", "")),
            hour=_to_int(runtime_raw.get("hour", 0)),
            minute=_to_int(runtime_raw.get("minute", 0)),
            second=_to_int(runtime_raw.get("second", 0)),
            source_label=str(runtime_raw.get("source_label", "")),
            warning_enabled=bool(runtime_raw.get("warning_enabled", False)),
            warning_active_count=_to_int(runtime_raw.get("warning_active_count", 0)),
            warning_pulse_on=bool(runtime_raw.get("warning_pulse_on", False)),
            warning_lead_time_ms=_to_int(runtime_raw.get("warning_lead_time_ms", 0)),
            warning_pulse_time_ms=_to_int(runtime_raw.get("warning_pulse_time_ms", 0)),
            triggered_count=_to_int(runtime_raw.get("triggered_count", 0)),
            armed_count=_to_int(runtime_raw.get("armed_count", 0)),
            updated_unix_ms=_to_int(runtime_raw.get("updated_unix_ms", 0)),
        )
        return StateResponse(
            runtime=runtime,
            clients_seen=_to_int(payload.get("clients_seen", 0)),
            total_requests=_to_int(payload.get("total_requests", 0)),
            total_in_bytes=_to_int(payload.get("total_in_bytes", 0)),
            total_out_bytes=_to_int(payload.get("total_out_bytes", 0)),
            session_in_bytes_per_sec=_to_float_or_none(
                payload.get("session_in_bytes_per_sec")
            )
            or 0.0,
            session_out_bytes_per_sec=_to_float_or_none(
                payload.get("session_out_bytes_per_sec")
            )
            or 0.0,
            server_started_unix_ms=_to_int(payload.get("server_started_unix_ms", 0)),
            session_first_in_unix_ms=_to_int(payload.get("session_first_in_unix_ms", 0)),
            session_last_in_unix_ms=_to_int(payload.get("session_last_in_unix_ms", 0)),
            session_last_out_unix_ms=_to_int(payload.get("session_last_out_unix_ms", 0)),
            client_debug_mode=bool(payload.get("client_debug_mode", False)),
            request_received_unix_ms=_to_int(payload.get("request_received_unix_ms", 0)),
            response_unix_ms=_to_int(payload.get("response_unix_ms", 0)),
            response_send_unix_ms=_to_int(payload.get("response_send_unix_ms", 0)),
            server_processing_ms=_to_int(payload.get("server_processing_ms", 0)),
            response_iso_local=str(payload.get("response_iso_local", "")),
        )

    def _parse_server_timestamps_ms(self, payload: dict) -> tuple[float | None, float | None]:
        runtime = payload.get("runtime", {})
        request_received_ms = _to_float_or_none(payload.get("request_received_unix_ms"))
        response_send_ms = _to_float_or_none(payload.get("response_send_unix_ms"))
        if response_send_ms is None:
            response_send_ms = _to_float_or_none(payload.get("response_unix_ms"))
        if response_send_ms is None:
            response_send_ms = _to_float_or_none(runtime.get("updated_unix_ms"))
        return request_received_ms, response_send_ms

    def _compute_network_sample(
        self,
        payload: dict,
        fallback_rtt_ms: float,
        client_send_ms: int,
        client_recv_ms: int,
    ) -> tuple[float, float]:
        request_received_ms, response_send_ms = self._parse_server_timestamps_ms(payload)
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

    def _estimate_low_jitter_target(self, samples: list[tuple[float, float]]) -> tuple[float, float]:
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

    def _update_offset_model(self, corrected_rtt_ms: float, offset_sample_ms: float) -> None:
        self.latency_samples.append((corrected_rtt_ms, offset_sample_ms))
        target_rtt_ms, target_offset_ms = self._estimate_low_jitter_target(
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

    def get_api_index(self) -> ApiIndexResponse:
        raw, _, _, _ = self._get(self.index_url, accept="application/json")
        payload = json.loads(raw.decode("utf-8"))
        return ApiIndexResponse(
            api_base=str(payload.get("api_base", "")),
            state_url=str(payload.get("state_url", "")),
            clients_url=str(payload.get("clients_url", "")),
            health_url=str(payload.get("health_url", "")),
            runtime_code_url=str(payload.get("runtime_code_url", "")),
            disconnect_url=str(payload.get("disconnect_url", "")),
            debug_url=str(payload.get("debug_url", "")),
            openapi_url=str(payload.get("openapi_url", "")),
        )

    def get_state(self) -> StateResponse:
        raw, _, _, _ = self._get(
            self.state_url,
            accept="application/json",
            query={"client_id": self.client_id, "instance_id": self.instance_id},
        )
        return self._state_from_payload(json.loads(raw.decode("utf-8")))

    def get_clients(self) -> ClientsResponse:
        raw, _, _, _ = self._get(
            self.clients_url,
            accept="application/json",
            query={"client_id": self.client_id, "instance_id": self.instance_id},
        )
        payload = json.loads(raw.decode("utf-8"))
        clients_raw = payload.get("clients", [])
        clients = [
            PublicClient(
                id=str(item.get("id", "")),
                instance_id=str(item.get("instance_id", "")),
                debug_mode=bool(item.get("debug_mode", False)),
                ip=str(item.get("ip", "")),
                request_count=_to_int(item.get("request_count", 0)),
                first_seen_unix_ms=_to_int(item.get("first_seen_unix_ms", 0)),
                last_seen_unix_ms=_to_int(item.get("last_seen_unix_ms", 0)),
                last_rtt_ms=_to_float_or_none(item.get("last_rtt_ms")),
                last_offset_ms=_to_float_or_none(item.get("last_offset_ms")),
                last_desync_ms=_to_float_or_none(item.get("last_desync_ms")),
                first_in_unix_ms=_to_int(item.get("first_in_unix_ms", 0)),
                last_in_unix_ms=_to_int(item.get("last_in_unix_ms", 0)),
                last_out_unix_ms=_to_int(item.get("last_out_unix_ms", 0)),
                last_in_bytes=_to_int(item.get("last_in_bytes", 0)),
                last_out_bytes=_to_int(item.get("last_out_bytes", 0)),
                total_in_bytes=_to_int(item.get("total_in_bytes", 0)),
                total_out_bytes=_to_int(item.get("total_out_bytes", 0)),
                in_bytes_per_sec=_to_float_or_none(item.get("in_bytes_per_sec")) or 0.0,
                out_bytes_per_sec=_to_float_or_none(item.get("out_bytes_per_sec")) or 0.0,
            )
            for item in clients_raw
        ]
        return ClientsResponse(count=_to_int(payload.get("count", len(clients))), clients=clients)

    def healthz(self) -> bool:
        raw, _, _, _ = self._get(self.health_url, accept="text/plain", send_client_headers=False)
        return raw.decode("utf-8").strip().lower() == "ok"

    def get_runtime_code(self) -> str:
        raw, _, _, _ = self._get(self.runtime_code_url, accept="text/x-python")
        return raw.decode("utf-8")

    def get_openapi_yaml(self) -> str:
        raw, _, _, _ = self._get(self.openapi_url, accept="application/yaml, text/yaml")
        return raw.decode("utf-8")

    def get_debug_html(self) -> str:
        raw, _, _, _ = self._get(self.debug_url, accept="text/html")
        return raw.decode("utf-8")

    def get_corrected_time(self) -> CorrectedTimeSnapshot:
        raw, measured_rtt_ms, send_ms, recv_ms = self._get(
            self.state_url,
            accept="application/json",
            query={"client_id": self.client_id, "instance_id": self.instance_id},
        )
        payload = json.loads(raw.decode("utf-8"))
        corrected_rtt_ms, offset_sample_ms = self._compute_network_sample(
            payload=payload,
            fallback_rtt_ms=measured_rtt_ms,
            client_send_ms=send_ms,
            client_recv_ms=recv_ms,
        )
        self._update_offset_model(corrected_rtt_ms, offset_sample_ms)

        corrected_unix_ms = int(time.time() * 1000 + self.offset_display_ms)
        corrected_dt = datetime.fromtimestamp(corrected_unix_ms / 1000.0)
        hour12 = corrected_dt.hour % 12
        if hour12 == 0:
            hour12 = 12
        meridiem = "PM" if corrected_dt.hour >= 12 else "AM"

        return CorrectedTimeSnapshot(
            corrected_unix_ms=corrected_unix_ms,
            corrected_iso_local=corrected_dt.isoformat(timespec="milliseconds"),
            time_12h=f"{hour12:02}:{corrected_dt.minute:02}:{corrected_dt.second:02} {meridiem}",
            date_text=corrected_dt.strftime("%A, %B %d %Y"),
            rtt_ms=self.rtt_ewma_ms,
            offset_ms=self.offset_display_ms,
            desync_ms=self.offset_desync_ms,
            state=self._state_from_payload(payload),
        )

    def disconnect(self) -> DisconnectResponse:
        raw, _, _, _ = self._get(
            f"{self.base_url}/v1/client/disconnect",
            accept="application/json",
            query={"client_id": self.client_id, "instance_id": self.instance_id},
        )
        payload = json.loads(raw.decode("utf-8"))
        response = DisconnectResponse(
            disconnected=bool(payload.get("disconnected", False)),
            client_id=str(payload.get("client_id", self.client_id)),
            instance_id=str(payload.get("instance_id", self.instance_id)),
        )
        self.disconnected = response.disconnected
        return response

    def get_connection_ip(self) -> str | None:
        # Returns the resolved IP used for the configured connection host.
        if _is_valid_ip(self.connection_host):
            return self.connection_host
        return _resolve_hostname_ip(self.connection_host)

    @property
    def connection_ip(self) -> str | None:
        return self.get_connection_ip()

    def get_connection_info(self) -> ConnectionInfo:
        return ConnectionInfo(
            host=self.connection_host,
            port=self.connection_port,
            base_url=self.base_url,
            local=self.local,
            connection_ip=self.get_connection_ip(),
        )

    def get_public_ip(self, *, timeout_seconds: float | None = None) -> str | None:
        lookup_timeout = (
            self.timeout_seconds if timeout_seconds is None else max(0.1, timeout_seconds)
        )
        return _lookup_public_ip(lookup_timeout)

    def get_device_ip_info(
        self,
        *,
        include_public_ip: bool = True,
        public_timeout_seconds: float | None = None,
    ) -> DeviceIpInfo:
        hostname = socket.gethostname()
        resolved_local_ip = _resolve_hostname_ip(hostname)
        lan_ip = _detect_lan_ip()
        public_ip = None
        if include_public_ip:
            public_ip = self.get_public_ip(timeout_seconds=public_timeout_seconds)
        return DeviceIpInfo(
            hostname=hostname,
            loopback_ip=LOCALHOST_IP,
            resolved_local_ip=resolved_local_ip,
            lan_ip=lan_ip,
            public_ip=public_ip,
        )


def connect(
    host: str | None = None,
    port: int = 8099,
    *,
    local: bool = False,
    https: bool = False,
    client_id: str = "python-time-lib",
    client_name: str | None = None,
    name: str | None = None,
    instance_id: str | None = None,
    timeout_seconds: float = 1.0,
) -> BetterClockTimeClient:
    """
    Convenience initializer so users can do:

        import betterclock_time as bct
        client = bct.connect("192.168.1.50", 8099)
        local_client = bct.connect(local=True)
    """

    return BetterClockTimeClient(
        host=host,
        port=port,
        local=local,
        https=https,
        client_id=client_id,
        client_name=client_name,
        name=name,
        instance_id=instance_id,
        timeout_seconds=timeout_seconds,
    )


def connect_local(
    port: int = 8099,
    *,
    https: bool = False,
    client_id: str = "python-time-lib",
    client_name: str | None = None,
    name: str | None = None,
    instance_id: str | None = None,
    timeout_seconds: float = 1.0,
) -> BetterClockTimeClient:
    return BetterClockTimeClient.local(
        port=port,
        https=https,
        client_id=client_id,
        client_name=client_name,
        name=name,
        instance_id=instance_id,
        timeout_seconds=timeout_seconds,
    )


def init_client(
    *,
    ip: str | None = None,
    port: int = 8099,
    local: bool = False,
    https: bool = False,
    client_id: str = "python-time-lib",
    client_name: str | None = None,
    name: str | None = None,
    instance_id: str | None = None,
    timeout_seconds: float = 1.0,
) -> BetterClockTimeClient:
    """
    Friendly init API:

        init_client(local=True)
        init_client(ip="192.168.1.50", port=8099)
    """

    return BetterClockTimeClient(
        host=ip,
        port=port,
        local=local,
        https=https,
        client_id=client_id,
        client_name=client_name,
        name=name,
        instance_id=instance_id,
        timeout_seconds=timeout_seconds,
    )


def connect_auto(
    *,
    port: int = 8099,
    client_id: str = "python-time-lib",
    client_name: str | None = None,
    name: str | None = None,
    instance_id: str | None = None,
    timeout_seconds: float = 1.0,
    discovery_timeout_seconds: float = 0.8,
    discovery_retries: int = 3,
    broadcast_address: str = "255.255.255.255",
    local_first: bool = True,
    mdns_first: bool = True,
    use_cache: bool = True,
    cache_path: str | None = None,
    subnet_sweep: bool = True,
    sweep_prefix: int = 24,
    sweep_cidr: str | None = None,
    sweep_max_hosts: int = 254,
    sweep_workers: int = 48,
) -> BetterClockTimeClient:
    discovery = discover_server(
        port=port,
        timeout_seconds=discovery_timeout_seconds,
        retries=discovery_retries,
        broadcast_address=broadcast_address,
        local_first=local_first,
        mdns_first=mdns_first,
        use_cache=use_cache,
        cache_path=cache_path,
        subnet_sweep=subnet_sweep,
        sweep_prefix=sweep_prefix,
        sweep_cidr=sweep_cidr,
        sweep_max_hosts=sweep_max_hosts,
        sweep_workers=sweep_workers,
    )
    if discovery is None:
        raise RuntimeError(f"no BetterClock server discovered on local network (port {port})")

    return BetterClockTimeClient(
        base_url=discovery.base_url,
        local=discovery.ip == LOCALHOST_IP,
        client_id=client_id,
        client_name=client_name,
        name=name,
        instance_id=instance_id,
        timeout_seconds=timeout_seconds,
    )
