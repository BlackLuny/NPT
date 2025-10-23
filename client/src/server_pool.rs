use dashmap::DashMap;
use shared::{LoadBalanceStrategy, LoadBalancerConfig};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::TcpStream;
use tokio::time::timeout;
use tracing::{debug, info, warn};
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct ServerInfo {
    pub address: SocketAddr,
    pub is_healthy: bool,
    pub last_health_check: Instant,
    pub consecutive_failures: u32,
    pub consecutive_successes: u32,
    pub active_connections: Arc<AtomicU32>,
    pub total_connections: Arc<AtomicU64>,
    pub last_used: Arc<AtomicU64>,
}

impl ServerInfo {
    pub fn new(address: SocketAddr) -> Self {
        Self {
            address,
            is_healthy: true,
            last_health_check: Instant::now(),
            consecutive_failures: 0,
            consecutive_successes: 0,
            active_connections: Arc::new(AtomicU32::new(0)),
            total_connections: Arc::new(AtomicU64::new(0)),
            last_used: Arc::new(AtomicU64::new(0)),
        }
    }

    pub fn increment_connections(&self) {
        self.active_connections.fetch_add(1, Ordering::Relaxed);
        self.total_connections.fetch_add(1, Ordering::Relaxed);
        self.last_used.store(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis() as u64,
            Ordering::Relaxed,
        );
    }

    pub fn decrement_connections(&self) {
        self.active_connections.fetch_sub(1, Ordering::Relaxed);
    }

    pub fn get_active_connections(&self) -> u32 {
        self.active_connections.load(Ordering::Relaxed)
    }

    pub fn get_total_connections(&self) -> u64 {
        self.total_connections.load(Ordering::Relaxed)
    }
}

#[derive(Clone)]
pub struct ServerPool {
    servers: Arc<DashMap<SocketAddr, ServerInfo>>,
    config: LoadBalancerConfig,
    round_robin_index: Arc<AtomicU64>,
}

impl ServerPool {
    pub fn new(addresses: Vec<SocketAddr>, config: LoadBalancerConfig) -> Self {
        let servers = DashMap::new();
        for addr in addresses {
            servers.insert(addr, ServerInfo::new(addr));
        }

        let pool = Self {
            servers: Arc::new(servers),
            config: config.clone(),
            round_robin_index: Arc::new(AtomicU64::new(0)),
        };

        if config.health_check.enabled && pool.servers.len() > 1 {
            pool.start_health_checker();
        }

        pool
    }

    pub fn get_server(&self) -> Option<SocketAddr> {
        let healthy_servers: Vec<_> = self
            .servers
            .iter()
            .filter(|server| server.is_healthy)
            .map(|s| s.value().clone())
            .collect();

        if healthy_servers.is_empty() {
            warn!("No healthy servers available");
            return None;
        }

        match self.config.strategy {
            LoadBalanceStrategy::RoundRobin => self.round_robin_select(&healthy_servers),
            LoadBalanceStrategy::Random => self.random_select(&healthy_servers),
            LoadBalanceStrategy::LeastConnections => {
                self.least_connections_select(&healthy_servers)
            }
            LoadBalanceStrategy::HashBased => self.hash_based_select(&healthy_servers),
        }
    }

    fn round_robin_select(&self, servers: &[ServerInfo]) -> Option<SocketAddr> {
        let index = self.round_robin_index.fetch_add(1, Ordering::Relaxed) as usize;
        servers.get(index % servers.len()).map(|s| s.address)
    }

    fn random_select(&self, servers: &[ServerInfo]) -> Option<SocketAddr> {
        use rand::Rng;
        let mut rng = rand::thread_rng();
        let index = rng.gen_range(0..servers.len());
        servers.get(index).map(|s| s.address)
    }

    fn least_connections_select(&self, servers: &[ServerInfo]) -> Option<SocketAddr> {
        servers
            .iter()
            .min_by_key(|server| server.get_active_connections())
            .map(|s| s.address)
    }

    fn hash_based_select(&self, servers: &[ServerInfo]) -> Option<SocketAddr> {
        let session_id = Uuid::new_v4();
        let hash = session_id.as_u128() % servers.len() as u128;
        servers.get(hash as usize).map(|s| s.address)
    }

    pub fn mark_connection_start(&self, address: SocketAddr) {
        if let Some(server) = self.servers.get(&address) {
            server.increment_connections();
            debug!(
                "Connection started to {}, active: {}",
                address,
                server.get_active_connections()
            );
        }
    }

