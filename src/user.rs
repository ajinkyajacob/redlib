#![allow(clippy::cmp_owned)]
use crate::client::json;
use crate::server::RequestExt;
use crate::utils::{error, filter_posts, format_url, get_filters, nsfw_landing, param, setting, template, Post, Preferences, User};
use crate::{config, utils};
use askama::Template;
use chrono::DateTime;
use htmlescape::decode_html;
use hyper::{Body, Request, Response};
use serde_json::Value;
use time::{macros::format_description, Date, OffsetDateTime};

// STRUCTS
#[derive(Template)]
#[template(path = "user.html")]
struct UserTemplate {
	user: User,
	posts: Vec<Post>,
	sort: (String, String),
	ends: (String, String),
	/// "overview", "comments", or "submitted"
	listing: String,
	prefs: Preferences,
	url: String,
	redirect_url: String,
	/// Whether the user themself is filtered.
	is_filtered: bool,
	/// Whether all fetched posts are filtered (to differentiate between no posts fetched in the first place,
	/// and all fetched posts being filtered).
	all_posts_filtered: bool,
	/// Whether all posts were hidden because they are NSFW (and user has disabled show NSFW)
	all_posts_hidden_nsfw: bool,
	no_posts: bool,
}

// FUNCTIONS
pub async fn profile(req: Request<Body>) -> Result<Response<Body>, String> {
	let listing = req.param("listing").unwrap_or_else(|| "overview".to_string());

	// Build the Reddit JSON API path
	let path = format!(
		"/user/{}/{listing}.json?{}&raw_json=1",
		req.param("name").unwrap_or_else(|| "reddit".to_string()),
		req.uri().query().unwrap_or_default(),
	);
	let url = String::from(req.uri().path_and_query().map_or("", |val| val.as_str()));
	let redirect_url = url[1..].replace('?', "%3F").replace('&', "%26");

	// Retrieve other variables from Redlib request
	let sort = param(&path, "sort").unwrap_or_default();
	let username = req.param("name").unwrap_or_default();

	// Retrieve info from user about page.
	let user = user(&username).await.unwrap_or_default();

	let req_url = req.uri().to_string();
	// Return landing page if this post if this Reddit deems this user NSFW,
	// but we have also disabled the display of NSFW content or if the instance
	// is SFW-only.
	if user.nsfw && utils::should_be_nsfw_gated(&req, &req_url) {
		return Ok(nsfw_landing(req, req_url).await.unwrap_or_default());
	}

	let filters = get_filters(&req);
	if filters.contains(&["u_", &username].concat()) {
		Ok(template(&UserTemplate {
			user,
			posts: Vec::new(),
			sort: (sort, param(&path, "t").unwrap_or_default()),
			ends: (param(&path, "after").unwrap_or_default(), String::new()),
			listing,
			prefs: Preferences::new(&req),
			url,
			redirect_url,
			is_filtered: true,
			all_posts_filtered: false,
			all_posts_hidden_nsfw: false,
			no_posts: false,
		}))
	} else {
		// Request user posts/comments from Reddit
		match Post::fetch(&path, false).await {
			Ok((mut posts, after)) => {
				let (_, all_posts_filtered) = filter_posts(&mut posts, &filters);
				let no_posts = posts.is_empty();
				let all_posts_hidden_nsfw = !no_posts && (posts.iter().all(|p| p.flags.nsfw) && setting(&req, "show_nsfw") != "on");
				Ok(template(&UserTemplate {
					user,
					posts,
					sort: (sort, param(&path, "t").unwrap_or_default()),
					ends: (param(&path, "after").unwrap_or_default(), after),
					listing,
					prefs: Preferences::new(&req),
					url,
					redirect_url,
					is_filtered: false,
					all_posts_filtered,
					all_posts_hidden_nsfw,
					no_posts,
				}))
			}
			// If there is an error show error page
			Err(msg) => error(req, &msg).await,
		}
	}
}

