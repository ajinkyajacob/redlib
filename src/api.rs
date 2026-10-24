//! Read-only JSON API (deferred A3): `/api/v1/r/:sub`, `/api/v1/r/:sub/comments/:id`,
//! `/api/v1/search`, `/api/v1/u/:name`, `/api/v1/health`. JSON errors on every path,
//! 30 req/min token bucket, `REDLIB_API_ENABLED` kill switch.
#![allow(clippy::cmp_owned)]

use crate::errors::{map_client_err, ErrorKind, RedditError};
use crate::normalize::{listing_out, post_and_comments};
use crate::params::{clamp_limit, require_allowed, sanitize_name, validate_query};
use crate::{client::json, server::RequestExt, utils::param};
use hyper::{Body, Request, Response};
use serde_json::{json, Value};
use std::sync::{LazyLock, Mutex};
use std::time::Instant;

const RATE_CAPACITY: f64 = 30.0;
const RATE_WINDOW_SECS: f64 = 60.0;

const SORTS_LISTING: &[&str] = &["hot", "new", "top", "rising"];
const SORTS_SEARCH: &[&str] = &["relevance", "hot", "top", "new", "comments"];
const SORTS_COMMENTS: &[&str] = &["confidence", "top", "new", "controversial", "old", "qa"];
const SORTS_USER: &[&str] = &["new", "hot", "top"];
const TIMES: &[&str] = &["hour", "day", "week", "month", "year", "all"];

pub fn enabled() -> bool {
	match std::env::var("REDLIB_API_ENABLED") {
		Ok(v) => parse_enabled(&v),
		Err(_) => false,
	}
}

fn parse_enabled(value: &str) -> bool {
	matches!(value.trim().to_ascii_lowercase().as_str(), "1" | "on" | "true" | "yes")
}

struct Bucket {
	tokens: f64,
	last: Instant,
}

impl Bucket {
	fn new() -> Self {
		Self { tokens: RATE_CAPACITY, last: Instant::now() }
	}

	fn take_at(&mut self, now: Instant) -> bool {
		let elapsed = now.duration_since(self.last).as_secs_f64();
		self.tokens = (self.tokens + elapsed * RATE_CAPACITY / RATE_WINDOW_SECS).min(RATE_CAPACITY);
		self.last = now;
		if self.tokens >= 1.0 {
			self.tokens -= 1.0;
			true
		} else {
			false
		}
	}
}

static BUCKET: LazyLock<Mutex<Bucket>> = LazyLock::new(|| Mutex::new(Bucket::new()));

fn rate_limit_take() -> bool {
	BUCKET.lock().unwrap_or_else(|e| e.into_inner()).take_at(Instant::now())
}

fn encode(value: &str) -> String {
	let mut out = String::with_capacity(value.len());
	for byte in value.bytes() {
		match byte {
			b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(byte as char),
			_ => out.push_str(&format!("%{byte:02X}")),
		}
	}
	out
}

fn subreddit_path(sub: &str, sort: Option<&str>, time: Option<&str>, after: Option<&str>, limit: Option<u32>) -> Result<String, RedditError> {
	let sub = sanitize_name(sub)?;
	let sort = require_allowed(sort.unwrap_or("hot"), SORTS_LISTING)?;
	let mut parts: Vec<String> = Vec::new();
	if sort == "top" {
		if let Some(time) = time {
			parts.push(format!("t={}", require_allowed(time, TIMES)?));
		}
	}
	if let Some(after) = after {
		parts.push(format!("after={}", encode(after)));
	}
	parts.push(format!("limit={}", clamp_limit(limit)));
	parts.push("raw_json=1".into());
	Ok(format!("/r/{sub}/{sort}.json?{}", parts.join("&")))
}

fn user_path(name: &str, sort: Option<&str>, after: Option<&str>, limit: Option<u32>) -> Result<String, RedditError> {
	let name = sanitize_name(name)?;
	let sort = require_allowed(sort.unwrap_or("new"), SORTS_USER)?;
	let mut parts: Vec<String> = Vec::new();
	if let Some(after) = after {
		parts.push(format!("after={}", encode(after)));
	}
	parts.push(format!("limit={}", clamp_limit(limit)));
	parts.push("raw_json=1".into());
	Ok(format!("/user/{name}/{sort}.json?{}", parts.join("&")))
}

