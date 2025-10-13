use rand::Rng;
use shared::{ConnectionType, Message, MessageType, MetricsCollector, UserActivity, ErrorType};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use uuid::Uuid;

#[derive(Clone)]
pub struct TcpClient {
    server_addr: SocketAddr,
    metrics: Arc<MetricsCollector>,
}

impl TcpClient {
    pub fn new(server_addr: SocketAddr, metrics: Arc<MetricsCollector>) -> Self {
        Self {
            server_addr,
            metrics,
        }
    }

    pub fn get_metrics(&self) -> Arc<MetricsCollector> {
        self.metrics.clone()
    }

    pub async fn simulate_user_activity(
        &self,
        activity: UserActivity,
        stoped: Arc<AtomicBool>,
    ) -> anyhow::Result<()> {
        match activity {
            UserActivity::WebBrowsing {
                pages_to_visit,
                requests_per_page,
            } => {
                self.simulate_web_browsing(pages_to_visit, requests_per_page, stoped)
                    .await
            }
            UserActivity::FileDownload {
                file_size,
                chunk_size,
            } => {
                self.simulate_file_download(file_size, chunk_size, stoped)
                    .await
            }
            UserActivity::FileUpload {
                file_size,
                chunk_size,
            } => {
                self.simulate_file_upload(file_size, chunk_size, stoped)
                    .await
            }
            _ => Err(anyhow::anyhow!("TCP client cannot handle UDP activities")),
        }
    }

    async fn simulate_web_browsing(
        &self,
        pages_to_visit: u32,
        requests_per_page: (u32, u32),
        stoped: Arc<AtomicBool>,
    ) -> anyhow::Result<()> {
        for _page in 0..pages_to_visit {
            let connection_id = Uuid::new_v4();
            let session_id = Uuid::new_v4();

            self.metrics.start_connection_with_activity(
                connection_id,
                ConnectionType::Tcp,
                UserActivity::WebBrowsing {
                    pages_to_visit,
                    requests_per_page,
                },
            );

            match self
                .simulate_single_page(connection_id, session_id, requests_per_page, stoped.clone())
                .await
            {
                Ok(_) => {}
                Err(e) => {
                    let error_type = if e.to_string().contains("connection refused") {
                        ErrorType::ConnectionFailed
                    } else if e.to_string().contains("timeout") {
                        ErrorType::NetworkTimeout
                    } else if e.to_string().contains("broken pipe") || e.to_string().contains("reset") {
                        ErrorType::UnexpectedDisconnection
                    } else {
                        ErrorType::Other
                    };
                    
                    tracing::warn!("Web browsing simulation failed: {}", e);
                    self.metrics.record_error_with_detail(
                        &connection_id,
                        error_type,
                        e.to_string(),
                        Some(format!("Web browsing - page {}", _page + 1))
                    );
                }
            }

            self.metrics.end_connection(&connection_id);

            let think_time = Duration::from_millis(rand::thread_rng().gen_range(100..500));
            tokio::time::sleep(think_time).await;
        }

        Ok(())
    }

    async fn simulate_single_page(
        &self,
        connection_id: Uuid,
        session_id: Uuid,
        requests_per_page: (u32, u32),
        stoped: Arc<AtomicBool>,
    ) -> anyhow::Result<()> {
        let mut stream = TcpStream::connect(self.server_addr).await?;

        let _ = stream.set_nodelay(true);

        let handshake_size = rand::thread_rng().gen_range(400..=2000);
        let handshake_data = vec![0u8; handshake_size];
        let handshake_msg = Message::new(MessageType::TlsHandshake, handshake_data, session_id);

        let request_start = Instant::now();
        self.send_message(&mut stream, &handshake_msg).await?;
        self.metrics.record_packet_sent(&connection_id);
        self.metrics
            .record_bytes_sent(&connection_id, handshake_size as u64);

        let response = self.receive_response(&mut stream).await?;
        let handshake_latency = request_start.elapsed();
        self.metrics
            .record_latency(&connection_id, handshake_latency);
        self.metrics.record_packet_received(&connection_id);
        self.metrics
            .record_bytes_received(&connection_id, response.payload.len() as u64);

        let num_requests = rand::thread_rng().gen_range(requests_per_page.0..=requests_per_page.1);

        for request_num in 0..num_requests {
            let request_size = rand::thread_rng().gen_range(1024..= 4*1024);
            let request_data = vec![0u8; request_size];
            let http_request = Message::new(MessageType::HttpRequest, request_data, session_id)
                .with_sequence(request_num as u64);

            let request_start = Instant::now();
            self.send_message(&mut stream, &http_request).await?;
            self.metrics.record_packet_sent(&connection_id);
            self.metrics
                .record_bytes_sent(&connection_id, request_size as u64);

            let response = self.receive_response(&mut stream).await?;
            let request_latency = request_start.elapsed();
            self.metrics.record_latency(&connection_id, request_latency);
            self.metrics.record_packet_received(&connection_id);
            self.metrics
                .record_bytes_received(&connection_id, response.payload.len() as u64);
            drop(response);
            if stoped.load(Ordering::Relaxed) {
                break;
            }
        }

        let close_msg = Message::new(MessageType::ConnectionClose, vec![], session_id);
        self.send_message(&mut stream, &close_msg).await?;

        Ok(())
    }

