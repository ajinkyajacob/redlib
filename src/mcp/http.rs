//! Streamable HTTP transport for the MCP server.
//!
//! `rmcp` speaks `http` 1.x while redlib's web UI and REST API stay on `hyper` 0.14, which
//! speaks `http` 0.2. The two stacks therefore cannot share a connection type, so this
//! module runs its own `hyper` 1 listener on a separate port and calls
//! [`StreamableHttpService::handle`] directly instead of going through a `tower` stack.
//!
//! Every request to `/mcp` must carry `Authorization: Bearer <REDLIB_API_TOKEN>`. The
//! process-wide 30 req/min token bucket is shared with the REST API, so a client cannot
//! double its budget by alternating surfaces.

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Full};
use hyper1::body::Incoming;
use hyper1::header::{HeaderValue, AUTHORIZATION, CACHE_CONTROL, CONTENT_TYPE, WWW_AUTHENTICATE};
use hyper1::{Request, Response, StatusCode};
use hyper_util::rt::TokioTimer;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use serde_json::json;

use crate::api::{bearer_matches, rate_limit_take, rate_limited_error};
use crate::errors::{ErrorKind, RedditError};
use crate::mcp::tools::RedlibServer;

/// Hosts accepted by `rmcp`'s DNS-rebinding guard when `REDLIB_MCP_ALLOWED_HOSTS` is unset.
const LOOPBACK_HOSTS: &[&str] = &["localhost", "127.0.0.1", "::1"];

/// A client that opens a connection and never finishes its request headers must not
/// be able to pin a task forever. The web listener is hyper 0.14, which applies its
/// own default; this listener has to opt in.
const HEADER_READ_TIMEOUT: Duration = Duration::from_secs(15);

/// Parses `REDLIB_MCP_HTTP_PORT`: `"off"` disables the listener, a `u16` selects the port,
/// and anything else is an error so a typo cannot silently fall back to the default.
pub fn resolve_port(raw: &str) -> Result<Option<u16>, String> {
	let trimmed = raw.trim();
	if trimmed.eq_ignore_ascii_case("off") {
		return Ok(None);
	}
	trimmed.parse::<u16>().map(Some).map_err(|_| format!("expected a port number or 'off', got {raw:?}"))
}

/// Parses `REDLIB_MCP_ALLOWED_HOSTS`. An empty list falls back to loopback: `rmcp` rejects
/// non-loopback `Host` headers by default, which would break a public deployment silently.
pub fn parse_allowed_hosts(raw: Option<&str>) -> Vec<String> {
	let hosts: Vec<String> = raw
		.map(|value| value.split(',').map(str::trim).filter(|host| !host.is_empty()).map(str::to_owned).collect())
		.unwrap_or_default();

	if hosts.is_empty() {
		LOOPBACK_HOSTS.iter().map(|host| (*host).to_owned()).collect()
	} else {
		hosts
	}
}

/// Serves MCP over Streamable HTTP on `bind` until the process exits.
pub async fn serve(bind: String, token: String) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
	let mut config = StreamableHttpServerConfig::default();
	config.allowed_hosts = parse_allowed_hosts(std::env::var("REDLIB_MCP_ALLOWED_HOSTS").ok().as_deref());
	let service = StreamableHttpService::new(|| Ok(RedlibServer::new()), Arc::new(LocalSessionManager::default()), config);

	let listener = tokio::net::TcpListener::bind(&bind).await?;
	loop {
		let (stream, _peer) = match listener.accept().await {
			Ok(accepted) => accepted,
			// A single failed accept (fd exhaustion, a client that vanished mid-handshake)
			// must not take the listener down for the rest of the process.
			Err(e) => {
				eprintln!("MCP HTTP accept failed: {e}");
				continue;
			}
		};
		let io = hyper_util::rt::TokioIo::new(stream);
		let svc = service.clone();
		let token = token.clone();
		tokio::spawn(async move {
			let conn = hyper1::server::conn::http1::Builder::new()
				.timer(TokioTimer::new())
				.header_read_timeout(HEADER_READ_TIMEOUT)
				.serve_connection(
					io,
					hyper1::service::service_fn(move |req| {
						let svc = svc.clone();
						let token = token.clone();
						async move { Ok::<_, Infallible>(dispatch(svc, token, req).await) }
					}),
				);
			if let Err(e) = conn.await {
				eprintln!("MCP HTTP connection error: {e}");
			}
		});
	}
}

/// Applies the path, bearer, and rate-limit guards, then hands the request to `rmcp`.
async fn dispatch(service: StreamableHttpService<RedlibServer, LocalSessionManager>, token: String, req: Request<Incoming>) -> Response<BoxBody<Bytes, Infallible>> {
	if req.uri().path() != "/mcp" {
		return json_response(
			StatusCode::NOT_FOUND,
			json!({ "ok": false, "kind": "NOT_FOUND", "message": "MCP is served at /mcp" }).to_string(),
		);
	}

	let header = req.headers().get(AUTHORIZATION).and_then(|value| value.to_str().ok());
	if !bearer_matches(header, &token) {
		// Same reasoning as the REST gate: eprintln so the default log filter does
		// not hide rejected credentials, and never print the credential itself.
		eprintln!("mcp: rejected unauthenticated {}", req.uri().path());
		return error_response(RedditError::new(ErrorKind::Unauthorized, "missing or invalid bearer token"));
	}

	if !rate_limit_take() {
		return error_response(rate_limited_error());
	}

	service.handle(req).await
}

fn error_response(error: RedditError) -> Response<BoxBody<Bytes, Infallible>> {
	let status = StatusCode::from_u16(error.status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
	let mut response = json_response(status, error.to_json().to_string());
	if status == StatusCode::UNAUTHORIZED {
		response.headers_mut().insert(WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
	}
	response
}

fn json_response(status: StatusCode, body: String) -> Response<BoxBody<Bytes, Infallible>> {
	let mut response = Response::new(Full::new(Bytes::from(body)).map_err(|never| match never {}).boxed());
	*response.status_mut() = status;
	response.headers_mut().insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
	response.headers_mut().insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
	response
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn resolves_off_to_no_listener() {
		assert_eq!(resolve_port("off").unwrap(), None);
		assert_eq!(resolve_port(" OFF ").unwrap(), None);
	}

	#[test]
	fn resolves_numeric_port() {
		assert_eq!(resolve_port("9090").unwrap(), Some(9090));
		assert_eq!(resolve_port("0").unwrap(), Some(0));
	}

	#[test]
	fn rejects_non_numeric_port_instead_of_defaulting() {
		assert!(resolve_port("90o0").is_err(), "a typo must not silently become 9090");
		assert!(resolve_port("").is_err());
		assert!(resolve_port("70000").is_err(), "out of u16 range");
	}

	#[test]
	fn allowed_hosts_default_to_loopback() {
		assert_eq!(parse_allowed_hosts(None), vec!["localhost", "127.0.0.1", "::1"]);
		assert_eq!(parse_allowed_hosts(Some("   ")), vec!["localhost", "127.0.0.1", "::1"]);
	}

	#[test]
	fn allowed_hosts_parse_a_comma_separated_list() {
		assert_eq!(parse_allowed_hosts(Some("example.com, example.com:8080 ,,")), vec!["example.com", "example.com:8080"]);
	}
}
