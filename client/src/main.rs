mod server_pool;
mod simulator;
mod tcp_client;
mod tui;
mod udp_client;

use clap::{Arg, Command};
use shared::{LogBuffer, MetricsCollector, OutputFormat, TestConfig, TestReport, TuiLogFormatter};
use simulator::UserSimulator;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::signal;
use tokio::sync::mpsc;
use tracing::{info, warn};
use tui::ClientTui;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Create log buffer for TUI
    let log_buffer = LogBuffer::new(1000);

    // Initialize logging
    if std::env::args().any(|arg| arg == "--no-tui") {
        // For headless mode, use standard logging
        tracing_subscriber::fmt::init();
    } else {
        // For TUI mode, use custom formatter
        use tracing_subscriber::prelude::*;
        use tracing_subscriber::{fmt, Registry};

        let tui_formatter = TuiLogFormatter::new_with_suppression(log_buffer.clone(), true);

        // Completely suppress ALL output in TUI mode - redirect stderr to sink as well
        let subscriber = Registry::default()
            .with(
                fmt::layer()
                    .event_format(tui_formatter)
                    .with_writer(std::io::sink),
            ) // Suppress normal output
            .with(fmt::layer().with_writer(std::io::sink)); // Double layer to catch any leaks

        tracing::subscriber::set_global_default(subscriber)?;
    }

    let matches = Command::new("npt-client")
        .version("0.1.0")
        .about("Network Proxy Testing Client")
        .arg(
            Arg::new("servers")
                .short('s')
                .long("servers")
                .value_name("ADDRESSES")
                .help("Server addresses to connect to (comma-separated)")
                .default_value("127.0.0.1:8080"),
        )
        .arg(
            Arg::new("users")
                .short('u')
                .long("users")
                .value_name("COUNT")
                .help("Number of concurrent users to simulate")
                .default_value("100"),
        )
        .arg(
            Arg::new("duration")
                .short('d')
                .long("duration")
                .value_name("SECONDS")
                .help("Test duration in seconds")
                .default_value("300"),
        )
        .arg(
            Arg::new("no-tui")
                .long("no-tui")
                .help("Disable the TUI and run in headless mode")
                .action(clap::ArgAction::SetTrue),
        )
        .arg(
            Arg::new("output")
                .short('o')
                .long("output")
                .value_name("PATH")
                .help("Output path for test report")
                .default_value("./client_report"),
        )
        .arg(
            Arg::new("format")
                .short('f')
                .long("format")
                .value_name("FORMAT")
                .help("Output format: json, csv, html")
                .default_value("json"),
        )
        .get_matches();

    let servers_str = matches.get_one::<String>("servers").unwrap();
    let server_addresses: Result<Vec<SocketAddr>, _> = servers_str
        .split(',')
        .map(|addr| addr.trim().parse())
        .collect();

    let server_addresses =
        server_addresses.map_err(|_| anyhow::anyhow!("Invalid server address format"))?;

    if server_addresses.is_empty() {
        return Err(anyhow::anyhow!("At least one server address is required"));
    }

    let concurrent_users: u32 = matches
        .get_one::<String>("users")
        .unwrap()
        .parse()
        .map_err(|_| anyhow::anyhow!("Invalid user count"))?;

    let duration_secs: u64 = matches
        .get_one::<String>("duration")
        .unwrap()
        .parse()
        .map_err(|_| anyhow::anyhow!("Invalid duration"))?;

    let no_tui = matches.get_flag("no-tui");
    let output_path = matches.get_one::<String>("output").unwrap();
    let format_str = matches.get_one::<String>("format").unwrap();

    let output_format = match format_str.as_str() {
        "json" => OutputFormat::Json,
        "csv" => OutputFormat::Csv,
        "html" => OutputFormat::Html,
        _ => {
            return Err(anyhow::anyhow!(
                "Invalid output format. Use: json, csv, or html"
            ))
        }
    };

    let mut config = TestConfig::default();
    config.server_addresses = server_addresses.clone();
    config.concurrent_users = concurrent_users;
    config.duration = Duration::from_secs(duration_secs);
    config.reporting.output_format = output_format;
    config.reporting.output_path = output_path.clone();

    info!(
        "Starting client with config: servers={:?}, users={}, duration={:?}",
        server_addresses, concurrent_users, config.duration
    );

    let metrics = Arc::new(MetricsCollector::new());
    metrics.set_total_users(concurrent_users);
    let simulator = UserSimulator::new(config.clone(), metrics.clone());

    let (shutdown_tx, mut shutdown_rx) = mpsc::channel(1);

    tokio::spawn(async move {
        signal::ctrl_c().await.expect("Failed to listen for ctrl+c");
        let _ = shutdown_tx.send(()).await;
    });

    let metrics_clone = metrics.clone();
    let metrics_task = tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(1));
        loop {
            interval.tick().await;
            metrics_clone.sample_throughput();
            metrics_clone.sample_latency();
            metrics_clone.sample_errors();
        }
    });

    let simulation_task = tokio::spawn(async move { simulator.run_simulation().await });

    if no_tui {
        info!("Running in headless mode. Press Ctrl+C to stop.");
        tokio::select! {
            result = simulation_task => {
                match result {
                    Ok(Ok(_)) => info!("Simulation completed successfully"),
                    Ok(Err(e)) => warn!("Simulation failed: {}", e),
                    Err(e) => warn!("Simulation task panicked: {}", e),
                }
            }
            _ = shutdown_rx.recv() => {
                info!("Received shutdown signal, stopping simulation...");
            }
        }
    } else {
        // In TUI mode, completely silence stderr to prevent any character leakage
        unsafe {
            let dev_null = std::ffi::CString::new("/dev/null").unwrap();
            let null_fd = libc::open(dev_null.as_ptr(), libc::O_WRONLY);
            if null_fd >= 0 {
                libc::dup2(null_fd, libc::STDERR_FILENO);
                libc::close(null_fd);
            }
        }

        let mut tui = ClientTui::new(metrics.clone(), log_buffer.clone());

        let tui_result = tokio::task::spawn_blocking(move || tui.run());
        tokio::select! {
            result = simulation_task => {
                match result {
                    Ok(Ok(_)) => info!("Simulation completed successfully"),
                    Ok(Err(e)) => warn!("Simulation failed: {}", e),
                    Err(e) => warn!("Simulation task panicked: {}", e),
                }
            }
            tui_result = tui_result => {
                match tui_result {
                    Ok(_) => info!("TUI exited normally"),
                    Err(e) => warn!("TUI error: {}", e),
                }
            }

            _ = shutdown_rx.recv() => {
                info!("Received shutdown signal, stopping...");
            }
        }
    }

    metrics_task.abort();

    info!("Generating test report...");
    generate_test_report(&metrics, &config).await?;

    Ok(())
}

async fn generate_test_report(
    metrics: &Arc<MetricsCollector>,
    config: &TestConfig,
) -> anyhow::Result<()> {
    let connections: Vec<_> = metrics.connections.iter().map(|f| f.clone()).collect();
    let throughput_samples: Vec<_> = metrics.throughput_history.read().clone();
    let latency_samples: Vec<_> = metrics.latency_history.read().clone();
    let error_samples: Vec<_> = metrics.error_history.read().clone();

    let report = TestReport::generate(
        connections,
        throughput_samples,
        latency_samples,
        error_samples,
        config.reporting.include_raw_data,
    );

    report
        .export(
            &config.reporting.output_format,
            &config.reporting.output_path,
        )
        .await?;

    info!(
        "Test report generated: {}.{}",
        config.reporting.output_path,
        match config.reporting.output_format {
            OutputFormat::Json => "json",
            OutputFormat::Csv => "csv",
            OutputFormat::Html => "html",
        }
    );

    Ok(())
}
