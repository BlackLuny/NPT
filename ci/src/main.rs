//! npt-ci — automated forwarding stress/continuity test for zf-worker ports.
//!
//! Two roles, one binary:
//!   * `npt-ci serve`  — echo endpoint, deployed at the forward target.
//!   * `npt-ci run`    — resolves ports via the panel API, drives traffic through
//!                       each forward port, flags stalls (断流) / disconnects,
//!                       archives logs on failure, prints a pass/fail summary.

mod archive;
mod client;
mod config;
mod detect;
mod panel;
mod protocol;
mod report;
mod server;

use clap::{Parser, Subcommand};
use config::{CiConfig, Proto};
use detect::{PortVerdict, Verdict};
use panel::Panel;
use report::{PortOutcome, RunSummary};
use std::net::SocketAddr;
use std::path::PathBuf;
use tracing::{error, info, warn};

#[derive(Parser)]
#[command(name = "npt-ci", version, about = "zf-worker forwarding stress/continuity CI")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the test suite against panel-resolved forward ports.
    Run {
        /// Path to the TOML config file.
        #[arg(short, long, default_value = "ci.toml")]
        config: String,
        /// Override panel_url.
        #[arg(long)]
        panel_url: Option<String>,
        /// Override api_key.
        #[arg(long)]
        api_key: Option<String>,
        /// Override port_ids (comma-separated).
        #[arg(long)]
        ports: Option<String>,
        /// Override test.duration_secs.
        #[arg(long)]
        duration: Option<u64>,
        /// Override test.connections_per_port.
        #[arg(long)]
        connections: Option<usize>,
    },
    /// Run the echo endpoint (deploy at the forward target).
    Serve {
        /// Listen address, e.g. 0.0.0.0:9000.
        #[arg(short, long, default_value = "0.0.0.0:9000")]
        listen: String,
        /// Disable the UDP echo listener.
        #[arg(long)]
        no_udp: bool,
    },
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();
    let code = match cli.cmd {
        Cmd::Serve { listen, no_udp } => run_serve(&listen, !no_udp).await,
        Cmd::Run {
            config,
            panel_url,
            api_key,
            ports,
            duration,
            connections,
        } => {
            run_suite(RunOverrides {
                config,
                panel_url,
                api_key,
                ports,
                duration,
                connections,
            })
            .await
        }
    };
    std::process::exit(code);
}

async fn run_serve(listen: &str, enable_udp: bool) -> i32 {
    let addr: SocketAddr = match listen.parse() {
        Ok(a) => a,
        Err(e) => {
            error!("invalid --listen {listen}: {e}");
            return 2;
        }
    };
    if let Err(e) = server::run(addr, enable_udp).await {
        error!("serve failed: {e}");
        return 1;
    }
    0
}

struct RunOverrides {
    config: String,
    panel_url: Option<String>,
    api_key: Option<String>,
    ports: Option<String>,
    duration: Option<u64>,
    connections: Option<usize>,
}

async fn run_suite(ov: RunOverrides) -> i32 {
    let mut cfg = match CiConfig::load(&ov.config) {
        Ok(c) => c,
        Err(e) => {
            error!("{e}");
            return 2;
        }
    };
    if let Some(v) = ov.panel_url {
        cfg.panel_url = v;
    }
    if let Some(v) = ov.api_key {
        cfg.api_key = v;
    }
    if let Some(v) = ov.ports {
        cfg.port_ids = v
            .split(',')
            .filter_map(|s| s.trim().parse::<i32>().ok())
            .collect();
    }
    if let Some(v) = ov.duration {
        cfg.test.duration_secs = v;
    }
    if let Some(v) = ov.connections {
        cfg.test.connections_per_port = v;
    }
    if cfg.port_ids.is_empty() {
        error!("no port_ids to test");
        return 2;
    }

    // UDP datagrams have hard size limits the OS enforces with EMSGSIZE
    // (macOS net.inet.udp.maxdgram defaults to 9216), and anything over the path
    // MTU fragments. Warn so a too-large payload isn't misread as a dead path.
    let udp_in_play = matches!(cfg.test.protocol, Proto::Udp | Proto::Both);
    if udp_in_play && cfg.test.payload_bytes > 1400 {
        warn!(
            "test.payload_bytes={} is large for UDP: datagrams >~1472B fragment, and >OS limit \
             (macOS 9216) fail to send (EMSGSIZE). Use <=1400 for UDP flows.",
            cfg.test.payload_bytes
        );
    }

    let panel = match Panel::new(&cfg.panel_url, &cfg.api_key) {
        Ok(p) => p,
        Err(e) => {
            error!("{e}");
            return 2;
        }
    };

    let started = chrono::Local::now();
    let run_dir = PathBuf::from(&cfg.archive.out_dir)
        .join(format!("run-{}", started.format("%Y%m%d-%H%M%S")));
    info!(
        "npt-ci run: panel={} ports={:?} -> {}",
        cfg.panel_url,
        cfg.port_ids,
        run_dir.display()
    );

    let mut outcomes: Vec<PortOutcome> = Vec::new();
    for &port_id in &cfg.port_ids {
        test_one_port(&panel, &cfg, port_id, &run_dir, &mut outcomes).await;
    }

    let summary = RunSummary::build(
        &cfg.panel_url,
        started.to_rfc3339(),
        outcomes,
    );
    if let Err(e) = summary.write_json(&run_dir) {
        warn!("failed to write summary.json: {e}");
    }
    summary.print_table();
    info!("artifacts: {}", run_dir.display());
    summary.exit_code()
}

