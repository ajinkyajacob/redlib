# Consolidate Unified CLI Work on `main` — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close the gaps found in the `main` audit — unvalidated CI, undocumented `/api/v1` + MCP surface, the 30-token burst that throttles Reddit, misleading `502` on upstream throttling, missing `Retry-After`, and search pagination parity — then redeploy and prove it live.

**Architecture:** All work lands on `fork/main` of `ajinkyajacob/redlib`, which already contains the merged `feat/http-api`, `feat/mcp-server` and `feat/unified-cli` branches. Code changes are confined to the rate-limit gate shared by `/api/v1` and `/mcp` (`src/api.rs`, `src/mcp/http.rs`), the upstream error path (`src/client.rs`, `src/errors.rs`), and search pagination (`src/api.rs`, `src/mcp/tools.rs`). Deployment stays a host systemd service behind the existing nginx; the binary is built only on the Windows workstation.

**Tech Stack:** Rust 2021, redlib (`hyper` 0.14 web + `hyper` 1 MCP listener), `rmcp`, `nextest`, `cargo fmt`/`clippy`, GitHub Actions, systemd, nginx, Docker for cross-builds only.

**Spec:** `docs/superpowers/specs/2026-10-17-unified-cli-design.md`

## Global Constraints

- **No builds on oracleServer.** Every Rust build happens in Docker (`rust:bookworm` via `mirror.gcr.io`) or natively on the Windows workstation. Never `cargo build` over SSH.
- **Do not touch the searxng stack.** No changes to `/home/ubuntu/searxng/`, its compose file, or its containers except an nginx reload when a vhost changes.
- **Rate-limit contract after this plan:** burst `5`, sustained `30` requests/minute, `Retry-After: 5` on every `429`.
- Preserve `Cache-Control: no-store` on every API/MCP response, `WWW-Authenticate: Bearer` on `401`, and the constant-time `bearer_matches` comparison.
- `/api/v1/health` stays unauthenticated and must never consume a rate-limit token.
- Rust sources are tab-indented; run `cargo fmt --all` before every commit.
- Tests that need Reddit's network or a live OAuth token must stay `#[ignore]`; no new network-dependent test in the default run.
- The public token is never printed in logs, docs, or commits.

## Review Focus

1. **More than five API or MCP calls in the same instant** must yield `429` with `Retry-After`, and must never produce five-plus simultaneous Reddit calls — that fan-out is what triggered the 29 Oct block.
2. **Reddit answering `403`/`429` to the OAuth client** must surface as `429 RATE_LIMITED`, not `502 PARSE_ERROR`; a `5xx` stays `502` but reads as an outage.
3. **An `after` cursor containing `&`, `=`, `+`, spaces or non-ASCII** must be percent-encoded, never spliced into the query string raw.
4. **Someone following the README with only `REDLIB_API_TOKEN` set** must find MCP on port `9090`, and `REDLIB_MCP_HTTP_PORT=off` must leave `/api/v1` working with no second listener.
5. **Health JSON and the 429 message** must not claim a 30-request burst once the burst is 5.

---

### Task 1: First green CI run on the fork

**Files:**
- Modify: none expected on the happy path; `.github/workflows/pull-request.yml` or `main-rust.yml` only if a gate genuinely fails.

**Interfaces:**
- Consumes: nothing.
- Produces: a `fork/main` where `cargo nextest run`, `cargo fmt --all -- --check`, `cargo clippy -- -D warnings` are known-green in CI. Later tasks rely on this to catch regressions.

- [ ] **Step 1: Run the three CI gates locally**

```powershell
cargo fmt --all -- --check
cargo clippy -- -D warnings
cargo nextest run --no-fail-fast
```

Expected: `fmt` and `clippy` clean. `nextest` passes except for the known pre-existing network-dependent failures (`client`, `oauth`, `instance_info`, `live`); list any failure outside that set and fix it before continuing.

- [ ] **Step 2: Trigger the first run ever on the fork**

The fork has zero workflow runs in its history. Push an empty commit on a branch and open a PR so `pull_request` fires:

```powershell
git switch -c chore/ci-baseline
git commit --allow-empty -m "chore: trigger the first CI run on the fork"
git push -u fork chore/ci-baseline
gh pr create --repo ajinkyajacob/redlib --base main --head chore/ci-baseline --title "chore: first CI run on the fork" --body "No code change. Establishes a CI baseline for the unified CLI work."
gh pr checks --repo ajinkyajacob/redlib --watch
```

