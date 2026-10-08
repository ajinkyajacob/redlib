use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

pub struct Harness {
	pub child: Child,
	pub stdin: ChildStdin,
	pub stdout: BufReader<ChildStdout>,
}

impl Harness {
	pub fn spawn() -> Self {
		let exe = env!("CARGO_BIN_EXE_redlib-mcp");
		let mut child = Command::new(exe)
			.stdin(Stdio::piped())
			.stdout(Stdio::piped())
			.stderr(Stdio::piped())
			.spawn()
			.expect("spawn redlib-mcp");
		let stdin = child.stdin.take().expect("stdin");
		let stdout = BufReader::new(child.stdout.take().expect("stdout"));
		Self { child, stdin, stdout }
	}

	pub fn send(&mut self, payload: &str) {
		self.stdin.write_all(payload.as_bytes()).expect("write request");
		self.stdin.write_all(b"\n").expect("newline");
		self.stdin.flush().expect("flush");
	}

	pub fn read_line(&mut self) -> String {
		let mut line = String::new();
		self.stdout.read_line(&mut line).expect("read response");
		line
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