// USER
async fn user(name: &str) -> Result<User, String> {
	// The anonymous `/user/{name}/about.json` endpoint answers 403, so read the
	// profile from the GraphQL surface the Android app uses, and keep the JSON
	// endpoint as a fallback for the day the GraphQL one goes away.
	match crate::gql::user(name.to_owned()).await {
		Ok(Some(info)) => Ok(user_from_graphql(&info, name)),
		Ok(None) => Err(format!("no Redditor named {name}")),
		Err(e) => {
			log::warn!("Falling back to the user JSON endpoint: {e}");
			user_json(name).await
		}
	}
}

async fn user_json(name: &str) -> Result<User, String> {
	// Build the Reddit JSON API path
	let path: String = format!("/user/{name}/about.json?raw_json=1");

	// Send a request to the url
	json(path, false).await.map(|res| {
		// Grab creation date as unix timestamp
		let created_unix = res["data"]["created"].as_f64().unwrap_or(0.0).round() as i64;
		let created = OffsetDateTime::from_unix_timestamp(created_unix).unwrap_or(OffsetDateTime::UNIX_EPOCH);

		// Closure used to parse JSON from Reddit APIs
		let about = |item| res["data"]["subreddit"][item].as_str().unwrap_or_default().to_string();

		// Parse the JSON output into a User struct
		User {
			name: res["data"]["name"].as_str().unwrap_or(name).to_owned(),
			title: about("title"),
			icon: format_url(&about("icon_img")),
			karma: res["data"]["total_karma"].as_i64().unwrap_or(0),
			created: created.format(format_description!("[month repr:short] [day] '[year repr:last_two]")).unwrap_or_default(),
			banner: about("banner_img"),
			description: about("public_description"),
			nsfw: res["data"]["subreddit"]["over_18"].as_bool().unwrap_or_default(),
		}
	})
}

/// Map the `UserProfile` GraphQL object onto the fields the templates use.
fn user_from_graphql(info: &Value, fallback_name: &str) -> User {
	let profile = &info["profile"];
	let styles = &profile["styles"];
	let text = |value: &Value| value.as_str().unwrap_or_default().to_string();
	let name = text(&info["name"]);

	User {
		name: if name.is_empty() { fallback_name.to_owned() } else { name },
		title: text(&profile["title"]),
		// legacyIcon is the 256px version of the avatar; icon is 64px.
		icon: format_url(styles["legacyIcon"]["url"].as_str().filter(|url| !url.is_empty()).or_else(|| styles["icon"].as_str()).unwrap_or_default()),
		karma: info["karma"]["total"].as_i64().unwrap_or_default(),
		created: graphql_date(profile["createdAt"].as_str()),
		banner: text(&profile["styles"]["profileBanner"]),
		description: text(&profile["publicDescriptionText"]),
		nsfw: profile["isNsfw"].as_bool().unwrap_or_default(),
	}
}

fn graphql_date(created: Option<&str>) -> String {
	// Reddit sends either RFC 3339 (`2005-06-06T09:30:00+05:30`) or the compact
	// form with microseconds (`2005-06-06T04:00:00.000000+0000`). Only the date
	// is displayed, so parse that and skip the offset entirely.
	created
		.and_then(|created| created.get(..10))
		.and_then(|date| Date::parse(date, format_description!("[year]-[month]-[day]")).ok())
		.and_then(|date| date.format(format_description!("[month repr:short] [day] '[year repr:last_two]")).ok())
		.unwrap_or_default()
}

