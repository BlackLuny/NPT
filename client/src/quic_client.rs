use crate::server_pool::ServerPool;
use futures::StreamExt;
use quinn::{crypto::rustls::QuicClientConfig, ClientConfig, Endpoint, VarInt};
use rand::Rng;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use shared::{ConnectionType, ErrorType, MetricsCollector, UserActivity};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Semaphore;
use uuid::Uuid;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone)]
pub struct QuicClient {
    server_pool: Arc<ServerPool>,
    metrics: Arc<MetricsCollector>,
}

/// Dummy certificate verifier that treats any certificate as valid.
/// NOTE, such verification is vulnerable to MITM attacks, but convenient for testing.
#[derive(Debug)]
struct SkipServerVerification(Arc<rustls::crypto::CryptoProvider>);

impl SkipServerVerification {
    fn new() -> Arc<Self> {
        Arc::new(Self(Arc::new(rustls::crypto::ring::default_provider())))
    }
}

impl rustls::client::danger::ServerCertVerifier for SkipServerVerification {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

impl QuicClient {
    pub fn new(server_pool: Arc<ServerPool>, metrics: Arc<MetricsCollector>) -> Self {
        Self {
            server_pool,
            metrics,
        }
    }

    pub async fn simulate_user_activity(&self, activity: UserActivity) -> anyhow::Result<()> {
        match activity {
            UserActivity::QuicWebBrowsing {
                pages_to_visit,
                resources_per_page,
                concurrent_requests,
            } => {
                self.simulate_web_browsing_session(
                    pages_to_visit,
                    resources_per_page,
                    concurrent_requests,
                )
                .await
            }
            _ => Err(anyhow::anyhow!(
                "QUIC client can only handle QuicWebBrowsing activities"
            )),
        }
    }

    async fn simulate_web_browsing_session(
        &self,
        pages_to_visit: u32,
        resources_per_page: (u32, u32),
        concurrent_requests: u32,
    ) -> anyhow::Result<()> {
        let connection_id = Uuid::new_v4();
        let session_id = Uuid::new_v4();

        self.metrics.start_connection_with_activity(
            connection_id,
            ConnectionType::Quic,
            UserActivity::QuicWebBrowsing {
                pages_to_visit,
                resources_per_page,
                concurrent_requests,
            },
        );

        let server_addr =
            self.server_pool.get_server().await.ok_or_else(|| {
                anyhow::anyhow!("No healthy servers available for QUIC connection")
            })?;

        self.server_pool.mark_connection_start(server_addr).await;

        // Create QUIC endpoint with insecure certificate verification
        let mut endpoint = Endpoint::client("0.0.0.0:0".parse()?)?;
        endpoint.set_default_client_config(ClientConfig::new(Arc::new(
            QuicClientConfig::try_from(
                rustls::ClientConfig::builder()
                    .dangerous()
                    .with_custom_certificate_verifier(SkipServerVerification::new())
                    .with_no_client_auth(),
            )?,
        )));

        tracing::info!("QUIC Connecting to server: {}", server_addr);

        // Connect to server
        let connection = match tokio::time::timeout(
            REQUEST_TIMEOUT,
            endpoint.connect(server_addr, "localhost")?,
        )
        .await
        {
            Ok(Ok(connection)) => connection,
            Ok(Err(e)) => {
                tracing::warn!("Failed to connect to quic server: {}", e);
                self.metrics.record_error_with_detail(
                    &connection_id,
                    ErrorType::IoError,
                    e.to_string(),
                    Some(format!(
                        "QuicWebBrowsing - QUIC connect to server: {}",
                        server_addr
                    )),
                );
                return Err(anyhow::anyhow!("Failed to connect to quic server: {}", e));
            }
            Err(_) => {
                tracing::warn!("Connection timed out");
                self.metrics.record_error_with_detail(
                    &connection_id,
                    ErrorType::IoError,
                    "QUIC Connection timed out".to_string(),
                    Some(format!(
                        "QuicWebBrowsing - connect to server: {} timeout",
                        server_addr
                    )),
                );
                return Err(anyhow::anyhow!("Connection timed out"));
            }
        };

        tracing::info!("QUIC connected to: {}", connection.remote_address());

        let session_start = Instant::now();
        let mut total_requests = 0;
        let mut successful_responses = 0;

        // Browse multiple pages
        for page_num in 0..pages_to_visit {
            let num_resources =
                rand::thread_rng().gen_range(resources_per_page.0..=resources_per_page.1);

            match self
                .browse_page(
                    &connection,
                    &connection_id,
                    session_id,
                    page_num,
                    num_resources,
                    concurrent_requests,
                )
                .await
            {
                Ok(requests_made) => {
                    total_requests += requests_made;
                    successful_responses += requests_made;
                }
                Err(e) => {
                    tracing::warn!("Failed to browse page {}: {}", page_num, e);
                    self.metrics.record_error_with_detail(
                        &connection_id,
                        ErrorType::IoError,
                        e.to_string(),
                        Some(format!("QuicWebBrowsing - page: {}", page_num)),
                    );
                }
            }

            // Random think time between pages
            let think_time = Duration::from_millis(rand::thread_rng().gen_range(50..=500));
            tokio::time::sleep(think_time).await;
        }

        // Close the connection
        connection.close(VarInt::from_u32(0), b"session_complete");

        let session_duration = session_start.elapsed();
        tracing::info!(
            "QUIC session completed: {} pages, {} requests, {} successful, duration: {:?}",
            pages_to_visit,
            total_requests,
            successful_responses,
            session_duration
        );

        self.server_pool.mark_connection_end(server_addr).await;
        self.metrics.end_connection(&connection_id);
        Ok(())
    }