Expected: the `Pull Request` workflow appears and reports `success`.

- [ ] **Step 3: Record the result and merge**

```powershell
gh run list --repo ajinkyajacob/redlib --limit 5
gh pr merge --repo ajinkyajacob/redlib --squash --delete-branch
```

Expected: at least one run with conclusion `success`; PR merged into `main`.

If a gate fails for a reason unrelated to this plan, stop and report the failure instead of editing the workflow to make it pass.

- [ ] **Step 4: Commit**

```bash
git commit --allow-empty -m "chore: record CI baseline"
```

---

### Task 2: Cap the shared rate-limiter burst at 5

**Files:**
- Modify: `src/api.rs:17-18` (constants), `src/api.rs:57-84` (`Bucket`), `src/api.rs:164-166` (`gate`), `src/api.rs:190-200` (`health`), `src/api.rs:355-374` (bucket tests)
- Modify: `src/mcp/http.rs:119-120` (429 construction)

**Interfaces:**
- Consumes: nothing.
- Produces: `pub(crate) fn rate_limited_error() -> RedditError` in `src/api.rs`, used by both `gate` and the MCP listener; `pub(crate) const RETRY_AFTER_SECS: u64 = 5;` (consumed by Task 4).

- [ ] **Step 1: Replace the bucket tests with the burst contract**

In `src/api.rs` `mod tests`, delete `rate_bucket_allows_capacity_then_blocks` and `rate_bucket_refills_over_window`, and add:

```rust
#[test]
fn rate_bucket_allows_burst_then_blocks() {
	let mut bucket = Bucket::new();
	let start = Instant::now();
	for _ in 0..5 {
		assert!(bucket.take_at(start));
	}
	assert!(!bucket.take_at(start));
}

#[test]
fn rate_bucket_refills_one_token_per_two_seconds() {
	let mut bucket = Bucket::new();
	let start = Instant::now();
	for _ in 0..5 {
		bucket.take_at(start);
	}
	assert!(!bucket.take_at(start));
	assert!(!bucket.take_at(start + Duration::from_secs(1)));
	assert!(bucket.take_at(start + Duration::from_secs(2)));
}

#[test]
fn idle_bucket_never_exceeds_the_burst_cap() {
	let mut bucket = Bucket::new();
	let later = Instant::now() + Duration::from_secs(600);
	for _ in 0..5 {
		assert!(bucket.take_at(later));
	}
	assert!(!bucket.take_at(later));
}
```

- [ ] **Step 2: Run the tests to verify they fail**

```powershell
cargo nextest run --lib api::tests::rate_bucket
```

Expected: FAIL — the current bucket hands out 30 tokens, so `rate_bucket_allows_burst_then_blocks` fails on the 6th `assert!`.

- [ ] **Step 3: Implement the burst-limited bucket**

Change the constants at the top of `src/api.rs` to:

```rust
const RATE_PER_MINUTE: f64 = 30.0;
const RATE_BURST: f64 = 5.0;
const RATE_WINDOW_SECS: f64 = 60.0;
pub(crate) const RETRY_AFTER_SECS: u64 = 5;
```

`Bucket::new` seeds `tokens: RATE_BURST`. `take_at` refills at `RATE_PER_MINUTE / RATE_WINDOW_SECS` and clamps with `.min(RATE_BURST)`.

Add next to `rate_limit_take`:

```rust
pub(crate) fn rate_limited_error() -> RedditError {
	RedditError::new(ErrorKind::RateLimited, "rate limit exceeded: 5 request burst, 30 requests/minute")
}
```

Replace the two hand-built `RateLimited` errors in `gate` (`src/api.rs`) and `src/mcp/http.rs` with `rate_limited_error()`.

Update the health payload to report the real shape:

```rust
"rate_limit": { "burst": 5, "per_minute": 30, "retry_after_seconds": RETRY_AFTER_SECS },
```

- [ ] **Step 4: Run the tests to verify they pass**

```powershell
cargo nextest run --lib api::tests
```

Expected: PASS, including the three bucket tests.

- [ ] **Step 5: Commit**

