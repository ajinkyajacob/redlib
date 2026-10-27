//! End-to-end tests for the two HTTP surfaces a single process serves: the web
//! listener that also carries `/api/v1`, and the separate MCP Streamable HTTP
//! listener.
//!
//! `main.rs` mints an OAuth token before it binds, so starting the binary needs
//! Reddit to be reachable. When startup fails because Reddit refused or could not
//! be reached the test reports a skip: that is the environment, not the code under
//! test. Every assertion here runs after startup and needs no network.
//!
//! One test keeps a single server for the whole set of assertions, because each
//! spawn pays for the OAuth handshake.

use std::io::{ErrorKind as IoErrorKind, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// Must match the value the server is started with; the token is not a secret here.
const TOKEN: &str = "e2e-test-token";
const STARTUP_TIMEOUT: Duration = Duration::from_secs(150);
const IO_TIMEOUT: Duration = Duration::from_secs(15);

/// Asks the OS for a free loopback port and releases it for the child to bind.
/// The window between release and bind is small but real; a collision surfaces as
/// a startup failure rather than a silently wrong assertion.
fn free_port(exclude: &[u16]) -> u16 {
	for _ in 0..20 {
		let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind an ephemeral port");
		let port = listener.local_addr().expect("local addr").port();
		drop(listener);
		if !exclude.contains(&port) {
			return port;
		}
	}
	panic!("could not find two distinct free ports");
}

fn port_open(port: u16) -> bool {
	let addr = SocketAddr::from(([127, 0, 0, 1], port));
	TcpStream::connect_timeout(&addr, Duration::from_millis(250)).is_ok()
}

struct HttpResponse {
	status: u16,
	headers: Vec<(String, String)>,
	body: String,
}

impl HttpResponse {
	fn header(&self, name: &str) -> Option<&str> {
		self.headers.iter().find(|(key, _)| key.eq_ignore_ascii_case(name)).map(|(_, value)| value.as_str())
	}
}

fn request(port: u16, method: &str, path: &str, auth: Option<&str>, headers: &[(&str, &str)], body: Option<&str>) -> HttpResponse {
	let addr = SocketAddr::from(([127, 0, 0, 1], port));
	let mut stream = TcpStream::connect_timeout(&addr, IO_TIMEOUT).unwrap_or_else(|e| panic!("connect to {addr}: {e}"));
	stream.set_read_timeout(Some(IO_TIMEOUT)).expect("read timeout");
	stream.set_write_timeout(Some(IO_TIMEOUT)).expect("write timeout");

	let mut raw = format!("{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n");
	for (name, value) in headers {
		raw.push_str(&format!("{name}: {value}\r\n"));
	}
	if let Some(token) = auth {
		raw.push_str(&format!("Authorization: Bearer {token}\r\n"));
	}
	let body = body.unwrap_or_default();
	if !body.is_empty() {
		raw.push_str(&format!("Content-Type: application/json\r\nContent-Length: {}\r\n", body.len()));
	}
	raw.push_str("\r\n");
	raw.push_str(body);

	stream.write_all(raw.as_bytes()).unwrap_or_else(|e| panic!("write {method} {path}: {e}"));
	stream.flush().expect("flush request");

	// A streamed response may legitimately stay open; the status line and headers
	// always arrive first, so a read timeout still leaves enough to assert on.
	let mut buf = Vec::new();
	match stream.read_to_end(&mut buf) {
		Ok(_) => {}
		Err(e) if matches!(e.kind(), IoErrorKind::WouldBlock | IoErrorKind::TimedOut) => {}
		Err(e) => panic!("read {method} {path}: {e}"),
	}
	let raw = String::from_utf8_lossy(&buf).into_owned();
	let (head, body) = raw.split_once("\r\n\r\n").unwrap_or_else(|| panic!("malformed response to {method} {path}: {raw:?}"));
	let mut lines = head.split("\r\n");
	let status_line = lines.next().unwrap_or_default();
	let status = status_line
		.split_whitespace()
		.nth(1)
		.and_then(|code| code.parse().ok())
		.unwrap_or_else(|| panic!("malformed status line for {method} {path}: {status_line:?}"));
	let headers = lines
		.filter_map(|line| line.split_once(':'))
		.map(|(name, value)| (name.trim().to_owned(), value.trim().to_owned()))
		.collect();
	HttpResponse {
		status,
		headers,
		body: body.to_owned(),
	}
}

enum Startup {
	Ready(Server),
	/// Reddit was unreachable or refused the startup token.
	Skipped(String),
	Failed(String),
}

struct Server {
	child: Child,
	web: u16,
	mcp: u16,
	stderr: Arc<Mutex<String>>,
	reader: Option<JoinHandle<()>>,
}

impl Server {
	fn start() -> Startup {
		let web = free_port(&[]);
		let mcp = free_port(&[web]);
		let mut child = Command::new(env!("CARGO_BIN_EXE_redlib"))
			.args(["--address", "127.0.0.1", "--port", &web.to_string(), "--mcp-http", &mcp.to_string()])
			.env("REDLIB_API_TOKEN", TOKEN)
			// Surface the OAuth failure text so an unreachable Reddit can be told
			// apart from a bug in startup.
			.env("RUST_LOG", "error")
			.stdout(Stdio::null())
			.stderr(Stdio::piped())
			.spawn()
			.expect("spawn the redlib binary");

		let stderr = Arc::new(Mutex::new(String::new()));
		let mut pipe = child.stderr.take().expect("stderr is piped");
		let sink = Arc::clone(&stderr);
		let mut reader = Some(thread::spawn(move || {
			let mut buf = Vec::new();
			let _ = pipe.read_to_end(&mut buf);
			sink.lock().unwrap_or_else(|e| e.into_inner()).push_str(&String::from_utf8_lossy(&buf));
		}));

		let deadline = Instant::now() + STARTUP_TIMEOUT;
		loop {
			if port_open(web) && port_open(mcp) {
				return Startup::Ready(Self { child, web, mcp, stderr, reader });
			}
			if let Some(status) = child.try_wait().expect("poll the child") {
				if let Some(handle) = reader.take() {
					let _ = handle.join();
				}
				let text = stderr.lock().unwrap_or_else(|e| e.into_inner()).clone();
				return classify_exit(&status, &text);
			}
			if Instant::now() >= deadline {
				let _ = child.kill();
				let _ = child.wait();
				if let Some(handle) = reader.take() {
					let _ = handle.join();
				}
				let text = stderr.lock().unwrap_or_else(|e| e.into_inner()).clone();
				return Startup::Failed(format!("neither listener came up within {STARTUP_TIMEOUT:?}\nstderr:\n{text}"));
			}
			thread::sleep(Duration::from_millis(100));
		}
	}

	fn get(&self, path: &str, auth: Option<&str>) -> HttpResponse {
		request(self.web, "GET", path, auth, &[("Accept", "application/json")], None)
	}

	fn mcp_post(&self, auth: Option<&str>, body: &str) -> HttpResponse {
		request(self.mcp, "POST", "/mcp", auth, &[("Accept", "application/json, text/event-stream")], Some(body))
	}
}

/// Startup failures are only tolerated when Reddit is the cause; anything else is
/// a real defect and must fail the test.
fn classify_exit(status: &std::process::ExitStatus, stderr: &str) -> Startup {
	let oauth_failed = stderr.contains("Failed to create OAuth client");
	if oauth_failed {
		return Startup::Skipped(format!("redlib could not obtain a startup token (status {status})\nstderr:\n{}", stderr.trim()));
	}
	Startup::Failed(format!("redlib exited during startup (status {status})\nstderr:\n{}", stderr.trim()))
}

impl Drop for Server {
	fn drop(&mut self) {
		let _ = self.child.kill();
		let _ = self.child.wait();
		if let Some(handle) = self.reader.take() {
			let _ = handle.join();
		}
		// A failing assertion gives no other window into the child's log.
		if thread::panicking() {
			let text = self.stderr.lock().unwrap_or_else(|e| e.into_inner()).clone();
			if !text.trim().is_empty() {
				eprintln!("redlib stderr during the failed test:\n{}", text.trim());
			}
		}
	}
}

#[test]
fn http_surfaces_enforce_auth_and_validation() {
	let server = match Server::start() {
		Startup::Ready(server) => server,
		Startup::Skipped(why) => {
			eprintln!("SKIP: {why}\nThis test needs Reddit to be reachable so the binary can mint its OAuth token.");
			return;
		}
		Startup::Failed(why) => panic!("redlib did not start: {why}"),
	};

	// Health is the one route exempt from the bearer gate.
	let health = server.get("/api/v1/health", None);
	assert_eq!(health.status, 200, "health must not require a token: {}", health.body);
	assert!(health.body.contains("\"kind\":\"health\""), "unexpected health body: {}", health.body);

	// Missing and wrong credentials are indistinguishable, and both challenge.
	for auth in [None, Some("wrong-token")] {
		let res = server.get("/api/v1/r/rust", auth);
		assert_eq!(res.status, 401, "auth={auth:?} body={}", res.body);
		assert_eq!(res.header("www-authenticate"), Some("Bearer"), "401 must challenge the scheme");
		assert!(res.body.contains("\"kind\":\"UNAUTHORIZED\""), "unexpected body: {}", res.body);
	}

	// Unknown API paths answer with the JSON envelope, not the HTML catch-all.
	let unknown = server.get("/api/v1/nope", Some(TOKEN));
	assert_eq!(unknown.status, 404);
	assert!(unknown.body.contains("\"kind\":\"CONTENT_UNAVAILABLE\""), "unexpected body: {}", unknown.body);

	// A malformed post id is the caller's mistake; the GraphQL path parser would
	// otherwise report it as an upstream failure.
	let bad_id = server.get("/api/v1/r/rust/comments/bad-id", Some(TOKEN));
	assert_eq!(bad_id.status, 400, "body={}", bad_id.body);
	assert!(bad_id.body.contains("\"kind\":\"BAD_INPUT\""), "unexpected body: {}", bad_id.body);

	// Cursors are unpadded URL-safe base64; padding cannot decode and must not
	// reach the upstream client as a 502.
	let padded = server.get("/api/v1/r/rust/comments/abc123?cursor=abc==", Some(TOKEN));
	assert_eq!(padded.status, 400, "body={}", padded.body);
	assert!(padded.body.contains("\"kind\":\"BAD_INPUT\""), "unexpected body: {}", padded.body);

	// A well-formed id that does not exist is 404, not 502. Startup already needed
	// Reddit, but the GraphQL host can fail independently, so a transport failure
	// during this one probe is tolerated explicitly rather than flaking the test.
	let missing = server.get("/api/v1/r/rust/comments/zzzzzz", Some(TOKEN));
	match missing.status {
		404 => assert!(missing.body.contains("\"kind\":\"CONTENT_UNAVAILABLE\""), "unexpected body: {}", missing.body),
		502 if missing.body.contains("\"kind\":\"REDLIB_DOWN\"") => {
			eprintln!("note: skipping the missing-post assertion, Reddit GraphQL is unreachable right now: {}", missing.body)
		}
		other => panic!("a missing post must be 404, got {other}: {}", missing.body),
	}

	// The MCP listener serves exactly one path and always demands the token.
	let not_mcp = request(server.mcp, "GET", "/", None, &[("Accept", "application/json")], None);
	assert_eq!(not_mcp.status, 404, "body={}", not_mcp.body);
	assert!(not_mcp.body.contains("\"kind\":\"NOT_FOUND\""), "unexpected body: {}", not_mcp.body);

	let unauthenticated = server.mcp_post(None, r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#);
	assert_eq!(unauthenticated.status, 401, "body={}", unauthenticated.body);
	assert_eq!(unauthenticated.header("www-authenticate"), Some("Bearer"));
	assert!(unauthenticated.body.contains("\"kind\":\"UNAUTHORIZED\""), "unexpected body: {}", unauthenticated.body);

	// With the token the request reaches rmcp: a bad session or malformed payload
	// is rmcp's business, but it must not be the gate's 401.
	let authenticated = server.mcp_post(
		Some(TOKEN),
		r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"http-test","version":"0.0.1"}}}"#,
	);
	assert_ne!(authenticated.status, 401, "the token was rejected: {}", authenticated.body);
	assert_eq!(authenticated.status, 200, "initialize over Streamable HTTP failed: {}", authenticated.body);
}
