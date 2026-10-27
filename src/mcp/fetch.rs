use crate::errors::{map_client_err, RedditError};
use crate::mcp::pace::Pacer;
use serde_json::Value;

static PACER: std::sync::LazyLock<Pacer> = std::sync::LazyLock::new(crate::mcp::pace::from_env);

/// Percent-encode a value for use in a Reddit query string.
pub use crate::params::encode;

pub async fn get(path: &str) -> Result<Value, RedditError> {
	PACER.wait().await;
	crate::client::json(path.to_owned(), false).await.map_err(map_client_err)
}

/// Fetch a post with its comments. Goes through the GraphQL path because
/// Reddit answers the anonymous comments JSON endpoint with HTTP 403.
pub async fn post(path: &str) -> Result<Value, RedditError> {
	PACER.wait().await;
	crate::gql::thread(path.to_owned(), false).await.map_err(map_client_err)
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
