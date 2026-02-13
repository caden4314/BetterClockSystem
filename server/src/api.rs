use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, UdpSocket};
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use anyhow::Result;
use chrono::Local;
use mdns_sd::{ServiceDaemon, ServiceInfo};
use serde::Serialize;
use tiny_http::{Header, Method, Response, Server, StatusCode};

pub const DEFAULT_CONNECTED_CLIENT_TTL_MS: i64 = 15_000;
pub const DISCOVERY_PROBE_TOKEN: &str = "BETTERCLOCK_DISCOVER_V1";
pub const MDNS_SERVICE_TYPE: &str = "_betterclock._tcp.local.";
const CLIENT_RUNTIME_CODE: &str = include_str!("../client_payload/client_runtime.py");
const DEBUG_UI_HTML: &str = include_str!("../client_payload/debug_ui.html");
const OPENAPI_YAML: &str = include_str!("../openapi.yaml");

#[derive(Debug, Clone, Serialize, Default)]
pub struct RuntimeSnapshot {
    pub iso_local: String,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
    pub source_label: String,
    pub warning_enabled: bool,
    pub warning_active_count: usize,
    pub warning_pulse_on: bool,
    pub warning_lead_time_ms: u64,
    pub warning_pulse_time_ms: u64,
    pub triggered_count: usize,
    pub armed_count: usize,
    pub updated_unix_ms: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct PublicClient {
    pub id: String,
    pub instance_id: String,
    pub debug_mode: bool,
    pub ip: String,
    pub request_count: u64,
    pub first_seen_unix_ms: i64,
    pub last_seen_unix_ms: i64,
    pub last_rtt_ms: Option<f64>,
    pub last_offset_ms: Option<f64>,
    pub last_desync_ms: Option<f64>,
    pub first_in_unix_ms: i64,
    pub last_in_unix_ms: i64,
    pub last_out_unix_ms: i64,
    pub last_in_bytes: u64,
    pub last_out_bytes: u64,
    pub total_in_bytes: u64,
    pub total_out_bytes: u64,
    pub in_bytes_per_sec: f64,
    pub out_bytes_per_sec: f64,
}

#[derive(Debug, Clone)]
struct ClientRecord {
    id: String,
    instance_id: String,
    debug_mode: bool,
    ip: String,
    request_count: u64,
    first_seen_unix_ms: i64,
    last_seen_unix_ms: i64,
    last_rtt_ms: Option<f64>,
    last_offset_ms: Option<f64>,
    last_desync_ms: Option<f64>,
    first_in_unix_ms: i64,
    last_in_unix_ms: i64,
    last_out_unix_ms: i64,
    last_in_bytes: u64,
    last_out_bytes: u64,
    total_in_bytes: u64,
    total_out_bytes: u64,
}

#[derive(Debug)]
pub struct ApiSharedState {
    pub runtime: RuntimeSnapshot,
    clients: HashMap<String, ClientRecord>,
    total_requests: u64,
    total_in_bytes: u64,
    total_out_bytes: u64,
    server_started_unix_ms: i64,
    session_first_in_unix_ms: i64,
    session_last_in_unix_ms: i64,
    session_last_out_unix_ms: i64,
    debug_ui_enabled: bool,
}

impl Default for ApiSharedState {
    fn default() -> Self {
        Self {
            runtime: RuntimeSnapshot::default(),
            clients: HashMap::new(),
            total_requests: 0,
            total_in_bytes: 0,
            total_out_bytes: 0,
            server_started_unix_ms: Local::now().timestamp_millis(),
            session_first_in_unix_ms: 0,
            session_last_in_unix_ms: 0,
            session_last_out_unix_ms: 0,
            debug_ui_enabled: false,
        }
    }
}

impl ApiSharedState {
    fn compute_bytes_per_sec(total_bytes: u64, start_unix_ms: i64, now_unix_ms: i64) -> f64 {
        if start_unix_ms <= 0 {
            return 0.0;
        }
        let elapsed_ms = now_unix_ms.saturating_sub(start_unix_ms);
        if elapsed_ms <= 0 {
            return 0.0;
        }
        (total_bytes as f64 * 1000.0) / elapsed_ms as f64
    }

    pub fn connected_clients(&self, now_ms: i64, ttl_ms: i64) -> Vec<PublicClient> {
        let mut clients = self
            .clients
            .values()
            .filter(|client| now_ms.saturating_sub(client.last_seen_unix_ms) <= ttl_ms)
            .cloned()
            .map(|entry| {
                let in_bytes_per_sec =
                    Self::compute_bytes_per_sec(entry.total_in_bytes, entry.first_seen_unix_ms, now_ms);
                let out_bytes_per_sec =
                    Self::compute_bytes_per_sec(entry.total_out_bytes, entry.first_seen_unix_ms, now_ms);
                PublicClient {
                    id: entry.id,
                    instance_id: entry.instance_id,
                    debug_mode: entry.debug_mode,
                    ip: entry.ip,
                    request_count: entry.request_count,
                    first_seen_unix_ms: entry.first_seen_unix_ms,
                    last_seen_unix_ms: entry.last_seen_unix_ms,
                    last_rtt_ms: entry.last_rtt_ms,
                    last_offset_ms: entry.last_offset_ms,
                    last_desync_ms: entry.last_desync_ms,
                    first_in_unix_ms: entry.first_in_unix_ms,
                    last_in_unix_ms: entry.last_in_unix_ms,
                    last_out_unix_ms: entry.last_out_unix_ms,
                    last_in_bytes: entry.last_in_bytes,
                    last_out_bytes: entry.last_out_bytes,
                    total_in_bytes: entry.total_in_bytes,
                    total_out_bytes: entry.total_out_bytes,
                    in_bytes_per_sec,
                    out_bytes_per_sec,
                }
            })
            .collect::<Vec<_>>();
        clients.sort_by(|a, b| {
            let a_connection_ms = now_ms.saturating_sub(a.first_seen_unix_ms);
            let b_connection_ms = now_ms.saturating_sub(b.first_seen_unix_ms);
            b_connection_ms
                .cmp(&a_connection_ms)
                .then_with(|| b.last_seen_unix_ms.cmp(&a.last_seen_unix_ms))
                .then_with(|| a.id.cmp(&b.id))
                .then_with(|| a.instance_id.cmp(&b.instance_id))
        });
        clients
    }

