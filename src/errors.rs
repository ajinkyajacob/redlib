//! Shared error taxonomy for the REST API, the MCP tools and the stdio/HTTP
//! MCP transports.
//!
//! `Unauthorized` means an inbound request failed bearer auth (401).
//! `TokenStale` means Reddit rejected *our* OAuth token (502) — never use it for
//! inbound auth failures.

use rmcp::model::ErrorData;
use serde_json::{json, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
	BadInput,
	Unauthorized,
	RateLimited,
	TokenStale,
	Down,
	Unavailable,
	Parse,
}

impl ErrorKind {
	pub fn as_str(self) -> &'static str {
		match self {
			Self::BadInput => "BAD_INPUT",
			Self::Unauthorized => "UNAUTHORIZED",
			Self::RateLimited => "RATE_LIMITED",
			Self::TokenStale => "UPSTREAM_TOKEN_STALE",
			Self::Down => "REDLIB_DOWN",
			Self::Unavailable => "CONTENT_UNAVAILABLE",
			Self::Parse => "PARSE_ERROR",
		}
	}

	pub fn hint(self) -> &'static str {
		match self {
			Self::TokenStale => "Reddit rejected the OAuth token. Restart redlib to obtain a fresh one.",
			Self::RateLimited => "Back off a few seconds before retrying.",
			_ => "",
		}
	}
}

#[derive(Debug)]
pub struct RedditError {
	pub kind: ErrorKind,
	pub message: String,
}

impl RedditError {
	pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
		Self { kind, message: message.into() }
	}

	pub fn status(&self) -> u16 {
		match self.kind {
			ErrorKind::BadInput => 400,
			ErrorKind::Unauthorized => 401,
			ErrorKind::RateLimited => 429,
			ErrorKind::Unavailable => 404,
			ErrorKind::TokenStale | ErrorKind::Down | ErrorKind::Parse => 502,
		}
	}

	pub fn to_json(&self) -> Value {
		json!({ "ok": false, "kind": self.kind.as_str(), "message": self.message })
	}

	pub fn to_error_data(&self) -> ErrorData {
		let hint = self.kind.hint();
		let message = if hint.is_empty() {
			format!("{}: {}", self.kind.as_str(), self.message)
		} else {
			format!("{}: {} — {hint}", self.kind.as_str(), self.message)
		};
		match self.kind {
			ErrorKind::BadInput => ErrorData::invalid_params(message, None),
			_ => ErrorData::internal_error(message, None),
		}
	}
}

