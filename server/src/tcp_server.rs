use rand::Rng;
use shared::{ConnectionType, Message, MessageType, MetricsCollector};
use socket2::{Domain, Protocol, Socket, Type};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct TcpServerConfig {
    pub dual_stack: Option<bool>,
    pub udp_batch_size: Option<usize>,
    pub udp_buffer_size: Option<usize>,
}

impl Default for TcpServerConfig {
    fn default() -> Self {
        Self {
            dual_stack: None,
            udp_batch_size: None,
            udp_buffer_size: None,
        }
    }
}

pub struct TcpServer {
    listen_addr: SocketAddr,
    metrics: Arc<MetricsCollector>,
    config: TcpServerConfig,
}

impl TcpServer {
    pub fn new(listen_addr: SocketAddr, metrics: Arc<MetricsCollector>) -> Self {
        Self::new_with_config(listen_addr, metrics, TcpServerConfig::default())
    }

    pub fn new_with_config(
        listen_addr: SocketAddr,
        metrics: Arc<MetricsCollector>,
        config: TcpServerConfig,
    ) -> Self {
        Self {
            listen_addr,
            metrics,
            config,
        }
    }

    fn create_listener(&self) -> anyhow::Result<TcpListener> {
        let domain = if self.listen_addr.is_ipv6() {
            Domain::IPV6
        } else {
            Domain::IPV4
        };

        let socket = Socket::new(domain, Type::STREAM, Some(Protocol::TCP))?;

        // Configure dual stack for IPv6
        if let Some(dual_stack) = self.config.dual_stack {
            if self.listen_addr.is_ipv6() {
                socket.set_only_v6(!dual_stack)?;
            }
        }

        // Set UDP batch and buffer sizes (global variables from referenced code)
        if let Some(_udp_batch_size) = self.config.udp_batch_size {
            // Note: UDP_BATCH_SIZE would be set here if it were a global variable
            // This is included for compatibility with the referenced code pattern
        }
        if let Some(_udp_buffer_size) = self.config.udp_buffer_size {
            // Note: UDP_BUFFER_SIZE would be set here if it were a global variable
            // This is included for compatibility with the referenced code pattern
        }

        // Set TCP_NODELAY
        socket.set_tcp_nodelay(true)?;

        #[cfg(not(target_os = "windows"))]
        {
            // Enable SO_REUSEPORT for better load distribution
            socket.set_reuse_port(true)?;
        }

        // Bind to the address
        socket.bind(&socket2::SockAddr::from(self.listen_addr))?;

        // Listen with higher backlog (i32::MAX instead of default 1024)
        socket.listen(i32::MAX)?;

        // Set non-blocking mode
        socket.set_nonblocking(true)?;

        // Convert to tokio TcpListener
        let std_listener = std::net::TcpListener::from(socket);
        let tcp_listener = TcpListener::from_std(std_listener)?;

        Ok(tcp_listener)
    }

    pub async fn start(&self) -> anyhow::Result<()> {
        let listener = self.create_listener()?;
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
                    tracing::debug!("connection id: {connection_id:?} session id: {session_id:?} received message {}", message.msg_name());

                    metrics.record_packet_received(&connection_id);
                    metrics.record_bytes_received(&connection_id, message.payload.len() as u64);

                    let request_start = Instant::now();

                    match Self::handle_message(&mut stream, &message, &metrics, &connection_id)
                        .await
                    {
                        Ok(should_continue) => {
                            let response_time = request_start.elapsed();
                            if response_time.as_millis() > 1000 {
                                tracing::info!(
                                    "Response time: {:?} large message payload length: {:?} message type: {:?}",
                                    response_time,
                                    message.payload.len(),
                                    message.message_type
                                );
                            }
                            metrics.record_latency(&connection_id, response_time);

                            if !should_continue {
                                let _ = stream.shutdown().await;
                                drop(stream);
                                break;
                            }
                        }
                        Err(e) => {
                            let _ = stream.shutdown().await;
                            drop(stream);
                            tracing::warn!("Error handling message: {}", e);
                            metrics.record_error(&connection_id);
                            break;
                        }
                    }
                }
                Err(e) => {
                    let _ = stream.shutdown().await;
                    drop(stream);
                    if e.to_string().to_lowercase().contains("eof") {
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
                let response_size = rand::thread_rng().gen_range(4096..=32 * 1024);

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
                tracing::debug!(
                    "connection id: {connection_id:?} Received connection close request"
                );
                Ok(false)
            }
            _ => {
                tracing::warn!(
                    "connection id: {connection_id:?} Unexpected message type: {:?}",
                    message.message_type
                );
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
        let mut bytes_sent = 0u64;
        let mut sequence = 0u64;

        while bytes_sent < file_size {
            let chunk_size = rand::thread_rng().gen_range(4096..=65536);
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
        let serialized = rmp_serde::to_vec(&message)?;
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
        tokio::time::timeout(
            std::time::Duration::from_secs(30),
            stream.read_exact(&mut length_buf),
        )
        .await??;
        let length = u32::from_le_bytes(length_buf) as usize;

        if length > 10 * 1024 * 1024 {
            return Err(anyhow::anyhow!("Message too large: {} bytes", length));
        }

        let mut message_buf = vec![0u8; length];
        tokio::time::timeout(
            std::time::Duration::from_secs(30),
            stream.read_exact(&mut message_buf),
        )
        .await??;

        let message: Message = rmp_serde::from_slice(&message_buf)?;
        Ok(message)
    }
}