```bash
git add src/api.rs src/mcp/http.rs
git commit -m "fix(api): cap the shared rate-limit burst at 5 requests"
```

---

### Task 3: Map upstream Reddit 403/429 to `429 RATE_LIMITED`

**Files:**
- Modify: `src/client.rs:405-412` (add the status helper beside the `err` closure), `src/client.rs:511-518` (parse-failure branch)
- Modify: `src/errors.rs:86-104` (`map_client_err`), `src/errors.rs` tests

**Interfaces:**
- Consumes: nothing.
- Produces: `fn upstream_status_message(status: u16, detail: &str) -> Option<String>` in `src/client.rs`, private to that module.

- [ ] **Step 1: Write the failing mapping tests**

In `src/errors.rs` `mod tests`:

```rust
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
```

And in `src/client.rs` `mod tests`:

```rust
#[test]
fn upstream_status_message_only_translates_throttling_auth_and_outages() {
	assert!(upstream_status_message(403, "x").unwrap().starts_with("Too many requests"));
	assert!(upstream_status_message(429, "x").unwrap().starts_with("Too many requests"));
	assert!(upstream_status_message(401, "x").unwrap().starts_with("Unauthorized"));
	assert!(upstream_status_message(503, "x").unwrap().contains("outage"));
	assert!(upstream_status_message(404, "x").is_none());
}
```

- [ ] **Step 2: Run the tests to verify they fail**

```powershell
cargo nextest run --lib errors::tests::maps_upstream client::tests::upstream_status_message
```

Expected: FAIL — `upstream_status_message` does not exist, and the forbidden text currently lands in `Parse`.

- [ ] **Step 3: Implement the mapping**

Add `upstream_status_message` in `src/client.rs` (module-private, above `json`):

- `401` → `Some(format!("Unauthorized: Reddit rejected the OAuth token: {detail}"))`
- `403 | 429` → `Some(format!("Too many requests: Reddit returned {status}: {detail}"))`
- `500..=599` → `Some(format!("Reddit is having issues, check if there's an outage: {detail}"))`
- otherwise → `None`

In the parse-failure branch at `src/client.rs:511`, keep the `error!` line, then prefer the translated message:

```rust
match upstream_status_message(status.as_u16(), &e.to_string()) {
	Some(msg) => Err(format!("{msg} | {path}")),
	None => err("Failed to parse page JSON data", e.to_string(), path),
}
```

Replace the existing `if status.is_server_error()` arm, since `5xx` now flows through the helper.

In `src/errors.rs` `map_client_err`, add `lower.contains("having issues") => ErrorKind::Down` to the `Down` condition. The `RateLimited` condition already matches "too many requests", so no other change is needed there.

- [ ] **Step 4: Run the tests to verify they pass**

```powershell
cargo nextest run --lib errors::tests client::tests
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/client.rs src/errors.rs
git commit -m "fix(errors): report upstream Reddit throttling as 429, not 502"
```

---

### Task 4: Send `Retry-After` on every 429

**Files:**
- Modify: `src/api.rs:179-188` (`err_response`)
- Modify: `src/mcp/http.rs:126-134` (`error_response`)
- Test: same modules' `mod tests`

**Interfaces:**
- Consumes: `crate::api::{RETRY_AFTER_SECS, rate_limited_error}` from Task 2.
- Produces: nothing new.

- [ ] **Step 1: Write the failing header tests**

In `src/api.rs` `mod tests`:

```rust
#[test]
fn rate_limited_error_carries_retry_after() {
	let res = err_response(rate_limited_error()).unwrap();
	assert_eq!(res.status(), 429);
	assert_eq!(res.headers().get("retry-after").unwrap(), "5");
}

#[test]
fn unauthorized_error_has_no_retry_after() {
	let res = err_response(RedditError::new(ErrorKind::Unauthorized, "nope")).unwrap();
	assert!(res.headers().get("retry-after").is_none());
}
```

In `src/mcp/http.rs` `mod tests`:

```rust
#[test]
fn mcp_rate_limit_response_carries_retry_after() {
	let res = error_response(crate::api::rate_limited_error());
	assert_eq!(res.status(), StatusCode::TOO_MANY_REQUESTS);
	assert_eq!(res.headers().get("retry-after").unwrap(), "5");
}
```