    pub fn total_requests(&self) -> u64 {
        self.total_requests
    }

    pub fn total_in_bytes(&self) -> u64 {
        self.total_in_bytes
    }

    pub fn total_out_bytes(&self) -> u64 {
        self.total_out_bytes
    }

    pub fn server_started_unix_ms(&self) -> i64 {
        self.server_started_unix_ms
    }

    pub fn session_first_in_unix_ms(&self) -> i64 {
        self.session_first_in_unix_ms
    }

    pub fn session_last_in_unix_ms(&self) -> i64 {
        self.session_last_in_unix_ms
    }

    pub fn session_last_out_unix_ms(&self) -> i64 {
        self.session_last_out_unix_ms
    }

    pub fn session_in_bytes_per_sec(&self, now_ms: i64) -> f64 {
        Self::compute_bytes_per_sec(self.total_in_bytes, self.session_first_in_unix_ms, now_ms)
    }

    pub fn session_out_bytes_per_sec(&self, now_ms: i64) -> f64 {
        Self::compute_bytes_per_sec(self.total_out_bytes, self.session_first_in_unix_ms, now_ms)
    }

    pub fn client_debug_mode(&self, client_id: &str, instance_id: &str) -> Option<bool> {
        let key = client_key(client_id, instance_id);
        self.clients.get(&key).map(|record| record.debug_mode)
    }

    pub fn set_client_debug_mode(
        &mut self,
        client_id: &str,
        instance_id: &str,
        enabled: bool,
    ) -> bool {
        let key = client_key(client_id, instance_id);
        if let Some(record) = self.clients.get_mut(&key) {
            record.debug_mode = enabled;
            return true;
        }
        false
    }

    pub fn disconnect_client(&mut self, client_id: &str, instance_id: &str) -> bool {
        let key = client_key(client_id, instance_id);
        self.clients.remove(&key).is_some()
    }

    pub fn debug_ui_enabled(&self) -> bool {
        self.debug_ui_enabled
    }

