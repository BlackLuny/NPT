use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectionMetrics {
    pub id: Uuid,
    pub connection_type: crate::ConnectionType,
    pub start_time: chrono::DateTime<chrono::Utc>,
    pub end_time: Option<chrono::DateTime<chrono::Utc>>,
    pub bytes_sent: u64,
    pub bytes_received: u64,
    pub packets_sent: u64,
    pub packets_received: u64,
    pub errors: u64,
    pub latencies: Vec<Duration>,
    pub user_activity: Option<crate::UserActivity>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThroughputSample {
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub tcp_upload_bps: u64,
    pub tcp_download_bps: u64,
    pub udp_upload_bps: u64,
    pub udp_download_bps: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LatencySample {
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub tcp_latency_ms: f64,
    pub udp_latency_ms: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorSample {
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub tcp_errors: u64,
    pub udp_errors: u64,
}

#[derive(Debug)]
pub struct MetricsCollector {
    pub connections: Arc<RwLock<HashMap<Uuid, ConnectionMetrics>>>,
    pub throughput_history: Arc<RwLock<Vec<ThroughputSample>>>,
    pub latency_history: Arc<RwLock<Vec<LatencySample>>>,
    pub error_history: Arc<RwLock<Vec<ErrorSample>>>,
    pub start_time: Option<Instant>,
    last_sample_time: Arc<RwLock<Instant>>,
    last_tcp_bytes_sent: Arc<RwLock<u64>>,
    last_tcp_bytes_received: Arc<RwLock<u64>>,
    last_udp_bytes_sent: Arc<RwLock<u64>>,
    last_udp_bytes_received: Arc<RwLock<u64>>,
}

impl Default for MetricsCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl MetricsCollector {
    pub fn new() -> Self {
        let now = Instant::now();
        Self {
            connections: Arc::new(RwLock::new(HashMap::new())),
            throughput_history: Arc::new(RwLock::new(Vec::new())),
            latency_history: Arc::new(RwLock::new(Vec::new())),
            error_history: Arc::new(RwLock::new(Vec::new())),
            start_time: Some(now),
            last_sample_time: Arc::new(RwLock::new(now)),
            last_tcp_bytes_sent: Arc::new(RwLock::new(0)),
            last_tcp_bytes_received: Arc::new(RwLock::new(0)),
            last_udp_bytes_sent: Arc::new(RwLock::new(0)),
            last_udp_bytes_received: Arc::new(RwLock::new(0)),
        }
    }

    pub fn start_connection(&self, id: Uuid, connection_type: crate::ConnectionType) {
        let metrics = ConnectionMetrics {
            id,
            connection_type,
            start_time: chrono::Utc::now(),
            end_time: None,
            bytes_sent: 0,
            bytes_received: 0,
            packets_sent: 0,
            packets_received: 0,
            errors: 0,
            latencies: Vec::new(),
            user_activity: None,
        };
        self.connections.write().insert(id, metrics);
    }

    pub fn start_connection_with_activity(&self, id: Uuid, connection_type: crate::ConnectionType, activity: crate::UserActivity) {
        let metrics = ConnectionMetrics {
            id,
            connection_type,
            start_time: chrono::Utc::now(),
            end_time: None,
            bytes_sent: 0,
            bytes_received: 0,
            packets_sent: 0,
            packets_received: 0,
            errors: 0,
            latencies: Vec::new(),
            user_activity: Some(activity),
        };
        self.connections.write().insert(id, metrics);
    }

    pub fn end_connection(&self, id: &Uuid) {
        if let Some(metrics) = self.connections.write().get_mut(id) {
            metrics.end_time = Some(chrono::Utc::now());
        }
    }

    pub fn record_bytes_sent(&self, id: &Uuid, bytes: u64) {
        if let Some(metrics) = self.connections.write().get_mut(id) {
            metrics.bytes_sent += bytes;
        }
    }

    pub fn record_bytes_received(&self, id: &Uuid, bytes: u64) {
        if let Some(metrics) = self.connections.write().get_mut(id) {
            metrics.bytes_received += bytes;
        }
    }

    pub fn record_packet_sent(&self, id: &Uuid) {
        if let Some(metrics) = self.connections.write().get_mut(id) {
            metrics.packets_sent += 1;
        }
    }

    pub fn record_packet_received(&self, id: &Uuid) {
        if let Some(metrics) = self.connections.write().get_mut(id) {
            metrics.packets_received += 1;
        }
    }

    pub fn record_error(&self, id: &Uuid) {
        if let Some(metrics) = self.connections.write().get_mut(id) {
            metrics.errors += 1;
        }
    }

    pub fn record_latency(&self, id: &Uuid, latency: Duration) {
        if let Some(metrics) = self.connections.write().get_mut(id) {
            metrics.latencies.push(latency);
        }
    }

    pub fn sample_throughput(&self) {
        let now = Instant::now();
        let timestamp = chrono::Utc::now();
        
        let connections = self.connections.read();
        let mut tcp_upload = 0u64;
        let mut tcp_download = 0u64;
        let mut udp_upload = 0u64;
        let mut udp_download = 0u64;

        for metrics in connections.values() {
            match metrics.connection_type {
                crate::ConnectionType::Tcp => {
                    tcp_upload += metrics.bytes_sent;
                    tcp_download += metrics.bytes_received;
                }
                crate::ConnectionType::Udp => {
                    udp_upload += metrics.bytes_sent;
                    udp_download += metrics.bytes_received;
                }
            }
        }
        drop(connections);

        let mut last_sample_time = self.last_sample_time.write();
        let mut last_tcp_bytes_sent = self.last_tcp_bytes_sent.write();
        let mut last_tcp_bytes_received = self.last_tcp_bytes_received.write();
        let mut last_udp_bytes_sent = self.last_udp_bytes_sent.write();
        let mut last_udp_bytes_received = self.last_udp_bytes_received.write();

        let elapsed = now.duration_since(*last_sample_time).as_secs_f64();
        
        // Only calculate if enough time has passed to avoid division by very small numbers
        if elapsed >= 0.1 {
            // Calculate TCP deltas
            let tcp_upload_delta = tcp_upload.saturating_sub(*last_tcp_bytes_sent);
            let tcp_download_delta = tcp_download.saturating_sub(*last_tcp_bytes_received);
            
            // Calculate UDP deltas
            let udp_upload_delta = udp_upload.saturating_sub(*last_udp_bytes_sent);
            let udp_download_delta = udp_download.saturating_sub(*last_udp_bytes_received);
            
            // Calculate bytes per second for each protocol
            let tcp_upload_bps = (tcp_upload_delta as f64 / elapsed) as u64;
            let tcp_download_bps = (tcp_download_delta as f64 / elapsed) as u64;
            let udp_upload_bps = (udp_upload_delta as f64 / elapsed) as u64;
            let udp_download_bps = (udp_download_delta as f64 / elapsed) as u64;
            
            self.throughput_history.write().push(ThroughputSample {
                timestamp,
                tcp_upload_bps,
                tcp_download_bps,
                udp_upload_bps,
                udp_download_bps,
            });

            // Update last values for next calculation
            *last_tcp_bytes_sent = tcp_upload;
            *last_tcp_bytes_received = tcp_download;
            *last_udp_bytes_sent = udp_upload;
            *last_udp_bytes_received = udp_download;
            *last_sample_time = now;
        }
    }

    pub fn sample_latency(&self) {
        let timestamp = chrono::Utc::now();
        let connections = self.connections.read();
        
        let mut tcp_latencies = Vec::new();
        let mut udp_latencies = Vec::new();

        for metrics in connections.values() {
            match metrics.connection_type {
                crate::ConnectionType::Tcp => {
                    if !metrics.latencies.is_empty() {
                        let avg = metrics.latencies.iter().sum::<Duration>().as_millis() as f64 
                            / metrics.latencies.len() as f64;
                        tcp_latencies.push(avg);
                    }
                }
                crate::ConnectionType::Udp => {
                    if !metrics.latencies.is_empty() {
                        let avg = metrics.latencies.iter().sum::<Duration>().as_millis() as f64 
                            / metrics.latencies.len() as f64;
                        udp_latencies.push(avg);
                    }
                }
            }
        }
        drop(connections);

        let tcp_avg = if tcp_latencies.is_empty() {
            0.0
        } else {
            tcp_latencies.iter().sum::<f64>() / tcp_latencies.len() as f64
        };

        let udp_avg = if udp_latencies.is_empty() {
            0.0
        } else {
            udp_latencies.iter().sum::<f64>() / udp_latencies.len() as f64
        };

        self.latency_history.write().push(LatencySample {
            timestamp,
            tcp_latency_ms: tcp_avg,
            udp_latency_ms: udp_avg,
        });
    }

    pub fn sample_errors(&self) {
        let timestamp = chrono::Utc::now();
        let connections = self.connections.read();
        
        let mut tcp_errors = 0u64;
        let mut udp_errors = 0u64;

        for metrics in connections.values() {
            match metrics.connection_type {
                crate::ConnectionType::Tcp => tcp_errors += metrics.errors,
                crate::ConnectionType::Udp => udp_errors += metrics.errors,
            }
        }
        drop(connections);

        self.error_history.write().push(ErrorSample {
            timestamp,
            tcp_errors,
            udp_errors,
        });
    }

    pub fn get_active_tcp_connections(&self) -> usize {
        self.connections
            .read()
            .values()
            .filter(|m| matches!(m.connection_type, crate::ConnectionType::Tcp) && m.end_time.is_none())
            .count()
    }

    pub fn get_active_udp_connections(&self) -> usize {
        self.connections
            .read()
            .values()
            .filter(|m| matches!(m.connection_type, crate::ConnectionType::Udp) && m.end_time.is_none())
            .count()
    }

    pub fn get_latest_throughput(&self) -> Option<ThroughputSample> {
        self.throughput_history.read().last().cloned()
    }

    pub fn get_latest_latency(&self) -> Option<LatencySample> {
        self.latency_history.read().last().cloned()
    }

    pub fn get_latest_errors(&self) -> Option<ErrorSample> {
        self.error_history.read().last().cloned()
    }

    pub fn get_active_web_browsing_tasks(&self) -> usize {
        self.connections
            .read()
            .values()
            .filter(|m| {
                m.end_time.is_none() && 
                matches!(m.user_activity, Some(crate::UserActivity::WebBrowsing { .. }))
            })
            .count()
    }

    pub fn get_active_file_download_tasks(&self) -> usize {
        self.connections
            .read()
            .values()
            .filter(|m| {
                m.end_time.is_none() && 
                matches!(m.user_activity, Some(crate::UserActivity::FileDownload { .. }))
            })
            .count()
    }

    pub fn get_active_file_upload_tasks(&self) -> usize {
        self.connections
            .read()
            .values()
            .filter(|m| {
                m.end_time.is_none() && 
                matches!(m.user_activity, Some(crate::UserActivity::FileUpload { .. }))
            })
            .count()
    }
}