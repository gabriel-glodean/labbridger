use actix_web::{web, Error, HttpRequest, HttpResponse};
use futures_util::StreamExt;
use reqwest::Client;
use std::collections::HashMap;
use std::time::Duration;

use crate::app_config::RelayTarget;
use crate::network_scanner::NetworkScanner;

// ── State ────────────────────────────────────────────────────────────────────

/// Shared state: a single reqwest client + the map of named upstream targets
/// + a handle to the network scanner (used to resolve MAC addresses to IPs).
pub struct RelayState {
    pub client: Client,
    pub targets: HashMap<String, RelayTarget>,
    pub scanner: NetworkScanner,
}

impl RelayState {
    pub fn new(targets: HashMap<String, RelayTarget>, scanner: NetworkScanner) -> Self {
        Self {
            client: Client::builder()
                .no_proxy()
                // Only limit how long we wait to *connect*; reads/streams are unlimited
                .connect_timeout(Duration::from_secs(10))
                .build()
                .expect("Failed to build relay HTTP client"),
            targets,
            scanner,
        }
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Headers that must not be forwarded between hops (RFC 7230 §6.1).
fn is_hop_by_hop(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailers"
            | "transfer-encoding"
            | "upgrade"
            | "host"
            | "proxy-connection"
    )
}

// ── Core proxy logic ─────────────────────────────────────────────────────────

/// Forwards the request to the named upstream target and streams **both**
/// the request body and the response body, so it works for:
///   - plain JSON / binary responses
///   - NDJSON streams (Ollama generate / chat)
///   - Server-Sent Events
///   - large file uploads / downloads
async fn proxy(
    req: &HttpRequest,
    target_name: &str,
    sub_path: &str,
    payload: web::Payload,       // raw stream – no size limit, no buffering
    state: &web::Data<RelayState>,
) -> Result<HttpResponse, Error> {
    // Resolve target base URL
    let base_url = match state.targets.get(target_name) {
        Some(RelayTarget::Static(url)) => url.trim_end_matches('/').to_owned(),
        Some(RelayTarget::StaticManaged { url, .. }) => url.trim_end_matches('/').to_owned(),
        Some(RelayTarget::Mac { mac, port, .. }) => {
            match state.scanner.get_ip_by_mac(mac) {
                Some(ip) => format!("http://{}:{}", ip, port),
                None => {
                    return Ok(HttpResponse::BadGateway().body(format!(
                        "Target '{}': MAC {} not found on the network (scan may still be running)",
                        target_name, mac
                    )));
                }
            }
        }
        None => {
            return Ok(HttpResponse::NotFound()
                .body(format!("Unknown relay target: '{}'", target_name)));
        }
    };

    // Build upstream URL (path + query string)
    let path_part = if sub_path.is_empty() {
        String::new()
    } else {
        format!("/{}", sub_path)
    };
    let qs = req.query_string();
    let qs_part = if qs.is_empty() {
        String::new()
    } else {
        format!("?{}", qs)
    };
    let upstream_url = format!("{}{}{}", base_url, path_part, qs_part);

    // Build reqwest request
    let method = reqwest::Method::from_bytes(req.method().as_str().as_bytes())
        .map_err(actix_web::error::ErrorBadRequest)?;

    let mut rb = state.client.request(method, &upstream_url);

    // Forward request headers (skip hop-by-hop)
    for (name, value) in req.headers() {
        if !is_hop_by_hop(name.as_str()) {
            rb = rb.header(name.as_str(), value.as_bytes());
        }
    }

    // Collect request body without any size limit.
    // web::Payload is !Send so it can't be wrapped in reqwest's Body::wrap_stream;
    // we drain the stream manually into Bytes instead. For Ollama the request is
    // JSON (small); the important streaming path is the *response* below.
    let mut body_buf: Vec<u8> = Vec::new();
    let mut payload_stream = payload;
    while let Some(chunk) = payload_stream.next().await {
        let chunk = chunk.map_err(actix_web::error::ErrorBadRequest)?;
        body_buf.extend_from_slice(&chunk);
    }
    if !body_buf.is_empty() {
        rb = rb.body(body_buf);
    }

    // Send to upstream
    let upstream_resp = rb
        .send()
        .await
        .map_err(|e| actix_web::error::ErrorBadGateway(e.to_string()))?;

    // Map upstream status
    let status = actix_web::http::StatusCode::from_u16(upstream_resp.status().as_u16())
        .unwrap_or(actix_web::http::StatusCode::BAD_GATEWAY);

    let mut builder = HttpResponse::build(status);

    // Forward response headers (skip hop-by-hop AND content-length).
    // content-length must be dropped: actix will use chunked transfer-encoding
    // for the streaming body, so a stale content-length would confuse clients.
    for (name, value) in upstream_resp.headers() {
        let n = name.as_str();
        if !is_hop_by_hop(n) && n != "content-length" {
            builder.insert_header((n, value.as_bytes()));
        }
    }

    // Stream response body — preserves every content type including NDJSON /
    // SSE tokens emitted one chunk at a time by Ollama.
    let byte_stream = upstream_resp.bytes_stream().map(|chunk| {
        chunk.map_err(|e| actix_web::error::ErrorBadGateway(e.to_string()))
    });

    Ok(builder.streaming(byte_stream))
}

// ── Route handlers ────────────────────────────────────────────────────────────

/// Handles  /relay/{target}/{path:.*}
pub async fn relay_handler(
    req: HttpRequest,
    path: web::Path<(String, String)>,
    payload: web::Payload,
    state: web::Data<RelayState>,
) -> Result<HttpResponse, Error> {
    let (target, sub_path) = path.into_inner();
    proxy(&req, &target, &sub_path, payload, &state).await
}

/// Handles  /relay/{target}  (no trailing path)
pub async fn relay_root_handler(
    req: HttpRequest,
    target: web::Path<String>,
    payload: web::Payload,
    state: web::Data<RelayState>,
) -> Result<HttpResponse, Error> {
    proxy(&req, &target.into_inner(), "", payload, &state).await
}