    pub fn set_debug_ui_enabled(&mut self, enabled: bool) {
        self.debug_ui_enabled = enabled;
    }
}

#[derive(Debug, Clone)]
pub struct ApiServerConfig {
    pub bind_addr: String,
    pub port: u16,
    pub discovery_enabled: bool,
    pub discovery_udp_port: u16,
    pub mdns_enabled: bool,
    pub mdns_instance: String,
}

pub struct ApiServer {
    pub state: Arc<Mutex<ApiSharedState>>,
    stop: Arc<AtomicBool>,
    http_join: Option<JoinHandle<()>>,
    discovery_join: Option<JoinHandle<()>>,
    mdns: Option<ServiceDaemon>,
}

impl ApiServer {
    pub fn start(config: ApiServerConfig) -> Result<Self> {
        let bind = format!("{}:{}", config.bind_addr, config.port);
        let server = Server::http(&bind)
            .map_err(|err| anyhow::anyhow!("failed to start API server on {bind}: {err}"))?;
        let state = Arc::new(Mutex::new(ApiSharedState::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let state_for_thread = Arc::clone(&state);
        let stop_for_thread = Arc::clone(&stop);
        let http_join =
            thread::spawn(move || run_server_loop(server, state_for_thread, stop_for_thread));

        let discovery_join = if config.discovery_enabled {
            let discovery_bind = format!("{}:{}", config.bind_addr, config.discovery_udp_port);
            let discovery_socket = UdpSocket::bind(&discovery_bind).map_err(|err| {
                anyhow::anyhow!(
                    "failed to start discovery UDP responder on {discovery_bind}: {err}"
                )
            })?;
            let _ = discovery_socket.set_read_timeout(Some(Duration::from_millis(200)));
            let stop_for_discovery = Arc::clone(&stop);
            Some(thread::spawn(move || {
                run_discovery_loop(discovery_socket, config.port, stop_for_discovery)
            }))
        } else {
            None
        };

        let mdns = if config.mdns_enabled {
            match start_mdns_advertisement(config.port, &config.mdns_instance) {
                Ok(daemon) => Some(daemon),
                Err(err) => {
                    eprintln!("warning: mDNS advertisement disabled: {err}");
                    None
                }
            }
        } else {
            None
        };

        Ok(Self {
            state,
            stop,
            http_join: Some(http_join),
            discovery_join,
            mdns,
        })
    }
}

impl Drop for ApiServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(join) = self.http_join.take() {
            let _ = join.join();
        }
        if let Some(join) = self.discovery_join.take() {
            let _ = join.join();
        }
        if let Some(mdns) = self.mdns.take() {
            let _ = mdns.shutdown();
        }
    }
}

fn run_server_loop(server: Server, state: Arc<Mutex<ApiSharedState>>, stop: Arc<AtomicBool>) {
    while !stop.load(Ordering::Relaxed) {
        match server.recv_timeout(Duration::from_millis(200)) {
            Ok(Some(request)) => handle_request(request, &state),
            Ok(None) => continue,
            Err(_) => continue,
        }
    }
}

fn run_discovery_loop(socket: UdpSocket, api_port: u16, stop: Arc<AtomicBool>) {
    let mut buffer = [0_u8; 512];
    while !stop.load(Ordering::Relaxed) {
        match socket.recv_from(&mut buffer) {
            Ok((len, source)) => {
                if !is_local_network_ip(source.ip()) {
                    continue;
                }
                let probe = std::str::from_utf8(&buffer[..len]).unwrap_or_default();
                if !is_discovery_probe(probe) {
                    continue;
                }
                let payload = build_discovery_payload(api_port);
                let _ = socket.send_to(payload.as_bytes(), source);
            }
            Err(err)
                if err.kind() == std::io::ErrorKind::WouldBlock
                    || err.kind() == std::io::ErrorKind::TimedOut =>
            {
                continue;
            }
            Err(_) => continue,
        }
    }
}

fn is_discovery_probe(raw: &str) -> bool {
    raw.trim() == DISCOVERY_PROBE_TOKEN
}

fn build_discovery_payload(api_port: u16) -> String {
    #[derive(Serialize)]
    struct DiscoveryResponse {
        service: &'static str,
        version: u8,
        api_port: u16,
        server_time_unix_ms: i64,
    }

    let payload = DiscoveryResponse {
        service: "betterclock",
        version: 1,
        api_port,
        server_time_unix_ms: Local::now().timestamp_millis(),
    };
    serde_json::to_string(&payload).unwrap_or_else(|_| {
        format!(
            "{{\"service\":\"betterclock\",\"version\":1,\"api_port\":{},\"server_time_unix_ms\":{}}}",
            api_port,
            Local::now().timestamp_millis()
        )
    })
}

fn start_mdns_advertisement(api_port: u16, instance_prefix: &str) -> Result<ServiceDaemon> {
    let daemon = ServiceDaemon::new()
        .map_err(|err| anyhow::anyhow!("could not create mDNS daemon: {err}"))?;

    let hostname = detect_hostname();
    let instance = if instance_prefix.trim().is_empty() {
        hostname.clone()
    } else {
        format!("{}-{}", instance_prefix.trim(), hostname)
    };
    let host_name = format!("{hostname}.local.");
    let mut addresses = detect_mdns_addresses();
    if addresses.is_empty() {
        addresses.push(Ipv4Addr::LOCALHOST.into());
    }

    let service = ServiceInfo::new(
        MDNS_SERVICE_TYPE,
        &instance,
        &host_name,
        addresses.as_slice(),
        api_port,
        None,
    )
    .map_err(|err| anyhow::anyhow!("could not create mDNS service info: {err}"))?;
    daemon
        .register(service)
        .map_err(|err| anyhow::anyhow!("could not register mDNS service: {err}"))?;
    Ok(daemon)
}

fn detect_hostname() -> String {
    let candidate = std::env::var("HOSTNAME")
        .ok()
        .or_else(|| std::env::var("COMPUTERNAME").ok())
        .unwrap_or_else(|| "betterclock".to_string());
    let trimmed = candidate.trim();
    if trimmed.is_empty() {
        "betterclock".to_string()
    } else {
        trimmed
            .chars()
            .map(|ch| {
                if ch.is_ascii_alphanumeric() || ch == '-' {
                    ch.to_ascii_lowercase()
                } else {
                    '-'
                }
            })
            .collect::<String>()
    }
}

fn detect_mdns_addresses() -> Vec<IpAddr> {
    let mut addresses = Vec::<IpAddr>::new();
    if let Ok(socket) = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))
        && socket.connect((Ipv4Addr::new(8, 8, 8, 8), 80)).is_ok()
        && let Ok(local) = socket.local_addr()
    {
        let ip = local.ip();
        if ip.is_ipv4() && !ip.is_loopback() {
            addresses.push(ip);
        }
    }
    addresses.sort();
    addresses.dedup();
    addresses
}