- [ ] **Step 2: Run the tests to verify they fail**

```powershell
cargo nextest run --lib api::tests::rate_limited_error_carries_retry_after http::tests::mcp_rate_limit_response_carries_retry_after
```

Expected: FAIL — the header is absent.

- [ ] **Step 3: Implement the header on both surfaces**

In `src/api.rs` `err_response`, after the `401` branch, add:

```rust
if status == 429 {
	res.headers_mut().insert("retry-after", HeaderValue::from(RETRY_AFTER_SECS));
}
```

Do the same in `src/mcp/http.rs` `error_response`, using `StatusCode::TOO_MANY_REQUESTS` and `HeaderValue::from(RETRY_AFTER_SECS)`.

- [ ] **Step 4: Run the tests to verify they pass**

```powershell
cargo nextest run --lib api::tests http::tests
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/api.rs src/mcp/http.rs
git commit -m "feat(api): send Retry-After on 429 responses"
```

---

### Task 5: Paginate search on both surfaces

**Files:**
- Modify: `src/api.rs:115-130` (`search_path`), `src/api.rs:240-252` (`search`)
- Modify: `src/mcp/tools.rs` (`SearchArgs`, `inner_search`)
- Test: `src/api.rs` `mod tests`

**Interfaces:**
- Consumes: `crate::params::encode` (already re-exported as `crate::mcp::fetch::encode`).
- Produces: `fn search_path(query: &str, subreddit: Option<&str>, sort: Option<&str>, time: Option<&str>, after: Option<&str>, limit: Option<u32>) -> Result<String, RedditError>`; `SearchArgs { query, subreddit, sort, time, after, limit }`.

- [ ] **Step 1: Write the failing path test**

In `src/api.rs` `mod tests`:

```rust
#[test]
fn search_path_encodes_the_after_cursor() {
	let p = search_path("rust lang", None, None, None, Some("t3_a&b=c+d"), None).unwrap();
	assert!(p.starts_with("/search.json?"), "{p}");
	assert!(p.contains("after=t3_a%26b%3Dc%2Bd"), "{p}");
}

#[test]
fn search_path_omits_after_when_absent() {
	let p = search_path("rust", None, None, None, None, None).unwrap();
	assert!(!p.contains("after="), "{p}");
}
```

- [ ] **Step 2: Run it to verify it fails**

```powershell
cargo nextest run --lib api::tests::search_path
```

Expected: FAIL — `search_path` takes six arguments today, not seven.

- [ ] **Step 3: Add the parameter end to end**

`search_path` gains `after` and, after the optional `t=` part, pushes `format!("after={}", encode(after))` when present. `api::search` passes `query(&req, "after").as_deref()` in the matching position.

`SearchArgs` gains:

```rust
	/// Cursor from a previous result's `cursor` field, to fetch the next page.
	pub after: Option<String>,
```

`inner_search` appends `&after={}` using `fetch::encode` when the field is `Some`, immediately after the `t=` block and before the `limit=` line.

- [ ] **Step 4: Run the tests to verify they pass**

```powershell
cargo nextest run --lib api::tests
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/api.rs src/mcp/tools.rs
git commit -m "feat(search): support cursor pagination in the API and MCP search tool"
```

---

### Task 6: Document `/api/v1` and MCP in the README

**Files:**
- Modify: `README.md` (table of contents at line 18, flag list at line 397, instance-settings table at line 412, new section after `Forward Proxies`)

**Interfaces:**
- Consumes: the endpoint set registered in `src/main.rs:432-437`, the env vars read in `src/api.rs:28` and `src/mcp/http.rs`, and the flag defined in `src/main.rs:149-158`.
- Produces: user-facing documentation only; no code.

- [ ] **Step 1: Write the assertion that currently fails**

```powershell
$need = @('/api/v1/r/', '/api/v1/search', '/api/v1/u/', '/api/v1/health', 'reddit_subreddit', 'reddit_post', 'REDLIB_API_TOKEN', 'REDLIB_MCP_HTTP_PORT', 'REDLIB_MCP_ALLOWED_HOSTS', '--mcp-http')
$miss = $need | Where-Object { -not (Select-String -Path README.md -Pattern ([regex]::Escape($_) ) -Quiet) }
if ($miss) { "MISSING: $($miss -join ', ')"; exit 1 } else { "README complete" }
```

