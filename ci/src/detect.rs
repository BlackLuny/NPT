//! Pass/fail evaluation for a port's probe metrics.

use crate::client::PortMetrics;
use crate::config::DetectParams;
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    Pass,
    Fail,
    Skipped,
}

#[derive(Debug, Clone, Serialize)]
pub struct PortVerdict {
    pub port_id: i32,
    pub verdict: Verdict,
    pub reasons: Vec<String>,
}

/// Evaluate detection rules. The core failure classes are stalls (断流) and
/// mid-stream disconnects, which is exactly the forwarding breakage this CI hunts.
pub fn evaluate(port_id: i32, m: &PortMetrics, d: &DetectParams) -> PortVerdict {
    let mut reasons = Vec::new();

    if m.connected == 0 {
        reasons.push(format!(
            "no connection succeeded ({}/{} attempts failed)",
            m.connect_failed, m.connections_attempted
        ));
        return PortVerdict {
            port_id,
            verdict: Verdict::Fail,
            reasons,
        };
    }

    if d.fail_on_stall && !m.stalls.is_empty() {
        let worst = m.stalls.iter().map(|s| s.gap_ms).max().unwrap_or(0);
        reasons.push(format!(
            "{} stall event(s) (断流), worst gap {} ms",
            m.stalls.len(),
            worst
        ));
    }

    if d.fail_on_midstream_disconnect && m.midstream_disconnects > 0 {
        reasons.push(format!(
            "{} mid-stream disconnect(s) before test end",
            m.midstream_disconnects
        ));
    }

    // Distinguish "client could not send at all" (local send error, e.g. UDP
    // datagram exceeding the OS limit / MTU) from a genuine one-way-dead path.
    if m.connected > 0 && m.bytes_sent == 0 {
        let hint = m
            .errors
            .iter()
            .find(|e| e.contains("send"))
            .or_else(|| m.errors.first())
            .cloned()
            .unwrap_or_else(|| "no bytes left the client".to_string());
        reasons.push(format!(
            "client sent 0 bytes — all sends failed ({hint}); check payload_bytes vs MTU / UDP datagram limit"
        ));
    } else if d.fail_on_no_return && m.flows_no_return > 0 {
        reasons.push(format!(
            "{} flow(s) got zero return traffic (one-way dead / 单通)",
            m.flows_no_return
        ));
    }

    let attempted = m.connections_attempted.max(1) as f64;
    let fail_ratio = m.connect_failed as f64 / attempted;
    if fail_ratio > d.max_connect_fail_ratio {
        reasons.push(format!(
            "connect-fail ratio {:.1}% exceeds limit {:.1}%",
            fail_ratio * 100.0,
            d.max_connect_fail_ratio * 100.0
        ));
    }

    if d.min_throughput_kbps > 0.0 && m.avg_down_kbps < d.min_throughput_kbps {
        reasons.push(format!(
            "avg download {:.1} kbit/s below floor {:.1} kbit/s",
            m.avg_down_kbps, d.min_throughput_kbps
        ));
    }

    if d.max_rtt_p95_ms > 0.0 && m.rtt.p95_ms > d.max_rtt_p95_ms {
        reasons.push(format!(
            "p95 RTT {:.1} ms exceeds limit {:.1} ms",
            m.rtt.p95_ms, d.max_rtt_p95_ms
        ));
    }

    let verdict = if reasons.is_empty() {
        Verdict::Pass
    } else {
        Verdict::Fail
    };
    PortVerdict {
        port_id,
        verdict,
        reasons,
    }
}
