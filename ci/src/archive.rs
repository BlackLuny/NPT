//! On-failure artifact collection: panel state snapshots + worker debug ZIPs.

use crate::panel::{Panel, PortServer};
use std::path::Path;
use tracing::{info, warn};

/// Archive everything useful for diagnosing a failed port into `dir`.
/// Best-effort: individual fetch failures are logged, not fatal.
pub async fn archive_failure(
    panel: &Panel,
    dir: &Path,
    port_id: i32,
    servers: &[PortServer],
    collect_logs: bool,
    collector_ids: &[String],
) {
    if let Err(e) = std::fs::create_dir_all(dir) {
        warn!("archive: failed to create {}: {e}", dir.display());
        return;
    }

    // Panel state snapshots (raw JSON, verbatim).
    let pid = port_id.to_string();
    let snapshots = [
        ("connection_stats.json", "/api/connection_stats", "port_id"),
        ("test_latency.json", "/api/test_latency", "port_id"),
        ("blocked_ips.json", "/api/port_blocked_ips", "port_id"),
    ];
    for (file, path, key) in snapshots {
        match panel.get_text(path, &[(key, pid.clone())]).await {
            Ok(text) => write_file(dir, file, text.as_bytes()),
            Err(e) => warn!("archive: snapshot {path} failed: {e}"),
        }
    }

    if !collect_logs {
        return;
    }

    // Worker diagnostic ZIPs (requires admin-owned API key).
    for srv in servers {
        let ids = if collector_ids.is_empty() {
            match panel.debug_collectors(srv.server_id).await {
                Ok(ids) if !ids.is_empty() => ids,
                Ok(_) => {
                    warn!("archive: no debug collectors for server {}", srv.server_id);
                    continue;
                }
                Err(e) => {
                    warn!(
                        "archive: list collectors for server {} failed: {e}",
                        srv.server_id
                    );
                    continue;
                }
            }
        } else {
            collector_ids.to_vec()
        };
        match panel.debug_export(srv.server_id, &ids).await {
            Ok(zip) => {
                let name = format!("server-{}-{}-debug.zip", srv.server_id, sanitize(&srv.server_name));
                write_file(dir, &name, &zip);
                info!(
                    "archive: saved {} ({} bytes) for server {}",
                    name,
                    zip.len(),
                    srv.server_id
                );
            }
            Err(e) => warn!("archive: debug export for server {} failed: {e}", srv.server_id),
        }
    }
}

fn write_file(dir: &Path, name: &str, bytes: &[u8]) {
    let path = dir.join(name);
    if let Err(e) = std::fs::write(&path, bytes) {
        warn!("archive: write {} failed: {e}", path.display());
    }
}

fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect()
}
