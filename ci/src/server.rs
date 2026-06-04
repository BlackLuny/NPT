//! `npt-ci serve` — the echo endpoint deployed at the forward target.
//!
//! Every TCP connection and UDP datagram is echoed back verbatim. This is the
//! controlled "other end" (Mode A): the CI client drives traffic through the
//! zf-worker forward port and verifies it comes back continuously.

use crate::protocol::{self, Frame};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::net::{TcpListener, UdpSocket};
use tracing::{error, info, warn};

#[derive(Default)]
struct ServeStats {
    tcp_conns: AtomicU64,
    tcp_active: AtomicU64,
    udp_datagrams: AtomicU64,
    bytes_echoed: AtomicU64,
}

pub async fn run(listen: SocketAddr, enable_udp: bool) -> anyhow::Result<()> {
    let stats = Arc::new(ServeStats::default());

    let tcp = TcpListener::bind(listen)
        .await
        .map_err(|e| anyhow::anyhow!("bind TCP {listen}: {e}"))?;
    info!("echo server listening on tcp://{listen}");

    if enable_udp {
        let udp = UdpSocket::bind(listen)
            .await
            .map_err(|e| anyhow::anyhow!("bind UDP {listen}: {e}"))?;
        info!("echo server listening on udp://{listen}");
        let s = stats.clone();
        tokio::spawn(async move { udp_echo(udp, s).await });
    }

    // Periodic heartbeat so the operator can see it is alive.
    {
        let s = stats.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(10));
            loop {
                tick.tick().await;
                info!(
                    "tcp_conns={} tcp_active={} udp_datagrams={} bytes_echoed={}",
                    s.tcp_conns.load(Ordering::Relaxed),
                    s.tcp_active.load(Ordering::Relaxed),
                    s.udp_datagrams.load(Ordering::Relaxed),
                    s.bytes_echoed.load(Ordering::Relaxed),
                );
            }
        });
    }

    loop {
        match tcp.accept().await {
            Ok((stream, peer)) => {
                let s = stats.clone();
                s.tcp_conns.fetch_add(1, Ordering::Relaxed);
                s.tcp_active.fetch_add(1, Ordering::Relaxed);
                tokio::spawn(async move {
                    if let Err(e) = tcp_echo(stream, &s).await {
                        warn!("tcp echo {peer} ended: {e}");
                    }
                    s.tcp_active.fetch_sub(1, Ordering::Relaxed);
                });
            }
            Err(e) => {
                error!("accept failed: {e}");
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
    }
}

async fn tcp_echo(stream: tokio::net::TcpStream, stats: &ServeStats) -> std::io::Result<()> {
    let _ = stream.set_nodelay(true);
    let (mut r, mut w) = stream.into_split();
    // Echo at the frame level so a corrupted/foreign stream is rejected fast and
    // we account bytes accurately.
    while let Some(frame) = Frame::read(&mut r).await? {
        let bytes = (protocol::HEADER_LEN + frame.payload.len()) as u64;
        frame.write(&mut w).await?;
        w.flush().await?;
        stats.bytes_echoed.fetch_add(bytes, Ordering::Relaxed);
    }
    Ok(())
}

async fn udp_echo(sock: UdpSocket, stats: Arc<ServeStats>) {
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        match sock.recv_from(&mut buf).await {
            Ok((n, peer)) => {
                // Only echo valid probe datagrams; ignore stray traffic.
                if protocol::decode_udp(&buf[..n]).is_some() {
                    if let Err(e) = sock.send_to(&buf[..n], peer).await {
                        warn!("udp send to {peer} failed: {e}");
                        continue;
                    }
                    stats.udp_datagrams.fetch_add(1, Ordering::Relaxed);
                    stats.bytes_echoed.fetch_add(n as u64, Ordering::Relaxed);
                }
            }
            Err(e) => {
                warn!("udp recv failed: {e}");
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }
    }
}