Expected: FAIL listing every item — the README contains none of them.

- [ ] **Step 2: Add the section**

Add a `## JSON API and MCP` section after `Forward Proxies`, containing:

- One paragraph: setting `REDLIB_API_TOKEN` turns on a read-only JSON API under `/api/v1` and a Model Context Protocol Streamable HTTP listener at `/mcp`; unset means neither exists and `/api/v1` returns `404`.
- An endpoint table with exact rows: `GET /api/v1/health` (no auth, no token consumed), `GET /api/v1/r/{sub}` (`sort`, `t`, `after`, `limit`), `GET /api/v1/r/{sub}/comments/{id}` (`sort`, `comment`, `cursor`, `max_comments`), `GET /api/v1/search` (`q`, `subreddit`, `sort`, `t`, `after`, `limit`), `GET /api/v1/u/{name}` (`sort`, `after`, `limit`).
- Auth: `Authorization: Bearer <token>`, compared in constant time; `401` carries `WWW-Authenticate: Bearer`.
- Rate limit: burst 5, then 30 requests/minute refilling at 0.5/s, shared by the API and MCP; `429` carries `Retry-After: 5`.
- MCP tools: `reddit_search`, `reddit_subreddit`, `reddit_post`, `reddit_user`, with one line each on when to use it.
- A pointer to `docs/deploy-unified-binary.md` for reverse-proxy and systemd wiring.

- [ ] **Step 3: Add the flag and the environment variables**

- Flag list: `--mcp-http <PORT>` — MCP Streamable HTTP listener port, default `9090`, `off` disables it; also settable as `REDLIB_MCP_HTTP_PORT`, and only active when `REDLIB_API_TOKEN` is set.
- Instance-settings table: `REDLIB_API_TOKEN` (string, empty = API and MCP disabled, read-only surface), `REDLIB_MCP_HTTP_PORT` (`Integer 0-65535` or `off`, default `9090`), `REDLIB_MCP_ALLOWED_HOSTS` (comma-separated host names; when set, a request whose `Host` is not listed gets `403`).

- [ ] **Step 4: Verify the assertion passes**

```powershell
$need = @('/api/v1/r/', '/api/v1/search', '/api/v1/u/', '/api/v1/health', 'reddit_subreddit', 'reddit_post', 'REDLIB_API_TOKEN', 'REDLIB_MCP_HTTP_PORT', 'REDLIB_MCP_ALLOWED_HOSTS', '--mcp-http')
$miss = $need | Where-Object { -not (Select-String -Path README.md -Pattern ([regex]::Escape($_) ) -Quiet) }
if ($miss) { "MISSING: $($miss -join ', ')"; exit 1 } else { "README complete" }
```

Expected: `README complete`.

- [ ] **Step 5: Commit**

```bash
git add README.md
git commit -m "docs: document the JSON API, MCP endpoint and their settings"
```

---

### Task 7: Remove the abandoned ignored test

**Files:**
- Modify: `src/oauth.rs:487-520` (`test_generic_web_backend`)

**Interfaces:**
- Consumes: nothing.
- Produces: nothing; removes a test that can never run in CI.

- [ ] **Step 1: Confirm it is the only such marker**

```powershell
Select-String -Path src\*.rs -Pattern 'ignore = "todolol"'
```

Expected: exactly one hit, `src/oauth.rs`.

- [ ] **Step 2: Delete the test**

Remove the `#[tokio::test(flavor = "multi_thread")]` / `#[ignore = "todolol"]` attribute pair and the `test_generic_web_backend` body. It calls a live Reddit endpoint, so it cannot be made deterministic without a network fixture that does not exist.

- [ ] **Step 3: Verify**

```powershell
Select-String -Path src\*.rs -Pattern 'todolol'
cargo nextest run --lib oauth::tests
```

Expected: no hits for `todolol`; the `oauth` unit tests pass.

- [ ] **Step 4: Commit**

```bash
git add src/oauth.rs
git commit -m "test(oauth): drop the abandoned network test marked todolol"
```

---

### Task 8: Drop the stale stashes

**Files:** none (repository metadata).

**Interfaces:** none.

- [ ] **Step 1: Confirm both stashes only touch `Cargo.lock`**

