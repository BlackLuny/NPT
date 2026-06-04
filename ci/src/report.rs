//! Run summary: JSON artifact + console table + exit-code signal.

use crate::client::PortMetrics;
use crate::detect::{PortVerdict, Verdict};
use serde::Serialize;
use std::path::Path;

#[derive(Debug, Clone, Serialize)]
pub struct PortOutcome {
    pub port_id: i32,
    pub display_name: String,
    pub verdict: PortVerdict,
    /// None when the port was skipped before any test ran.
    pub metrics: Option<PortMetrics>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RunSummary {
    pub started_at: String,
    pub panel_url: String,
    pub total: usize,
    pub passed: usize,
    pub failed: usize,
    pub skipped: usize,
    pub outcomes: Vec<PortOutcome>,
}

impl RunSummary {
    pub fn build(panel_url: &str, started_at: String, outcomes: Vec<PortOutcome>) -> Self {
        let mut passed = 0;
        let mut failed = 0;
        let mut skipped = 0;
        for o in &outcomes {
            match o.verdict.verdict {
                Verdict::Pass => passed += 1,
                Verdict::Fail => failed += 1,
                Verdict::Skipped => skipped += 1,
            }
        }
        RunSummary {
            started_at,
            panel_url: panel_url.to_string(),
            total: outcomes.len(),
            passed,
            failed,
            skipped,
            outcomes,
        }
    }

    pub fn write_json(&self, dir: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(dir)?;
        let json = serde_json::to_string_pretty(self).unwrap_or_default();
        std::fs::write(dir.join("summary.json"), json)
    }

    /// Non-zero exit if any port failed.
    pub fn exit_code(&self) -> i32 {
        if self.failed > 0 {
            1
        } else {
            0
        }
    }

    pub fn print_table(&self) {
        println!("\n========== NPT-CI RESULT ==========");
        println!(
            "{:<8} {:<20} {:<22} {:<7} {:>9} {:>7} {:>7} reasons",
            "PORT", "NAME", "ENDPOINT", "VERDICT", "down kbps", "stalls", "drops"
        );
        for o in &self.outcomes {
            let (endpoint, down, stalls, drops) = match &o.metrics {
                Some(m) => (
                    m.endpoint.clone(),
                    format!("{:.0}", m.avg_down_kbps),
                    m.stalls.len().to_string(),
                    m.midstream_disconnects.to_string(),
                ),
                None => ("-".into(), "-".into(), "-".into(), "-".into()),
            };
            let verdict = match o.verdict.verdict {
                Verdict::Pass => "PASS",
                Verdict::Fail => "FAIL",
                Verdict::Skipped => "SKIP",
            };
            let reasons = o.verdict.reasons.join("; ");
            println!(
                "{:<8} {:<20} {:<22} {:<7} {:>9} {:>7} {:>7} {}",
                o.port_id,
                truncate(&o.display_name, 20),
                truncate(&endpoint, 22),
                verdict,
                down,
                stalls,
                drops,
                reasons
            );
        }
        println!("-----------------------------------");
        println!(
            "total={} passed={} failed={} skipped={}",
            self.total, self.passed, self.failed, self.skipped
        );
        println!("===================================\n");
    }
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(n.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}
