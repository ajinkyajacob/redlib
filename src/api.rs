//! Read-only JSON API (deferred A3): `/api/v1/r/:sub`, `/api/v1/r/:sub/comments/:id`,
//! `/api/v1/search`, `/api/v1/u/:name`, `/api/v1/health`. JSON errors on every path,
//! 30 req/min token bucket, `REDLIB_API_ENABLED` kill switch.
#![allow(clippy::cmp_owned)]

use crate::{client::json, server::RequestExt, utils::param};
use hyper::{Body, Request, Response};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{LazyLock, Mutex};
use std::time::Instant;

pub const COMMENT_BODY_CAP: usize = 1200;
pub const POST_BODY_CAP: usize = 4000;
const TRUNCATION_MARK: &str = " …[truncated]";
pub const MAX_QUERY_LEN: usize = 120;
const DEFAULT_LIMIT: u32 = 25;
const MAX_LIMIT: u32 = 100;
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

pub fn clamp_limit(limit: Option<u32>) -> u32 {
	limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT)
}

fn validate_query(query: &str) -> Result<String, ApiError> {
	let trimmed = query.trim();
	if trimmed.is_empty() {
		return Err(ApiError::new(ErrorKind::BadInput, "query must not be empty"));
	}
	if trimmed.chars().count() > MAX_QUERY_LEN {
		return Err(ApiError::new(ErrorKind::BadInput, format!("query must be at most {MAX_QUERY_LEN} characters")));
	}
	Ok(trimmed.to_owned())
}

fn sanitize_name(name: &str) -> Result<String, ApiError> {
	let trimmed = name.trim().trim_start_matches('/').trim_end_matches('/');
	let valid = !trimmed.is_empty() && trimmed.len() <= 21 && trimmed.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
	if valid {
		Ok(trimmed.to_owned())
	} else {
		Err(ApiError::new(ErrorKind::BadInput, format!("invalid Reddit name: {name:?}")))
	}
}

fn require_allowed<'a>(value: &str, allowed: &[&'a str]) -> Result<&'a str, ApiError> {
	allowed.iter().copied().find(|c| *c == value).ok_or_else(|| ApiError::new(ErrorKind::BadInput, format!("{value:?} is not one of {}", allowed.join(", "))))
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ErrorKind {
	BadInput,
	RateLimited,
	TokenStale,
	Down,
	Unavailable,
	Parse,
}

impl ErrorKind {
	fn as_str(self) -> &'static str {
		match self {
			Self::BadInput => "BAD_INPUT",
			Self::RateLimited => "RATE_LIMITED",
			Self::TokenStale => "UPSTREAM_TOKEN_STALE",
			Self::Down => "REDLIB_DOWN",
			Self::Unavailable => "CONTENT_UNAVAILABLE",
			Self::Parse => "PARSE_ERROR",
		}
	}
}

#[derive(Debug)]
struct ApiError {
	kind: ErrorKind,
	message: String,
	status: u16,
}

impl ApiError {
	fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
		let status = match kind {
			ErrorKind::BadInput => 400,
			ErrorKind::RateLimited => 429,
			ErrorKind::Unavailable => 404,
			_ => 502,
		};
		Self { kind, message: message.into(), status }
	}

	fn to_json(&self) -> Value {
		json!({ "ok": false, "kind": self.kind.as_str(), "message": self.message })
	}
}

