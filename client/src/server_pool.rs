use shared::{LoadBalanceStrategy, LoadBalancerConfig};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::TcpStream;
use tokio::sync::RwLock;
use tokio::time::timeout;
use tracing::{debug, warn, info};
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
    servers: Arc<RwLock<HashMap<SocketAddr, ServerInfo>>>,
    config: LoadBalancerConfig,
    round_robin_index: Arc<AtomicU64>,
}

impl ServerPool {
    pub fn new(addresses: Vec<SocketAddr>, config: LoadBalancerConfig) -> Self {
        let mut servers = HashMap::new();
        for addr in addresses {
            servers.insert(addr, ServerInfo::new(addr));
        }

        let pool = Self {
            servers: Arc::new(RwLock::new(servers)),
            config: config.clone(),
            round_robin_index: Arc::new(AtomicU64::new(0)),
        };

        if config.health_check.enabled {
            pool.start_health_checker();
        }

        pool
    }

    pub async fn get_server(&self) -> Option<SocketAddr> {
        let servers = self.servers.read().await;
        let healthy_servers: Vec<_> = servers
            .values()
            .filter(|server| server.is_healthy)
            .collect();

        if healthy_servers.is_empty() {
            warn!("No healthy servers available");
            return None;
        }

        match self.config.strategy {
            LoadBalanceStrategy::RoundRobin => {
                self.round_robin_select(&healthy_servers).await
            }
            LoadBalanceStrategy::Random => {
                self.random_select(&healthy_servers).await
            }
            LoadBalanceStrategy::LeastConnections => {
                self.least_connections_select(&healthy_servers).await
            }
            LoadBalanceStrategy::HashBased => {
                self.hash_based_select(&healthy_servers).await
            }
        }
    }

    async fn round_robin_select(&self, servers: &[&ServerInfo]) -> Option<SocketAddr> {
        let index = self.round_robin_index.fetch_add(1, Ordering::Relaxed) as usize;
        servers.get(index % servers.len()).map(|s| s.address)
    }

    async fn random_select(&self, servers: &[&ServerInfo]) -> Option<SocketAddr> {
        use rand::Rng;
        let mut rng = rand::thread_rng();
        let index = rng.gen_range(0..servers.len());
        servers.get(index).map(|s| s.address)
    }

    async fn least_connections_select(&self, servers: &[&ServerInfo]) -> Option<SocketAddr> {
        servers
            .iter()
            .min_by_key(|server| server.get_active_connections())
            .map(|s| s.address)
    }

    async fn hash_based_select(&self, servers: &[&ServerInfo]) -> Option<SocketAddr> {
        let session_id = Uuid::new_v4();
        let hash = session_id.as_u128() % servers.len() as u128;
        servers.get(hash as usize).map(|s| s.address)
    }

    pub async fn mark_connection_start(&self, address: SocketAddr) {
        let servers = self.servers.read().await;
        if let Some(server) = servers.get(&address) {
            server.increment_connections();
            debug!("Connection started to {}, active: {}", address, server.get_active_connections());
        }
    }

    pub async fn mark_connection_end(&self, address: SocketAddr) {
        let servers = self.servers.read().await;
        if let Some(server) = servers.get(&address) {
            server.decrement_connections();
            debug!("Connection ended to {}, active: {}", address, server.get_active_connections());
        }
    }

    pub async fn mark_connection_failed(&self, address: SocketAddr, error: &str) {
        let mut servers = self.servers.write().await;
        if let Some(server) = servers.get_mut(&address) {
            server.consecutive_failures += 1;
            server.consecutive_successes = 0;
            
            if server.consecutive_failures >= self.config.health_check.failure_threshold {
                server.is_healthy = false;
                warn!("Marking server {} as unhealthy after {} consecutive failures. Error: {}", 
                     address, server.consecutive_failures, error);
            }
        }
    }