```powershell
git stash show --stat "stash@{0}"
git stash show --stat "stash@{1}"
```

Expected: each shows only `Cargo.lock`.

- [ ] **Step 2: Confirm the working tree lock is already current**

```powershell
git status --short Cargo.lock
git diff --stat HEAD -- Cargo.lock
```

Expected: no output. `Cargo.lock` is committed and clean, so the stashes carry nothing.

- [ ] **Step 3: Drop them**

```powershell
git stash drop "stash@{0}"
git stash drop "stash@{0}"
git stash list
```

Expected: `git stash list` prints nothing.

- [ ] **Step 4: Report**

No commit; record the removal in the final summary.

---

### Task 9: Redeploy and prove the burst behaviour live

**Files:**
- No repository file changes. Deploys `/opt/redlib/redlib` on oracleServer.

**Interfaces:**
- Consumes: the binary produced from Tasks 2-5.
- Produces: a live instance where >5 parallel calls are throttled locally instead of at Reddit.

- [ ] **Step 1: Build the release binary on the workstation**

Reuse the existing Docker volumes (`redlib-src`, `redlib-target`, `redlib-cargo-reg`) with `rust:bookworm` and `cargo build --release`; copy the artifact out of a `debian:bookworm-slim` container. Do not run `cargo` on oracleServer.

Expected: a Linux x86_64 binary whose size is within a few hundred KB of the current 16,585,480 B.

- [ ] **Step 2: Deploy and restart**

```powershell
scp .\redlib oracleServer:/tmp/redlib.new
ssh oracleServer 'sudo install -o root -g root -m 0755 /tmp/redlib.new /opt/redlib/redlib && sudo systemctl restart redlib && sleep 6 && systemctl show redlib -p ActiveState -p NRestarts -p MemoryCurrent'
```

Expected: `ActiveState=active`, `NRestarts=0`, memory well below `MemoryLimit`.

- [ ] **Step 3: Probe the burst shape**

```powershell
ssh oracleServer 'for i in $(seq 1 8); do curl -s -o /dev/null -w "%{http_code} " -H "Authorization: Bearer $REDLIB_TOKEN" "http://127.0.0.1:8090/api/v1/r/rust?limit=1" & done; wait; echo'
```

Expected: exactly five `200` and three `429`.

- [ ] **Step 4: Verify `Retry-After` and that Reddit was not hit**

```powershell
ssh oracleServer 'curl -s -D - -o /dev/null -H "Authorization: Bearer $REDLIB_TOKEN" "http://127.0.0.1:8090/api/v1/r/rust?limit=1" | grep -i retry-after; sudo journalctl -u redlib --since "-5min" --no-pager | grep -c "403 Forbidden"'
```

Expected: `retry-after: 5` (after the burst is exhausted) and `0` upstream 403s.

- [ ] **Step 5: Verify the public path still renders and MCP still answers**

```powershell
curl.exe -s -o NUL -w "%{http_code}`n" https://ajinkya.duckdns.org/redlib/r/rust
```

Expected: `200`, and an MCP `initialize` through OpenCode still connects once the bucket refills.

- [ ] **Step 6: Confirm CI is green on the final commit**

```powershell
git push fork main
gh run list --repo ajinkyajacob/redlib --branch main --limit 5
```

Expected: `Pull Request` and `Rust Build & Publish` both `success` for the pushed commit.

---

## Out of scope (deliberately deferred)

- **GraphQL migration for listings.** `docs/reddit-api-shutdown.md` records Reddit's deadlines (2027-01-12 for unregistered apps, 2027-03 final removal). Listings, search and user pages still read the public JSON API. That is a multi-week change to `external` fetch paths and needs its own spec; it is not part of this consolidation.
- **Per-token or per-IP buckets.** One consumer today; isolation adds state without a current benefit.
- **Binary size work.** `rmcp` plus two `hyper` generations explains the +60% over upstream; not a correctness issue.
- **Sibling directories.** `redlibe` is a clone of `snapetech/redlibe` (one commit, clean, unrelated). `redlib-mcp`, `redlib-now`, `redview-platform`, `reddit-rs` hold no redlib work. Nothing to merge.
- **`contrib/docker` port 8080.** It is the documented container alternative to the 8090 host service; `docs/deploy-unified-binary.md` tells the reader to pick one.