fn handle_request(request: tiny_http::Request, state: &Arc<Mutex<ApiSharedState>>) {
    if request.method() != &Method::Get {
        let _ = send_text(request, StatusCode(405), "method not allowed");
        return;
    }

    let Some(remote_addr) = request.remote_addr() else {
        let _ = send_text(request, StatusCode(400), "missing remote address");
        return;
    };
    let remote_ip = remote_addr.ip();
    if !is_local_network_ip(remote_ip) {
        let _ = send_text(request, StatusCode(403), "forbidden: local network only");
        return;
    }
    let request_received_unix_ms = Local::now().timestamp_millis();
    let request_in_bytes = estimate_request_bytes(&request);

    let url = request.url().to_string();
    let (path, query) = split_path_query(&url);
    let base_url = request_base_url(&request);
    let client_id = extract_client_id(query, &request).unwrap_or_else(|| remote_ip.to_string());
    let client_instance =
        extract_client_instance(query, &request).unwrap_or_else(|| "default".to_string());
    let client_rtt_ms = extract_client_rtt_ms(query, &request);
    let client_offset_ms = extract_client_offset_ms(query, &request);
    let client_desync_ms = extract_client_desync_ms(query, &request);

    let mut guard = match state.lock() {
        Ok(guard) => guard,
        Err(_) => {
            let _ = send_text(request, StatusCode(500), "internal state lock error");
            return;
        }
    };
    let is_disconnect_route = path == "/v1/client/disconnect";
    record_server_inbound(&mut guard, request_received_unix_ms, request_in_bytes);
    if !is_disconnect_route {
        register_client(
            &mut guard,
            &client_id,
            &client_instance,
            remote_ip,
            request_received_unix_ms,
            request_in_bytes,
            client_rtt_ms,
            client_offset_ms,
            client_desync_ms,
        );
    }
    let requester_debug_mode = if is_disconnect_route {
        false
    } else {
        guard
            .client_debug_mode(&client_id, &client_instance)
            .unwrap_or(false)
    };

    match path {
        "/v1" => {
            #[derive(Serialize)]
            struct ApiIndexResponse {
                api_base: String,
                state_url: String,
                clients_url: String,
                health_url: String,
                runtime_code_url: String,
                disconnect_url: String,
                debug_url: String,
                openapi_url: String,
            }

            let payload = ApiIndexResponse {
                state_url: format!("{base_url}/v1/state"),
                clients_url: format!("{base_url}/v1/clients"),
                health_url: format!("{base_url}/healthz"),
                runtime_code_url: format!("{base_url}/v1/client/code"),
                disconnect_url: format!("{base_url}/v1/client/disconnect"),
                debug_url: format!("{base_url}/debug"),
                openapi_url: format!("{base_url}/openapi.yaml"),
                api_base: base_url,
            };
            let response_out_bytes = json_payload_len(&payload);
            let _ = send_json(request, StatusCode(200), &payload);
            let response_send_unix_ms = Local::now().timestamp_millis();
            record_server_outbound(&mut guard, response_send_unix_ms, response_out_bytes);
            if !is_disconnect_route {
                register_client_response(
                    &mut guard,
                    &client_id,
                    &client_instance,
                    response_send_unix_ms,
                    response_out_bytes,
                );
            }
        }
        "/" | "/v1/state" => {
            #[derive(Serialize)]
            struct StateResponse {
                runtime: RuntimeSnapshot,
                clients_seen: usize,
                total_requests: u64,
                total_in_bytes: u64,
                total_out_bytes: u64,
                session_in_bytes_per_sec: f64,
                session_out_bytes_per_sec: f64,
                server_started_unix_ms: i64,
                session_first_in_unix_ms: i64,
                session_last_in_unix_ms: i64,
                session_last_out_unix_ms: i64,
                client_debug_mode: bool,
                request_received_unix_ms: i64,
                response_unix_ms: i64,
                response_send_unix_ms: i64,
                server_processing_ms: i64,
                response_iso_local: String,
            }

            let response_now = Local::now();
            let connected_clients = guard.connected_clients(
                response_now.timestamp_millis(),
                DEFAULT_CONNECTED_CLIENT_TTL_MS,
            );
            let response_send_unix_ms = response_now.timestamp_millis();
            let payload = StateResponse {
                runtime: guard.runtime.clone(),
                clients_seen: connected_clients.len(),
                total_requests: guard.total_requests(),
                total_in_bytes: guard.total_in_bytes(),
                total_out_bytes: guard.total_out_bytes(),
                session_in_bytes_per_sec: guard.session_in_bytes_per_sec(response_send_unix_ms),
                session_out_bytes_per_sec: guard.session_out_bytes_per_sec(response_send_unix_ms),
                server_started_unix_ms: guard.server_started_unix_ms(),
                session_first_in_unix_ms: guard.session_first_in_unix_ms(),
                session_last_in_unix_ms: guard.session_last_in_unix_ms(),
                session_last_out_unix_ms: guard.session_last_out_unix_ms(),
                client_debug_mode: requester_debug_mode,
                request_received_unix_ms,
                response_unix_ms: response_send_unix_ms,
                response_send_unix_ms,
                server_processing_ms: response_send_unix_ms
                    .saturating_sub(request_received_unix_ms),
                response_iso_local: response_now.to_rfc3339(),
            };
            let response_out_bytes = json_payload_len(&payload);
            let _ = send_json(request, StatusCode(200), &payload);
            let response_send_unix_ms = Local::now().timestamp_millis();
            record_server_outbound(&mut guard, response_send_unix_ms, response_out_bytes);
            if !is_disconnect_route {
                register_client_response(
                    &mut guard,
                    &client_id,
                    &client_instance,
                    response_send_unix_ms,
                    response_out_bytes,
                );
            }
        }
        "/v1/clients" => {
            #[derive(Serialize)]
            struct ClientsResponse {
                count: usize,
                clients: Vec<PublicClient>,
            }

            let clients = guard.connected_clients(
                Local::now().timestamp_millis(),
                DEFAULT_CONNECTED_CLIENT_TTL_MS,
            );

            let payload = ClientsResponse {
                count: clients.len(),
                clients,
            };
            let response_out_bytes = json_payload_len(&payload);
            let _ = send_json(request, StatusCode(200), &payload);
            let response_send_unix_ms = Local::now().timestamp_millis();
            record_server_outbound(&mut guard, response_send_unix_ms, response_out_bytes);
            if !is_disconnect_route {
                register_client_response(
                    &mut guard,
                    &client_id,
                    &client_instance,
                    response_send_unix_ms,
                    response_out_bytes,
                );
            }
        }
        "/healthz" => {
            let _ = send_text(request, StatusCode(200), "ok");
            let response_out_bytes = "ok".len() as u64;
            let response_send_unix_ms = Local::now().timestamp_millis();
            record_server_outbound(&mut guard, response_send_unix_ms, response_out_bytes);
            if !is_disconnect_route {
                register_client_response(
                    &mut guard,
                    &client_id,
                    &client_instance,
                    response_send_unix_ms,
                    response_out_bytes,
                );
            }
        }
        "/v1/client/code" => {
            let _ = send_python(request, StatusCode(200), CLIENT_RUNTIME_CODE);
            let response_out_bytes = CLIENT_RUNTIME_CODE.len() as u64;
            let response_send_unix_ms = Local::now().timestamp_millis();
            record_server_outbound(&mut guard, response_send_unix_ms, response_out_bytes);
            if !is_disconnect_route {
                register_client_response(
                    &mut guard,
                    &client_id,
                    &client_instance,
                    response_send_unix_ms,
                    response_out_bytes,
                );
            }
        }
        "/v1/client/disconnect" => {
            #[derive(Serialize)]
            struct DisconnectResponse {
                disconnected: bool,
                client_id: String,
                instance_id: String,
            }

            let disconnected = guard.disconnect_client(&client_id, &client_instance);
            let payload = DisconnectResponse {
                disconnected,
                client_id,
                instance_id: client_instance,
            };
            let response_out_bytes = json_payload_len(&payload);
            let _ = send_json(request, StatusCode(200), &payload);
            let response_send_unix_ms = Local::now().timestamp_millis();
            record_server_outbound(&mut guard, response_send_unix_ms, response_out_bytes);
        }
        "/openapi.yaml" => {
            let _ = send_yaml(request, StatusCode(200), OPENAPI_YAML);
            let response_out_bytes = OPENAPI_YAML.len() as u64;
            let response_send_unix_ms = Local::now().timestamp_millis();
            record_server_outbound(&mut guard, response_send_unix_ms, response_out_bytes);
            if !is_disconnect_route {
                register_client_response(
                    &mut guard,
                    &client_id,
                    &client_instance,
                    response_send_unix_ms,
                    response_out_bytes,
                );
            }
        }
        "/debug" => {
            if guard.debug_ui_enabled() {
                let _ = send_html(request, StatusCode(200), DEBUG_UI_HTML);
                let response_out_bytes = DEBUG_UI_HTML.len() as u64;
                let response_send_unix_ms = Local::now().timestamp_millis();
                record_server_outbound(&mut guard, response_send_unix_ms, response_out_bytes);
                if !is_disconnect_route {
                    register_client_response(
                        &mut guard,
                        &client_id,
                        &client_instance,
                        response_send_unix_ms,
                        response_out_bytes,
                    );
                }
            } else {
                let disabled_text = "debug web ui is disabled on the server";
                let _ = send_text(
                    request,
                    StatusCode(404),
                    disabled_text,
                );
                let response_out_bytes = disabled_text.len() as u64;
                let response_send_unix_ms = Local::now().timestamp_millis();
                record_server_outbound(&mut guard, response_send_unix_ms, response_out_bytes);
                if !is_disconnect_route {
                    register_client_response(
                        &mut guard,
                        &client_id,
                        &client_instance,
                        response_send_unix_ms,
                        response_out_bytes,
                    );
                }
            }
        }
        _ => {
            let not_found_text = "not found";
            let _ = send_text(request, StatusCode(404), not_found_text);
            let response_out_bytes = not_found_text.len() as u64;
            let response_send_unix_ms = Local::now().timestamp_millis();
            record_server_outbound(&mut guard, response_send_unix_ms, response_out_bytes);
            if !is_disconnect_route {
                register_client_response(
                    &mut guard,
                    &client_id,
                    &client_instance,
                    response_send_unix_ms,
                    response_out_bytes,
                );
            }
        }
    }
}

