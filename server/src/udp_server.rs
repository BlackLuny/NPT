use shared::{Message, MessageType, MetricsCollector, ConnectionType};
use rand::Rng;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::UdpSocket;
use uuid::Uuid;
use parking_lot::RwLock;

pub struct UdpServer {
    listen_addr: SocketAddr,
    metrics: Arc<MetricsCollector>,
    active_sessions: Arc<RwLock<HashMap<SocketAddr, UdpSession>>>,
}

#[derive(Debug)]
struct UdpSession {
    id: Uuid,
    session_id: Uuid,
    start_time: Instant,
    last_activity: Instant,
    packets_received: u64,
    bytes_received: u64,
}

impl UdpServer {
    pub fn new(listen_addr: SocketAddr, metrics: Arc<MetricsCollector>) -> Self {
        Self {
            listen_addr,
            metrics,
            active_sessions: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub async fn start(&self) -> anyhow::Result<()> {
        let socket = UdpSocket::bind(self.listen_addr).await?;
        tracing::info!("UDP server listening on {}", self.listen_addr);

        let cleanup_sessions = self.active_sessions.clone();
        let cleanup_metrics = self.metrics.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(30));
            loop {
                interval.tick().await;
                Self::cleanup_inactive_sessions(&cleanup_sessions, &cleanup_metrics).await;
            }
        });

        let mut buf = vec![0u8; 65536];
        
        loop {
            match socket.recv_from(&mut buf).await {
                Ok((size, addr)) => {
                    let packet_data = buf[..size].to_vec();
                    let sessions = self.active_sessions.clone();
                    let metrics = self.metrics.clone();
                    let socket_ref = &socket;
                    
                    if let Err(e) = Self::handle_packet(
                        socket_ref, 
                        packet_data, 
                        addr, 
                        sessions, 
                        metrics
                    ).await {
                        tracing::warn!("Error handling UDP packet from {}: {}", addr, e);
                    }
                }
                Err(e) => {
                    tracing::error!("Failed to receive UDP packet: {}", e);
                }
            }
        }
    }

    async fn handle_packet(
        socket: &UdpSocket,
        packet_data: Vec<u8>,
        addr: SocketAddr,
        sessions: Arc<RwLock<HashMap<SocketAddr, UdpSession>>>,
        metrics: Arc<MetricsCollector>,
    ) -> anyhow::Result<()> {
        let message: Message = serde_json::from_slice(&packet_data)?;
        
        let _session_id = {
            let mut sessions_write = sessions.write();
            match sessions_write.get_mut(&addr) {
                Some(session) => {
                    session.last_activity = Instant::now();
                    session.packets_received += 1;
                    session.bytes_received += packet_data.len() as u64;
                    
                    metrics.record_packet_received(&session.id);
                    metrics.record_bytes_received(&session.id, packet_data.len() as u64);
                    
                    session.session_id
                }
                None => {
                    let connection_id = Uuid::new_v4();
                    let session = UdpSession {
                        id: connection_id,
                        session_id: message.session_id,
                        start_time: Instant::now(),
                        last_activity: Instant::now(),
                        packets_received: 1,
                        bytes_received: packet_data.len() as u64,
                    };
                    
                    metrics.start_connection(connection_id, ConnectionType::Udp);
                    metrics.record_packet_received(&connection_id);
                    metrics.record_bytes_received(&connection_id, packet_data.len() as u64);
                    
                    tracing::debug!("New UDP session from {} (id: {})", addr, connection_id);
                    
                    let session_id = session.session_id;
                    sessions_write.insert(addr, session);
                    session_id
                }
            }
        };

        if matches!(message.message_type, MessageType::GamePacket) {
            Self::handle_game_packet(socket, &message, addr, &sessions, &metrics).await?;
        }

        Ok(())
    }

    async fn handle_game_packet(
        socket: &UdpSocket,
        message: &Message,
        addr: SocketAddr,
        sessions: &Arc<RwLock<HashMap<SocketAddr, UdpSession>>>,
        metrics: &Arc<MetricsCollector>,
    ) -> anyhow::Result<()> {
        let should_respond = rand::thread_rng().gen_bool(0.7);
        
        if should_respond {
            let response_size = rand::thread_rng().gen_range(64..=1400);
            let response_data = Self::generate_game_response(response_size, message.sequence);
            let response = Message::new(MessageType::GamePacket, response_data, message.session_id)
                .with_sequence(message.sequence);
            
            let response_packet = serde_json::to_vec(&response)?;
            socket.send_to(&response_packet, addr).await?;
            
            if let Some(session) = sessions.read().get(&addr) {
                metrics.record_packet_sent(&session.id);
                metrics.record_bytes_sent(&session.id, response_packet.len() as u64);
                
                let response_latency = Duration::from_millis(rand::thread_rng().gen_range(1..=50));
                metrics.record_latency(&session.id, response_latency);
            }
        }

        Ok(())
    }

    fn generate_game_response(size: usize, sequence: u64) -> Vec<u8> {
        let mut data = Vec::with_capacity(size);
        
        data.extend_from_slice(&sequence.to_le_bytes());
        data.extend_from_slice(&chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default().to_le_bytes());
        
        while data.len() < size {
            let byte_to_add = if data.len() < size {
                rand::thread_rng().gen::<u8>()
            } else {
                break;
            };
            data.push(byte_to_add);
        }
        
        data.truncate(size);
        data
    }

    async fn cleanup_inactive_sessions(
        sessions: &Arc<RwLock<HashMap<SocketAddr, UdpSession>>>,
        metrics: &Arc<MetricsCollector>,
    ) {
        let mut to_remove = Vec::new();
        let inactive_threshold = Duration::from_secs(60);
        let now = Instant::now();

        {
            let sessions_read = sessions.read();
            for (addr, session) in sessions_read.iter() {
                if now.duration_since(session.last_activity) > inactive_threshold {
                    to_remove.push(*addr);
                }
            }
        }

        if !to_remove.is_empty() {
            let mut sessions_write = sessions.write();
            for addr in to_remove {
                if let Some(session) = sessions_write.remove(&addr) {
                    metrics.end_connection(&session.id);
                    let duration = session.last_activity.duration_since(session.start_time);
                    tracing::debug!(
                        "Cleaned up inactive UDP session {} from {} after {:?} ({} packets, {} bytes)",
                        session.id,
                        addr,
                        duration,
                        session.packets_received,
                        session.bytes_received
                    );
                }
            }
        }
    }
}