fn search_path(query: &str, subreddit: Option<&str>, sort: Option<&str>, time: Option<&str>, limit: Option<u32>) -> Result<String, RedditError> {
	let query = validate_query(query)?;
	let mut parts: Vec<String> = vec![format!("q={}", encode(&query))];
	if let Some(sort) = sort {
		parts.push(format!("sort={}", require_allowed(sort, SORTS_SEARCH)?));
	}
	if let Some(time) = time {
		parts.push(format!("t={}", require_allowed(time, TIMES)?));
	}
	parts.push(format!("limit={}", clamp_limit(limit)));
	parts.push("raw_json=1".into());
	match subreddit {
		Some(sub) => Ok(format!("/r/{}/search.json?restrict_sr=on&{}", sanitize_name(sub)?, parts.join("&"))),
		None => Ok(format!("/search.json?{}", parts.join("&"))),
	}
}

fn post_path(id: &str, sort: Option<&str>, comment: Option<&str>, max_comments: Option<u32>) -> Result<String, RedditError> {
	let id = sanitize_name(id)?;
	let mut parts: Vec<String> = vec![format!("limit={}", clamp_limit(max_comments))];
	if let Some(sort) = sort {
		parts.push(format!("sort={}", require_allowed(sort, SORTS_COMMENTS)?));
	}
	if let Some(comment) = comment {
		parts.push(format!("comment={}", encode(&sanitize_name(comment)?)));
	}
	Ok(format!("/comments/{id}.json?depth=10&{}", parts.join("&")))
}

fn query(req: &Request<Body>, key: &str) -> Option<String> {
	param(req.uri().path_and_query().map_or("/", |pq| pq.as_str()), key)
}

fn param_u32(req: &Request<Body>, key: &str) -> Option<u32> {
	query(req, key).and_then(|v| v.parse().ok())
}

fn gate() -> Result<(), RedditError> {
	if !enabled() {
		return Err(RedditError::new(ErrorKind::Unavailable, "API is disabled; set REDLIB_API_ENABLED=1 to enable"));
	}
	if !rate_limit_take() {
		return Err(RedditError::new(ErrorKind::RateLimited, "rate limit exceeded: 30 requests/minute"));
	}
	Ok(())
}

fn json_response(status: u16, body: Value) -> Result<Response<Body>, String> {
	Ok(Response::builder()
		.status(status)
		.header("content-type", "application/json")
		.header("Cache-Control", "no-store")
		.body(Body::from(body.to_string()))
		.unwrap_or_default())
}

fn err_response(e: RedditError) -> Result<Response<Body>, String> {
	json_response(e.status(), e.to_json())
}

pub async fn health(_req: Request<Body>) -> Result<Response<Body>, String> {
	json_response(
		200,
		json!({
			"ok": true,
			"kind": "health",
			"version": env!("CARGO_PKG_VERSION"),
			"rate_limit": { "capacity": 30, "window_seconds": 60 },
		}),
	)
}

pub async fn not_found() -> Result<Response<Body>, String> {
	err_response(RedditError::new(ErrorKind::Unavailable, "unknown API endpoint"))
}

pub async fn subreddit(req: Request<Body>) -> Result<Response<Body>, String> {
	if let Err(e) = gate() {
		return err_response(e);
	}
	let sub = req.param("sub").unwrap_or_default();
	let path = match subreddit_path(&sub, query(&req, "sort").as_deref(), query(&req, "t").as_deref(), query(&req, "after").as_deref(), param_u32(&req, "limit")) {
		Ok(p) => p,
		Err(e) => return err_response(e),
	};
	match json(path, false).await {
		Ok(v) => json_response(200, serde_json::to_value(listing_out(&v)).map_err(|e| e.to_string())?),
		Err(e) => err_response(map_client_err(e)),
	}
}

pub async fn comments(req: Request<Body>) -> Result<Response<Body>, String> {
	if let Err(e) = gate() {
		return err_response(e);
	}
	let id = req.param("id").unwrap_or_default();
	let max = param_u32(&req, "max_comments");
	let path = match post_path(&id, query(&req, "sort").as_deref(), query(&req, "comment").as_deref(), max) {
		Ok(p) => p,
		Err(e) => return err_response(e),
	};
	match json(path, false).await {
		Ok(v) => match post_and_comments(&v, max.unwrap_or(50).max(1) as usize) {
			Some(page) => json_response(200, serde_json::to_value(page).map_err(|e| e.to_string())?),
			None => err_response(RedditError::new(ErrorKind::Unavailable, format!("no post with id {id}"))),
		},
		Err(e) => err_response(map_client_err(e)),
	}
}