fn register_client(
    state: &mut ApiSharedState,
    client_id: &str,
    client_instance: &str,
    remote_ip: IpAddr,
    request_received_unix_ms: i64,
    request_in_bytes: u64,
    client_rtt_ms: Option<f64>,
    client_offset_ms: Option<f64>,
    client_desync_ms: Option<f64>,
) {
    state.total_requests = state.total_requests.saturating_add(1);

    let key = client_key(client_id, client_instance);
    let entry = state.clients.entry(key).or_insert_with(|| ClientRecord {
        id: client_id.to_string(),
        instance_id: client_instance.to_string(),
        debug_mode: false,
        ip: remote_ip.to_string(),
        request_count: 0,
        first_seen_unix_ms: request_received_unix_ms,
        last_seen_unix_ms: request_received_unix_ms,
        last_rtt_ms: client_rtt_ms,
        last_offset_ms: client_offset_ms,
        last_desync_ms: client_desync_ms,
        first_in_unix_ms: request_received_unix_ms,
        last_in_unix_ms: request_received_unix_ms,
        last_out_unix_ms: 0,
        last_in_bytes: 0,
        last_out_bytes: 0,
        total_in_bytes: 0,
        total_out_bytes: 0,
    });
    entry.ip = remote_ip.to_string();
    entry.request_count = entry.request_count.saturating_add(1);
    entry.last_seen_unix_ms = request_received_unix_ms;
    entry.last_in_unix_ms = request_received_unix_ms;
    entry.last_in_bytes = request_in_bytes;
    entry.total_in_bytes = entry.total_in_bytes.saturating_add(request_in_bytes);
    if let Some(rtt_ms) = client_rtt_ms {
        entry.last_rtt_ms = Some(rtt_ms);
    }
    if let Some(offset_ms) = client_offset_ms {
        entry.last_offset_ms = Some(offset_ms);
    }
    if let Some(desync_ms) = client_desync_ms {
        entry.last_desync_ms = Some(desync_ms);
    }
}

