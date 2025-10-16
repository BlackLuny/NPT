use parking_lot::RwLock;
use quinn::{Endpoint, ServerConfig};
use rand::Rng;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use shared::{ConnectionType, MetricsCollector};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use uuid::Uuid;

pub struct QuicServer {
    listen_addr: SocketAddr,
    metrics: Arc<MetricsCollector>,
    active_sessions: Arc<RwLock<HashMap<SocketAddr, QuicSession>>>,
}

#[derive(Debug)]
struct QuicSession {
    id: Uuid,
    start_time: Instant,
    last_activity: Instant,
    requests_handled: u64,
    bytes_sent: u64,
    bytes_received: u64,
}

impl QuicServer {
    pub fn new(listen_addr: SocketAddr, metrics: Arc<MetricsCollector>) -> Self {
        Self {
            listen_addr,
            metrics,
            active_sessions: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub async fn start(&self) -> anyhow::Result<()> {
        let (endpoint, _cert) = make_server_endpoint(self.listen_addr)?;
        tracing::info!("QUIC server listening on {}", self.listen_addr);

        // Spawn cleanup task for inactive sessions
        let cleanup_sessions = self.active_sessions.clone();
        let cleanup_metrics = self.metrics.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(30));
            loop {
                interval.tick().await;
                Self::cleanup_inactive_sessions(&cleanup_sessions, &cleanup_metrics).await;
            }
        });

        // Handle incoming connections
        loop {
            match endpoint.accept().await {
                Some(incoming_conn) => {
                    let sessions = self.active_sessions.clone();
                    let metrics = self.metrics.clone();

                    tokio::spawn(async move {
                        if let Err(e) =
                            Self::handle_connection(incoming_conn, sessions, metrics).await
                        {
                            tracing::warn!("Error handling QUIC connection: {}", e);
                        }
                    });
                }
                None => {
                    tracing::info!("QUIC server shutting down");
                    break;
                }
            }
        }