    async fn simulate_file_download(
        &self,
        file_size: u64,
        chunk_size: (u32, u32),
        stoped: Arc<AtomicBool>,
    ) -> anyhow::Result<()> {
        let connection_id = Uuid::new_v4();
        let session_id = Uuid::new_v4();

        self.metrics.start_connection_with_activity(
            connection_id,
            ConnectionType::Tcp,
            UserActivity::FileDownload {
                file_size,
                chunk_size,
            },
        );

        match self
            .perform_file_download(connection_id, session_id, file_size, chunk_size, stoped)
            .await
        {
            Ok(_) => {}
            Err(e) => {
                let error_type = if e.to_string().contains("connection refused") {
                    ErrorType::ConnectionFailed
                } else if e.to_string().contains("timeout") {
                    ErrorType::NetworkTimeout
                } else if e.to_string().contains("broken pipe") || e.to_string().contains("reset") {
                    ErrorType::UnexpectedDisconnection
                } else if e.to_string().contains("too large") {
                    ErrorType::MessageTooLarge
                } else {
                    ErrorType::IoError
                };
                
                tracing::warn!("File download simulation failed: {}", e);
                self.metrics.record_error_with_detail(
                    &connection_id,
                    error_type,
                    e.to_string(),
                    Some(format!("File download - size: {} bytes", file_size))
                );
            }
        }

        self.metrics.end_connection(&connection_id);
        Ok(())
    }

    async fn perform_file_download(
        &self,
        connection_id: Uuid,
        session_id: Uuid,
        file_size: u64,
        _chunk_size: (u32, u32),
        stoped: Arc<AtomicBool>,
    ) -> anyhow::Result<()> {
        let mut stream = TcpStream::connect(self.server_addr).await?;

        let _ = stream.set_nodelay(true);
        let request_data = file_size.to_le_bytes().to_vec();
        let download_request =
            Message::new(MessageType::FileDownloadRequest, request_data, session_id);

        let request_start = Instant::now();
        self.send_message(&mut stream, &download_request).await?;
        self.metrics.record_packet_sent(&connection_id);
        self.metrics
            .record_bytes_sent(&connection_id, download_request.payload.len() as u64);

        let mut bytes_received = 0u64;
        let mut chunk_count = 0u64;

        while bytes_received < file_size {
            let chunk = self.receive_response(&mut stream).await?;
            bytes_received += chunk.payload.len() as u64;
            chunk_count += 1;

            self.metrics.record_packet_received(&connection_id);
            self.metrics
                .record_bytes_received(&connection_id, chunk.payload.len() as u64);

            if chunk_count == 1 {
                let initial_latency = request_start.elapsed();
                self.metrics.record_latency(&connection_id, initial_latency);
            }

            if bytes_received >= file_size || stoped.load(Ordering::Relaxed) {
                break;
            }
        }

        let close_msg = Message::new(MessageType::ConnectionClose, vec![], session_id);
        self.send_message(&mut stream, &close_msg).await?;

        Ok(())
    }