fn register_client_response(
    state: &mut ApiSharedState,
    client_id: &str,
    client_instance: &str,
    response_send_unix_ms: i64,
    response_out_bytes: u64,
) {
    let key = client_key(client_id, client_instance);
    if let Some(entry) = state.clients.get_mut(&key) {
        entry.last_out_unix_ms = response_send_unix_ms;
        entry.last_out_bytes = response_out_bytes;
        entry.total_out_bytes = entry.total_out_bytes.saturating_add(response_out_bytes);
    }
}

fn record_server_inbound(state: &mut ApiSharedState, now_ms: i64, bytes: u64) {
    state.total_in_bytes = state.total_in_bytes.saturating_add(bytes);
    if state.session_first_in_unix_ms == 0 {
        state.session_first_in_unix_ms = now_ms;
    }
    state.session_last_in_unix_ms = now_ms;
}

fn record_server_outbound(state: &mut ApiSharedState, now_ms: i64, bytes: u64) {
    state.total_out_bytes = state.total_out_bytes.saturating_add(bytes);
    state.session_last_out_unix_ms = now_ms;
}

fn estimate_request_bytes(request: &tiny_http::Request) -> u64 {
    let mut total = 0usize;
    total = total.saturating_add(3); // GET
    total = total.saturating_add(1); // space
    total = total.saturating_add(request.url().len());
    total = total.saturating_add(1); // space
    total = total.saturating_add("HTTP/1.1\r\n".len());
    for header in request.headers() {
        let field = format!("{}", header.field);
        let value = header.value.as_str();
        total = total.saturating_add(field.len());
        total = total.saturating_add(2); // ": "
        total = total.saturating_add(value.len());
        total = total.saturating_add(2); // CRLF
    }
    total = total.saturating_add(2); // final CRLF
    total as u64
}

fn json_payload_len<T: Serialize>(payload: &T) -> u64 {
    serde_json::to_vec(payload)
        .map(|body| body.len() as u64)
        .unwrap_or(0)
}

fn client_key(client_id: &str, instance_id: &str) -> String {
    format!("{client_id}::{instance_id}")
}

fn send_json<T: Serialize>(
    request: tiny_http::Request,
    status: StatusCode,
    body: &T,
) -> Result<()> {
    let payload = serde_json::to_vec(body)?;
    let content_type = Header::from_str("Content-Type: application/json; charset=utf-8")
        .map_err(|_| anyhow::anyhow!("failed to build content-type header"))?;
    request.respond(
        Response::from_data(payload)
            .with_status_code(status)
            .with_header(content_type),
    )?;
    Ok(())
}

fn send_text(request: tiny_http::Request, status: StatusCode, body: &str) -> Result<()> {
    let content_type = Header::from_str("Content-Type: text/plain; charset=utf-8")
        .map_err(|_| anyhow::anyhow!("failed to build content-type header"))?;
    request.respond(
        Response::from_string(body.to_string())
            .with_status_code(status)
            .with_header(content_type),
    )?;
    Ok(())
}

fn send_python(request: tiny_http::Request, status: StatusCode, body: &str) -> Result<()> {
    let content_type = Header::from_str("Content-Type: text/x-python; charset=utf-8")
        .map_err(|_| anyhow::anyhow!("failed to build content-type header"))?;
    request.respond(
        Response::from_string(body.to_string())
            .with_status_code(status)
            .with_header(content_type),
    )?;
    Ok(())
}

fn send_html(request: tiny_http::Request, status: StatusCode, body: &str) -> Result<()> {
    let content_type = Header::from_str("Content-Type: text/html; charset=utf-8")
        .map_err(|_| anyhow::anyhow!("failed to build content-type header"))?;
    request.respond(
        Response::from_string(body.to_string())
            .with_status_code(status)
            .with_header(content_type),
    )?;
    Ok(())
}

fn send_yaml(request: tiny_http::Request, status: StatusCode, body: &str) -> Result<()> {
    let content_type = Header::from_str("Content-Type: application/yaml; charset=utf-8")
        .map_err(|_| anyhow::anyhow!("failed to build content-type header"))?;
    request.respond(
        Response::from_string(body.to_string())
            .with_status_code(status)
            .with_header(content_type),
    )?;
    Ok(())
}

fn split_path_query(url: &str) -> (&str, &str) {
    match url.split_once('?') {
        Some((path, query)) => (path, query),
        None => (url, ""),
    }
}

fn request_base_url(request: &tiny_http::Request) -> String {
    for header in request.headers() {
        if header.field.equiv("Host") {
            let host = header.value.as_str().trim();
            if !host.is_empty() {
                return format!("http://{host}");
            }
        }
    }
    "http://127.0.0.1:8099".to_string()
}

