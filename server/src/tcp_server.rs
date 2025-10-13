use rand::Rng;
use shared::{ConnectionType, Message, MessageType, MetricsCollector};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use uuid::Uuid;

pub struct TcpServer {
    listen_addr: SocketAddr,
    metrics: Arc<MetricsCollector>,
}

impl TcpServer {
    pub fn new(listen_addr: SocketAddr, metrics: Arc<MetricsCollector>) -> Self {
        Self {
            listen_addr,
            metrics,
        }
    }

    pub async fn start(&self) -> anyhow::Result<()> {
        let listener = TcpListener::bind(self.listen_addr).await?;
        tracing::info!("TCP server listening on {}", self.listen_addr);

        loop {
            match listener.accept().await {
                Ok((stream, addr)) => {
                    let _ = stream.set_nodelay(true);
                    let metrics = self.metrics.clone();
                    tokio::spawn(async move {
                        if let Err(e) = Self::handle_connection(stream, addr, metrics).await {
                            tracing::warn!("TCP connection error from {}: {}", addr, e);
                        }
                    });
                }
                Err(e) => {
                    tracing::error!("Failed to accept TCP connection: {}", e);
                }
            }
        }
    }

    async fn handle_connection(
        mut stream: TcpStream,
        addr: SocketAddr,
        metrics: Arc<MetricsCollector>,
    ) -> anyhow::Result<()> {
        let connection_id = Uuid::new_v4();
        let _local_addr = stream.local_addr()?;

        tracing::debug!("New TCP connection from {} (id: {})", addr, connection_id);
        metrics.start_connection(connection_id, ConnectionType::Tcp);

        let connection_start = Instant::now();
        let mut session_id: Option<Uuid> = None;

        loop {
            match Self::receive_message(&mut stream).await {
                Ok(message) => {
                    if session_id.is_none() {
                        session_id = Some(message.session_id);
                    }

                    metrics.record_packet_received(&connection_id);
                    metrics.record_bytes_received(&connection_id, message.payload.len() as u64);

                    let request_start = Instant::now();

                    match Self::handle_message(&mut stream, &message, &metrics, &connection_id)
                        .await
                    {
                        Ok(should_continue) => {
                            let response_time = request_start.elapsed();
                            metrics.record_latency(&connection_id, response_time);

                            if !should_continue {
                                break;
                            }
                        }
                        Err(e) => {
                            tracing::warn!("Error handling message: {}", e);
                            metrics.record_error(&connection_id);
                            break;
                        }
                    }
                }
                Err(e) => {
                    if e.to_string().contains("EOF") {
                        tracing::debug!("Client disconnected: {}", addr);
                    } else {
                        tracing::warn!("Error receiving message from {}: {}", addr, e);
                        metrics.record_error(&connection_id);
                    }
                    break;
                }
            }
        }

        let connection_duration = connection_start.elapsed();
        tracing::debug!(
            "TCP connection {} closed after {:?}",
            connection_id,
            connection_duration
        );
        metrics.end_connection(&connection_id);

        Ok(())
    }

    async fn handle_message(
        stream: &mut TcpStream,
        message: &Message,
        metrics: &Arc<MetricsCollector>,
        connection_id: &Uuid,
    ) -> anyhow::Result<bool> {
        match message.message_type {
            MessageType::TlsHandshake => {
                let response_size = rand::thread_rng().gen_range(2000..=4000);
                let response_data = vec![0u8; response_size];
                let response =
                    Message::new(MessageType::HttpResponse, response_data, message.session_id);

                Self::send_message(stream, response).await?;
                metrics.record_packet_sent(connection_id);
                metrics.record_bytes_sent(connection_id, response_size as u64);

                Ok(true)
            }
            MessageType::HttpRequest => {
                let response_size = rand::thread_rng().gen_range(4096..=512 * 1024);

                Self::send_large_response_streaming(
                    stream,
                    response_size,
                    message.session_id,
                    message.sequence,
                    metrics,
                    connection_id,
                )
                .await?;

                Ok(true)
            }
            MessageType::FileDownloadRequest => {
                let requested_size = if message.payload.len() >= 8 {
                    u64::from_le_bytes(message.payload[0..8].try_into().unwrap())
                } else {
                    1024 * 1024
                };

                Self::handle_file_download(stream, message, requested_size, metrics, connection_id)
                    .await?;
                Ok(true)
            }
            MessageType::FileUploadRequest => {
                let file_size = if message.payload.len() >= 8 {
                    u64::from_le_bytes(message.payload[0..8].try_into().unwrap())
                } else {
                    1024 * 1024
                };

                let ack_response = Message::new(
                    MessageType::HttpResponse,
                    b"ACK".to_vec(),
                    message.session_id,
                );
                Self::send_message(stream, ack_response).await?;
                metrics.record_packet_sent(connection_id);
                metrics.record_bytes_sent(connection_id, 3);

                Self::handle_file_upload(stream, message, file_size, metrics, connection_id)
                    .await?;
                Ok(true)
            }
            MessageType::FileUploadChunk => {
                let ack_response = Message::new(
                    MessageType::HttpResponse,
                    b"CHUNK_ACK".to_vec(),
                    message.session_id,
                )
                .with_sequence(message.sequence);

                Self::send_message(stream, ack_response).await?;
                metrics.record_packet_sent(connection_id);
                metrics.record_bytes_sent(connection_id, 9);

                Ok(true)
            }
            MessageType::ConnectionClose => {
                tracing::debug!("Received connection close request");
                Ok(false)
            }
            _ => {
                tracing::warn!("Unexpected message type: {:?}", message.message_type);
                Ok(true)
            }
        }
    }

