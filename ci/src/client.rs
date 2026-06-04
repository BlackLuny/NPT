//! `npt-ci run` probe client: drives N concurrent flows through a forward port,
//! measures continuity, and flags stalls (断流) / mid-stream disconnects.

use crate::config::TestParams;
use crate::protocol::{self, Frame};
use serde::Serialize;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::AsyncWriteExt;
use tokio::net::{TcpStream, UdpSocket};
use tokio::sync::Mutex;

const RTT_SAMPLE_CAP: usize = 50_000;

#[derive(Debug, Clone, Serialize)]
pub struct StallEvent {
    pub conn_id: usize,
    pub at_secs: f64,
    pub gap_ms: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ThroughputSample {
    pub t_secs: u64,
    pub down_kbps: f64,
    pub up_kbps: f64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct RttSummary {
    pub samples: usize,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    pub max_ms: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct PortMetrics {
    pub protocol: String,
    pub endpoint: String,
    pub duration_secs: u64,
    pub connections_attempted: usize,
    pub connected: usize,
    pub connect_failed: usize,
    pub midstream_disconnects: usize,
    /// Flows that connected but received zero echoed bytes for the whole test
    /// (one-way dead / 单通) — e.g. inbound firewall reject, or upstream that
    /// accepts then never relays back.
    pub flows_no_return: usize,
    pub stalls: Vec<StallEvent>,
    pub bytes_sent: u64,
    pub bytes_recv: u64,
    pub avg_down_kbps: f64,
    pub avg_up_kbps: f64,
    pub udp_sent: u64,
    pub udp_recv: u64,
    pub rtt: RttSummary,
    pub throughput_series: Vec<ThroughputSample>,
    pub errors: Vec<String>,
}

/// Per-flow shared state, inspected by the stall watchdog.
struct ConnState {
    id: usize,
    connected: AtomicBool,
    first_recv: AtomicBool,
    /// ms since the run base instant of the last received frame.
    last_recv_ms: AtomicU64,
    bytes_sent: AtomicU64,
    bytes_recv: AtomicU64,
    udp_sent: AtomicU64,
    udp_recv: AtomicU64,
    disconnected: AtomicBool,
    /// Currently inside a stall episode (dedups repeated watchdog hits).
    in_stall: AtomicBool,
}

impl ConnState {
    fn new(id: usize) -> Self {
        Self {
            id,
            connected: AtomicBool::new(false),
            first_recv: AtomicBool::new(false),
            last_recv_ms: AtomicU64::new(0),
            bytes_sent: AtomicU64::new(0),
            bytes_recv: AtomicU64::new(0),
            udp_sent: AtomicU64::new(0),
            udp_recv: AtomicU64::new(0),
            disconnected: AtomicBool::new(false),
            in_stall: AtomicBool::new(false),
        }
    }
}

struct PortRun {
    base: Instant,
    conns: Vec<Arc<ConnState>>,
    winding_down: AtomicBool,
    connect_failed: AtomicU64,
    midstream_disconnects: AtomicU64,
    rtt_us: Mutex<Vec<f64>>,
    stalls: Mutex<Vec<StallEvent>>,
    errors: Mutex<Vec<String>>,
}

impl PortRun {
    fn now_ms(&self) -> u64 {
        self.base.elapsed().as_millis() as u64
    }
    async fn record_rtt(&self, rtt_us: f64) {
        let mut v = self.rtt_us.lock().await;
        if v.len() < RTT_SAMPLE_CAP {
            v.push(rtt_us);
        }
    }
    async fn note_error(&self, msg: String) {
        let mut v = self.errors.lock().await;
        if v.len() < 200 {
            v.push(msg);
        }
    }
}

pub async fn run_tcp(endpoint: SocketAddr, p: &TestParams, stall_secs: u64) -> PortMetrics {
    let n = p.connections_per_port.max(1);
    let run = Arc::new(PortRun {
        base: Instant::now(),
        conns: (0..n).map(|i| Arc::new(ConnState::new(i))).collect(),
        winding_down: AtomicBool::new(false),
        connect_failed: AtomicU64::new(0),
        midstream_disconnects: AtomicU64::new(0),
        rtt_us: Mutex::new(Vec::new()),
        stalls: Mutex::new(Vec::new()),
        errors: Mutex::new(Vec::new()),
    });
    let deadline = run.base + Duration::from_secs(p.duration_secs);

    let watchdog = spawn_watchdog(run.clone(), stall_secs, deadline);
    let sampler = spawn_throughput_sampler(run.clone(), p.duration_secs);

    let mut handles = Vec::with_capacity(n);
    for conn in run.conns.iter().cloned() {
        let run = run.clone();
        let params = p.clone();
        let ramp = stagger(p.ramp_ms, conn.id, n);
        handles.push(tokio::spawn(async move {
            if ramp > Duration::ZERO {
                tokio::time::sleep(ramp).await;
            }
            tcp_flow(endpoint, conn, run, params, deadline).await;
        }));
    }
    for h in handles {
        let _ = h.await;
    }
    // Stop background tasks; collect.
    run.winding_down.store(true, Ordering::SeqCst);
    let series = sampler.await.unwrap_or_default();
    watchdog.abort();

    finalize(run, "tcp", endpoint, p.duration_secs, series).await
}

async fn tcp_flow(
    endpoint: SocketAddr,
    conn: Arc<ConnState>,
    run: Arc<PortRun>,
    p: TestParams,
    deadline: Instant,
) {
    let connect = tokio::time::timeout(
        Duration::from_secs(p.connect_timeout_secs),
        TcpStream::connect(endpoint),
    )
    .await;
    let stream = match connect {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => {
            run.connect_failed.fetch_add(1, Ordering::Relaxed);
            run.note_error(format!("conn {}: connect failed: {e}", conn.id)).await;
            return;
        }
        Err(_) => {
            run.connect_failed.fetch_add(1, Ordering::Relaxed);
            run.note_error(format!("conn {}: connect timeout", conn.id)).await;
            return;
        }
    };
    let _ = stream.set_nodelay(true);
    conn.connected.store(true, Ordering::Relaxed);
    conn.last_recv_ms.store(run.now_ms(), Ordering::Relaxed);
    let (mut rd, mut wr) = stream.into_split();

    // Writer
    let writer = {
        let conn = conn.clone();
        let run = run.clone();
        let payload = vec![0xA5u8; p.payload_bytes.max(1)];
        let rate = p.send_rate_bps;
        tokio::spawn(async move {
            let mut seq: u64 = 0;
            let frame_len = (protocol::HEADER_LEN + payload.len()) as u64;
            while Instant::now() < deadline {
                let frame = Frame {
                    seq,
                    send_ts_micros: protocol::now_micros(),
                    payload: payload.clone(),
                };
                if let Err(e) = frame.write(&mut wr).await.and(wr.flush().await) {
                    if !run.winding_down.load(Ordering::SeqCst) && Instant::now() < deadline {
                        // Count each broken flow once (reader + writer race to notice).
                        if !conn.disconnected.swap(true, Ordering::Relaxed) {
                            run.midstream_disconnects.fetch_add(1, Ordering::Relaxed);
                        }
                        run.note_error(format!("conn {}: write error: {e}", conn.id)).await;
                    }
                    break;
                }
                conn.bytes_sent.fetch_add(frame_len, Ordering::Relaxed);
                seq = seq.wrapping_add(1);
                if rate > 0 {
                    let secs = frame_len as f64 / rate as f64;
                    tokio::time::sleep(Duration::from_secs_f64(secs)).await;
                }
            }
            let _ = wr.shutdown().await;
        })
    };

    // Reader (runs in this task)
    loop {
        match Frame::read(&mut rd).await {
            Ok(Some(frame)) => {
                let len = (protocol::HEADER_LEN + frame.payload.len()) as u64;
                conn.bytes_recv.fetch_add(len, Ordering::Relaxed);
                conn.last_recv_ms.store(run.now_ms(), Ordering::Relaxed);
                conn.first_recv.store(true, Ordering::Relaxed);
                conn.in_stall.store(false, Ordering::Relaxed);
                let rtt = protocol::now_micros().saturating_sub(frame.send_ts_micros) as f64;
                run.record_rtt(rtt).await;
            }
            Ok(None) => {
                // Clean EOF. A close we did not initiate, before the deadline, is a
                // mid-stream disconnect.
                if !run.winding_down.load(Ordering::SeqCst) && Instant::now() < deadline {
                    if !conn.disconnected.swap(true, Ordering::Relaxed) {
                        run.midstream_disconnects.fetch_add(1, Ordering::Relaxed);
                    }
                    run.note_error(format!("conn {}: peer closed mid-test (EOF)", conn.id)).await;
                }
                break;
            }
            Err(e) => {
                if !run.winding_down.load(Ordering::SeqCst) && Instant::now() < deadline {
                    if !conn.disconnected.swap(true, Ordering::Relaxed) {
                        run.midstream_disconnects.fetch_add(1, Ordering::Relaxed);
                    }
                    run.note_error(format!("conn {}: read error: {e}", conn.id)).await;
                }
                break;
            }
        }
    }
    let _ = writer.await;
}

pub async fn run_udp(endpoint: SocketAddr, p: &TestParams, stall_secs: u64) -> PortMetrics {
    let n = p.connections_per_port.max(1);
    let run = Arc::new(PortRun {
        base: Instant::now(),
        conns: (0..n).map(|i| Arc::new(ConnState::new(i))).collect(),
        winding_down: AtomicBool::new(false),
        connect_failed: AtomicU64::new(0),
        midstream_disconnects: AtomicU64::new(0),
        rtt_us: Mutex::new(Vec::new()),
        stalls: Mutex::new(Vec::new()),
        errors: Mutex::new(Vec::new()),
    });
    let deadline = run.base + Duration::from_secs(p.duration_secs);
    let watchdog = spawn_watchdog(run.clone(), stall_secs, deadline);
    let sampler = spawn_throughput_sampler(run.clone(), p.duration_secs);

    let mut handles = Vec::with_capacity(n);
    for conn in run.conns.iter().cloned() {
        let run = run.clone();
        let params = p.clone();
        let ramp = stagger(p.ramp_ms, conn.id, n);
        handles.push(tokio::spawn(async move {
            if ramp > Duration::ZERO {
                tokio::time::sleep(ramp).await;
            }
            udp_flow(endpoint, conn, run, params, deadline).await;
        }));
    }
    for h in handles {
        let _ = h.await;
    }
    run.winding_down.store(true, Ordering::SeqCst);
    let series = sampler.await.unwrap_or_default();
    watchdog.abort();
    finalize(run, "udp", endpoint, p.duration_secs, series).await
}

async fn udp_flow(
    endpoint: SocketAddr,
    conn: Arc<ConnState>,
    run: Arc<PortRun>,
    p: TestParams,
    deadline: Instant,
) {
    let bind: SocketAddr = if endpoint.is_ipv4() {
        "0.0.0.0:0".parse().unwrap()
    } else {
        "[::]:0".parse().unwrap()
    };
    let sock = match UdpSocket::bind(bind).await {
        Ok(s) => s,
        Err(e) => {
            run.connect_failed.fetch_add(1, Ordering::Relaxed);
            run.note_error(format!("conn {}: udp bind failed: {e}", conn.id)).await;
            return;
        }
    };
    if let Err(e) = sock.connect(endpoint).await {
        run.connect_failed.fetch_add(1, Ordering::Relaxed);
        run.note_error(format!("conn {}: udp connect failed: {e}", conn.id)).await;
        return;
    }
    conn.connected.store(true, Ordering::Relaxed);
    conn.last_recv_ms.store(run.now_ms(), Ordering::Relaxed);
    let sock = Arc::new(sock);

    let payload = vec![0xA5u8; p.payload_bytes.max(1)];
    let interval = Duration::from_millis(p.udp_interval_ms.max(1));

    // Reader
    let reader = {
        let conn = conn.clone();
        let run = run.clone();
        let sock = sock.clone();
        tokio::spawn(async move {
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                match tokio::time::timeout(Duration::from_secs(1), sock.recv(&mut buf)).await {
                    Ok(Ok(n)) => {
                        if let Some((_seq, ts)) = protocol::decode_udp(&buf[..n]) {
                            conn.bytes_recv.fetch_add(n as u64, Ordering::Relaxed);
                            conn.udp_recv.fetch_add(1, Ordering::Relaxed);
                            conn.last_recv_ms.store(run.now_ms(), Ordering::Relaxed);
                            conn.first_recv.store(true, Ordering::Relaxed);
                            conn.in_stall.store(false, Ordering::Relaxed);
                            let rtt = protocol::now_micros().saturating_sub(ts) as f64;
                            run.record_rtt(rtt).await;
                        }
                    }
                    Ok(Err(_)) => break,
                    Err(_) => {
                        if Instant::now() >= deadline {
                            break;
                        }
                    }
                }
            }
        })
    };

    // Writer
    let mut seq: u64 = 0;
    let mut tick = tokio::time::interval(interval);
    while Instant::now() < deadline {
        tick.tick().await;
        let dgram = protocol::encode_udp(seq, protocol::now_micros(), &payload);
        match sock.send(&dgram).await {
            Ok(sent) => {
                conn.bytes_sent.fetch_add(sent as u64, Ordering::Relaxed);
                conn.udp_sent.fetch_add(1, Ordering::Relaxed);
            }
            Err(e) => {
                run.note_error(format!("conn {}: udp send error: {e}", conn.id)).await;
            }
        }
        seq = seq.wrapping_add(1);
    }
    let _ = reader.await;
}

fn stagger(ramp_ms: u64, id: usize, n: usize) -> Duration {
    if ramp_ms == 0 || n <= 1 {
        Duration::ZERO
    } else {
        Duration::from_millis(ramp_ms * id as u64 / n as u64)
    }
}

fn spawn_watchdog(
    run: Arc<PortRun>,
    stall_secs: u64,
    deadline: Instant,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let stall_ms = stall_secs * 1000;
        let mut tick = tokio::time::interval(Duration::from_millis(250));
        loop {
            tick.tick().await;
            if Instant::now() >= deadline || run.winding_down.load(Ordering::SeqCst) {
                break;
            }
            let now = run.now_ms();
            for c in &run.conns {
                if !c.connected.load(Ordering::Relaxed)
                    || c.disconnected.load(Ordering::Relaxed)
                    || !c.first_recv.load(Ordering::Relaxed)
                {
                    continue;
                }
                let gap = now.saturating_sub(c.last_recv_ms.load(Ordering::Relaxed));
                if gap > stall_ms {
                    if !c.in_stall.swap(true, Ordering::Relaxed) {
                        run.stalls.lock().await.push(StallEvent {
                            conn_id: c.id,
                            at_secs: now as f64 / 1000.0,
                            gap_ms: gap,
                        });
                    }
                }
            }
        }
    })
}

