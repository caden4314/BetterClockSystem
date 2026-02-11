from __future__ import annotations

from dataclasses import dataclass


@dataclass
class RuntimeSnapshot:
    iso_local: str
    hour: int
    minute: int
    second: int
    source_label: str
    warning_enabled: bool
    warning_active_count: int
    warning_pulse_on: bool
    warning_lead_time_ms: int
    warning_pulse_time_ms: int
    triggered_count: int
    armed_count: int
    updated_unix_ms: int


@dataclass
class StateResponse:
    runtime: RuntimeSnapshot
    clients_seen: int
    total_requests: int
    client_debug_mode: bool
    request_received_unix_ms: int
    response_unix_ms: int
    response_send_unix_ms: int
    server_processing_ms: int
    response_iso_local: str


@dataclass
class PublicClient:
    id: str
    instance_id: str
    debug_mode: bool
    ip: str
    request_count: int
    first_seen_unix_ms: int
    last_seen_unix_ms: int
    last_rtt_ms: float | None
    last_offset_ms: float | None
    last_desync_ms: float | None


@dataclass
class ClientsResponse:
    count: int
    clients: list[PublicClient]


@dataclass
class ApiIndexResponse:
    api_base: str
    state_url: str
    clients_url: str
    health_url: str
    runtime_code_url: str
    disconnect_url: str
    debug_url: str
    openapi_url: str


@dataclass
class DisconnectResponse:
    disconnected: bool
    client_id: str
    instance_id: str


@dataclass
class DeviceIpInfo:
    hostname: str
    loopback_ip: str
    resolved_local_ip: str | None
    lan_ip: str | None
    public_ip: str | None


@dataclass
class ConnectionInfo:
    host: str
    port: int | None
    base_url: str
    local: bool
    connection_ip: str | None


@dataclass
class DiscoveryResult:
    base_url: str
    ip: str
    port: int
    service: str
    version: int
    via: str


@dataclass
class ScanStep:
    step: str
    status: str
    elapsed_ms: int
    message: str
    via: str | None
    base_url: str | None
    ip: str | None
    port: int | None


@dataclass
class ScanReport:
    started_unix_ms: int
    finished_unix_ms: int
    elapsed_ms: int
    selected: DiscoveryResult | None
    steps: list[ScanStep]
    cache_path: str
    local_first: bool
    mdns_first: bool
    use_cache: bool
    subnet_sweep: bool
    sweep_prefix: int
    sweep_cidr: str | None
    sweep_max_hosts: int
    sweep_workers: int
    retries: int
    timeout_seconds: float
    broadcast_address: str


@dataclass
class CorrectedTimeSnapshot:
    corrected_unix_ms: int
    corrected_iso_local: str
    time_12h: str
    date_text: str
    rtt_ms: float
    offset_ms: float
    desync_ms: float
    state: StateResponse