    async fn handle_file_download(
        stream: &mut TcpStream,
        message: &Message,
        file_size: u64,
        metrics: &Arc<MetricsCollector>,
        connection_id: &Uuid,
    ) -> anyhow::Result<()> {
        let chunk_size = rand::thread_rng().gen_range(4096..=65536);
        let mut bytes_sent = 0u64;
        let mut sequence = 0u64;

        while bytes_sent < file_size {
            let current_chunk_size =
                std::cmp::min(chunk_size as u64, file_size - bytes_sent) as usize;

            Self::send_chunk_streaming(
                stream,
                current_chunk_size,
                message.session_id,
                sequence,
                metrics,
                connection_id,
            )
            .await?;

            bytes_sent += current_chunk_size as u64;
            sequence += 1;

            let delay = Duration::from_millis(rand::thread_rng().gen_range(1..=10));
            tokio::time::sleep(delay).await;
        }

        Ok(())
    }

    async fn send_chunk_streaming(
        stream: &mut TcpStream,
        total_size: usize,
        session_id: Uuid,
        sequence: u64,
        metrics: &Arc<MetricsCollector>,
        connection_id: &Uuid,
    ) -> anyhow::Result<()> {
        let chunk_parts = vec![0u8; total_size];

        let chunk_message = Message::new(MessageType::FileDownloadChunk, chunk_parts, session_id)
            .with_sequence(sequence);

        Self::send_message(stream, chunk_message).await?;
        metrics.record_packet_sent(connection_id);
        metrics.record_bytes_sent(connection_id, total_size as u64);

        Ok(())
    }

    async fn handle_file_upload(
        stream: &mut TcpStream,
        _message: &Message,
        expected_size: u64,
        metrics: &Arc<MetricsCollector>,
        connection_id: &Uuid,
    ) -> anyhow::Result<()> {
        let mut bytes_received = 0u64;

        while bytes_received < expected_size {
            let chunk_message = Self::receive_message(stream).await?;

            if !matches!(chunk_message.message_type, MessageType::FileUploadChunk) {
                break;
            }

            metrics.record_packet_received(connection_id);
            metrics.record_bytes_received(connection_id, chunk_message.payload.len() as u64);
            bytes_received += chunk_message.payload.len() as u64;

            let ack_response = Message::new(
                MessageType::HttpResponse,
                b"CHUNK_ACK".to_vec(),
                chunk_message.session_id,
            )
            .with_sequence(chunk_message.sequence);

            Self::send_message(stream, ack_response).await?;
            metrics.record_packet_sent(connection_id);
            metrics.record_bytes_sent(connection_id, 9);

            if bytes_received >= expected_size {
                break;
            }
        }

        Ok(())
    }

    async fn send_large_response_streaming(
        stream: &mut TcpStream,
        response_size: usize,
        session_id: Uuid,
        sequence: u64,
        metrics: &Arc<MetricsCollector>,
        connection_id: &Uuid,
    ) -> anyhow::Result<()> {
        let response_data = vec![0u8; response_size];
        let response = Message::new(MessageType::HttpResponse, response_data, session_id)
            .with_sequence(sequence);

        Self::send_message(stream, response).await?;
        metrics.record_packet_sent(connection_id);
        metrics.record_bytes_sent(connection_id, response_size as u64);

        Ok(())
    }

    async fn send_message(stream: &mut TcpStream, message: Message) -> anyhow::Result<()> {
        let serialized = serde_json::to_vec(&message)?;
        drop(message);
        let length = serialized.len() as u32;

        stream.write_all(&length.to_le_bytes()).await?;
        stream.write_all(&serialized).await?;
        drop(serialized);
        stream.flush().await?;

        Ok(())
    }

    async fn receive_message(stream: &mut TcpStream) -> anyhow::Result<Message> {
        let mut length_buf = [0u8; 4];
        stream.read_exact(&mut length_buf).await?;
        let length = u32::from_le_bytes(length_buf) as usize;

        if length > 10 * 1024 * 1024 {
            return Err(anyhow::anyhow!("Message too large: {} bytes", length));
        }

        let mut message_buf = vec![0u8; length];
        stream.read_exact(&mut message_buf).await?;

        let message: Message = serde_json::from_slice(&message_buf)?;
        Ok(message)
    }
}
