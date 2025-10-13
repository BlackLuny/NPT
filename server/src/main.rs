mod tcp_server;
mod udp_server;
mod tui;

use clap::{Arg, Command};
use shared::{LogBuffer, MetricsCollector, TestReport, OutputFormat, TuiLogFormatter};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tcp_server::TcpServer;
use tokio::signal;
use tokio::sync::mpsc;
use tui::ServerTui;
use udp_server::UdpServer;
use tracing::{info, warn};

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
            .with(fmt::layer()
                .event_format(tui_formatter)
                .with_writer(std::io::sink)) // Suppress normal output
            .with(fmt::layer()
                .with_writer(std::io::sink)); // Double layer to catch any leaks
                
        tracing::subscriber::set_global_default(subscriber)?;
    }

    let matches = Command::new("npt-server")
        .version("0.1.0")
        .about("Network Proxy Testing Server")
        .arg(
            Arg::new("address")
                .short('a')
                .long("address")
                .value_name("ADDRESS")
                .help("Server bind address")
                .default_value("0.0.0.0:8080")
        )
        .arg(
            Arg::new("tcp-port")
                .long("tcp-port")
                .value_name("PORT")
                .help("TCP server port (default: same as main address)")
        )
        .arg(
            Arg::new("udp-port")
                .long("udp-port")
                .value_name("PORT")
                .help("UDP server port (default: same as main address)")
        )
        .arg(
            Arg::new("no-tui")
                .long("no-tui")
                .help("Disable the TUI and run in headless mode")
                .action(clap::ArgAction::SetTrue)
        )
        .arg(
            Arg::new("output")
                .short('o')
                .long("output")
                .value_name("PATH")
                .help("Output path for server metrics report")
                .default_value("./server_report")
        )
        .arg(
            Arg::new("format")
                .short('f')
                .long("format")
                .value_name("FORMAT")
                .help("Output format: json, csv, html")
                .default_value("json")
        )
        .get_matches();

    let bind_addr: SocketAddr = matches.get_one::<String>("address")
        .unwrap()
        .parse()
        .map_err(|_| anyhow::anyhow!("Invalid bind address"))?;

    let tcp_port = matches.get_one::<String>("tcp-port")
        .map(|p| p.parse::<u16>())
        .transpose()
        .map_err(|_| anyhow::anyhow!("Invalid TCP port"))?
        .unwrap_or(bind_addr.port());

    let udp_port = matches.get_one::<String>("udp-port")
        .map(|p| p.parse::<u16>())
        .transpose()
        .map_err(|_| anyhow::anyhow!("Invalid UDP port"))?
        .unwrap_or(bind_addr.port());

    let no_tui = matches.get_flag("no-tui");
    let output_path = matches.get_one::<String>("output").unwrap();
    let format_str = matches.get_one::<String>("format").unwrap();

    let output_format = match format_str.as_str() {
        "json" => OutputFormat::Json,
        "csv" => OutputFormat::Csv,
        "html" => OutputFormat::Html,
        _ => return Err(anyhow::anyhow!("Invalid output format. Use: json, csv, or html")),
    };

    let tcp_addr = SocketAddr::new(bind_addr.ip(), tcp_port);
    let udp_addr = SocketAddr::new(bind_addr.ip(), udp_port);

    info!("Starting server - TCP: {}, UDP: {}", tcp_addr, udp_addr);

    let metrics = Arc::new(MetricsCollector::new());
    let tcp_server = TcpServer::new(tcp_addr, metrics.clone());
    let udp_server = UdpServer::new(udp_addr, metrics.clone());

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

    let tcp_task = tokio::spawn(async move {
        if let Err(e) = tcp_server.start().await {
            warn!("TCP server error: {}", e);
        }
    });

    let udp_task = tokio::spawn(async move {
        if let Err(e) = udp_server.start().await {
            warn!("UDP server error: {}", e);
        }
    });

    if no_tui {
        info!("Running in headless mode. Press Ctrl+C to stop.");
        tokio::select! {
            _ = tcp_task => warn!("TCP server task ended"),
            _ = udp_task => warn!("UDP server task ended"),
            _ = shutdown_rx.recv() => {
                info!("Received shutdown signal, stopping server...");
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
        
        let mut tui = ServerTui::new(metrics.clone(), log_buffer.clone());
        
        tokio::select! {
            _ = tcp_task => warn!("TCP server task ended"),
            _ = udp_task => warn!("UDP server task ended"),
            tui_result = tui.run() => {
                match tui_result {
                    Ok(_) => info!("TUI exited normally"),
                    Err(e) => warn!("TUI error: {}", e),
                }
            }
            _ = shutdown_rx.recv() => {
                info!("Received shutdown signal, stopping server...");
            }
        }
    }

    metrics_task.abort();

    info!("Generating server metrics report...");
    generate_server_report(&metrics, output_path, &output_format).await?;

    Ok(())
}

async fn generate_server_report(
    metrics: &Arc<MetricsCollector>,
    output_path: &str,
    output_format: &OutputFormat,
) -> anyhow::Result<()> {
    let connections: Vec<_> = metrics.connections.read().values().cloned().collect();
    let throughput_samples: Vec<_> = metrics.throughput_history.read().clone();
    let latency_samples: Vec<_> = metrics.latency_history.read().clone();
    let error_samples: Vec<_> = metrics.error_history.read().clone();

    let report = TestReport::generate(
        connections,
        throughput_samples,
        latency_samples,
        error_samples,
        true,
    );

    report.export(output_format, output_path)?;

    info!("Server metrics report generated: {}.{}", 
          output_path,
          match output_format {
              OutputFormat::Json => "json",
              OutputFormat::Csv => "csv",
              OutputFormat::Html => "html",
          });

    Ok(())
}