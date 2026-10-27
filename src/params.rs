//! Validation helpers shared by the REST API and the MCP tools.
//!
//! These functions validate and trim; they do not percent-encode. Callers must
//! encode query, cursor and comment values before interpolating them into an
//! upstream path.

use crate::errors::{ErrorKind, RedditError};

pub const MAX_QUERY_LEN: usize = 120;
pub const MAX_LIMIT: u32 = 100;
pub const DEFAULT_LIMIT: u32 = 25;

pub fn clamp_limit(limit: Option<u32>) -> u32 {
	limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT)
}

pub const DEFAULT_COMMENTS: u32 = 50;

/// A comment budget is a node count, so it gets the same cap as a listing
/// `limit`. The two surfaces disagreed before: REST clamped the upstream fetch
/// but then walked the raw `max_comments`, and MCP clamped neither.
pub fn clamp_comments(count: Option<u32>) -> u32 {
	count.unwrap_or(DEFAULT_COMMENTS).clamp(1, MAX_LIMIT)
}

/// Percent-encode a value for use in a Reddit query string. Everything outside the
/// unreserved set is encoded, so a caller cannot smuggle an `&` or `=` into the path.
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

pub fn validate_query(query: &str) -> Result<String, RedditError> {
	let trimmed = query.trim();
	if trimmed.is_empty() {
		return Err(RedditError::new(ErrorKind::BadInput, "query must not be empty"));
	}
	if trimmed.chars().count() > MAX_QUERY_LEN {
		return Err(RedditError::new(ErrorKind::BadInput, format!("query must be at most {MAX_QUERY_LEN} characters")));
	}
	Ok(trimmed.to_owned())
}

/// Reddit names (subreddit, user) are [A-Za-z0-9_-]{1,21}; anything else is either a
/// typo or an injection attempt into the proxied path.
pub fn sanitize_name(name: &str) -> Result<String, RedditError> {
	let trimmed = name.trim().trim_start_matches('/').trim_end_matches('/');
	let ok = !trimmed.is_empty() && trimmed.len() <= 21 && trimmed.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
	if ok {
		Ok(trimmed.to_owned())
	} else {
		Err(RedditError::new(ErrorKind::BadInput, format!("invalid Reddit name: {name:?}")))
	}
}

pub fn require_allowed<'a>(value: &str, allowed: &[&'a str]) -> Result<&'a str, RedditError> {
	allowed
		.iter()
		.copied()
		.find(|c| *c == value)
		.ok_or_else(|| RedditError::new(ErrorKind::BadInput, format!("{value:?} is not one of {}", allowed.join(", "))))
}

pub const MAX_POST_ID_LEN: usize = 16;