    pub async fn mark_connection_success(&self, address: SocketAddr) {
        let mut servers = self.servers.write().await;
        if let Some(server) = servers.get_mut(&address) {
            server.consecutive_successes += 1;
            server.consecutive_failures = 0;
            
            if !server.is_healthy && server.consecutive_successes >= self.config.health_check.recovery_threshold {
                server.is_healthy = true;
                info!("Server {} recovered after {} consecutive successes", 
                     address, server.consecutive_successes);
            }
        }
    }

    pub async fn get_healthy_servers(&self) -> Vec<SocketAddr> {
        let servers = self.servers.read().await;
        servers
            .values()
            .filter(|server| server.is_healthy)
            .map(|server| server.address)
            .collect()
    }

    pub async fn get_server_stats(&self) -> Vec<(SocketAddr, u32, u64, bool)> {
        let servers = self.servers.read().await;
        servers
            .values()
            .map(|server| (
                server.address,
                server.get_active_connections(),
                server.get_total_connections(),
                server.is_healthy,
            ))
            .collect()
    }

    fn start_health_checker(&self) {
        let servers = self.servers.clone();
        let config = self.config.clone();
        
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(config.health_check.interval);
            
            loop {
                interval.tick().await;
                
                let server_addrs: Vec<SocketAddr> = {
                    let servers_guard = servers.read().await;
                    servers_guard.keys().cloned().collect()
                };

                for addr in server_addrs {
                    let servers_clone = servers.clone();
                    let config_clone = config.clone();
                    
                    tokio::spawn(async move {
                        let health_result = Self::check_server_health(addr, config_clone.health_check.timeout).await;
                        let mut servers_guard = servers_clone.write().await;
                        
                        if let Some(server) = servers_guard.get_mut(&addr) {
                            server.last_health_check = Instant::now();
                            
                            match health_result {
                                Ok(_) => {
                                    server.consecutive_successes += 1;
                                    server.consecutive_failures = 0;
                                    
                                    if !server.is_healthy && 
                                       server.consecutive_successes >= config_clone.health_check.recovery_threshold {
                                        server.is_healthy = true;
                                        info!("Health check: Server {} recovered", addr);
                                    }
                                }
                                Err(e) => {
                                    server.consecutive_failures += 1;
                                    server.consecutive_successes = 0;
                                    
                                    if server.is_healthy && 
                                       server.consecutive_failures >= config_clone.health_check.failure_threshold {
                                        server.is_healthy = false;
                                        warn!("Health check: Server {} marked unhealthy: {}", addr, e);
                                    }
                                }
                            }
                        }
                    });
                }
            }
        });
    }

    async fn check_server_health(address: SocketAddr, health_timeout: Duration) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
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
    use shared::{LoadBalancerConfig, LoadBalanceStrategy, HealthCheckConfig, ConnectionPoolConfig};
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
            if let Some(addr) = pool.get_server().await {
                selected_addresses.push(addr);
            }
        }
        
        assert_eq!(selected_addresses.len(), 6);
        assert_eq!(selected_addresses[0], addresses[0]);
        assert_eq!(selected_addresses[1], addresses[1]);
        assert_eq!(selected_addresses[2], addresses[2]);
        assert_eq!(selected_addresses[3], addresses[0]);
    }

    #[tokio::test]
    async fn test_server_failure_recovery() {
        let addresses = vec!["127.0.0.1:8001".parse().unwrap()];
        let pool = ServerPool::new(addresses.clone(), create_test_config());
        
        pool.mark_connection_failed(addresses[0], "Connection refused").await;
        pool.mark_connection_failed(addresses[0], "Connection refused").await;
        
        let healthy_servers = pool.get_healthy_servers().await;
        assert!(healthy_servers.is_empty());
        
        pool.mark_connection_success(addresses[0]).await;
        
        let healthy_servers = pool.get_healthy_servers().await;
        assert_eq!(healthy_servers, addresses);
    }
}