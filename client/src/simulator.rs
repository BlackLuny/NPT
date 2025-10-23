use crate::server_pool::ServerPool;
use rand::Rng;
use shared::{MetricsCollector, TestConfig, UserActivity};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

pub struct UserSimulator {
    config: TestConfig,
    tcp_client: TcpClient,
    quic_client: QuicClient,
    #[allow(dead_code)]
    metrics: Arc<MetricsCollector>,
    connection_semaphore: Arc<Semaphore>,
    server_pool: Arc<ServerPool>,
}

impl UserSimulator {
    pub fn new(config: TestConfig, metrics: Arc<MetricsCollector>) -> Self {
        let server_pool = Arc::new(ServerPool::new(
            config.server_addresses.clone(),
            config.load_balancer.clone(),
        ));

        let tcp_client = TcpClient::new(server_pool.clone(), metrics.clone());
        let quic_client = QuicClient::new(server_pool.clone(), metrics.clone());
        let connection_semaphore = Arc::new(Semaphore::new(config.concurrent_users as usize));

        Self {
            config,
            tcp_client,
            quic_client,
            metrics,
            connection_semaphore,
            server_pool,
        }
    }

    pub async fn run_simulation(&self) -> anyhow::Result<()> {
        let start_time = Instant::now();
        let mut join_set = JoinSet::new();

        tracing::info!(
            "Starting simulation with {} concurrent users for {:?}",
            self.config.concurrent_users,
            self.config.duration
        );

        for user_id in 0..self.config.concurrent_users {
            let tcp_client = self.tcp_client.clone();
            let quic_client = self.quic_client.clone();
            let config = self.config.clone();
            let semaphore = self.connection_semaphore.clone();
            let server_pool = self.server_pool.clone();

            join_set.spawn(async move {
                Self::simulate_user_behavior(
                    user_id,
                    tcp_client,
                    quic_client,
                    config,
                    semaphore,
                    server_pool,
                )
                .await
            });
        }

        let mut completed_users = 0;
        while let Some(result) = join_set.join_next().await {
            match result {
                Ok(Ok(_)) => {
                    completed_users += 1;
                    tracing::debug!("User {} completed simulation", completed_users);
                }
                Ok(Err(e)) => {
                    tracing::warn!("User simulation failed: {}", e);
                }
                Err(e) => {
                    tracing::error!("User simulation task panicked: {}", e);
                }
            }
        }

        let total_duration = start_time.elapsed();
        tracing::info!(
            "Simulation completed in {:?} with {} users",
            total_duration,
            completed_users
        );

        Ok(())
    }

    async fn simulate_user_behavior(
        user_id: u32,
        tcp_client: TcpClient,
        quic_client: QuicClient,
        config: TestConfig,
        semaphore: Arc<Semaphore>,
        _server_pool: Arc<ServerPool>,
    ) -> anyhow::Result<()> {
        let _permit = semaphore.acquire().await?;

        // Increment active users when starting
        tcp_client.inner.get_metrics().increment_active_users();

        let start_time = Instant::now();
        let user_session_duration = Duration::from_secs(
            rand::thread_rng().gen_range(config.duration.as_secs() / 2..=config.duration.as_secs()),
        );

        tracing::debug!(
            "User {} starting session for {:?}",
            user_id,
            user_session_duration
        );
        let stoped = Arc::new(AtomicBool::new(false));
        let stoped_clone = stoped.clone();
        let set_stop = tokio::spawn(async move {
            tokio::time::sleep(user_session_duration).await;
            stoped_clone.store(true, Ordering::Relaxed);
        });

        while start_time.elapsed() < user_session_duration {
            let activity = Self::choose_random_activity(&config);

            let activity_start = Instant::now();
            let result = match &activity {
                UserActivity::QuicWebBrowsing { .. } => {
                    quic_client.simulate_user_activity(activity).await
                }
                _ => {
                    tcp_client
                        .simulate_user_activity(activity, stoped.clone())
                        .await
                }
            };

            match result {
                Ok(_) => {
                    tracing::debug!(
                        "User {} completed activity in {:?}",
                        user_id,
                        activity_start.elapsed()
                    );
                }
                Err(e) => {
                    tracing::warn!("User {} activity failed: {}", user_id, e);
                }
            }

            let think_time = Self::generate_think_time(&config);
            tokio::time::sleep(think_time).await;
            if think_time + start_time.elapsed() > user_session_duration
                || stoped.load(Ordering::Relaxed)
            {
                break;
            }
        }

        tracing::debug!("User {} finished session", user_id);

        set_stop.abort();

        // Decrement active users when finishing
        tcp_client.inner.get_metrics().decrement_active_users();

        Ok(())
    }

    fn choose_random_activity(config: &TestConfig) -> UserActivity {
        let mut rng = rand::thread_rng();
        let random_weight: f32 = rng.gen();

        let weights = &config.user_behavior.activity_weights;
        
        // Calculate cumulative probabilities
        let web_threshold = weights.web_browsing;
        let download_threshold = web_threshold + weights.file_download;
        let upload_threshold = download_threshold + weights.file_upload;
        // quic_browsing covers the remaining weight (1.0 - upload_threshold)

        if random_weight < web_threshold {
            UserActivity::WebBrowsing {
                pages_to_visit: rng.gen_range(1..=5),
                requests_per_page: (rng.gen_range(2..=5), rng.gen_range(5..=10)),
            }
        } else if random_weight < download_threshold {
            UserActivity::FileDownload {
                file_size: rng.gen_range(1024..=1 * 1024 * 1024),
                chunk_size: config.user_behavior.tcp_settings.request_size_range,
            }
        } else if random_weight < upload_threshold {
            UserActivity::FileUpload {
                file_size: rng.gen_range(1024..=1 * 1024 * 1024),
                chunk_size: config.user_behavior.tcp_settings.request_size_range,
            }
        } else {
            UserActivity::QuicWebBrowsing {
                pages_to_visit: rng.gen_range(1..=5),
                resources_per_page: (rng.gen_range(2..=5), rng.gen_range(5..=10)),
                concurrent_requests: rng.gen_range(1..=4),
            }
        }
    }

    fn generate_think_time(config: &TestConfig) -> Duration {
        let range = &config.user_behavior.think_time_range;
        let min_ms = range.0.as_millis() as u64;
        let max_ms = range.1.as_millis() as u64;

        Duration::from_millis(rand::thread_rng().gen_range(min_ms..=max_ms))
    }
}

#[derive(Clone)]
pub struct TcpClient {
    inner: crate::tcp_client::TcpClient,
}

impl TcpClient {
    pub fn new(server_pool: Arc<ServerPool>, metrics: Arc<MetricsCollector>) -> Self {
        Self {
            inner: crate::tcp_client::TcpClient::new(server_pool, metrics),
        }
    }

    pub async fn simulate_user_activity(
        &self,
        activity: UserActivity,
        stoped: Arc<AtomicBool>,
    ) -> anyhow::Result<()> {
        self.inner.simulate_user_activity(activity, stoped).await
    }
}

#[derive(Clone)]
pub struct QuicClient {
    inner: crate::quic_client::QuicClient,
}

impl QuicClient {
    pub fn new(server_pool: Arc<ServerPool>, metrics: Arc<MetricsCollector>) -> Self {
        Self {
            inner: crate::quic_client::QuicClient::new(server_pool, metrics),
        }
    }

    pub async fn simulate_user_activity(&self, activity: UserActivity) -> anyhow::Result<()> {
        self.inner.simulate_user_activity(activity).await
    }
}