async fn test_one_port(
    panel: &Panel,
    cfg: &CiConfig,
    port_id: i32,
    run_dir: &std::path::Path,
    outcomes: &mut Vec<PortOutcome>,
) {
    // Resolve connection info.
    let port = match panel.get_port(port_id).await {
        Ok(p) => p,
        Err(e) => {
            outcomes.push(skipped_or_failed(
                port_id,
                String::new(),
                Verdict::Fail,
                format!("panel resolve failed: {e}"),
            ));
            return;
        }
    };
    let name = port.display_name.clone();

    if port.is_suspended {
        outcomes.push(skipped_or_failed(port_id, name, Verdict::Skipped, "port suspended".into()));
        return;
    }
    if port.inbound_proxy_enabled == Some(true) {
        let proto = port.inbound_proxy_protocol.clone().unwrap_or_default();
        outcomes.push(skipped_or_failed(
            port_id,
            name,
            Verdict::Skipped,
            format!("inbound proxy '{proto}' not supported (raw-forward only in this version)"),
        ));
        return;
    }

    // Resolve endpoint (supports hostname or IP).
    let endpoint_str = format!("{}:{}", port.ip_addr, port.port_v4);
    let endpoint: SocketAddr = match tokio::net::lookup_host(&endpoint_str).await.ok().and_then(|mut it| it.next()) {
        Some(a) => a,
        None => {
            outcomes.push(skipped_or_failed(
                port_id,
                name,
                Verdict::Fail,
                format!("cannot resolve endpoint {endpoint_str}"),
            ));
            return;
        }
    };

    // Backing servers (best-effort, for log archival).
    let servers = panel.port_servers(port_id).await.unwrap_or_else(|e| {
        warn!("port {port_id}: port_detail_info failed: {e}");
        Vec::new()
    });

    // Which protocols to drive.
    let mut protos: Vec<Proto> = Vec::new();
    match cfg.test.protocol {
        Proto::Tcp => protos.push(Proto::Tcp),
        Proto::Udp => protos.push(Proto::Udp),
        Proto::Both => {
            protos.push(Proto::Tcp);
            if port.enable_udp == Some(true) {
                protos.push(Proto::Udp);
            } else {
                info!("port {port_id}: UDP not enabled on port, skipping UDP leg");
            }
        }
    }

    for proto in protos {
        let proto_name = match proto {
            Proto::Udp => "udp",
            _ => "tcp",
        };
        info!(
            "port {port_id} ({name}) {proto_name} -> {endpoint}: {} conns x {}s",
            cfg.test.connections_per_port, cfg.test.duration_secs
        );
        let metrics = match proto {
            Proto::Udp => client::run_udp(endpoint, &cfg.test, cfg.detect.stall_secs).await,
            _ => client::run_tcp(endpoint, &cfg.test, cfg.detect.stall_secs).await,
        };
        let mut verdict = detect::evaluate(port_id, &metrics, &cfg.detect);

        let label = if matches!(cfg.test.protocol, Proto::Both) {
            format!("{name} ({proto_name})")
        } else {
            name.clone()
        };

        if verdict.verdict == Verdict::Fail {
            warn!("port {port_id} {proto_name} FAILED: {}", verdict.reasons.join("; "));
            let dir = run_dir.join(format!("port-{port_id}-{proto_name}"));
            archive::archive_failure(
                panel,
                &dir,
                port_id,
                &servers,
                cfg.archive.collect_logs,
                &cfg.archive.debug_collector_ids,
            )
            .await;
            verdict.reasons.push(format!("artifacts: {}", dir.display()));
        } else {
            info!("port {port_id} {proto_name} PASS");
        }

        outcomes.push(PortOutcome {
            port_id,
            display_name: label,
            verdict,
            metrics: Some(metrics),
        });
    }
}

fn skipped_or_failed(port_id: i32, name: String, verdict: Verdict, reason: String) -> PortOutcome {
    PortOutcome {
        port_id,
        display_name: name,
        verdict: PortVerdict {
            port_id,
            verdict,
            reasons: vec![reason],
        },
        metrics: None,
    }
}