/// Classifies the strings produced by `client::json` and `gql::thread`. The latter
/// reports caller mistakes and missing posts as prose, so those phrases are
/// recognised here rather than being lumped into `Parse` (502).
pub fn map_client_err(message: String) -> RedditError {
	// client::json appends " | {path}" to every error; scanning the path makes a
	// post id or a subreddit like /r/401k look like a 401/404 status.
	let scan = message.rsplit_once(" | ").map_or(message.as_str(), |(head, _)| head);
	let lower = scan.to_lowercase();
	let kind = if lower.contains("rate limit") || lower.contains("too many requests") {
		ErrorKind::RateLimited
	} else if lower.contains("oauth") || lower.contains("unauthorized") || lower.contains("401") || lower.contains("token") {
		ErrorKind::TokenStale
	} else if lower.contains("invalid post path") || lower.contains("invalid post id") || lower.contains("invalid cursor") || lower.contains("invalid comments cursor") {
		ErrorKind::BadInput
	} else if lower.contains("not found") || lower.contains("no post") || lower.contains("removed") || lower.contains("404") {
		ErrorKind::Unavailable
	} else if lower.contains("error sending request") || lower.contains("connection") || lower.contains("dns") || lower.contains("timed out") || lower.contains("having issues")
	{
		ErrorKind::Down
	} else {
		ErrorKind::Parse
	};
	RedditError::new(kind, message)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn error_kinds_map_to_status() {
		assert_eq!(RedditError::new(ErrorKind::BadInput, "x").status(), 400);
		assert_eq!(RedditError::new(ErrorKind::Unauthorized, "x").status(), 401);
		assert_eq!(RedditError::new(ErrorKind::RateLimited, "x").status(), 429);
		assert_eq!(RedditError::new(ErrorKind::Unavailable, "x").status(), 404);
		assert_eq!(RedditError::new(ErrorKind::Down, "x").status(), 502);
		assert_eq!(RedditError::new(ErrorKind::Parse, "x").status(), 502);
	}

	#[test]
	fn error_json_envelope() {
		let body = RedditError::new(ErrorKind::BadInput, "nope").to_json();
		assert_eq!(body["ok"], false);
		assert_eq!(body["kind"], "BAD_INPUT");
		assert_eq!(body["message"], "nope");
	}

	#[test]
	fn unauthorized_kind_string() {
		assert_eq!(ErrorKind::Unauthorized.as_str(), "UNAUTHORIZED");
	}

	#[test]
	fn bad_input_maps_to_invalid_params() {
		assert_eq!(RedditError::new(ErrorKind::BadInput, "query must not be empty").to_error_data().code.0, -32602);
	}

	#[test]
	fn internal_kinds_map_to_internal_error() {
		assert_eq!(RedditError::new(ErrorKind::Down, "no route to host").to_error_data().code.0, -32603);
	}

	#[test]
	fn error_data_includes_the_hint_when_one_exists() {
		let e = RedditError::new(ErrorKind::TokenStale, "boom");
		assert_eq!(
			e.to_error_data().message,
			"UPSTREAM_TOKEN_STALE: boom — Reddit rejected the OAuth token. Restart redlib to obtain a fresh one."
		);
	}

	#[test]
	fn error_data_omits_the_separator_without_a_hint() {
		let e = RedditError::new(ErrorKind::Down, "boom");
		assert_eq!(e.to_error_data().message, "REDLIB_DOWN: boom");
	}

	#[test]
	fn maps_dns_text_to_down() {
		assert_eq!(map_client_err("dns error: failed to lookup address".into()).kind, ErrorKind::Down);
	}

	#[test]
	fn ignores_status_digits_in_the_request_path() {
		// /r/401k contains "401" but the failure is a parse error, not a stale token
		assert_eq!(map_client_err("Failed to parse page JSON data: bad | /r/401k/hot.json".into()).kind, ErrorKind::Parse);
		// the path's "404" would win the Unavailable clause if the path were scanned;
		// the head is a transport failure, so it must stay Down
		assert_eq!(
			map_client_err("error sending request for url: connection closed | /r/rust/comments/404abc/x".into()).kind,
			ErrorKind::Down
		);
	}

	#[test]
	fn maps_rate_limit_text() {
		assert_eq!(map_client_err("Rate limit exceeded, please retry".into()).kind, ErrorKind::RateLimited);
	}

	#[test]
	fn maps_token_stale_text() {
		assert_eq!(map_client_err("Unauthorized: oauth token expired".into()).kind, ErrorKind::TokenStale);
	}

	#[test]
	fn maps_transport_text() {
		assert_eq!(
			map_client_err("error sending request for url (https://www.reddit.com/): connection closed".into()).kind,
			ErrorKind::Down
		);
	}

	#[test]
	fn maps_unavailable_text() {
		assert_eq!(map_client_err("404: removed post".into()).kind, ErrorKind::Unavailable);
	}

	#[test]
	fn unknown_text_falls_back_to_parse() {
		assert_eq!(map_client_err("something odd".into()).kind, ErrorKind::Parse);
	}

	#[test]
	fn maps_missing_post_to_unavailable() {
		assert_eq!(map_client_err("Post not found".into()).kind, ErrorKind::Unavailable);
		assert_eq!(map_client_err("no post with id zzz".into()).kind, ErrorKind::Unavailable);
	}

	#[test]
	fn maps_caller_mistakes_to_bad_input() {
		// gql::thread reports these as prose; they are 400s, not upstream failures.
		assert_eq!(map_client_err("Invalid post path".into()).kind, ErrorKind::BadInput);
		assert_eq!(map_client_err("Invalid comments cursor".into()).kind, ErrorKind::BadInput);
	}

	#[test]
	fn maps_upstream_forbidden_to_rate_limited() {
		let e = map_client_err("Too many requests: Reddit returned 403 Forbidden: expected value at line 1 column 1 | /r/rust/hot.json".into());
		assert_eq!(e.kind, ErrorKind::RateLimited);
	}

	#[test]
	fn maps_upstream_429_to_rate_limited() {
		let e = map_client_err("Too many requests: Reddit returned 429 Too Many Requests | /r/rust/hot.json".into());
		assert_eq!(e.kind, ErrorKind::RateLimited);
	}

	#[test]
	fn maps_outage_text_to_down() {
		let e = map_client_err("Reddit is having issues, check if there's an outage: connection reset | /r/rust/hot.json".into());
		assert_eq!(e.kind, ErrorKind::Down);
	}
}
