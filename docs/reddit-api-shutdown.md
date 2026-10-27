# Reddit API shutdown watch

Status: **deferred** — reviewed 2026-10-27, no code change planned until a
listing endpoint actually fails.

This file is the reminder for the other half of the work. Comments, user
profiles and the subreddit sidebar already come from Reddit's app GraphQL;
everything else in redlib still reads the public JSON API, which Reddit has
announced it is shutting down.

## Confirmed timeline

From Reddit's r/redditdev announcement, 2026-09-30
(["Moving Data API Apps to the Developer Platform"](https://www.reddit.com/r/redditdev/comments/1wubcvf/moving_data_api_apps_to_the_developer_platform/)):

| Date | What happens |
|---|---|
| 2026-10-31 | New requests for public API access are no longer accepted. |
| 2026-11-13 | Reddit's own RSS feeds retire (does not affect redlib's generated RSS, but the JSON it renders from has the same deadline). |
| 2027-01-12 | API access removed for apps/users that have not registered. |
| 2027-03 | Remaining public API access ends. |

Secondary coverage: [TechCrunch, 2026-09-30](https://techcrunch.com/2026/09/30/reddit-is-killing-rss-feeds-ending-public-api-access-because-of-ai-bots/),
[TechRepublic](https://www.techrepublic.com/article/news-reddit-rss-public-api-shutdown/).

## What is *not* confirmed

No public statement says `gql-fed.reddit.com` retires in 2027. It is the
official app's internal persisted-query endpoint, separate from the public
API, and the app still needs it. Treat it as the durable path, not a
scheduled casualty.

What does decay there is the **persisted hashes**: they change with app
releases. When a query starts failing, re-extract the hash (see below); the
endpoint itself is not expected to disappear.

## Still on the public JSON API

All of these go through `client::json` and are public API surface:

| Area | Call site |
|---|---|
| Subreddit listings | `src/subreddit.rs` (`/r/{sub}/{sort}.json`) |
| Front page / popular | `src/subreddit.rs`, `src/client.rs` (`/r/popular/hot.json`) |
| Post metadata for the comments path | `src/gql.rs` (`/by_id/t3_{id}.json`) |
| Post detail fallback | `src/post.rs` |
| User listings | `src/user.rs` (`/user/{name}/{listing}.json`) |
| Search (posts, comments, subreddits) | `src/search.rs` |
| Wiki pages | `src/subreddit.rs` (`/r/{sub}/wiki/{page}.json`) |
| Duplicate-post lookup | `src/duplicates.rs` |
| RSS feeds | `src/subreddit.rs::rss` (renders from listing JSON) |
| Subreddit/user/subreddit-name fallbacks | `src/subreddit.rs`, `src/user.rs`, `src/utils.rs` |

`src/api.rs` and `src/mcp/fetch.rs` build their own `.json` paths, so the REST
and MCP surfaces inherit the same risk.

## Already migrated (this branch)

- Comments: `PostComments` persisted op — `src/gql.rs::thread`.
- Post comments on web, REST and MCP all route through it.
- User profiles: `UserProfile` — `src/gql.rs::user`.
- Subreddit sidebar: `SubredditInfoByName` — `src/gql.rs::subreddit`.

Each keeps the JSON route as a fallback, so a GraphQL error degrades instead
of blanking the page.

## Symptom when it breaks

A listing stops rendering with an error page, or `client::json` returns a 403
with Reddit's "blocked" body. Comments and profiles keep working, which makes
the failure look partial.

## Mitigation ladder (when it happens)

1. Confirm scope: hit the endpoint with `curl` and a browser user-agent. Check
   whether it is one path (`/r/x/hot.json`) or the whole JSON surface.
2. If listings are gone, the replacement is a GraphQL feed operation. The APK
   exposes `SubredditFeedSdui`, `HomeFeedWithDefer`, `PopularFeedSdui`,
   `LatestFeedSdui`, `ExploreFeedSdui` and friends — but they return
   server-driven UI components, not a `Listing`. This is a mapping project, not
   a hash swap.
3. Failing everything else, the JSON network can be re-enabled by pointing the
   OAuth client at a working backend (upstream redlib instances, a self-hosted
   proxy), but that changes the deployment story.

The single choke point to add a fallback in is `client::json` in
`src/client.rs` (plus the two path builders in `src/api.rs` and
`src/mcp/fetch.rs`), not each call site.

## Maintaining the persisted hashes

Hashes live at the top of `src/gql.rs`:

- `POST_COMMENTS_HASH`
- `USER_PROFILE_HASH`
- `SUBREDDIT_INFO_HASH`

To refresh one:

1. Get the current official Android build (APK/APKM).
2. Decompile with jadx (`rea/tools/jadx/lib/jadx-1.5.6-all.jar`, JDK 21).
3. Find the operation class: it has an `a()` method returning the hash and a
   `c()` method returning the full query text.
4. Copy the hash, keep the operation name, and confirm the variable names from
   the query string (`$subredditName`, `$name`, …).
5. Verify against `https://gql-fed.reddit.com/` with an anonymous loid token
   before changing code.

Reddit rejects raw query text; only the persisted hash works, so the hash is
the only thing that needs updating when the app changes its operations.

## Revisit triggers

- Any listing/REST/MCP error caused by a 403 from the JSON endpoints.
- **By 2027-02-01**, to test the listing path against the March 2027 cutoff,
  rather than discovering it in production.