pub async fn rss(req: Request<Body>) -> Result<Response<Body>, String> {
	if config::get_setting("REDLIB_ENABLE_RSS").is_none() {
		return Ok(error(req, "RSS is disabled on this instance.").await.unwrap_or_default());
	}
	use crate::utils::rewrite_urls;
	use hyper::header::CONTENT_TYPE;
	use rss::{ChannelBuilder, Item};

	// Get user
	let user_str = req.param("name").unwrap_or_default();

	let listing = req.param("listing").unwrap_or_else(|| "overview".to_string());

	// Get path
	let path = format!("/user/{user_str}/{listing}.json?{}&raw_json=1", req.uri().query().unwrap_or_default(),);

	// Get user
	let user_obj = user(&user_str).await.unwrap_or_default();

	// Get posts
	let (posts, _) = Post::fetch(&path, false).await?;

	// Build the RSS feed
	let channel = ChannelBuilder::default()
		.title(user_str)
		.description(user_obj.description)
		.items(
			posts
				.into_iter()
				.map(|post| Item {
					title: Some(post.title.to_string()),
					link: Some(format_url(&utils::get_post_url(&post))),
					author: Some(post.author.name),
					pub_date: Some(DateTime::from_timestamp(post.created_ts as i64, 0).unwrap_or_default().to_rfc2822()),
					content: Some(rewrite_urls(&decode_html(&post.body).unwrap_or_else(|_| post.body.clone()))),
					..Default::default()
				})
				.collect::<Vec<_>>(),
		)
		.build();

	// Serialize the feed to RSS
	let body = channel.to_string().into_bytes();

	// Create the HTTP response
	let mut res = Response::new(Body::from(body));
	res.headers_mut().insert(CONTENT_TYPE, hyper::header::HeaderValue::from_static("application/rss+xml"));

	Ok(res)
}

#[cfg(test)]
mod tests {
	use super::*;
	use serde_json::json;

	#[tokio::test(flavor = "multi_thread")]
	async fn test_fetching_user() {
		let user = user("spez").await;
		assert!(user.is_ok());
		assert!(user.unwrap().karma > 100);
	}

	#[test]
	fn maps_graphql_profile_onto_user() {
		let info = json!({
			"name": "spez",
			"karma": { "total": 941124 },
			"profile": {
				"title": "spez",
				"createdAt": "2005-06-06T09:30:00+05:30",
				"publicDescriptionText": "Reddit CEO",
				"isNsfw": false,
				"styles": {
					"icon": "https://styles.redditmedia.com/icon64.png",
					"legacyIcon": { "url": "https://styles.redditmedia.com/icon256.png" },
					"profileBanner": "https://styles.redditmedia.com/banner.png"
				}
			}
		});

		let user = user_from_graphql(&info, "fallback");

		assert_eq!(user.name, "spez");
		assert_eq!(user.title, "spez");
		assert_eq!(user.karma, 941124);
		assert_eq!(user.icon, "/style/icon256.png");
		assert_eq!(user.banner, "https://styles.redditmedia.com/banner.png");
		assert_eq!(user.description, "Reddit CEO");
		assert!(!user.nsfw);
		assert_eq!(user.created, "Jun 06 '05");
	}

	#[test]
	fn graphql_profile_defaults_when_fields_are_missing() {
		let user = user_from_graphql(&json!({ "profile": {} }), "someone");

		assert_eq!(user.name, "someone");
		assert_eq!(user.title, "");
		assert_eq!(user.karma, 0);
		assert_eq!(user.icon, "");
		assert_eq!(user.banner, "");
		assert_eq!(user.description, "");
		assert!(!user.nsfw);
		assert_eq!(user.created, "");
	}

	#[test]
	fn graphql_profile_prefers_legacy_icon_and_falls_back_to_small_icon() {
		let only_small = json!({ "profile": { "styles": { "icon": "https://styles.redditmedia.com/icon64.png" } } });
		assert_eq!(user_from_graphql(&only_small, "x").icon, "/style/icon64.png");
	}

	#[test]
	fn graphql_profile_treats_null_description_as_empty() {
		let info = json!({ "profile": { "publicDescriptionText": null, "styles": { "profileBanner": null } } });
		let user = user_from_graphql(&info, "x");
		assert_eq!(user.description, "");
		assert_eq!(user.banner, "");
	}

	#[test]
	fn graphql_profile_parses_microsecond_timestamps_with_compact_offsets() {
		// ProfileDetailsByName returns this shape; UserProfile returns RFC 3339.
		let info = json!({ "profile": { "createdAt": "2005-06-06T04:00:00.000000+0000" } });
		assert_eq!(user_from_graphql(&info, "x").created, "Jun 06 '05");
	}

	#[test]
	fn graphql_profile_ignores_an_unparseable_created_date() {
		let info = json!({ "profile": { "createdAt": "not a date" } });
		assert_eq!(user_from_graphql(&info, "x").created, "");
	}
}
