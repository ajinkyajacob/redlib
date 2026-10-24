use crate::errors::{map_client_err, RedditError};
use crate::mcp::pace::Pacer;
use serde_json::Value;

static PACER: std::sync::LazyLock<Pacer> = std::sync::LazyLock::new(crate::mcp::pace::from_env);

/// Percent-encode a value for use in a Reddit query string.
pub fn encode(value: &str) -> String {
	let mut out = String::with_capacity(value.len());
	for byte in value.bytes() {
		match byte {
			b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(byte as char),
			_ => out.push_str(&format!("%{byte:02X}")),
		}
	}
	out
}

pub async fn get(path: &str) -> Result<Value, RedditError> {
	PACER.wait().await;
	crate::client::json(path.to_owned(), false).await.map_err(map_client_err)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn encodes_reserved_characters() {
		assert_eq!(encode("rust lang"), "rust%20lang");
		assert_eq!(encode("a&b=c"), "a%26b%3Dc");
		assert_eq!(encode("plain-Text_1.0~"), "plain-Text_1.0~");
	}

	#[test]
	fn encodes_non_ascii_as_utf8_bytes() {
		assert_eq!(encode("caf\u{e9}"), "caf%C3%A9");
	}
}
