#![forbid(unsafe_code)]

use chrono::{SecondsFormat, Utc};
use std::collections::HashMap;
use std::env;
use std::fmt;
use std::io;
use std::net::IpAddr;
use std::process;
use std::str::FromStr;
use std::sync::Arc;
use std::time::SystemTime;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;
use tokio::time::{Duration, sleep};
use uuid::Uuid;

#[derive(Debug, Clone, Copy)]
enum LoadBalancingAlgorithm {
    RoundRobin,
    Ordered,
    LeastConn,
    StickyClients,
}

impl fmt::Display for LoadBalancingAlgorithm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LoadBalancingAlgorithm::RoundRobin => write!(f, "round-robin"),
            LoadBalancingAlgorithm::Ordered => write!(f, "ordered"),
            LoadBalancingAlgorithm::LeastConn => write!(f, "least-conn"),
            LoadBalancingAlgorithm::StickyClients => write!(f, "sticky-clients"),
        }
    }
}

impl FromStr for LoadBalancingAlgorithm {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "" => Ok(LoadBalancingAlgorithm::Ordered),
            "round-robin" => Ok(LoadBalancingAlgorithm::RoundRobin),
            "ordered" => Ok(LoadBalancingAlgorithm::Ordered),
            "least-conn" => Ok(LoadBalancingAlgorithm::LeastConn),
            "sticky-clients" => Ok(LoadBalancingAlgorithm::StickyClients),
            other => Err(format!("unsupported LB_ALGO value: {other}")),
        }
    }
}

#[derive(Debug)]
struct BalancerState {
    rr_next: usize,
    open_connections: Vec<usize>,
    sticky_clients: HashMap<IpAddr, usize>,
}

impl BalancerState {
    fn new(server_count: usize) -> Self {
        Self {
            rr_next: 0,
            open_connections: vec![0; server_count],
            sticky_clients: HashMap::new(),
        }
    }
}

fn timestamp() -> String {
    chrono::DateTime::<Utc>::from(SystemTime::now()).to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn log_init(message: &str) {
    let ts = timestamp();
    println!("{ts} - INIT - INFO: {message}");
}

fn log_info(txid: &str, message: &str) {
    let ts = timestamp();
    println!("{ts} - {txid} - INFO: {message}");
}

fn log_error(txid: &str, message: &str) {
    let ts = timestamp();
    println!("{ts} - {txid} - ERROR: {message}");
}

async fn retry_connect(
    txid: &str,
    address: &str,
    max_retries: u32,
    delay: Duration,
) -> io::Result<TcpStream> {
    let mut retries = 0;

    while retries < max_retries {
        match TcpStream::connect(address).await {
            Ok(stream) => {
                return Ok(stream);
            }
            Err(e) => {
                retries += 1;
                log_error(
                    txid,
                    &format!(
                        "failed to connect to {address} attempt {retries} of {max_retries} - {e:?}"
                    ),
                );

                if retries < max_retries {
                    sleep(delay).await;
                } else {
                    return Err(e);
                }
            }
        }
    }

    Err(io::Error::other("max retries reached"))
}

async fn health(addr: &str) -> bool {
    TcpStream::connect(addr).await.is_ok()
}

async fn ordered_select(txid: &str, servers: &[String]) -> Option<usize> {
    for (idx, srv) in servers.iter().enumerate() {
        log_info(txid, &format!("checking backend {srv}"));

        if health(srv).await {
            log_info(txid, &format!("selected ordered backend {srv}"));
            return Some(idx);
        }
    }

    None
}

async fn round_robin_select(
    txid: &str,
    servers: &[String],
    state: Arc<Mutex<BalancerState>>,
) -> Option<usize> {
    let server_count = servers.len();

    let start = {
        let guard = state.lock().await;
        guard.rr_next % server_count
    };

    for offset in 0..server_count {
        let idx = (start + offset) % server_count;
        let srv = &servers[idx];

        log_info(txid, &format!("checking backend {srv}"));

        if health(srv).await {
            {
                let mut guard = state.lock().await;
                guard.rr_next = (idx + 1) % server_count;
            }

            log_info(txid, &format!("selected round-robin backend {srv}"));
            return Some(idx);
        }
    }

    None
}

async fn least_conn_select(
    txid: &str,
    servers: &[String],
    state: Arc<Mutex<BalancerState>>,
) -> Option<usize> {
    let server_count = servers.len();
    let mut healthy = Vec::new();

    for (idx, srv) in servers.iter().enumerate() {
        log_info(txid, &format!("checking backend {srv}"));

        if health(srv).await {
            healthy.push(idx);
        }
    }

    if healthy.is_empty() {
        return None;
    }

    let (counts, rr_start) = {
        let guard = state.lock().await;
        (guard.open_connections.clone(), guard.rr_next % server_count)
    };

    let min_count = healthy.iter().map(|idx| counts[*idx]).min().unwrap_or(0);

    let tied: Vec<usize> = healthy
        .into_iter()
        .filter(|idx| counts[*idx] == min_count)
        .collect();

    for offset in 0..server_count {
        let candidate = (rr_start + offset) % server_count;

        if tied.contains(&candidate) {
            {
                let mut guard = state.lock().await;
                guard.rr_next = (candidate + 1) % server_count;
            }

            log_info(
                txid,
                &format!(
                    "selected least-conn backend {} with {min_count} open connection(s)",
                    servers[candidate]
                ),
            );

            return Some(candidate);
        }
    }

    None
}

async fn sticky_clients_select(
    txid: &str,
    client_ip: IpAddr,
    servers: &[String],
    state: Arc<Mutex<BalancerState>>,
) -> Option<usize> {
    let existing = {
        let guard = state.lock().await;
        guard.sticky_clients.get(&client_ip).copied()
    };

    if let Some(idx) = existing
        && let Some(srv) = servers.get(idx) {
            log_info(
                txid,
                &format!("checking sticky backend {srv} for client {client_ip}"),
            );

            if health(srv).await {
                log_info(
                    txid,
                    &format!("selected sticky backend {srv} for client {client_ip}"),
                );
                return Some(idx);
            }

            log_info(
                txid,
                &format!(
                    "sticky backend {srv} for client {client_ip} is down, falling back to round-robin"
                ),
            );
    }

    let selected = round_robin_select(txid, servers, state.clone()).await;

    if let Some(idx) = selected {
        let mut guard = state.lock().await;
        guard.sticky_clients.insert(client_ip, idx);

        log_info(
            txid,
            &format!(
                "stored sticky backend {} for client {client_ip}",
                servers[idx]
            ),
        );
    }

    selected
}

async fn select_backend(
    txid: &str,
    client_ip: IpAddr,
    servers: &[String],
    state: Arc<Mutex<BalancerState>>,
    algorithm: LoadBalancingAlgorithm,
) -> Option<usize> {
    match algorithm {
        LoadBalancingAlgorithm::RoundRobin => round_robin_select(txid, servers, state).await,
        LoadBalancingAlgorithm::Ordered => ordered_select(txid, servers).await,
        LoadBalancingAlgorithm::LeastConn => least_conn_select(txid, servers, state).await,
        LoadBalancingAlgorithm::StickyClients => {
            sticky_clients_select(txid, client_ip, servers, state).await
        }
    }
}

async fn increment_open_connections(state: Arc<Mutex<BalancerState>>, backend_idx: usize) {
    let mut guard = state.lock().await;

    if let Some(count) = guard.open_connections.get_mut(backend_idx) {
        *count += 1;
    }
}

async fn decrement_open_connections(state: Arc<Mutex<BalancerState>>, backend_idx: usize) {
    let mut guard = state.lock().await;

    if let Some(count) = guard.open_connections.get_mut(backend_idx) {
        *count = count.saturating_sub(1);
    }
}

fn read_required_env(name: &str, help: &str) -> String {
    match env::var(name) {
        Ok(value) if !value.trim().is_empty() => value,
        _ => {
            println!("{help}");
            process::exit(1);
        }
    }
}

fn parse_servers(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToString::to_string)
        .collect()
}