fn extract_client_id(query: &str, request: &tiny_http::Request) -> Option<String> {
    if let Some(value) = query_param(query, "client_id") {
        let trimmed = value.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }

    for header in request.headers() {
        if header.field.equiv("X-Client-Id") {
            let value = header.value.as_str().trim();
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

fn extract_client_instance(query: &str, request: &tiny_http::Request) -> Option<String> {
    if let Some(value) = query_param(query, "instance_id") {
        let trimmed = value.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }

    for header in request.headers() {
        if header.field.equiv("X-Client-Instance") {
            let value = header.value.as_str().trim();
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

fn extract_client_rtt_ms(query: &str, request: &tiny_http::Request) -> Option<f64> {
    if let Some(raw) = query_param(query, "rtt_ms")
        && let Some(parsed) = parse_rtt_ms(raw)
    {
        return Some(parsed);
    }

    for header in request.headers() {
        if header.field.equiv("X-Client-Rtt-Ms")
            && let Some(parsed) = parse_rtt_ms(header.value.as_str())
        {
            return Some(parsed);
        }
    }
    None
}

fn extract_client_offset_ms(query: &str, request: &tiny_http::Request) -> Option<f64> {
    if let Some(raw) = query_param(query, "offset_ms")
        && let Some(parsed) = parse_offset_ms(raw)
    {
        return Some(parsed);
    }

    for header in request.headers() {
        if header.field.equiv("X-Client-Offset-Ms")
            && let Some(parsed) = parse_offset_ms(header.value.as_str())
        {
            return Some(parsed);
        }
    }
    None
}

fn extract_client_desync_ms(query: &str, request: &tiny_http::Request) -> Option<f64> {
    if let Some(raw) = query_param(query, "desync_ms")
        && let Some(parsed) = parse_offset_ms(raw)
    {
        return Some(parsed);
    }

    for header in request.headers() {
        if header.field.equiv("X-Client-Desync-Ms")
            && let Some(parsed) = parse_offset_ms(header.value.as_str())
        {
            return Some(parsed);
        }
    }
    None
}

fn parse_rtt_ms(input: &str) -> Option<f64> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }
    let value = trimmed.parse::<f64>().ok()?;
    if !value.is_finite() || !(0.0..=60_000.0).contains(&value) {
        return None;
    }
    Some(value)
}

fn parse_offset_ms(input: &str) -> Option<f64> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }
    let value = trimmed.parse::<f64>().ok()?;
    if !value.is_finite() || !(-60_000.0..=60_000.0).contains(&value) {
        return None;
    }
    Some(value)
}

fn query_param<'a>(query: &'a str, key: &str) -> Option<&'a str> {
    for pair in query.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (k, v) = match pair.split_once('=') {
            Some((k, v)) => (k, v),
            None => (pair, ""),
        };
        if k == key {
            return Some(v);
        }
    }
    None
}

fn is_local_network_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_loopback() || v4.is_private() || v4.is_link_local(),
        IpAddr::V6(v6) => {
            v6.is_loopback()
                || v6.is_unique_local()
                || v6.is_unicast_link_local()
                || is_ipv4_mapped_local(v6)
        }
    }
}

