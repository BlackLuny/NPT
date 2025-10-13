use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::time::Duration;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestConfig {
    pub duration: Duration,
    pub concurrent_users: u32,
    pub server_addresses: Vec<SocketAddr>,
    pub load_balancer: LoadBalancerConfig,
    pub client_bind_address: Option<SocketAddr>,
    pub user_behavior: UserBehaviorConfig,
    pub reporting: ReportingConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoadBalancerConfig {
    pub strategy: LoadBalanceStrategy,
    pub health_check: HealthCheckConfig,
    pub connection_pool: ConnectionPoolConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum LoadBalanceStrategy {
    RoundRobin,
    Random,
    LeastConnections,
    HashBased,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthCheckConfig {
    pub enabled: bool,
    pub timeout: Duration,
    pub interval: Duration,
    pub failure_threshold: u32,
    pub recovery_threshold: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectionPoolConfig {
    pub max_connections_per_server: u32,
    pub connection_timeout: Duration,
    pub retry_attempts: u32,
    pub retry_backoff_base: Duration,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserBehaviorConfig {
    pub think_time_range: (Duration, Duration),
    pub connection_interval_range: (Duration, Duration),
    pub activity_weights: ActivityWeights,
    pub tcp_settings: TcpSettings,
    pub udp_settings: UdpSettings,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActivityWeights {
    pub web_browsing: f32,
    pub file_download: f32,
    pub file_upload: f32,
    pub gaming: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TcpSettings {
    pub handshake_size_range: (u32, u32),
    pub initial_response_size_range: (u32, u32),
    pub request_size_range: (u32, u32),
    pub response_size_range: (u32, u32),
    pub max_requests_per_connection: u32,
    pub keep_alive_timeout: Duration,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UdpSettings {
    pub packet_size_range: (u32, u32),
    pub packets_per_second_range: (u32, u32),
    pub session_duration_range: (Duration, Duration),
    pub fragmentation_threshold: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportingConfig {
    pub output_format: OutputFormat,
    pub output_path: String,
    pub include_raw_data: bool,
    pub metrics_interval: Duration,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum OutputFormat {
    Json,
    Csv,
    Html,
}

impl Default for TestConfig {
    fn default() -> Self {
        Self {
            duration: Duration::from_secs(300),
            concurrent_users: 100,
            server_addresses: vec!["127.0.0.1:8080".parse().unwrap()],
            load_balancer: LoadBalancerConfig::default(),
            client_bind_address: None,
            user_behavior: UserBehaviorConfig::default(),
            reporting: ReportingConfig::default(),
        }
    }
}

impl Default for LoadBalancerConfig {
    fn default() -> Self {
        Self {
            strategy: LoadBalanceStrategy::RoundRobin,
            health_check: HealthCheckConfig::default(),
            connection_pool: ConnectionPoolConfig::default(),
        }
    }
}

impl Default for HealthCheckConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            timeout: Duration::from_secs(5),
            interval: Duration::from_secs(30),
            failure_threshold: 3,
            recovery_threshold: 2,
        }
    }
}

impl Default for ConnectionPoolConfig {
    fn default() -> Self {
        Self {
            max_connections_per_server: 1000,
            connection_timeout: Duration::from_secs(10),
            retry_attempts: 3,
            retry_backoff_base: Duration::from_millis(100),
        }
    }
}

impl TestConfig {
    pub fn with_single_server(mut self, server_address: SocketAddr) -> Self {
        self.server_addresses = vec![server_address];
        self
    }
    
    pub fn with_servers(mut self, server_addresses: Vec<SocketAddr>) -> Self {
        self.server_addresses = server_addresses;
        self
    }
    
    pub fn primary_server(&self) -> Option<SocketAddr> {
        self.server_addresses.first().copied()
    }
}

impl Default for UserBehaviorConfig {
    fn default() -> Self {
        Self {
            think_time_range: (Duration::from_millis(100), Duration::from_secs(10)),
            connection_interval_range: (Duration::from_millis(50), Duration::from_secs(2)),
            activity_weights: ActivityWeights {
                web_browsing: 0.98,
                file_download: 0.000,
                file_upload: 0.000,
                gaming: 0.000,
            },
            tcp_settings: TcpSettings::default(),
            udp_settings: UdpSettings::default(),
        }
    }
}

impl Default for TcpSettings {
    fn default() -> Self {
        Self {
            handshake_size_range: (400, 2000),
            initial_response_size_range: (2000, 4000),
            request_size_range: (1024, 32768),
            response_size_range: (4096, 5242880),
            max_requests_per_connection: 20,
            keep_alive_timeout: Duration::from_secs(30),
        }
    }
}

impl Default for UdpSettings {
    fn default() -> Self {
        Self {
            packet_size_range: (64, 1280),
            packets_per_second_range: (10, 100),
            session_duration_range: (Duration::from_secs(10), Duration::from_secs(300)),
            fragmentation_threshold: 1280,
        }
    }
}

impl Default for ReportingConfig {
    fn default() -> Self {
        Self {
            output_format: OutputFormat::Json,
            output_path: "./test_report".to_string(),
            include_raw_data: false,
            metrics_interval: Duration::from_secs(1),
        }
    }
}