fn spawn_throughput_sampler(
    run: Arc<PortRun>,
    duration_secs: u64,
) -> tokio::task::JoinHandle<Vec<ThroughputSample>> {
    tokio::spawn(async move {
        let mut series = Vec::new();
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        tick.tick().await; // consume the immediate first tick
        let mut prev_down: u64 = 0;
        let mut prev_up: u64 = 0;
        for t in 1..=duration_secs {
            tick.tick().await;
            let down: u64 = run.conns.iter().map(|c| c.bytes_recv.load(Ordering::Relaxed)).sum();
            let up: u64 = run.conns.iter().map(|c| c.bytes_sent.load(Ordering::Relaxed)).sum();
            let d_down = down.saturating_sub(prev_down);
            let d_up = up.saturating_sub(prev_up);
            prev_down = down;
            prev_up = up;
            series.push(ThroughputSample {
                t_secs: t,
                down_kbps: d_down as f64 * 8.0 / 1000.0,
                up_kbps: d_up as f64 * 8.0 / 1000.0,
            });
            if run.winding_down.load(Ordering::SeqCst) {
                break;
            }
        }
        series
    })
}

async fn finalize(
    run: Arc<PortRun>,
    proto: &str,
    endpoint: SocketAddr,
    duration_secs: u64,
    series: Vec<ThroughputSample>,
) -> PortMetrics {
    let connected = run.conns.iter().filter(|c| c.connected.load(Ordering::Relaxed)).count();
    let flows_no_return = run
        .conns
        .iter()
        .filter(|c| c.connected.load(Ordering::Relaxed) && c.bytes_recv.load(Ordering::Relaxed) == 0)
        .count();
    let bytes_sent: u64 = run.conns.iter().map(|c| c.bytes_sent.load(Ordering::Relaxed)).sum();
    let bytes_recv: u64 = run.conns.iter().map(|c| c.bytes_recv.load(Ordering::Relaxed)).sum();
    let udp_sent: u64 = run.conns.iter().map(|c| c.udp_sent.load(Ordering::Relaxed)).sum();
    let udp_recv: u64 = run.conns.iter().map(|c| c.udp_recv.load(Ordering::Relaxed)).sum();

    let rtt = {
        let mut v = run.rtt_us.lock().await.clone();
        rtt_summary(&mut v)
    };
    let stalls = run.stalls.lock().await.clone();
    let errors = run.errors.lock().await.clone();

    let dur = duration_secs.max(1) as f64;
    PortMetrics {
        protocol: proto.to_string(),
        endpoint: endpoint.to_string(),
        duration_secs,
        connections_attempted: run.conns.len(),
        connected,
        connect_failed: run.connect_failed.load(Ordering::Relaxed) as usize,
        midstream_disconnects: run.midstream_disconnects.load(Ordering::Relaxed) as usize,
        flows_no_return,
        stalls,
        bytes_sent,
        bytes_recv,
        avg_down_kbps: bytes_recv as f64 * 8.0 / 1000.0 / dur,
        avg_up_kbps: bytes_sent as f64 * 8.0 / 1000.0 / dur,
        udp_sent,
        udp_recv,
        rtt,
        throughput_series: series,
        errors,
    }
}

