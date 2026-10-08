use rmcp::schemars;
use rmcp::schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const COMMENT_BODY_CAP: usize = 1200;
pub const POST_BODY_CAP: usize = 4000;
const TRUNCATION_MARK: &str = " …[truncated]";

/// Reddit's JSON carries no OP or moderator marker for comments — those exist only in
/// redlib's rendered HTML — so they are deliberately absent here rather than faked.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PostOut {
	pub id: String,
	pub title: String,
	pub subreddit: String,
	pub author: Option<String>,
	pub score: Option<i64>,
	pub comment_count: Option<i64>,
	pub created_utc: Option<f64>,
	pub permalink: String,
	pub url: Option<String>,
	pub flair: Option<String>,
	pub over_18: bool,
	pub is_self: bool,
	pub selftext: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CommentOut {
	pub id: String,
	pub author: Option<String>,
	pub score: Option<i64>,
	pub body: String,
	pub is_deleted: bool,
	pub more_id: Option<String>,
	pub replies: Vec<CommentOut>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ListingOut {
	pub ok: bool,
	pub kind: String,
	pub result_count: usize,
	pub cursor: Option<String>,
	pub posts: Vec<PostOut>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PostPage {
	pub ok: bool,
	pub kind: String,
	pub post: PostOut,
	pub comments_returned: usize,
	pub comments: Vec<CommentOut>,
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

pub fn normalize_post(data: &Value) -> Option<PostOut> {
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

pub fn listing_posts(listing: &Value) -> Vec<PostOut> {
	let Some(children) = listing.pointer("/data/children").and_then(Value::as_array) else { return Vec::new() };
	children.iter().filter(|c| c.get("kind").and_then(Value::as_str) == Some("t3")).filter_map(|c| normalize_post(&c["data"])).collect()
}

pub fn cursor(listing: &Value) -> Option<String> {
	str_field(&listing["data"], "after")
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

pub fn post_and_comments(page: &Value, budget: usize) -> Option<PostPage> {
	let post = page.get(0)?.pointer("/data/children")?.as_array()?.iter().find(|c| c.get("kind").and_then(Value::as_str) == Some("t3")).and_then(|c| normalize_post(&c["data"]))?;
	let raw = page.get(1).and_then(|c| c.pointer("/data/children")).and_then(Value::as_array).cloned().unwrap_or_default();
	let roots: Vec<CommentOut> = raw.iter().filter(|c| c.get("kind").and_then(Value::as_str) == Some("t1")).filter_map(normalize_comment).collect();
	let (comments, comments_returned) = prune_bfs(roots, budget.max(1));
	Some(PostPage { ok: true, kind: "post".into(), post, comments_returned, comments })
}

#[cfg(test)]
mod tests {
	use super::*;
	use serde_json::json;

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
}
