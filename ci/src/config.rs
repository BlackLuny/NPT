//! CI test configuration (TOML file + CLI overrides).

use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Proto {
    Tcp,
    Udp,
    /// Run TCP, plus UDP for ports whose panel config has `enable_udp = true`.
    Both,
}

impl Default for Proto {
    fn default() -> Self {
        Proto::Tcp
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct CiConfig {
    /// Panel base URL, e.g. `https://panel.example.com` (no trailing slash needed).
    pub panel_url: String,
    /// `X-API-Key`. Must be admin-owned if `archive.collect_logs = true`.
    pub api_key: String,
    /// Port IDs to test.
    pub port_ids: Vec<i32>,

    #[serde(default)]
    pub test: TestParams,
    #[serde(default)]
    pub detect: DetectParams,
    #[serde(default)]
    pub archive: ArchiveParams,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct TestParams {
    pub protocol: Proto,
    /// Concurrent connections (TCP) / flows (UDP) opened per port.
    pub connections_per_port: usize,
    /// How long each port is driven, in seconds.
    pub duration_secs: u64,
    /// Per-frame payload size in bytes.
    pub payload_bytes: usize,
    /// TCP connect timeout in seconds.
    pub connect_timeout_secs: u64,
    /// Spread connection starts over this window (ms) to avoid a thundering herd.
    pub ramp_ms: u64,
    /// Per-connection send pacing in bytes/sec. 0 = unbounded (socket backpressure
    /// governs the rate — best for revealing throughput collapse).
    pub send_rate_bps: u64,
    /// UDP send interval in milliseconds (only used for UDP flows).
    pub udp_interval_ms: u64,
}

impl Default for TestParams {
    fn default() -> Self {
        Self {
            protocol: Proto::Tcp,
            connections_per_port: 32,
            duration_secs: 120,
            payload_bytes: 16 * 1024,
            connect_timeout_secs: 10,
            ramp_ms: 2000,
            send_rate_bps: 0,
            udp_interval_ms: 20,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct DetectParams {
    /// No echoed bytes for this many seconds on an active flow ⇒ stall (断流).
    pub stall_secs: u64,
    /// Fail the port if any active flow stalls mid-test.
    pub fail_on_stall: bool,
    /// Fail the port if any flow disconnects (EOF/RST) before the test window ends.
    pub fail_on_midstream_disconnect: bool,
    /// Fail the port if any flow connected but got zero echoed bytes back
    /// (one-way dead / 单通 — e.g. firewall reject, upstream never relays back).
    pub fail_on_no_return: bool,
    /// Fail the port if the connect-failure ratio exceeds this (0.0..=1.0).
    pub max_connect_fail_ratio: f64,
    /// Optional: fail if aggregate download throughput stays below this (kbit/s).
    /// 0 = disabled.
    pub min_throughput_kbps: f64,
    /// Optional: fail if p95 RTT exceeds this (ms). 0 = disabled.
    pub max_rtt_p95_ms: f64,
}

impl Default for DetectParams {
    fn default() -> Self {
        Self {
            stall_secs: 5,
            fail_on_stall: true,
            fail_on_midstream_disconnect: true,
            fail_on_no_return: true,
            max_connect_fail_ratio: 0.10,
            min_throughput_kbps: 0.0,
            max_rtt_p95_ms: 0.0,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ArchiveParams {
    /// Root directory for run artifacts. A `run-<timestamp>` subdir is created per run.
    pub out_dir: String,
    /// On failure, pull worker diagnostic ZIPs via the admin debug export API.
    /// Requires an admin-owned API key.
    pub collect_logs: bool,
    /// Specific debug collector IDs to export. Empty = all available collectors.
    pub debug_collector_ids: Vec<String>,
}

impl Default for ArchiveParams {
    fn default() -> Self {
        Self {
            out_dir: "./runs".to_string(),
            collect_logs: true,
            debug_collector_ids: Vec::new(),
        }
    }
}

impl CiConfig {
    pub fn load(path: &str) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("failed to read config {path}: {e}"))?;
        let cfg: CiConfig =
            toml::from_str(&text).map_err(|e| anyhow::anyhow!("invalid config {path}: {e}"))?;
        if cfg.port_ids.is_empty() {
            anyhow::bail!("config has no port_ids to test");
        }
        Ok(cfg)
    }
}