fn is_ipv4_mapped_local(v6: Ipv6Addr) -> bool {
    match v6.to_ipv4_mapped() {
        Some(v4) => v4.is_loopback() || v4.is_private() || v4.is_link_local(),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    use super::*;

    #[test]
    fn local_network_ip_filter_accepts_private_and_loopback() {
        assert!(is_local_network_ip(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1))));
        assert!(is_local_network_ip(IpAddr::V4(Ipv4Addr::new(
            192, 168, 1, 44
        ))));
        assert!(is_local_network_ip(IpAddr::V4(Ipv4Addr::new(10, 1, 2, 3))));
        assert!(is_local_network_ip(IpAddr::V6(Ipv6Addr::LOCALHOST)));
        assert!(is_local_network_ip(IpAddr::V6(Ipv6Addr::new(
            0xfc00, 0, 0, 0, 0, 0, 0, 1
        ))));
        assert!(!is_local_network_ip(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8))));
    }

    #[test]
    fn query_param_extracts_value() {
        let query = "client_id=client-a&foo=bar";
        assert_eq!(query_param(query, "client_id"), Some("client-a"));
        assert_eq!(query_param(query, "missing"), None);
    }

    #[test]
    fn query_param_extracts_instance_value() {
        let query = "client_id=client-a&instance_id=inst-01";
        assert_eq!(query_param(query, "instance_id"), Some("inst-01"));
    }

    #[test]
    fn client_debug_mode_can_be_updated() {
        let now_ms = 1_000_000;
        let mut state = ApiSharedState::default();
        state.clients.insert(
            "client-a::inst-01".to_string(),
            ClientRecord {
                id: "client-a".to_string(),
                instance_id: "inst-01".to_string(),
                debug_mode: false,
                ip: "127.0.0.1".to_string(),
                request_count: 1,
                first_seen_unix_ms: now_ms,
                last_seen_unix_ms: now_ms,
                last_rtt_ms: None,
                last_offset_ms: None,
                last_desync_ms: None,
                first_in_unix_ms: now_ms,
                last_in_unix_ms: now_ms,
                last_out_unix_ms: now_ms,
                last_in_bytes: 0,
                last_out_bytes: 0,
                total_in_bytes: 0,
                total_out_bytes: 0,
            },
        );

        assert_eq!(state.client_debug_mode("client-a", "inst-01"), Some(false));
        assert!(state.set_client_debug_mode("client-a", "inst-01", true));
        assert_eq!(state.client_debug_mode("client-a", "inst-01"), Some(true));
    }

    #[test]
    fn disconnect_client_removes_client_record() {
        let now_ms = 1_000_000;
        let mut state = ApiSharedState::default();
        state.clients.insert(
            "client-a::inst-01".to_string(),
            ClientRecord {
                id: "client-a".to_string(),
                instance_id: "inst-01".to_string(),
                debug_mode: false,
                ip: "127.0.0.1".to_string(),
                request_count: 1,
                first_seen_unix_ms: now_ms,
                last_seen_unix_ms: now_ms,
                last_rtt_ms: None,
                last_offset_ms: None,
                last_desync_ms: None,
                first_in_unix_ms: now_ms,
                last_in_unix_ms: now_ms,
                last_out_unix_ms: now_ms,
                last_in_bytes: 0,
                last_out_bytes: 0,
                total_in_bytes: 0,
                total_out_bytes: 0,
            },
        );

        assert!(state.disconnect_client("client-a", "inst-01"));
        assert!(!state.disconnect_client("client-a", "inst-01"));
        assert_eq!(state.client_debug_mode("client-a", "inst-01"), None);
    }

    #[test]
    fn parse_rtt_ms_rejects_invalid_values() {
        assert_eq!(parse_rtt_ms("12.5"), Some(12.5));
        assert_eq!(parse_rtt_ms("-1"), None);
        assert_eq!(parse_rtt_ms("nan"), None);
        assert_eq!(parse_rtt_ms(""), None);
        assert_eq!(parse_rtt_ms("999999"), None);
    }

    #[test]
    fn parse_offset_ms_accepts_signed_values_in_range() {
        assert_eq!(parse_offset_ms("+12.5"), Some(12.5));
        assert_eq!(parse_offset_ms("-42.0"), Some(-42.0));
        assert_eq!(parse_offset_ms("60001"), None);
        assert_eq!(parse_offset_ms("-70000"), None);
        assert_eq!(parse_offset_ms(""), None);
    }

    #[test]
    fn embedded_client_runtime_is_present() {
        assert!(CLIENT_RUNTIME_CODE.contains("class BetterClockClient"));
        assert!(CLIENT_RUNTIME_CODE.contains("def main("));
    }

    #[test]
    fn debug_ui_toggle_defaults_off_and_can_be_enabled() {
        let mut state = ApiSharedState::default();
        assert!(!state.debug_ui_enabled());
        state.set_debug_ui_enabled(true);
        assert!(state.debug_ui_enabled());
        assert!(DEBUG_UI_HTML.contains("BetterClock Debug UI"));
    }

    #[test]
    fn connected_clients_are_sorted_by_connection_time_descending() {
        let now_ms = 1_000_000;
        let mut state = ApiSharedState::default();
        state.clients.insert(
            "short".to_string(),
            ClientRecord {
                id: "short".to_string(),
                instance_id: "short-a".to_string(),
                debug_mode: false,
                ip: "127.0.0.1".to_string(),
                request_count: 10,
                first_seen_unix_ms: now_ms - 2_000,
                last_seen_unix_ms: now_ms - 10,
                last_rtt_ms: None,
                last_offset_ms: None,
                last_desync_ms: None,
                first_in_unix_ms: now_ms - 2_000,
                last_in_unix_ms: now_ms - 10,
                last_out_unix_ms: now_ms - 10,
                last_in_bytes: 0,
                last_out_bytes: 0,
                total_in_bytes: 0,
                total_out_bytes: 0,
            },
        );
        state.clients.insert(
            "long".to_string(),
            ClientRecord {
                id: "long".to_string(),
                instance_id: "long-a".to_string(),
                debug_mode: false,
                ip: "127.0.0.1".to_string(),
                request_count: 10,
                first_seen_unix_ms: now_ms - 10_000,
                last_seen_unix_ms: now_ms - 20,
                last_rtt_ms: None,
                last_offset_ms: None,
                last_desync_ms: None,
                first_in_unix_ms: now_ms - 10_000,
                last_in_unix_ms: now_ms - 20,
                last_out_unix_ms: now_ms - 20,
                last_in_bytes: 0,
                last_out_bytes: 0,
                total_in_bytes: 0,
                total_out_bytes: 0,
            },
        );

        let clients = state.connected_clients(now_ms, 15_000);
        let ids = clients.iter().map(|c| c.id.as_str()).collect::<Vec<_>>();
        assert_eq!(ids, vec!["long", "short"]);
    }

    #[test]
    fn discovery_probe_token_matches_expected_message() {
        assert!(is_discovery_probe("BETTERCLOCK_DISCOVER_V1"));
        assert!(is_discovery_probe("  BETTERCLOCK_DISCOVER_V1  "));
        assert!(!is_discovery_probe("BETTERCLOCK_DISCOVER_V2"));
    }

    #[test]
    fn discovery_payload_contains_service_and_port() {
        let payload = build_discovery_payload(8099);
        let parsed = serde_json::from_str::<serde_json::Value>(&payload)
            .expect("discovery payload should be valid json");
        assert_eq!(
            parsed.get("service").and_then(|v| v.as_str()),
            Some("betterclock")
        );
        assert_eq!(parsed.get("api_port").and_then(|v| v.as_u64()), Some(8099));
    }
}
