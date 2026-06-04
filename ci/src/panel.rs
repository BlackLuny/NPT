//! zf-web panel HTTP client (authenticated with `X-API-Key`).
//!
//! Only the subset of endpoints the CI needs is implemented. Snapshot endpoints
//! (connection_stats / test_latency / blocked_ips) are fetched as raw JSON text
//! and archived verbatim — we never need to interpret them, only preserve them.

use anyhow::Context;
use bytes::Bytes;
use serde::Deserialize;
use std::time::Duration;

pub struct Panel {
    base: String,
    http: reqwest::Client,
}

/// Subset of zf-web `PortInfo` (unknown fields ignored). Field names match the
/// panel JSON exactly. Optional inbound-proxy fields are absent in the JSON when
/// disabled, hence `#[serde(default)]`.
#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)] // some fields kept for archival context / future protocol support
pub struct PortInfo {
    pub id: i32,
    #[serde(default)]
    pub display_name: String,
    pub ip_addr: String,
    #[serde(default)]
    pub extra_inbound_address: Option<Vec<String>>,
    pub port_v4: i32,
    #[serde(default)]
    pub is_suspended: bool,
    #[serde(default)]
    pub enable_udp: Option<bool>,
    #[serde(default)]
    pub inbound_proxy_enabled: Option<bool>,
    #[serde(default)]
    pub inbound_proxy_protocol: Option<String>,
    #[serde(default)]
    pub target_address_list: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct PaginatedPortsResponse {
    ports: Vec<PortInfo>,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)] // server_ip/is_selected surfaced for archive context
pub struct PortServer {
    pub server_id: i32,
    #[serde(default)]
    pub server_name: String,
    #[serde(default)]
    pub server_ip: String,
    #[serde(default)]
    pub is_selected: bool,
}

#[derive(Debug, Deserialize)]
struct PortDetailInfoResponse {
    #[serde(default)]
    servers: Vec<PortServer>,
}

/// Generic `ApiResponse<T>` envelope used by most zf-web handlers.
#[derive(Debug, Deserialize)]
struct ApiResponse<T> {
    #[serde(default)]
    success: Option<bool>,
    data: Option<T>,
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    error: Option<String>,
}

impl Panel {
    pub fn new(base: &str, api_key: &str) -> anyhow::Result<Self> {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            "X-API-Key",
            reqwest::header::HeaderValue::from_str(api_key)
                .context("api_key is not a valid header value")?,
        );
        let http = reqwest::Client::builder()
            .default_headers(headers)
            .timeout(Duration::from_secs(60))
            .build()
            .context("failed to build HTTP client")?;
        Ok(Self {
            base: base.trim_end_matches('/').to_string(),
            http,
        })
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base, path)
    }

    /// `GET /api/ports?id=<port_id>` → raw `PaginatedPortsResponse` (not wrapped).
    pub async fn get_port(&self, port_id: i32) -> anyhow::Result<PortInfo> {
        let resp = self
            .http
            .get(self.url("/api/ports"))
            .query(&[("id", port_id.to_string())])
            .send()
            .await
            .context("GET /api/ports failed")?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            anyhow::bail!("GET /api/ports?id={port_id} -> {status}: {text}");
        }
        let parsed: PaginatedPortsResponse = serde_json::from_str(&text)
            .with_context(|| format!("parse /api/ports response: {text}"))?;
        parsed
            .ports
            .into_iter()
            .find(|p| p.id == port_id)
            .ok_or_else(|| anyhow::anyhow!("port {port_id} not found in panel response"))
    }

    /// `GET /api/port_detail_info?port_id=<id>` → resolved backing servers.
    pub async fn port_servers(&self, port_id: i32) -> anyhow::Result<Vec<PortServer>> {
        let resp = self
            .http
            .get(self.url("/api/port_detail_info"))
            .query(&[("port_id", port_id.to_string())])
            .send()
            .await
            .context("GET /api/port_detail_info failed")?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            anyhow::bail!("GET /api/port_detail_info?port_id={port_id} -> {status}: {text}");
        }
        // Try the ApiResponse envelope first, fall back to a bare object.
        if let Ok(env) = serde_json::from_str::<ApiResponse<PortDetailInfoResponse>>(&text) {
            if let Some(d) = env.data {
                return Ok(d.servers);
            }
        }
        let bare: PortDetailInfoResponse = serde_json::from_str(&text)
            .with_context(|| format!("parse /api/port_detail_info response: {text}"))?;
        Ok(bare.servers)
    }

    /// Fetch an arbitrary GET endpoint as raw JSON text (for snapshot archival).
    pub async fn get_text(&self, path: &str, query: &[(&str, String)]) -> anyhow::Result<String> {
        let resp = self
            .http
            .get(self.url(path))
            .query(query)
            .send()
            .await
            .with_context(|| format!("GET {path} failed"))?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            anyhow::bail!("GET {path} -> {status}: {text}");
        }
        Ok(text)
    }

    /// `GET /api/admin/debug/collectors?server_id=<id>` → available collector IDs.
    /// Requires an admin-owned API key.
    pub async fn debug_collectors(&self, server_id: i32) -> anyhow::Result<Vec<String>> {
        let text = self
            .get_text(
                "/api/admin/debug/collectors",
                &[("server_id", server_id.to_string())],
            )
            .await?;
        let env: ApiResponse<Vec<serde_json::Value>> = serde_json::from_str(&text)
            .with_context(|| format!("parse debug collectors: {text}"))?;
        if env.success == Some(false) {
            anyhow::bail!(
                "debug collectors error: {}",
                env.error.or(env.message).unwrap_or_default()
            );
        }
        let metas = env.data.unwrap_or_default();
        // zf-web returns `collector_id` (fall back to `id` for safety).
        let ids = metas
            .into_iter()
            .filter_map(|m| {
                m.get("collector_id")
                    .or_else(|| m.get("id"))
                    .and_then(|v| v.as_str())
                    .map(String::from)
            })
            .collect();
        Ok(ids)
    }

    /// `POST /api/admin/debug/export {server_id, collector_ids}` → diagnostic ZIP.
    /// Requires an admin-owned API key.
    pub async fn debug_export(
        &self,
        server_id: i32,
        collector_ids: &[String],
    ) -> anyhow::Result<Bytes> {
        let body = DebugExportRequest {
            server_id,
            collector_ids: collector_ids.to_vec(),
        };
        let resp = self
            .http
            .post(self.url("/api/admin/debug/export"))
            .json(&body)
            .timeout(Duration::from_secs(180))
            .send()
            .await
            .context("POST /api/admin/debug/export failed")?;
        let status = resp.status();
        let ctype = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("debug export -> {status}: {text}");
        }
        if !ctype.contains("application/zip") {
            // Server returned a JSON error with 200; surface it.
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("debug export returned non-zip ({ctype}): {text}");
        }
        resp.bytes().await.context("read debug export bytes")
    }
}

#[derive(serde::Serialize)]
struct DebugExportRequest {
    server_id: i32,
    collector_ids: Vec<String>,
}