fn map_client_err(message: String) -> ApiError {
	let lower = message.to_lowercase();
	let kind = if lower.contains("rate limit") || lower.contains("too many requests") {
		ErrorKind::RateLimited
	} else if lower.contains("oauth") || lower.contains("unauthorized") || lower.contains("401") || lower.contains("token") {
		ErrorKind::TokenStale
	} else if lower.contains("removed") || lower.contains("404") {
		ErrorKind::Unavailable
	} else if lower.contains("error sending request") || lower.contains("connection") || lower.contains("timed out") {
		ErrorKind::Down
	} else {
		ErrorKind::Parse
	};
	ApiError::new(kind, message)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PostOut {
	id: String,
	title: String,
	subreddit: String,
	author: Option<String>,
	score: Option<i64>,
	comment_count: Option<i64>,
	created_utc: Option<f64>,
	permalink: String,
	url: Option<String>,
	flair: Option<String>,
	over_18: bool,
	is_self: bool,
	selftext: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CommentOut {
	id: String,
	author: Option<String>,
	score: Option<i64>,
	body: String,
	is_deleted: bool,
	more_id: Option<String>,
	replies: Vec<CommentOut>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ListingOut {
	ok: bool,
	kind: String,
	result_count: usize,
	cursor: Option<String>,
	posts: Vec<PostOut>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PostPage {
	ok: bool,
	kind: String,
	post: PostOut,
	comments_returned: usize,
	comments: Vec<CommentOut>,
}

fn str_field(data: &Value, key: &str) -> Option<String> {
	data.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn int_field(data: &Value, key: &str) -> Option<i64> {
	data.get(key).and_then(Value::as_i64)
}

/// Reddit sends `null` and `""` for "no flair"; collapse both to None.
fn flair(data: &Value) -> Option<String> {
	str_field(data, "link_flair_text").filter(|s| !s.trim().is_empty())
}

fn cap(text: String, limit: usize) -> String {
	if text.chars().count() > limit {
		let head: String = text.chars().take(limit).collect();
		format!("{head}{TRUNCATION_MARK}")
	} else {
		text
	}
}

fn normalize_post(data: &Value) -> Option<PostOut> {
	let id = str_field(data, "id")?;
	let title = str_field(data, "title")?;
	let permalink = str_field(data, "permalink").unwrap_or_default();
	let url = ["url", "url_overridden_by_dest"].iter().find_map(|k| str_field(data, k));
	let selftext = str_field(data, "selftext").filter(|s| !s.is_empty()).map(|s| cap(s, POST_BODY_CAP));
	Some(PostOut {
		id,
		title,
		subreddit: str_field(data, "subreddit").unwrap_or_default(),
		author: str_field(data, "author").filter(|a| a != "[deleted]"),
		score: int_field(data, "score"),
		comment_count: int_field(data, "num_comments"),
		created_utc: data.get("created_utc").and_then(Value::as_f64),
		permalink: if permalink.starts_with("http") { permalink } else { format!("https://www.reddit.com{permalink}") },
		url,
		flair: flair(data),
		over_18: data.get("over_18").and_then(Value::as_bool).unwrap_or(false),
		is_self: data.get("is_self").and_then(Value::as_bool).unwrap_or(false),
		selftext,
	})
}

fn listing_posts(listing: &Value) -> Vec<PostOut> {
	let Some(children) = listing.pointer("/data/children").and_then(Value::as_array) else { return Vec::new() };
	children.iter().filter(|c| c.get("kind").and_then(Value::as_str) == Some("t3")).filter_map(|c| normalize_post(&c["data"])).collect()
}

fn cursor(listing: &Value) -> Option<String> {
	str_field(&listing["data"], "after")
}

fn listing_out(listing: &Value) -> Value {
	let posts = listing_posts(listing);
	json!({
		"ok": true,
		"kind": "listing",
		"result_count": posts.len(),
		"cursor": cursor(listing),
		"posts": posts,
	})
}

fn normalize_comment(node: &Value) -> Option<CommentOut> {
	let data = node.get("data")?;
	let id = str_field(data, "id")?;
	let author = str_field(data, "author");
	let body = cap(str_field(data, "body").unwrap_or_default(), COMMENT_BODY_CAP);
	let is_deleted = author.as_deref().map(|a| a == "[deleted]").unwrap_or(true);
	let mut replies = Vec::new();
	let mut more_id = None;
	if let Some(children) = data.pointer("/replies/data/children").and_then(Value::as_array) {
		for child in children {
			match child.get("kind").and_then(Value::as_str) {
				Some("t1") => {
					if let Some(reply) = normalize_comment(child) {
						replies.push(reply);
					}
				}
				Some("more") => more_id = str_field(&child["data"], "id"),
				_ => {}
			}
		}
	}
	Some(CommentOut {
		id,
		author: if is_deleted { None } else { author },
		score: int_field(data, "score"),
		body,
		is_deleted,
		more_id,
		replies,
	})
}

/// Breadth-first budget so a single deep thread cannot starve the rest of the page.
fn prune_bfs(roots: Vec<CommentOut>, budget: usize) -> (Vec<CommentOut>, usize) {
	let mut kept: Vec<String> = Vec::new();
	let mut level = roots.clone();
	let mut count = 0;
	while !level.is_empty() && count < budget {
		let mut next = Vec::new();
		for node in level {
			if count >= budget {
				break;
			}
			kept.push(node.id.clone());
			count += 1;
			next.extend(node.replies);
		}
		level = next;
	}
	fn rebuild(node: &mut CommentOut, kept: &[String]) {
		let mut out = Vec::new();
		for mut reply in std::mem::take(&mut node.replies) {
			if kept.contains(&reply.id) {
				rebuild(&mut reply, kept);
				out.push(reply);
			}
		}
		node.replies = out;
	}
	let mut roots_out = Vec::new();
	for mut root in roots {
		if kept.contains(&root.id) {
			rebuild(&mut root, &kept);
			roots_out.push(root);
		}
	}
	(roots_out, count)
}

fn post_and_comments(page: &Value, budget: usize) -> Option<PostPage> {
	let post = page.get(0)?.pointer("/data/children")?.as_array()?.iter().find(|c| c.get("kind").and_then(Value::as_str) == Some("t3")).and_then(|c| normalize_post(&c["data"]))?;
	let raw = page.get(1).and_then(|c| c.pointer("/data/children")).and_then(Value::as_array).cloned().unwrap_or_default();
	let roots: Vec<CommentOut> = raw.iter().filter(|c| c.get("kind").and_then(Value::as_str) == Some("t1")).filter_map(normalize_comment).collect();
	let (comments, comments_returned) = prune_bfs(roots, budget.max(1));
	Some(PostPage { ok: true, kind: "post".into(), post, comments_returned, comments })
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

fn subreddit_path(sub: &str, sort: Option<&str>, time: Option<&str>, after: Option<&str>, limit: Option<u32>) -> Result<String, ApiError> {
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

fn user_path(name: &str, sort: Option<&str>, after: Option<&str>, limit: Option<u32>) -> Result<String, ApiError> {
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

fn search_path(query: &str, subreddit: Option<&str>, sort: Option<&str>, time: Option<&str>, limit: Option<u32>) -> Result<String, ApiError> {
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

fn post_path(id: &str, sort: Option<&str>, comment: Option<&str>, max_comments: Option<u32>) -> Result<String, ApiError> {
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

fn gate() -> Result<(), ApiError> {
	if !enabled() {
		return Err(ApiError::new(ErrorKind::Unavailable, "API is disabled; set REDLIB_API_ENABLED=1 to enable"));
	}
	if !rate_limit_take() {
		return Err(ApiError::new(ErrorKind::RateLimited, "rate limit exceeded: 30 requests/minute"));
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

fn err_response(e: ApiError) -> Result<Response<Body>, String> {
	json_response(e.status, e.to_json())
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
	err_response(ApiError::new(ErrorKind::Unavailable, "unknown API endpoint"))
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
		Ok(v) => json_response(200, listing_out(&v)),
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
			Some(page) => json_response(200, serde_json::to_value(page).unwrap_or_else(|_| json!({ "ok": false, "kind": "PARSE_ERROR" }))),
			None => err_response(ApiError::new(ErrorKind::Unavailable, format!("no post with id {id}"))),
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
		Ok(v) => json_response(200, listing_out(&v)),
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
		Ok(v) => json_response(200, listing_out(&v)),
		Err(e) => err_response(map_client_err(e)),
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use serde_json::json;
	use std::time::{Duration, Instant};

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
	}

	#[test]
	fn requires_allowlisted_value() {
		assert_eq!(require_allowed("new", &["hot", "new"]).unwrap(), "new");
		assert_eq!(require_allowed("chaotic", &["hot", "new"]).unwrap_err().kind, ErrorKind::BadInput);
	}

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
	fn error_kinds_map_to_status() {
		assert_eq!(ApiError::new(ErrorKind::BadInput, "x").status, 400);
		assert_eq!(ApiError::new(ErrorKind::RateLimited, "x").status, 429);
		assert_eq!(ApiError::new(ErrorKind::Unavailable, "x").status, 404);
		assert_eq!(ApiError::new(ErrorKind::Down, "x").status, 502);
	}

	#[test]
	fn error_json_envelope() {
		let body = ApiError::new(ErrorKind::BadInput, "nope").to_json();
		assert_eq!(body["ok"], false);
		assert_eq!(body["kind"], "BAD_INPUT");
		assert_eq!(body["message"], "nope");
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
		assert_eq!(map_client_err("error sending request for url: connection closed".into()).kind, ErrorKind::Down);
	}

	#[test]
	fn maps_unavailable_text() {
		assert_eq!(map_client_err("404: removed post".into()).kind, ErrorKind::Unavailable);
	}

	#[test]
	fn unknown_text_falls_back_to_parse() {
		assert_eq!(map_client_err("something odd".into()).kind, ErrorKind::Parse);
	}

	fn listing(children: serde_json::Value, after: Option<&str>) -> serde_json::Value {
		json!({"kind": "Listing", "data": {"after": after, "children": children}})
	}

	#[test]
	fn normalizes_post_and_cursor() {
		let v = listing(
			json!([{"kind": "t3", "data": {
				"id": "abc123", "title": "Hello", "subreddit": "rust", "author": "someone",
				"score": 42, "num_comments": 7, "created_utc": 1.7e9, "over_18": false,
				"is_self": true, "selftext": "body text", "link_flair_text": "news",
				"permalink": "/r/rust/comments/abc123/hello/"
			}}]),
			Some("t3_def456"),
		);
		let posts = listing_posts(&v);
		assert_eq!(posts.len(), 1);
		assert_eq!(posts[0].id, "abc123");
		assert_eq!(posts[0].title, "Hello");
		assert_eq!(posts[0].subreddit, "rust");
		assert_eq!(posts[0].score, Some(42));
		assert_eq!(posts[0].comment_count, Some(7));
		assert_eq!(posts[0].flair.as_deref(), Some("news"));
		assert!(!posts[0].over_18);
		assert!(posts[0].permalink.starts_with("https://www.reddit.com/"));
		assert_eq!(cursor(&v).as_deref(), Some("t3_def456"));
	}

	#[test]
	fn flair_null_and_empty_both_become_none() {
		let v = listing(json!([
			{"kind": "t3", "data": {"id": "a", "title": "t", "subreddit": "s", "link_flair_text": null}},
			{"kind": "t3", "data": {"id": "b", "title": "t", "subreddit": "s", "link_flair_text": ""}}
		]), None);
		let posts = listing_posts(&v);
		assert_eq!(posts[0].flair, None);
		assert_eq!(posts[1].flair, None);
	}

	#[test]
	fn skips_children_without_id_or_title() {
		let v = listing(json!([
			{"kind": "t3", "data": {"title": "no id", "subreddit": "s"}},
			{"kind": "t3", "data": {"id": "x", "subreddit": "s"}},
			{"kind": "t3", "data": {"id": "ok", "title": "fine", "subreddit": "s"}}
		]), None);
		assert_eq!(listing_posts(&v).len(), 1);
	}

	#[test]
	fn builds_nested_comment_tree_with_more_ids() {
		let comment = |id: &str, replies: serde_json::Value| {
			json!({"kind": "t1", "data": {"id": id, "author": "u", "score": 1, "body": "b", "replies": replies}})
		};
		let more = json!({"kind": "more", "data": {"id": "t1_more", "count": 7, "children": ["a", "b"]}});
		let c3 = comment("c3", json!({"kind": "Listing", "data": {"children": [more]}}));
		let replies = json!({"kind": "Listing", "data": {"children": [comment("c2", json!({"kind": "Listing", "data": {"children": [c3]}}))]}});
		let post = json!({"kind": "Listing", "data": {"children": [
			json!({"kind": "t3", "data": {"id": "abc123", "title": "Post", "subreddit": "rust"}})
		]}});
		let v = json!([
			post,
			json!({"kind": "Listing", "data": {"children": [comment("c1", replies)]}}),
			json!({"kind": "Listing", "data": {"children": []}})
		]);
		let page = post_and_comments(&v, 50).expect("page");
		assert_eq!(page.post.id, "abc123");
		assert_eq!(page.comments.len(), 1);
		assert_eq!(page.comments[0].replies[0].id, "c2");
		assert_eq!(page.comments[0].replies[0].replies[0].more_id.as_deref(), Some("t1_more"));
	}

	#[test]
	fn marks_deleted_comments() {
		let v = json!([
			{"kind": "Listing", "data": {"children": [
				json!({"kind": "t3", "data": {"id": "abc123", "title": "Post", "subreddit": "rust"}})
			]}},
			{"kind": "Listing", "data": {"children": [{"kind": "t1", "data": {"id": "c1", "author": "[deleted]", "body": "[deleted]"}}]}}
		]);
		let page = post_and_comments(&v, 50).expect("page");
		assert!(page.comments[0].is_deleted);
	}

	#[test]
	fn bfs_budget_keeps_shallow_comments_first() {
		let deep = |id: &str, depth: usize| -> serde_json::Value {
			let mut node = json!({"kind": "t1", "data": {"id": id, "author": "u", "body": "b"}});
			for level in 0..depth {
				let child = format!("{id}-{level}");
				let replies = json!({"kind": "Listing", "data": {"children": [
					json!({"kind": "t1", "data": {"id": child, "author": "u", "body": "b"}})
				]}});
				node = json!({"kind": "t1", "data": {"id": child, "author": "u", "body": "b", "replies": replies}});
			}
			node
		};
		let children: Vec<serde_json::Value> = (0..3).map(|i| deep(&format!("c{i}"), 3)).collect();
		let v = json!([
			{"kind": "Listing", "data": {"children": [
				json!({"kind": "t3", "data": {"id": "abc123", "title": "Post", "subreddit": "rust"}})
			]}},
			{"kind": "Listing", "data": {"children": children}}
		]);
		let page = post_and_comments(&v, 4).expect("page");
		assert_eq!(page.comments_returned, 4);
		assert!(page.comments.iter().all(|c| c.replies.len() <= 1));
	}

	#[test]
	fn body_caps_apply() {
		let long = "x".repeat(COMMENT_BODY_CAP + 50);
		let v = json!([
			{"kind": "Listing", "data": {"children": [
				json!({"kind": "t3", "data": {"id": "abc123", "title": "Post", "subreddit": "rust"}})
			]}},
			{"kind": "Listing", "data": {"children": [{"kind": "t1", "data": {"id": "c1", "body": long}}]}}
		]);
		let page = post_and_comments(&v, 50).expect("page");
		assert_eq!(page.comments[0].body.len(), COMMENT_BODY_CAP + " …[truncated]".len());
	}

	#[test]
	fn returns_none_when_first_listing_has_no_post() {
		let v = json!([{"kind": "Listing", "data": {"children": []}}, {"kind": "Listing", "data": {"children": []}}]);
		assert!(post_and_comments(&v, 50).is_none());
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