    pub async fn mark_connection_end(&self, address: SocketAddr) {
        if let Some(server) = self.servers.get(&address) {
            server.decrement_connections();
            debug!(
                "Connection ended to {}, active: {}",
                address,
                server.get_active_connections()
            );
        }
    }

    pub fn mark_connection_failed(&self, address: SocketAddr, error: &str) {
        if let Some(mut server) = self.servers.get_mut(&address) {
            server.consecutive_failures += 1;
            server.consecutive_successes = 0;

            if server.consecutive_failures >= self.config.health_check.failure_threshold {
                server.is_healthy = false;
                warn!(
                    "Marking server {} as unhealthy after {} consecutive failures. Error: {}",
                    address, server.consecutive_failures, error
                );
            }
        }
    }

    pub fn mark_connection_success(&self, address: SocketAddr) {
        if let Some(mut server) = self.servers.get_mut(&address) {
            server.consecutive_successes += 1;
            server.consecutive_failures = 0;

            if !server.is_healthy
                && server.consecutive_successes >= self.config.health_check.recovery_threshold
            {
                server.is_healthy = true;
                info!(
                    "Server {} recovered after {} consecutive successes",
                    address, server.consecutive_successes
                );
            }
        }
    }

    fn start_health_checker(&self) {
        let servers = self.servers.clone();
        let config = self.config.clone();

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(config.health_check.interval);

            loop {
                interval.tick().await;

                let server_addrs: Vec<SocketAddr> =
                    { servers.iter().map(|s| s.key().clone()).collect() };

                for addr in server_addrs {
                    let servers_clone = servers.clone();
                    let config_clone = config.clone();

                    tokio::spawn(async move {
                        let health_result =
                            Self::check_server_health(addr, config_clone.health_check.timeout)
                                .await;

                        if let Some(mut server) = servers_clone.get_mut(&addr) {
                            server.last_health_check = Instant::now();

                            match health_result {
                                Ok(_) => {
                                    server.consecutive_successes += 1;
                                    server.consecutive_failures = 0;

                                    if !server.is_healthy
                                        && server.consecutive_successes
                                            >= config_clone.health_check.recovery_threshold
                                    {
                                        server.is_healthy = true;
                                        info!("Health check: Server {} recovered", addr);
                                    }
                                }
                                Err(e) => {
                                    server.consecutive_failures += 1;
                                    server.consecutive_successes = 0;

                                    if server.is_healthy
                                        && server.consecutive_failures
                                            >= config_clone.health_check.failure_threshold
                                    {
                                        server.is_healthy = false;
                                        warn!(
                                            "Health check: Server {} marked unhealthy: {}",
                                            addr, e
                                        );
                                    }
                                }
                            }
                        }
                    });
                }
            }
        });
    }

    async fn check_server_health(
        address: SocketAddr,
        health_timeout: Duration,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        timeout(health_timeout, TcpStream::connect(address))
            .await
            .map_err(|_| "Health check timeout")?
            .map_err(|e| e.to_string())?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shared::{
        ConnectionPoolConfig, HealthCheckConfig, LoadBalanceStrategy, LoadBalancerConfig,
    };
    use std::time::Duration;

    fn create_test_config() -> LoadBalancerConfig {
        LoadBalancerConfig {
            strategy: LoadBalanceStrategy::RoundRobin,
            health_check: HealthCheckConfig {
                enabled: false,
                timeout: Duration::from_secs(1),
                interval: Duration::from_secs(10),
                failure_threshold: 2,
                recovery_threshold: 1,
            },
            connection_pool: ConnectionPoolConfig::default(),
        }
    }

    #[tokio::test]
    async fn test_round_robin_selection() {
        let addresses = vec![
            "127.0.0.1:8001".parse().unwrap(),
            "127.0.0.1:8002".parse().unwrap(),
            "127.0.0.1:8003".parse().unwrap(),
        ];

        let pool = ServerPool::new(addresses.clone(), create_test_config());

        let mut selected_addresses = Vec::new();
        for _ in 0..6 {
            if let Some(addr) = pool.get_server() {
                selected_addresses.push(addr);
            }
        }

        assert_eq!(selected_addresses.len(), 6);
        assert_eq!(selected_addresses[0], addresses[0]);
        assert_eq!(selected_addresses[1], addresses[1]);
        assert_eq!(selected_addresses[2], addresses[2]);
        assert_eq!(selected_addresses[3], addresses[0]);
    }
}