    async fn simulate_file_upload(
        &self,
        file_size: u64,
        chunk_size: (u32, u32),
        stoped: Arc<AtomicBool>,
    ) -> anyhow::Result<()> {
        let connection_id = Uuid::new_v4();
        let session_id = Uuid::new_v4();

        self.metrics.start_connection_with_activity(
            connection_id,
            ConnectionType::Tcp,
            UserActivity::FileUpload {
                file_size,
                chunk_size,
            },
        );

        match self
            .perform_file_upload(connection_id, session_id, file_size, chunk_size, stoped)
            .await
        {
            Ok(_) => {}
            Err(e) => {
                let error_type = if e.to_string().contains("connection refused") {
                    ErrorType::ConnectionFailed
                } else if e.to_string().contains("timeout") {
                    ErrorType::NetworkTimeout
                } else if e.to_string().contains("broken pipe") || e.to_string().contains("reset") {
                    ErrorType::UnexpectedDisconnection
                } else if e.to_string().contains("too large") {
                    ErrorType::MessageTooLarge
                } else {
                    ErrorType::IoError
                };
                
                tracing::warn!("File upload simulation failed: {}", e);
                self.metrics.record_error_with_detail(
                    &connection_id,
                    error_type,
                    e.to_string(),
                    Some(format!("File upload - size: {} bytes", file_size))
                );
            }
        }

        self.metrics.end_connection(&connection_id);
        Ok(())
    }

    async fn perform_file_upload(
        &self,
        connection_id: Uuid,
        session_id: Uuid,
        file_size: u64,
        chunk_size: (u32, u32),
        stoped: Arc<AtomicBool>,
    ) -> anyhow::Result<()> {
        let mut stream = TcpStream::connect(self.server_addr).await?;

        let _ = stream.set_nodelay(true);
        let upload_request_data = file_size.to_le_bytes().to_vec();
        let upload_request = Message::new(
            MessageType::FileUploadRequest,
            upload_request_data,
            session_id,
        );

        let request_start = Instant::now();
        self.send_message(&mut stream, &upload_request).await?;
        self.metrics.record_packet_sent(&connection_id);
        self.metrics
            .record_bytes_sent(&connection_id, upload_request.payload.len() as u64);

        let ack_response = self.receive_response(&mut stream).await?;
        let initial_latency = request_start.elapsed();
        self.metrics.record_latency(&connection_id, initial_latency);
        self.metrics.record_packet_received(&connection_id);
        self.metrics
            .record_bytes_received(&connection_id, ack_response.payload.len() as u64);

        let mut bytes_sent = 0u64;
        let mut sequence = 0u64;

        while bytes_sent < file_size {
            let current_chunk_size = std::cmp::min(
                rand::thread_rng().gen_range(chunk_size.0..=chunk_size.1) as u64,
                file_size - bytes_sent,
            ) as usize;

            let chunk_start = Instant::now();
            self.send_chunk_streaming(
                &mut stream,
                current_chunk_size,
                session_id,
                sequence,
                &connection_id,
            )
            .await?;

            let ack = self.receive_response(&mut stream).await?;
            let chunk_latency = chunk_start.elapsed();
            self.metrics.record_latency(&connection_id, chunk_latency);
            self.metrics.record_packet_received(&connection_id);
            self.metrics
                .record_bytes_received(&connection_id, ack.payload.len() as u64);

            bytes_sent += current_chunk_size as u64;
            sequence += 1;

            if stoped.load(Ordering::Relaxed) {
                break;
            }
        }

        let close_msg = Message::new(MessageType::ConnectionClose, vec![], session_id);
        self.send_message(&mut stream, &close_msg).await?;

        Ok(())
    }

    async fn send_chunk_streaming(
        &self,
        stream: &mut TcpStream,
        total_size: usize,
        session_id: Uuid,
        sequence: u64,
        connection_id: &Uuid,
    ) -> anyhow::Result<()> {
        let chunk_data = vec![0u8; total_size];

        let chunk_msg = Message::new(MessageType::FileUploadChunk, chunk_data, session_id)
            .with_sequence(sequence);

        self.send_message(stream, &chunk_msg).await?;
        self.metrics.record_packet_sent(connection_id);
        self.metrics
            .record_bytes_sent(connection_id, total_size as u64);

        Ok(())
    }

    async fn send_message(&self, stream: &mut TcpStream, message: &Message) -> anyhow::Result<()> {
        let serialized = serde_json::to_vec(message)?;
        let length = serialized.len() as u32;

        stream.write_all(&length.to_le_bytes()).await?;
        stream.write_all(&serialized).await?;
        stream.flush().await?;

        Ok(())
    }

    async fn receive_response(&self, stream: &mut TcpStream) -> anyhow::Result<Message> {
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