fn rtt_summary(samples_us: &mut [f64]) -> RttSummary {
    if samples_us.is_empty() {
        return RttSummary::default();
    }
    samples_us.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let pick = |q: f64| -> f64 {
        let idx = ((samples_us.len() as f64 - 1.0) * q).round() as usize;
        samples_us[idx] / 1000.0
    };
    RttSummary {
        samples: samples_us.len(),
        p50_ms: pick(0.50),
        p95_ms: pick(0.95),
        p99_ms: pick(0.99),
        max_ms: samples_us[samples_us.len() - 1] / 1000.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::TestParams;

    /// End-to-end data-plane smoke: echo server <-> probe client over loopback.
    /// A healthy direct path must show all connections up, bytes flowing both
    /// ways, and zero stalls / disconnects.
    #[tokio::test]
    async fn echo_roundtrip_is_clean() {
        // Grab a free port, then hand it to the echo server.
        let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = probe.local_addr().unwrap();
        drop(probe);

        tokio::spawn(async move {
            let _ = crate::server::run(addr, false).await;
        });
        // Give the listener a moment to bind.
        tokio::time::sleep(Duration::from_millis(200)).await;

        let params = TestParams {
            connections_per_port: 4,
            duration_secs: 2,
            payload_bytes: 1024,
            ramp_ms: 0,
            ..Default::default()
        };
        let m = run_tcp(addr, &params, 1).await;

        assert_eq!(m.connected, 4, "all connections should connect");
        assert_eq!(m.connect_failed, 0);
        assert!(m.bytes_sent > 0 && m.bytes_recv > 0, "bytes should flow both ways");
        assert!(m.stalls.is_empty(), "no stalls on a clean loopback: {:?}", m.stalls);
        assert_eq!(m.midstream_disconnects, 0, "no mid-stream disconnects");
        assert!(m.rtt.samples > 0, "RTT samples collected");
    }

    /// Same as above but UDP — proves the UDP probe sends, the echo server
    /// echoes datagrams, and the client counts return traffic (no false 单通).
    #[tokio::test]
    async fn udp_echo_roundtrip_is_clean() {
        let probe = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let addr = probe.local_addr().unwrap();
        drop(probe);

        tokio::spawn(async move {
            let _ = crate::server::run(addr, true).await;
        });
        tokio::time::sleep(Duration::from_millis(200)).await;

        let params = TestParams {
            protocol: crate::config::Proto::Udp,
            connections_per_port: 4,
            duration_secs: 2,
            payload_bytes: 512,
            ramp_ms: 0,
            udp_interval_ms: 10,
            ..Default::default()
        };
        let m = run_udp(addr, &params, 1).await;

        assert_eq!(m.connected, 4);
        assert!(m.udp_sent > 0, "datagrams should be sent");
        assert!(m.udp_recv > 0, "echoed datagrams should return: {:?}", m.errors);
        assert_eq!(m.flows_no_return, 0, "no flow should be one-way dead");
        assert!(m.bytes_recv > 0);
    }

    /// Reproduces the both-mode bug: run TCP then UDP against the same endpoint
    /// in one process. The UDP leg must still actually send and receive.
    #[tokio::test]
    async fn both_mode_udp_still_sends_after_tcp() {
        let probe = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let addr = probe.local_addr().unwrap();
        drop(probe);
        tokio::spawn(async move {
            let _ = crate::server::run(addr, true).await;
        });
        tokio::time::sleep(Duration::from_millis(200)).await;

        let tcp_params = TestParams {
            protocol: crate::config::Proto::Tcp,
            connections_per_port: 4,
            duration_secs: 2,
            payload_bytes: 512,
            ramp_ms: 0,
            ..Default::default()
        };
        let _ = run_tcp(addr, &tcp_params, 1).await;

        let udp_params = TestParams {
            protocol: crate::config::Proto::Udp,
            connections_per_port: 4,
            duration_secs: 2,
            payload_bytes: 512,
            ramp_ms: 0,
            udp_interval_ms: 10,
            ..Default::default()
        };
        let m = run_udp(addr, &udp_params, 1).await;
        assert!(m.udp_sent > 0, "UDP must send even after a TCP run: udp_sent={}", m.udp_sent);
        assert!(m.udp_recv > 0, "UDP must receive after a TCP run: {:?}", m.errors);
    }
}