        Ok(())
    }

    async fn handle_connection(
        incoming_conn: quinn::Incoming,
        sessions: Arc<RwLock<HashMap<SocketAddr, QuicSession>>>,
        metrics: Arc<MetricsCollector>,
    ) -> anyhow::Result<()> {
        let connection = incoming_conn.await?;
        let remote_addr = connection.remote_address();

        tracing::info!("New QUIC connection from: {}", remote_addr);

        // Create session
        let connection_id = Uuid::new_v4();
        let session = QuicSession {
            id: connection_id,
            start_time: Instant::now(),
            last_activity: Instant::now(),
            requests_handled: 0,
            bytes_sent: 0,
            bytes_received: 0,
        };

        sessions.write().insert(remote_addr, session);
        metrics.start_connection(connection_id, ConnectionType::Quic);

        // Handle multiple streams from this connection
        loop {
            match connection.accept_bi().await {
                Ok((tx, rx)) => {
                    let sessions_clone = sessions.clone();
                    let metrics_clone = metrics.clone();

                    tokio::spawn(async move {
                        if let Err(e) =
                            Self::handle_stream(tx, rx, remote_addr, sessions_clone, metrics_clone)
                                .await
                        {
                            tracing::debug!("Stream error: {}", e);
                        }
                    });
                }
                Err(quinn::ConnectionError::ApplicationClosed { .. }) => {
                    tracing::debug!("Connection closed by client: {}", remote_addr);
                    break;
                }
                Err(e) => {
                    tracing::warn!("Error accepting stream from {}: {}", remote_addr, e);
                    break;
                }
            }
        }

        // Clean up session
        if let Some(session) = sessions.write().remove(&remote_addr) {
            metrics.end_connection(&session.id);
            let duration = session.last_activity.duration_since(session.start_time);
            tracing::info!(
                "QUIC session ended: {} ({:?}, {} requests, {} bytes sent, {} bytes received)",
                remote_addr,
                duration,
                session.requests_handled,
                session.bytes_sent,
                session.bytes_received
            );
        }

        Ok(())
    }

    async fn handle_stream(
        mut tx: quinn::SendStream,
        mut rx: quinn::RecvStream,
        remote_addr: SocketAddr,
        sessions: Arc<RwLock<HashMap<SocketAddr, QuicSession>>>,
        metrics: Arc<MetricsCollector>,
    ) -> anyhow::Result<()> {
        // Read the request
        let mut request_data = Vec::new();
        let mut buffer = [0u8; 4096];

        while let Ok(Ok(Some(size))) =
            tokio::time::timeout(Duration::from_secs(10), rx.read(&mut buffer)).await
        {
            if size == 0 {
                break;
            }
            request_data.extend_from_slice(&buffer[..size]);

            // Stop reading once we have a complete HTTP request
            if request_data.ends_with(b"\r\n\r\n") {
                break;
            }
        }

        if request_data.is_empty() {
            return Err(anyhow::anyhow!("Empty request received"));
        }

        // Update session metrics
        {
            let mut sessions_write = sessions.write();
            if let Some(session) = sessions_write.get_mut(&remote_addr) {
                session.last_activity = Instant::now();
                session.requests_handled += 1;
                session.bytes_received += request_data.len() as u64;

                metrics.record_packet_received(&session.id);
                metrics.record_bytes_received(&session.id, request_data.len() as u64);
            }
        }

        // Parse request and generate response
        let request_str = String::from_utf8_lossy(&request_data);
        let response = Self::generate_response(&request_str);

        // Send response
        tx.write_all(&response).await?;
        tx.finish()?;

        // Update session metrics for response
        {
            let mut sessions_write = sessions.write();
            if let Some(session) = sessions_write.get_mut(&remote_addr) {
                session.bytes_sent += response.len() as u64;

                metrics.record_packet_sent(&session.id);
                metrics.record_bytes_sent(&session.id, response.len() as u64);

                // Simulate response latency
                let response_latency = Duration::from_millis(rand::thread_rng().gen_range(1..=50));
                metrics.record_latency(&session.id, response_latency);
            }
        }

        tracing::debug!(
            "Handled request from {}: {} bytes request, {} bytes response",
            remote_addr,
            request_data.len(),
            response.len()
        );

        Ok(())
    }

    fn generate_response(request: &str) -> Vec<u8> {
        // Parse the request to get the path
        let path = if let Some(first_line) = request.lines().next() {
            first_line
        } else {
            "/default"
        };

        // Determine response size based on resource type
        let response_size = if path.contains(".html") {
            1024
        } else if path.contains(".css") {
            512
        } else if path.contains(".js") {
            512
        } else if path.contains(".png") || path.contains(".jpg") {
            2048
        } else if path.contains(".json") {
            256
        } else {
            512
        }; // default

        // Generate response filled with zeros and some random data for validation
        let mut response = vec![0u8; response_size];

        // Add some identifiable header data
        let header = format!("RESP:{}\n", path);
        let header_bytes = header.as_bytes();
        let copy_len = std::cmp::min(header_bytes.len(), response.len());
        response[..copy_len].copy_from_slice(&header_bytes[..copy_len]);

        // Add random bytes at the end for uniqueness
        if response.len() > 16 {
            let end_start = response.len() - 16;
            for i in end_start..response.len() {
                response[i] = rand::thread_rng().gen::<u8>();
            }
        }

        response
    }

    async fn cleanup_inactive_sessions(
        sessions: &Arc<RwLock<HashMap<SocketAddr, QuicSession>>>,
        metrics: &Arc<MetricsCollector>,
    ) {
        let mut to_remove = Vec::new();
        let inactive_threshold = Duration::from_secs(120); // Longer timeout for QUIC
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
                        "Cleaned up inactive QUIC session {} after {:?} ({} requests, {} bytes sent, {} bytes received)",
                        addr,
                        duration,
                        session.requests_handled,
                        session.bytes_sent,
                        session.bytes_received
                    );
                }
            }
        }
    }
}

/// Creates a QUIC server endpoint with a self-signed certificate
fn make_server_endpoint(
    bind_addr: SocketAddr,
) -> anyhow::Result<(Endpoint, CertificateDer<'static>)> {
    let (cert, key) = generate_self_signed_cert()?;

    let mut server_config = ServerConfig::with_single_cert(vec![cert.clone()], key)?;
    let transport_config = Arc::new(quinn::TransportConfig::default());
    server_config.transport = transport_config;

    let endpoint = Endpoint::server(server_config, bind_addr)?;

    Ok((endpoint, cert))
}

/// Generate a self-signed certificate for testing
fn generate_self_signed_cert() -> anyhow::Result<(CertificateDer<'static>, PrivateKeyDer<'static>)>
{
    let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()])?;
    let cert_der = CertificateDer::from(cert.cert);
    let key_der = PrivateKeyDer::try_from(cert.key_pair.serialize_der())
        .map_err(|e| anyhow::anyhow!("Failed to convert key: {}", e))?;
    Ok((cert_der, key_der))
}