    async fn browse_page(
        &self,
        connection: &quinn::Connection,
        connection_id: &Uuid,
        session_id: Uuid,
        page_num: u32,
        num_resources: u32,
        concurrent_requests: u32,
    ) -> anyhow::Result<u32> {
        let mut successful_requests = 0;

        // Request main HTML page first
        match tokio::time::timeout(
            REQUEST_TIMEOUT,
            self.request_resource(
                connection,
                connection_id,
                session_id,
                &format!("/page_{}.html", page_num),
            ),
        )
        .await
        {
            Ok(Ok(_)) => successful_requests += 1,
            Ok(Err(e)) => {
                tracing::warn!("Failed to request main page {}: {}", page_num, e);
                return Err(e);
            }
            Err(_) => {
                tracing::warn!("Request timed out for page {}", page_num);
                return Err(anyhow::anyhow!("Request timed out"));
            }
        }

        let resource_futures = futures::stream::iter(0..num_resources)
            .map(|_resource_num| async {
                let resource_num = rand::thread_rng().gen_range(0..4);
                let resource_path = match resource_num % 4 {
                    0 => format!("/css/style_{}.css", resource_num),
                    1 => format!("/js/script_{}.js", resource_num),
                    2 => format!("/images/image_{}.png", resource_num),
                    _ => format!("/data/data_{}.json", resource_num),
                };

                match tokio::time::timeout(
                    REQUEST_TIMEOUT,
                    self.request_resource(connection, connection_id, session_id, &resource_path),
                )
                .await
                {
                    Ok(Ok(_)) => Ok(()),
                    Ok(Err(e)) => {
                        tracing::warn!("Failed to request resource {}: {}", resource_path, e);
                        return Err(e);
                    }
                    Err(_) => {
                        tracing::warn!("Request timed out for resource {}", resource_path);
                        return Err(anyhow::anyhow!("Request timed out"));
                    }
                }
            })
            .buffer_unordered(concurrent_requests as usize)
            .collect::<Vec<_>>();

        let results = tokio::time::timeout(REQUEST_TIMEOUT, resource_futures).await?;
        let all_successful = results.iter().all(|result| result.is_ok());
        if all_successful {
            successful_requests += num_resources;
        }
        Ok(successful_requests)
    }

    async fn request_resource(
        &self,
        connection: &quinn::Connection,
        connection_id: &Uuid,
        session_id: Uuid,
        path: &str,
    ) -> anyhow::Result<()> {
        let request_start = Instant::now();

        // Open a new bidirectional stream
        let (mut tx, mut rx) = connection.open_bi().await?;

        // Send simple request with path and session info
        let request_data = format!("{}\n{}", path, session_id);
        tx.write_all(request_data.as_bytes()).await?;
        tx.finish()?;

        self.metrics.record_packet_sent(connection_id);
        self.metrics
            .record_bytes_sent(connection_id, request_data.len() as u64);

        // Read response
        let mut response_data = Vec::new();
        let mut buffer = [0u8; 4096];

        while let Ok(Some(size)) = rx.read(&mut buffer).await {
            if size == 0 {
                break;
            }
            response_data.extend_from_slice(&buffer[..size]);
        }

        // Validate response - just check we got some data
        if response_data.is_empty() {
            return Err(anyhow::anyhow!("Empty response received"));
        }

        // Simple validation - check if response has expected size
        let expected_min_size = if path.contains(".html") {
            1024
        } else if path.contains(".css") {
            512
        } else if path.contains(".js") {
            512
        } else if path.contains(".png") {
            2048
        } else {
            256
        };

        if response_data.len() < expected_min_size {
            return Err(anyhow::anyhow!(
                "Response too small: expected at least {} bytes, got {}",
                expected_min_size,
                response_data.len()
            ));
        }

        let request_latency = request_start.elapsed();

        self.metrics.record_packet_received(connection_id);
        self.metrics
            .record_bytes_received(connection_id, response_data.len() as u64);
        self.metrics.record_latency(connection_id, request_latency);

        tracing::debug!(
            "Successfully requested {}: {} bytes, {:?} latency",
            path,
            response_data.len(),
            request_latency
        );

        Ok(())
    }
}
