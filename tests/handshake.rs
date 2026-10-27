use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

const READ_TIMEOUT: Duration = Duration::from_secs(60);

pub struct Harness {
	pub child: Child,
	pub stdin: ChildStdin,
	lines: Receiver<String>,
	stderr_buf: Arc<Mutex<String>>,
	stderr_thread: Option<JoinHandle<()>>,
}

impl Harness {
	pub fn spawn() -> Self {
		let exe = env!("CARGO_BIN_EXE_redlib");
		let mut child = Command::new(exe)
			.arg("mcp")
			.stdin(Stdio::piped())
			.stdout(Stdio::piped())
			.stderr(Stdio::piped())
			.spawn()
			.expect("spawn redlib mcp");
		let stdin = child.stdin.take().expect("stdin");

		let (tx, lines) = mpsc::channel();
		let stdout = child.stdout.take().expect("stdout");
		thread::spawn(move || {
			for line in BufReader::new(stdout).lines() {
				let Ok(line) = line else { break };
				if tx.send(line).is_err() {
					break;
				}
			}
		});

		let stderr_buf = Arc::new(Mutex::new(String::new()));
		let sink = Arc::clone(&stderr_buf);
		let mut stderr = child.stderr.take().expect("stderr");
		let stderr_thread = thread::spawn(move || {
			let mut buf = Vec::new();
			let _ = stderr.read_to_end(&mut buf);
			sink.lock().unwrap_or_else(|e| e.into_inner()).push_str(&String::from_utf8_lossy(&buf));
		});

		Self {
			child,
			stdin,
			lines,
			stderr_buf,
			stderr_thread: Some(stderr_thread),
		}
	}

	pub fn send(&mut self, payload: &str) {
		self.stdin.write_all(payload.as_bytes()).expect("write request");
		self.stdin.write_all(b"\n").expect("newline");
		self.stdin.flush().expect("flush");
	}

	pub fn read_line(&mut self) -> String {
		match self.lines.recv_timeout(READ_TIMEOUT) {
			Ok(line) => line,
			Err(RecvTimeoutError::Timeout) => panic!("no response within {READ_TIMEOUT:?}; stderr:\n{}", self.stderr_text()),
			Err(RecvTimeoutError::Disconnected) => panic!("server closed stdout before responding; stderr:\n{}", self.stderr_text()),
		}
	}

	/// Kill the child so its stderr pipe closes, then return whatever it wrote.
	pub fn stderr_text(&mut self) -> String {
		let _ = self.child.kill();
		_ = self.child.wait();
		if let Some(handle) = self.stderr_thread.take() {
			let _ = handle.join();
		}
		self.stderr_buf.lock().unwrap_or_else(|e| e.into_inner()).clone()
	}

	pub fn initialize(&mut self) -> String {
		self.send(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"test","version":"0.0.1"}}}"#);
		let line = self.read_line();
		self.send(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#);
		line
	}
}

impl Drop for Harness {
	fn drop(&mut self) {
		let _ = self.child.kill();
		let _ = self.child.wait();
	}
}

#[test]
fn initialize_handshake_and_stdout_is_pure() {
	let mut h = Harness::spawn();
	let line = h.initialize();
	assert!(line.contains("protocolVersion"), "unexpected response: {line}");
	assert!(!line.contains("Starting Redlib"), "stdout carried a non-protocol line: {line}");
}