fn read_algorithm() -> LoadBalancingAlgorithm {
    match env::var("LB_ALGO") {
        Ok(value) => match LoadBalancingAlgorithm::from_str(&value) {
            Ok(algo) => algo,
            Err(e) => {
                println!("{e}");
                println!(
                    "Supported LB_ALGO values: round-robin, ordered, least-conn, sticky-clients"
                );
                process::exit(1);
            }
        },
        Err(_) => LoadBalancingAlgorithm::Ordered,
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let listenerstr = read_required_env(
        "LISTENER",
        "Set the environment variable LISTENER to the endpoint kiaproxy is to listen on.\nExample:\n  export LISTENER=0.0.0.0:443",
    );

    let serversstr = read_required_env(
        "SERVERS",
        "Set the environment variable SERVERS to the list of backends for kiaproxy to proxy and route to.\nExample:\n  export SERVERS=192.168.1.120:443,192.168.1.121:443,192.168.1.122:443",
    );

    let algorithm = read_algorithm();
    let servers = parse_servers(&serversstr);

    if servers.is_empty() {
        println!("SERVERS must contain at least one backend.");
        process::exit(1);
    }

    let listener = TcpListener::bind(&listenerstr).await?;
    let servers = Arc::new(servers);
    let state = Arc::new(Mutex::new(BalancerState::new(servers.len())));

    log_init(&format!(
        "kiaproxy v0.2.0 TCP load balancer listening on {listenerstr} with LB_ALGO={algorithm} and backends {:?}",
        servers
    ));

    loop {
        let (mut inbound, client_addr) = listener.accept().await?;
        let servers = servers.clone();
        let state = state.clone();

        tokio::spawn(async move {
            let txid = Uuid::new_v4().to_string();
            let max_retries = 28;
            let delay = Duration::from_secs(1);
            let client_ip = client_addr.ip();

            let Some(selected_idx) =
                select_backend(&txid, client_ip, &servers, state.clone(), algorithm).await
            else {
                log_error(
                    &txid,
                    &format!("no healthy backend available for client {client_addr}"),
                );
                return;
            };

            let selected = servers[selected_idx].clone();

            match retry_connect(&txid, &selected, max_retries, delay).await {
                Ok(mut stream) => {
                    increment_open_connections(state.clone(), selected_idx).await;

                    let peer = match stream.peer_addr() {
                        Ok(addr) => addr.to_string(),
                        Err(_) => selected.clone(),
                    };

                    log_info(
                        &txid,
                        &format!("{client_addr} connecting to backend {peer}"),
                    );

                    if let Err(e) = tokio::io::copy_bidirectional(&mut inbound, &mut stream).await {
                        log_info(
                            &txid,
                            &format!("connection drop {client_addr} to backend {selected} - {e}"),
                        );
                    }

                    decrement_open_connections(state.clone(), selected_idx).await;

                    log_info(
                        &txid,
                        &format!("{client_addr} disconnected from backend {selected}"),
                    );
                }
                Err(e) => {
                    log_error(
                        &txid,
                        &format!(
                            "failed to connect after {max_retries} retries for {client_addr} to backend {selected} - {e:?}"
                        ),
                    );
                }
            }
        });
    }
}
