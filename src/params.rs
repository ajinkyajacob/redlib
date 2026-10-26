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

pub const MAX_CURSOR_LEN: usize = 8192;

/// Cursors are opaque URL-safe base64 strings echoed back by Reddit. Validate the
/// alphabet so a caller cannot smuggle extra query parameters into the upstream
/// path.
pub fn validate_cursor(cursor: &str) -> Result<String, RedditError> {
	let trimmed = cursor.trim();
	let ok = !trimmed.is_empty() && trimmed.len() <= MAX_CURSOR_LEN && trimmed.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'='));
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
	if id.is_empty() || id.len() > 16 || !id.chars().all(|c| c.is_ascii_alphanumeric()) {
		return Err(RedditError::new(ErrorKind::BadInput, format!("invalid post id: {id:?}")));
	}
	Ok((sub, id.to_owned()))
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
		assert_eq!(validate_cursor("a-_b==").unwrap(), "a-_b==");
	}

	#[test]
	fn rejects_cursors_that_could_inject_query_parameters() {
		for bad in ["", "   ", "abc&x=1", "abc?x=1", "abc/def", "abc%20def", "abc+def"] {
			assert_eq!(validate_cursor(bad).unwrap_err().kind, ErrorKind::BadInput, "accepted {bad:?}");
		}
		assert_eq!(validate_cursor(&"a".repeat(MAX_CURSOR_LEN + 1)).unwrap_err().kind, ErrorKind::BadInput);
		assert!(validate_cursor(&"a".repeat(MAX_CURSOR_LEN)).is_ok());
	}
}
