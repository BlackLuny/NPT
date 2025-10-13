use shared::{Message, MessageType, UserActivity, ConnectionType, MetricsCollector};
use rand::Rng;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::UdpSocket;
use uuid::Uuid;

#[derive(Clone)]
pub struct UdpClient {
    server_addr: SocketAddr,
    metrics: Arc<MetricsCollector>,
}

impl UdpClient {
    pub fn new(server_addr: SocketAddr, metrics: Arc<MetricsCollector>) -> Self {
        Self {
            server_addr,
            metrics,
        }
    }

    pub async fn simulate_user_activity(&self, activity: UserActivity) -> anyhow::Result<()> {
        match activity {
            UserActivity::Gaming { packets_per_second, packet_size_range } => {
                self.simulate_gaming_session(packets_per_second, packet_size_range).await
            }
            _ => Err(anyhow::anyhow!("UDP client can only handle gaming activities")),
        }
    }

    async fn simulate_gaming_session(
        &self,
        packets_per_second: u32,
        packet_size_range: (u32, u32),
    ) -> anyhow::Result<()> {
        let connection_id = Uuid::new_v4();
        let session_id = Uuid::new_v4();
        
        self.metrics.start_connection_with_activity(connection_id, ConnectionType::Udp, UserActivity::Gaming {
            packets_per_second,
            packet_size_range,
        });
        
        let session_duration = Duration::from_secs(rand::thread_rng().gen_range(10..=300));
        let packet_interval = Duration::from_nanos(1_000_000_000 / packets_per_second as u64);
        
        let socket = UdpSocket::bind("0.0.0.0:0").await?;
        socket.connect(self.server_addr).await?;
        
        let session_start = Instant::now();
        let mut packet_sequence = 0u64;
        let mut _last_packet_time = session_start;
        
        while session_start.elapsed() < session_duration {
            let packet_size = rand::thread_rng().gen_range(packet_size_range.0..=packet_size_range.1);
            
            match self.send_game_packet(&socket, &connection_id, session_id, packet_sequence, packet_size).await {
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!("Failed to send game packet: {}", e);
                    self.metrics.record_error(&connection_id);
                }
            }
            
            if rand::thread_rng().gen_bool(0.7) {
                match self.receive_game_packet(&socket, &connection_id).await {
                    Ok(_) => {}
                    Err(e) => {
                        tracing::debug!("No response packet received: {}", e);
                    }
                }
            }
            
            packet_sequence += 1;
            _last_packet_time = Instant::now();
            
            let jitter = Duration::from_nanos(rand::thread_rng().gen_range(0..packet_interval.as_nanos() / 4) as u64);
            let sleep_duration = packet_interval.saturating_sub(jitter);
            tokio::time::sleep(sleep_duration).await;
        }
        
        self.metrics.end_connection(&connection_id);
        Ok(())
    }

    async fn send_game_packet(
        &self,
        socket: &UdpSocket,
        connection_id: &Uuid,
        session_id: Uuid,
        sequence: u64,
        packet_size: u32,
    ) -> anyhow::Result<()> {
        let packet_data = self.generate_game_packet_data(packet_size, sequence);
        let game_packet = Message::new(MessageType::GamePacket, packet_data, session_id)
            .with_sequence(sequence);
        
        let fragments = self.fragment_packet(&game_packet, 1400)?;
        let send_start = Instant::now();
        
        for fragment in fragments {
            let serialized = serde_json::to_vec(&fragment)?;
            socket.send(&serialized).await?;
            
            self.metrics.record_packet_sent(connection_id);
            self.metrics.record_bytes_sent(connection_id, serialized.len() as u64);
        }
        
        let send_latency = send_start.elapsed();
        self.metrics.record_latency(connection_id, send_latency);
        
        Ok(())
    }

    async fn receive_game_packet(
        &self,
        socket: &UdpSocket,
        connection_id: &Uuid,
    ) -> anyhow::Result<()> {
        let mut buf = vec![0u8; 2048];
        
        tokio::time::timeout(Duration::from_millis(100), async {
            let (size, _) = socket.recv_from(&mut buf).await?;
            buf.truncate(size);
            
            let _response: Message = serde_json::from_slice(&buf)?;
            
            self.metrics.record_packet_received(connection_id);
            self.metrics.record_bytes_received(connection_id, size as u64);
            
            Ok::<(), anyhow::Error>(())
        }).await.map_err(|_| anyhow::anyhow!("Timeout waiting for response"))?
    }

    fn generate_game_packet_data(&self, size: u32, sequence: u64) -> Vec<u8> {
        let mut data = Vec::with_capacity(size as usize);
        
        data.extend_from_slice(&sequence.to_le_bytes());
        data.extend_from_slice(&chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default().to_le_bytes());
        
        while data.len() < size as usize {
            let byte_to_add = if data.len() < size as usize {
                rand::thread_rng().gen::<u8>()
            } else {
                break;
            };
            data.push(byte_to_add);
        }
        
        data.truncate(size as usize);
        data
    }

    fn fragment_packet(&self, packet: &Message, mtu: usize) -> anyhow::Result<Vec<Message>> {
        let serialized = serde_json::to_vec(packet)?;
        
        if serialized.len() <= mtu {
            return Ok(vec![packet.clone()]);
        }
        
        let mut fragments = Vec::new();
        let chunk_size = mtu - 100;
        let total_chunks = (serialized.len() + chunk_size - 1) / chunk_size;
        
        for (i, chunk) in serialized.chunks(chunk_size).enumerate() {
            let fragment_header = FragmentHeader {
                fragment_id: packet.id,
                total_fragments: total_chunks as u16,
                fragment_index: i as u16,
                is_last: i == total_chunks - 1,
            };
            
            let mut fragment_data = serde_json::to_vec(&fragment_header)?;
            fragment_data.extend_from_slice(chunk);
            
            let fragment_msg = Message::new(
                MessageType::GamePacket,
                fragment_data,
                packet.session_id,
            ).with_sequence(packet.sequence);
            
            fragments.push(fragment_msg);
        }
        
        Ok(fragments)
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
struct FragmentHeader {
    fragment_id: Uuid,
    total_fragments: u16,
    fragment_index: u16,
    is_last: bool,
}