/// Post and comment ids are base36, so `_` and `-` are never valid even though
/// [`sanitize_name`] accepts them.
fn valid_id(id: &str) -> bool {
	!id.is_empty() && id.len() <= MAX_POST_ID_LEN && id.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// Without this check an id like `bad-id` reaches the GraphQL path parser, which
/// rejects it and reports the caller's typo as an upstream failure.
pub fn validate_post_id(id: &str) -> Result<String, RedditError> {
	let trimmed = id.trim();
	if valid_id(trimmed) {
		Ok(trimmed.to_owned())
	} else {
		Err(RedditError::new(ErrorKind::BadInput, format!("invalid post id: {id:?}")))
	}
}

/// Comment ids appear as `t1_<id>` in permalinks while the GraphQL layer prefixes
/// `t1_` itself, so the prefix is accepted and removed here. Callers get the bare id.
pub fn validate_comment_id(id: &str) -> Result<String, RedditError> {
	let trimmed = id.trim();
	let trimmed = trimmed.strip_prefix("t1_").unwrap_or(trimmed);
	if valid_id(trimmed) {
		Ok(trimmed.to_owned())
	} else {
		Err(RedditError::new(ErrorKind::BadInput, format!("invalid comment id: {id:?}")))
	}
}

pub const MAX_CURSOR_LEN: usize = 8192;

/// Cursors are opaque URL-safe base64 strings echoed back by Reddit. Validate the
/// alphabet so a caller cannot smuggle extra query parameters into the upstream
/// path.
///
/// Padding is rejected: the GraphQL layer decodes with the unpadded engine, so a
/// `=` here would pass validation and then fail upstream as a 502 instead of a 400.
pub fn validate_cursor(cursor: &str) -> Result<String, RedditError> {
	let trimmed = cursor.trim();
	let ok = !trimmed.is_empty() && trimmed.len() <= MAX_CURSOR_LEN && trimmed.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'));
	if ok {
		Ok(trimmed.to_owned())
	} else {
		Err(RedditError::new(ErrorKind::BadInput, "invalid cursor"))
	}
}

/// Extract `(subreddit, post_id)` from a post permalink.
///
/// The host is deliberately ignored: only the `/r/<sub>/comments/<id>` path is
/// used and both values are revalidated, so redlib-mirror URLs work too. Never
/// treat the returned pair as proof that the URL pointed at reddit.com.
pub fn parse_post_url(url: &str) -> Result<(String, String), RedditError> {
	let path = match url.split_once("://") {
		Some((_, rest)) => rest.find('/').map_or("", |i| &rest[i..]),
		None => url,
	};
	let mut parts = path.split('/').filter(|s| !s.is_empty());
	let r = parts.next().ok_or_else(|| RedditError::new(ErrorKind::BadInput, "URL has no path"))?;
	if r != "r" {
		return Err(RedditError::new(ErrorKind::BadInput, "URL is not a subreddit post"));
	}
	let sub = parts.next().ok_or_else(|| RedditError::new(ErrorKind::BadInput, "missing subreddit name"))?;
	let sub = sanitize_name(sub)?;
	let comments = parts.next().ok_or_else(|| RedditError::new(ErrorKind::BadInput, "missing comments segment"))?;
	if comments != "comments" {
		return Err(RedditError::new(ErrorKind::BadInput, "URL is not a post permalink"));
	}
	let id = parts.next().ok_or_else(|| RedditError::new(ErrorKind::BadInput, "missing post id"))?;
	let id = validate_post_id(id)?;
	Ok((sub, id))
}

#[cfg(test)]
mod tests {
	use crate::errors::ErrorKind;

	use super::*;

	#[test]
	fn clamps_limit_into_range() {
		assert_eq!(clamp_limit(None), 25);
		assert_eq!(clamp_limit(Some(0)), 1);
		assert_eq!(clamp_limit(Some(500)), 100);
		assert_eq!(clamp_limit(Some(42)), 42);
	}

	/// The comment budget is a node count walked by the renderer, so an unbounded
	/// `max_comments` is a caller-controlled amount of work.
	#[test]
	fn clamps_comment_budget_into_range() {
		assert_eq!(clamp_comments(None), DEFAULT_COMMENTS);
		assert_eq!(clamp_comments(Some(0)), 1);
		assert_eq!(clamp_comments(Some(u32::MAX)), MAX_LIMIT);
		assert_eq!(clamp_comments(Some(42)), 42);
	}

	#[test]
	fn rejects_empty_query() {
		assert_eq!(validate_query("   ").unwrap_err().kind, ErrorKind::BadInput);
	}

	#[test]
	fn rejects_overlong_query() {
		let long = "x".repeat(MAX_QUERY_LEN + 1);
		assert_eq!(validate_query(&long).unwrap_err().kind, ErrorKind::BadInput);
	}

	#[test]
	fn trims_query() {
		assert_eq!(validate_query("  rust lang  ").unwrap(), "rust lang");
	}

	#[test]
	fn sanitizes_subreddit_names() {
		assert_eq!(sanitize_name("rust").unwrap(), "rust");
		assert!(sanitize_name("bad/name").is_err());
		assert!(sanitize_name("").is_err());
		assert!(sanitize_name(&"a".repeat(22)).is_err());
		assert_eq!(sanitize_name("/rust/").unwrap(), "rust");
		assert!(sanitize_name("rúst").is_err());
	}

	#[test]
	fn parses_post_urls() {
		assert_eq!(
			parse_post_url("https://www.reddit.com/r/rust/comments/abc123/title/").unwrap(),
			("rust".into(), "abc123".into())
		);
		assert_eq!(
			parse_post_url("https://old.reddit.com/r/rust/comments/abc123/title/").unwrap(),
			("rust".into(), "abc123".into())
		);
		assert!(parse_post_url("https://example.com/nope").is_err());
		assert!(parse_post_url(&format!("https://www.reddit.com/r/rust/comments/{}/x/", "a".repeat(32))).is_err());
		assert_eq!(
			parse_post_url("https://redlib.example/r/rust/comments/abc123/title/").unwrap(),
			("rust".into(), "abc123".into())
		);
		assert_eq!(parse_post_url("https://np.reddit.com/r/rust/comments/abc123/").unwrap(), ("rust".into(), "abc123".into()));
		assert!(parse_post_url("https://www.reddit.com/r/rust/comments/abc-def/").is_err());
		assert!(parse_post_url("https://www.reddit.com/r/rust/comments/abc_123/").is_err());
		assert_eq!(
			parse_post_url("https://www.reddit.com/r/rust/comments/abc123/x/?utm_source=share").unwrap(),
			("rust".into(), "abc123".into())
		);
	}

	#[test]
	fn requires_allowlisted_value() {
		assert_eq!(require_allowed("new", &["hot", "new"]).unwrap(), "new");
		assert_eq!(require_allowed("chaotic", &["hot", "new"]).unwrap_err().kind, ErrorKind::BadInput);
	}

	#[test]
	fn accepts_url_safe_cursors() {
		assert_eq!(validate_cursor(" WTI5dGJXVnVkSFJ5 ").unwrap(), "WTI5dGJXVnVkSFJ5");
		assert_eq!(validate_cursor("a-_b").unwrap(), "a-_b");
	}

	#[test]
	fn rejects_cursors_that_could_inject_query_parameters() {
		for bad in ["", "   ", "abc&x=1", "abc?x=1", "abc/def", "abc%20def", "abc+def"] {
			assert_eq!(validate_cursor(bad).unwrap_err().kind, ErrorKind::BadInput, "accepted {bad:?}");
		}
		assert_eq!(validate_cursor(&"a".repeat(MAX_CURSOR_LEN + 1)).unwrap_err().kind, ErrorKind::BadInput);
		assert!(validate_cursor(&"a".repeat(MAX_CURSOR_LEN)).is_ok());
	}

	/// The decoder in `gql.rs` is `URL_SAFE_NO_PAD`, so padding can never round-trip.
	/// Accepting it here would turn a caller error into a 502 from the upstream layer.
	#[test]
	fn rejects_padded_cursors() {
		for bad in ["abc=", "abc==", "a-_b=="] {
			assert_eq!(validate_cursor(bad).unwrap_err().kind, ErrorKind::BadInput, "accepted {bad:?}");
		}
	}

	#[test]
	fn validates_post_ids() {
		assert_eq!(validate_post_id("abc123").unwrap(), "abc123");
		assert_eq!(validate_post_id(" 1xdp2ww ").unwrap(), "1xdp2ww");
		for bad in ["", "  ", "bad-id", "abc_123", "abc/123", "abc?x=1", "русский"] {
			assert_eq!(validate_post_id(bad).unwrap_err().kind, ErrorKind::BadInput, "accepted {bad:?}");
		}
		assert_eq!(validate_post_id(&"a".repeat(MAX_POST_ID_LEN + 1)).unwrap_err().kind, ErrorKind::BadInput);
		assert!(validate_post_id(&"a".repeat(MAX_POST_ID_LEN)).is_ok());
	}

	#[test]
	fn validates_comment_ids_and_strips_the_type_prefix() {
		assert_eq!(validate_comment_id("abc123").unwrap(), "abc123");
		assert_eq!(validate_comment_id(" t1_abc123 ").unwrap(), "abc123");
		for bad in ["", "t1_", "bad-id", "t1_bad-id", "t1_t1_abc", "abc/123"] {
			assert_eq!(validate_comment_id(bad).unwrap_err().kind, ErrorKind::BadInput, "accepted {bad:?}");
		}
	}
}
