use crate::{ConnectionMetrics, ThroughputSample, LatencySample, ErrorSample, OutputFormat};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;

#[derive(Debug, Serialize, Deserialize)]
pub struct TestReport {
    pub summary: TestSummary,
    pub performance: PerformanceMetrics,
    pub connections: ConnectionStats,
    pub throughput: ThroughputStats,
    pub latency: LatencyStats,
    pub errors: ErrorStats,
    pub raw_data: Option<RawData>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TestSummary {
    pub test_duration: Duration,
    pub start_time: chrono::DateTime<chrono::Utc>,
    pub end_time: chrono::DateTime<chrono::Utc>,
    pub total_connections: u64,
    pub successful_connections: u64,
    pub failed_connections: u64,
    pub total_bytes_sent: u64,
    pub total_bytes_received: u64,
    pub total_packets_sent: u64,
    pub total_packets_received: u64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PerformanceMetrics {
    pub avg_throughput_mbps: f64,
    pub peak_throughput_mbps: f64,
    pub avg_latency_ms: f64,
    pub p50_latency_ms: f64,
    pub p95_latency_ms: f64,
    pub p99_latency_ms: f64,
    pub error_rate: f64,
    pub connections_per_second: f64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ConnectionStats {
    pub tcp_connections: u64,
    pub udp_connections: u64,
    pub avg_connection_duration: Duration,
    pub max_connection_duration: Duration,
    pub min_connection_duration: Duration,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ThroughputStats {
    pub tcp_upload: ThroughputBand,
    pub tcp_download: ThroughputBand,
    pub udp_upload: ThroughputBand,
    pub udp_download: ThroughputBand,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ThroughputBand {
    pub avg_bps: u64,
    pub peak_bps: u64,
    pub min_bps: u64,
    pub total_bytes: u64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct LatencyStats {
    pub tcp: LatencyBand,
    pub udp: LatencyBand,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct LatencyBand {
    pub avg_ms: f64,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    pub max_ms: f64,
    pub min_ms: f64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ErrorStats {
    pub total_errors: u64,
    pub tcp_errors: u64,
    pub udp_errors: u64,
    pub error_rate: f64,
    pub error_distribution: HashMap<String, u64>,
    pub error_types: HashMap<String, u64>,
    pub error_details: Vec<crate::ErrorDetail>,
    pub most_recent_errors: Vec<crate::ErrorDetail>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RawData {
    pub connections: Vec<ConnectionMetrics>,
    pub throughput_samples: Vec<ThroughputSample>,
    pub latency_samples: Vec<LatencySample>,
    pub error_samples: Vec<ErrorSample>,
}

impl TestReport {
    pub fn generate(
        connections: Vec<ConnectionMetrics>,
        throughput_samples: Vec<ThroughputSample>,
        latency_samples: Vec<LatencySample>,
        error_samples: Vec<ErrorSample>,
        include_raw_data: bool,
    ) -> Self {
        let summary = Self::calculate_summary(&connections);
        let performance = Self::calculate_performance(&connections, &throughput_samples, &latency_samples);
        let connection_stats = Self::calculate_connection_stats(&connections);
        let throughput_stats = Self::calculate_throughput_stats(&throughput_samples);
        let latency_stats = Self::calculate_latency_stats(&latency_samples);
        let error_stats = Self::calculate_error_stats(&connections, &error_samples);

        let raw_data = if include_raw_data {
            Some(RawData {
                connections,
                throughput_samples,
                latency_samples,
                error_samples,
            })
        } else {
            None
        };

        Self {
            summary,
            performance,
            connections: connection_stats,
            throughput: throughput_stats,
            latency: latency_stats,
            errors: error_stats,
            raw_data,
        }
    }

    fn calculate_summary(connections: &[ConnectionMetrics]) -> TestSummary {
        let total_connections = connections.len() as u64;
        let successful_connections = connections.iter().filter(|c| c.end_time.is_some() && c.errors == 0).count() as u64;
        let failed_connections = total_connections - successful_connections;

        let total_bytes_sent = connections.iter().map(|c| c.bytes_sent).sum();
        let total_bytes_received = connections.iter().map(|c| c.bytes_received).sum();
        let total_packets_sent = connections.iter().map(|c| c.packets_sent).sum();
        let total_packets_received = connections.iter().map(|c| c.packets_received).sum();

        let start_time = connections.iter().map(|c| c.start_time).min().unwrap_or_else(chrono::Utc::now);
        let end_time = connections.iter()
            .filter_map(|c| c.end_time)
            .max()
            .unwrap_or_else(chrono::Utc::now);

        let test_duration = (end_time - start_time).to_std().unwrap_or_default();

        TestSummary {
            test_duration,
            start_time,
            end_time,
            total_connections,
            successful_connections,
            failed_connections,
            total_bytes_sent,
            total_bytes_received,
            total_packets_sent,
            total_packets_received,
        }
    }

    fn calculate_performance(
        connections: &[ConnectionMetrics],
        throughput_samples: &[ThroughputSample],
        _latency_samples: &[LatencySample],
    ) -> PerformanceMetrics {
        let all_latencies: Vec<f64> = connections.iter()
            .flat_map(|c| c.latencies.iter().map(|d| d.as_millis() as f64))
            .collect();

        let avg_latency_ms = if all_latencies.is_empty() {
            0.0
        } else {
            all_latencies.iter().sum::<f64>() / all_latencies.len() as f64
        };

        let mut sorted_latencies = all_latencies.clone();
        sorted_latencies.sort_by(|a, b| a.partial_cmp(b).unwrap());

        let p50_latency_ms = percentile(&sorted_latencies, 50.0);
        let p95_latency_ms = percentile(&sorted_latencies, 95.0);
        let p99_latency_ms = percentile(&sorted_latencies, 99.0);

        let total_bytes = connections.iter().map(|c| c.bytes_sent + c.bytes_received).sum::<u64>();
        let total_duration = connections.iter()
            .filter_map(|c| c.end_time.map(|end| (end - c.start_time).num_seconds()))
            .max()
            .unwrap_or(1) as f64;

        let avg_throughput_mbps = (total_bytes as f64 * 8.0) / (total_duration * 1_000_000.0);

        let peak_throughput_mbps = throughput_samples.iter()
            .map(|s| (s.tcp_upload_bps + s.tcp_download_bps + s.udp_upload_bps + s.udp_download_bps) as f64 * 8.0 / 1_000_000.0)
            .fold(0.0f64, f64::max);

        let total_errors = connections.iter().map(|c| c.errors).sum::<u64>();
        let error_rate = if connections.is_empty() {
            0.0
        } else {
            total_errors as f64 / connections.len() as f64
        };

        let connections_per_second = if total_duration > 0.0 {
            connections.len() as f64 / total_duration
        } else {
            0.0
        };

        PerformanceMetrics {
            avg_throughput_mbps,
            peak_throughput_mbps,
            avg_latency_ms,
            p50_latency_ms,
            p95_latency_ms,
            p99_latency_ms,
            error_rate,
            connections_per_second,
        }
    }

    fn calculate_connection_stats(connections: &[ConnectionMetrics]) -> ConnectionStats {
        let tcp_connections = connections.iter()
            .filter(|c| matches!(c.connection_type, crate::ConnectionType::Tcp))
            .count() as u64;
        
        let udp_connections = connections.iter()
            .filter(|c| matches!(c.connection_type, crate::ConnectionType::Udp))
            .count() as u64;

        let durations: Vec<Duration> = connections.iter()
            .filter_map(|c| {
                c.end_time.map(|end| (end - c.start_time).to_std().unwrap_or_default())
            })
            .collect();

        let avg_connection_duration = if durations.is_empty() {
            Duration::default()
        } else {
            Duration::from_nanos((durations.iter().map(|d| d.as_nanos()).sum::<u128>() / durations.len() as u128) as u64)
        };

        let max_connection_duration = durations.iter().max().cloned().unwrap_or_default();
        let min_connection_duration = durations.iter().min().cloned().unwrap_or_default();

        ConnectionStats {
            tcp_connections,
            udp_connections,
            avg_connection_duration,
            max_connection_duration,
            min_connection_duration,
        }
    }

    fn calculate_throughput_stats(samples: &[ThroughputSample]) -> ThroughputStats {
        let tcp_upload_values: Vec<u64> = samples.iter().map(|s| s.tcp_upload_bps).collect();
        let tcp_download_values: Vec<u64> = samples.iter().map(|s| s.tcp_download_bps).collect();
        let udp_upload_values: Vec<u64> = samples.iter().map(|s| s.udp_upload_bps).collect();
        let udp_download_values: Vec<u64> = samples.iter().map(|s| s.udp_download_bps).collect();

        ThroughputStats {
            tcp_upload: Self::throughput_band_stats(&tcp_upload_values),
            tcp_download: Self::throughput_band_stats(&tcp_download_values),
            udp_upload: Self::throughput_band_stats(&udp_upload_values),
            udp_download: Self::throughput_band_stats(&udp_download_values),
        }
    }

    fn throughput_band_stats(values: &[u64]) -> ThroughputBand {
        if values.is_empty() {
            return ThroughputBand {
                avg_bps: 0,
                peak_bps: 0,
                min_bps: 0,
                total_bytes: 0,
            };
        }

        let avg_bps = values.iter().sum::<u64>() / values.len() as u64;
        let peak_bps = *values.iter().max().unwrap();
        let min_bps = *values.iter().min().unwrap();
        let total_bytes = values.iter().sum();

        ThroughputBand {
            avg_bps,
            peak_bps,
            min_bps,
            total_bytes,
        }
    }

    fn calculate_latency_stats(samples: &[LatencySample]) -> LatencyStats {
        let tcp_values: Vec<f64> = samples.iter().map(|s| s.tcp_latency_ms).collect();
        let udp_values: Vec<f64> = samples.iter().map(|s| s.udp_latency_ms).collect();

        LatencyStats {
            tcp: Self::latency_band_stats(&tcp_values),
            udp: Self::latency_band_stats(&udp_values),
        }
    }

    fn latency_band_stats(values: &[f64]) -> LatencyBand {
        if values.is_empty() {
            return LatencyBand {
                avg_ms: 0.0,
                p50_ms: 0.0,
                p95_ms: 0.0,
                p99_ms: 0.0,
                max_ms: 0.0,
                min_ms: 0.0,
            };
        }

        let mut sorted_values = values.to_vec();
        sorted_values.sort_by(|a, b| a.partial_cmp(b).unwrap());

        let avg_ms = values.iter().sum::<f64>() / values.len() as f64;
        let p50_ms = percentile(&sorted_values, 50.0);
        let p95_ms = percentile(&sorted_values, 95.0);
        let p99_ms = percentile(&sorted_values, 99.0);
        let max_ms = *sorted_values.last().unwrap();
        let min_ms = *sorted_values.first().unwrap();

        LatencyBand {
            avg_ms,
            p50_ms,
            p95_ms,
            p99_ms,
            max_ms,
            min_ms,
        }
    }

    fn calculate_error_stats(
        connections: &[ConnectionMetrics],
        _error_samples: &[ErrorSample],
    ) -> ErrorStats {
        let total_errors = connections.iter().map(|c| c.errors).sum::<u64>();
        let tcp_errors = connections.iter()
            .filter(|c| matches!(c.connection_type, crate::ConnectionType::Tcp))
            .map(|c| c.errors)
            .sum::<u64>();
        let udp_errors = connections.iter()
            .filter(|c| matches!(c.connection_type, crate::ConnectionType::Udp))
            .map(|c| c.errors)
            .sum::<u64>();

        let error_rate = if connections.is_empty() {
            0.0
        } else {
            total_errors as f64 / connections.len() as f64
        };

        let mut error_distribution = HashMap::new();
        error_distribution.insert("tcp_errors".to_string(), tcp_errors);
        error_distribution.insert("udp_errors".to_string(), udp_errors);

        // Collect all error details from connections
        let mut all_error_details = Vec::new();
        let mut error_types = HashMap::new();
        
        for connection in connections {
            for error_detail in &connection.error_details {
                all_error_details.push(error_detail.clone());
                
                let error_type_name = format!("{:?}", error_detail.error_type);
                *error_types.entry(error_type_name).or_insert(0) += 1;
            }
        }

        // Get most recent errors (last 10)
        let mut recent_errors = all_error_details.clone();
        recent_errors.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
        recent_errors.truncate(10);

        ErrorStats {
            total_errors,
            tcp_errors,
            udp_errors,
            error_rate,
            error_distribution,
            error_types,
            error_details: all_error_details,
            most_recent_errors: recent_errors,
        }
    }

    pub fn export(&self, format: &OutputFormat, path: &str) -> anyhow::Result<()> {
        match format {
            OutputFormat::Json => {
                let json = serde_json::to_string_pretty(self)?;
                std::fs::write(format!("{}.json", path), json)?;
            }
            OutputFormat::Csv => {
                self.export_csv(path)?;
            }
            OutputFormat::Html => {
                self.export_html(path)?;
            }
        }
        Ok(())
    }

    fn export_csv(&self, path: &str) -> anyhow::Result<()> {
        let mut csv_content = String::new();
        csv_content.push_str("metric,value\n");
        csv_content.push_str(&format!("total_connections,{}\n", self.summary.total_connections));
        csv_content.push_str(&format!("successful_connections,{}\n", self.summary.successful_connections));
        csv_content.push_str(&format!("failed_connections,{}\n", self.summary.failed_connections));
        csv_content.push_str(&format!("avg_throughput_mbps,{}\n", self.performance.avg_throughput_mbps));
        csv_content.push_str(&format!("peak_throughput_mbps,{}\n", self.performance.peak_throughput_mbps));
        csv_content.push_str(&format!("avg_latency_ms,{}\n", self.performance.avg_latency_ms));
        csv_content.push_str(&format!("p95_latency_ms,{}\n", self.performance.p95_latency_ms));
        csv_content.push_str(&format!("error_rate,{}\n", self.errors.error_rate));

        // Add error type breakdown
        for (error_type, count) in &self.errors.error_types {
            csv_content.push_str(&format!("error_type_{},{}\n", error_type, count));
        }

        std::fs::write(format!("{}.csv", path), csv_content)?;
        
        // Export detailed errors to separate CSV
        let mut errors_csv = String::new();
        errors_csv.push_str("timestamp,error_type,connection_id,message,context\n");
        for error in &self.errors.most_recent_errors {
            errors_csv.push_str(&format!(
                "{},{:?},{},\"{}\",\"{}\"\n",
                error.timestamp.format("%Y-%m-%d %H:%M:%S"),
                error.error_type,
                error.connection_id,
                error.message.replace('"', "\"\""),
                error.context.as_deref().unwrap_or("").replace('"', "\"\"")
            ));
        }
        std::fs::write(format!("{}_errors.csv", path), errors_csv)?;
        
        Ok(())
    }

    fn export_html(&self, path: &str) -> anyhow::Result<()> {
        // Generate error type breakdown HTML
        let mut error_types_html = String::new();
        for (error_type, count) in &self.errors.error_types {
            error_types_html.push_str(&format!(
                "<tr><td>{}</td><td>{}</td></tr>\n", 
                error_type, count
            ));
        }

        // Generate recent errors HTML
        let mut recent_errors_html = String::new();
        for error in &self.errors.most_recent_errors {
            recent_errors_html.push_str(&format!(
                "<tr><td>{}</td><td>{:?}</td><td>{}</td><td>{}</td></tr>\n",
                error.timestamp.format("%Y-%m-%d %H:%M:%S"),
                error.error_type,
                error.message,
                error.context.as_deref().unwrap_or("N/A")
            ));
        }

        let html_content = format!(
            r#"<!DOCTYPE html>
<html>
<head>
    <title>Network Proxy Test Report</title>
    <style>
        body {{ font-family: Arial, sans-serif; margin: 40px; }}
        .summary {{ background-color: #f0f0f0; padding: 20px; border-radius: 5px; }}
        .metric {{ margin: 10px 0; }}
        .section {{ margin: 30px 0; }}
        table {{ border-collapse: collapse; width: 100%; margin-top: 15px; }}
        th, td {{ border: 1px solid #ddd; padding: 8px; text-align: left; }}
        th {{ background-color: #f2f2f2; }}
        .error-message {{ max-width: 300px; word-wrap: break-word; }}
    </style>
</head>
<body>
    <h1>Network Proxy Test Report</h1>
    
    <div class="section">
        <h2>Test Summary</h2>
        <div class="summary">
            <div class="metric">Test Duration: {:?}</div>
            <div class="metric">Total Connections: {}</div>
            <div class="metric">Successful Connections: {}</div>
            <div class="metric">Failed Connections: {}</div>
            <div class="metric">Total Bytes Sent: {}</div>
            <div class="metric">Total Bytes Received: {}</div>
        </div>
    </div>

    <div class="section">
        <h2>Performance Metrics</h2>
        <table>
            <tr><th>Metric</th><th>Value</th></tr>
            <tr><td>Average Throughput (Mbps)</td><td>{:.2}</td></tr>
            <tr><td>Peak Throughput (Mbps)</td><td>{:.2}</td></tr>
            <tr><td>Average Latency (ms)</td><td>{:.2}</td></tr>
            <tr><td>P95 Latency (ms)</td><td>{:.2}</td></tr>
            <tr><td>P99 Latency (ms)</td><td>{:.2}</td></tr>
            <tr><td>Error Rate</td><td>{:.4}</td></tr>
        </table>
    </div>

    <div class="section">
        <h2>Error Analysis</h2>
        <h3>Error Type Distribution</h3>
        <table>
            <tr><th>Error Type</th><th>Count</th></tr>
            {}
        </table>
        
        <h3>Most Recent Errors</h3>
        <table>
            <tr><th>Timestamp</th><th>Type</th><th class="error-message">Message</th><th>Context</th></tr>
            {}
        </table>
    </div>
</body>
</html>"#,
            self.summary.test_duration,
            self.summary.total_connections,
            self.summary.successful_connections,
            self.summary.failed_connections,
            self.summary.total_bytes_sent,
            self.summary.total_bytes_received,
            self.performance.avg_throughput_mbps,
            self.performance.peak_throughput_mbps,
            self.performance.avg_latency_ms,
            self.performance.p95_latency_ms,
            self.performance.p99_latency_ms,
            self.errors.error_rate,
            error_types_html,
            recent_errors_html,
        );

        std::fs::write(format!("{}.html", path), html_content)?;
        Ok(())
    }
}

fn percentile(sorted_values: &[f64], p: f64) -> f64 {
    if sorted_values.is_empty() {
        return 0.0;
    }
    
    let index = (p / 100.0) * (sorted_values.len() - 1) as f64;
    let lower = index.floor() as usize;
    let upper = index.ceil() as usize;
    
    if lower == upper {
        sorted_values[lower]
    } else {
        let weight = index - lower as f64;
        sorted_values[lower] * (1.0 - weight) + sorted_values[upper] * weight
    }
}