pub async fn search(req: Request<Body>) -> Result<Response<Body>, String> {
	if let Err(e) = gate() {
		return err_response(e);
	}
	let path = match search_path(&query(&req, "q").unwrap_or_default(), query(&req, "subreddit").as_deref(), query(&req, "sort").as_deref(), query(&req, "t").as_deref(), param_u32(&req, "limit")) {
		Ok(p) => p,
		Err(e) => return err_response(e),
	};
	match json(path, false).await {
		Ok(v) => json_response(200, serde_json::to_value(listing_out(&v)).map_err(|e| e.to_string())?),
		Err(e) => err_response(map_client_err(e)),
	}
}

pub async fn user(req: Request<Body>) -> Result<Response<Body>, String> {
	if let Err(e) = gate() {
		return err_response(e);
	}
	let name = req.param("name").unwrap_or_default();
	let path = match user_path(&name, query(&req, "sort").as_deref(), query(&req, "after").as_deref(), param_u32(&req, "limit")) {
		Ok(p) => p,
		Err(e) => return err_response(e),
	};
	match json(path, false).await {
		Ok(v) => json_response(200, serde_json::to_value(listing_out(&v)).map_err(|e| e.to_string())?),
		Err(e) => err_response(map_client_err(e)),
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::time::{Duration, Instant};

	#[test]
	fn parses_enabled_flag() {
		assert!(parse_enabled("1"));
		assert!(parse_enabled("on"));
		assert!(parse_enabled("TRUE"));
		assert!(!parse_enabled(""));
		assert!(!parse_enabled("0"));
	}

	#[test]
	fn rate_bucket_allows_capacity_then_blocks() {
		let mut bucket = Bucket::new();
		let start = Instant::now();
		for _ in 0..30 {
			assert!(bucket.take_at(start));
		}
		assert!(!bucket.take_at(start));
	}

	#[test]
	fn rate_bucket_refills_over_window() {
		let mut bucket = Bucket::new();
		let start = Instant::now();
		for _ in 0..30 {
			bucket.take_at(start);
		}
		assert!(!bucket.take_at(start));
		assert!(bucket.take_at(start + Duration::from_secs(60)));
	}

	#[test]
	fn subreddit_path_defaults_to_hot() {
		assert_eq!(subreddit_path("rust", None, None, None, None).unwrap(), "/r/rust/hot.json?limit=25&raw_json=1");
	}

	#[test]
	fn subreddit_path_honors_top_time_and_limit() {
		let p = subreddit_path("rust", Some("top"), Some("week"), None, Some(3)).unwrap();
		assert!(p.starts_with("/r/rust/top.json"), "{p}");
		assert!(p.contains("t=week"), "{p}");
		assert!(p.contains("&limit=3"), "{p}");
	}

	#[test]
	fn subreddit_path_rejects_bad_input() {
		assert_eq!(subreddit_path("bad/name", None, None, None, None).unwrap_err().kind, ErrorKind::BadInput);
		assert_eq!(subreddit_path("rust", Some("chaotic"), None, None, None).unwrap_err().kind, ErrorKind::BadInput);
	}

	#[test]
	fn user_path_defaults_to_new() {
		assert_eq!(user_path("someone", None, None, None).unwrap(), "/user/someone/new.json?limit=25&raw_json=1");
	}

	#[test]
	fn search_path_encodes_query_and_restricts_to_subreddit() {
		let global = search_path("rust lang", None, None, None, None).unwrap();
		assert!(global.starts_with("/search.json"), "{global}");
		assert!(global.contains("q=rust%20lang"), "{global}");
		assert!(global.contains("&limit=25"), "{global}");
		let scoped = search_path("rust", Some("rust"), None, None, None).unwrap();
		assert!(scoped.starts_with("/r/rust/search.json?restrict_sr=on"), "{scoped}");
	}

	#[test]
	fn post_path_includes_sort_and_comment() {
		let p = post_path("abc123", Some("top"), Some("t1_more"), Some(50)).unwrap();
		assert!(p.starts_with("/comments/abc123.json?depth=10"), "{p}");
		assert!(p.contains("&sort=top"), "{p}");
		assert!(p.contains("&comment=t1_more"), "{p}");
		assert!(p.contains("&limit=50"), "{p}");
	}
}
