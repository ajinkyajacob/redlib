# Unified CLI (A4) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Collapse `redlib.exe` and `redlib-mcp.exe` into one binary whose bare invocation serves web + `/api/v1` + MCP-over-HTTP behind a single env-var switch, and whose `mcp` subcommand serves MCP over stdio.

**Architecture:** The `mcp/` crate is absorbed into the root crate. `normalize`, `params` and the error taxonomy each exist once in `src/`. MCP code moves to `src/mcp/` (`mod`, `tools`, `fetch`, `http`, `stdio`). Two listeners run on one tokio runtime: hyper 0.14 (web + REST) and hyper 1.x (MCP Streamable HTTP), because `rmcp` is built against `http` 1.x and redlib against `http` 0.2.

**Tech Stack:** Rust 1.88 (raised from 1.81 in Task 2 — `rmcp 3.5.1` requires it), hyper 0.14.31 (existing) + hyper 1.x (new, for `rmcp`), `rmcp =3.5.1` with `transport-io` and `transport-streamable-http-server`, tokio, serde/serde_json, askama.

**Spec:** `docs/superpowers/specs/2026-10-17-unified-cli-design.md`

## Global Constraints

- `rust-version` is raised from `"1.81"` to `"1.88"` in Task 2 and nowhere else. `rmcp 3.5.1` declares `rust-version = "1.88"` and `edition = "2024"`, so leaving `1.81` would be a false MSRV: a 1.81 toolchain cannot build this crate. The machine's default toolchain is already `1.88.0-x86_64-pc-windows-msvc`.
- `rmcp` is pinned exactly: `rmcp = { version = "=3.5.1", features = ["server", "macros", "transport-io", "transport-streamable-http-server"] }`.
- `REDLIB_API_TOKEN` (non-empty) is the only master switch. `REDLIB_API_ENABLED` is retired and must not appear in `src/` afterwards.
- Empty or whitespace-only `REDLIB_API_TOKEN` means "unset": no `/api/v1`, no MCP listener.
- Every `/api/v1/*` request except `/api/v1/health` and every MCP HTTP request requires `Authorization: Bearer <token>`, compared in constant time.
- Auth failure is `401` with `WWW-Authenticate: Bearer`.
- `redlib mcp` requires no token and must never write anything but MCP frames to stdout.
- MCP HTTP default port `9090`; `REDLIB_MCP_HTTP_PORT=off` disables it while leaving REST on; MCP port equal to the web port is a startup error.
- `REDLIB_MCP_ALLOWED_HOSTS` is comma-separated; default `localhost,127.0.0.1,::1`. Never call `disable_allowed_hosts()`.
- Origin validation stays at rmcp's default (disabled).
- No authentication on the web UI.
- Every cargo invocation goes through `pwsh -File dev-build.ps1 <cargo args>` from the repo root. Direct `cargo` fails on this machine (GNU rustc on PATH ⇒ `could not find native static library 'crypto'`).
- No behavioural change to upstream `src/` beyond additive modules and the localised edits named in each task.

## Review Focus

Inputs and conditions the spec implies but no task's happy-path test covers. Each has its test placed in the task that owns the code.

1. **Hosted hostname with `REDLIB_MCP_ALLOWED_HOSTS` unset** — rmcp's default is loopback-only, so every request through `https://host/mcp` 403s. Expected: the operator adds their authority and gets 200. (Task 10)
2. **Whitespace-only token** (`REDLIB_API_TOKEN="   "`) — must behave exactly like unset: no `/api/v1` routes, no MCP port. (Task 9)
3. **Non-numeric `REDLIB_MCP_HTTP_PORT`** (e.g. `90o0`) — must fail loudly at startup, never silently fall back to 9090. (Task 10)
4. **Port collision via different sources** — web port from `PORT`/`-p`, MCP port from `REDLIB_MCP_HTTP_PORT`; equal values must exit non-zero before either listener binds. (Task 11)
5. **Malformed `Authorization` header** — wrong scheme (`Basic`), trailing whitespace, missing header, wrong token: all 401 with `WWW-Authenticate`, never 200. (Task 9)

---

### Task 1: Move the MSVC build wrapper to the repo root

`mcp/dev-build.ps1` is the only working way to build on this machine. It must exist at the root before `mcp/` is deleted.

**Files:**
- Create: `dev-build.ps1`
- Reference: `mcp/dev-build.ps1` (do not delete yet)

**Interfaces:**
- Consumes: nothing.
- Produces: `dev-build.ps1` at repo root; every later task's verification commands use `pwsh -File dev-build.ps1 ...`.

- [ ] **Step 1: Create `dev-build.ps1` at the repo root**

Same body as `mcp/dev-build.ps1` except the root is now the script's own directory, not its parent:

```powershell
# dev-build.ps1
# Usage: pwsh -File dev-build.ps1 <cargo args...>
$ErrorActionPreference = 'Stop'
$vcvars = 'C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvars64.bat'
if (-not (Test-Path $vcvars)) { throw "vcvars64.bat not found at $vcvars" }
$repoRoot = $PSScriptRoot
$argsLine = ($args -join ' ')
$cmd = "call `"$vcvars`" >nul && set `"PATH=$env:USERPROFILE\.cargo\bin;%PATH%`" && cd /d `"$repoRoot`" && cargo $argsLine"
& cmd.exe /c $cmd
exit $LASTEXITCODE
```

- [ ] **Step 2: Verify it resolves the MSVC toolchain**

Run: `pwsh -File dev-build.ps1 --version`
Expected: `cargo 1.x.y (...)` and exit code 0.

- [ ] **Step 3: Commit**

```bash
git add dev-build.ps1
git commit -m "build: move MSVC cargo wrapper to repo root"
```

---

### Task 2: Add the dependencies and drop the workspace member

This is the risk gate the spec names first: `rmcp` needs `http` 1.x while redlib is on hyper 0.14 / `http` 0.2. Prove both majors coexist in one build before writing any listener code.

**Files:**
- Modify: `Cargo.toml` (dependencies block, `[workspace]` block at lines 74-76)

**Interfaces:**
- Consumes: `dev-build.ps1` from Task 1.
- Produces: root crate that links `rmcp` and hyper 1. Later tasks import `rmcp::...`, `hyper1::...`, `hyper_util::rt::TokioIo`, `http_body_util::{BodyExt, Full, combinators::BoxBody}`.

- [ ] **Step 1: Add the new dependencies**

Append to `[dependencies]` (keep the existing `hyper = "0.14.31"` entry unchanged; the two majors coexist by rename):

```toml
rmcp = { version = "=3.5.1", features = ["server", "macros", "transport-io", "transport-streamable-http-server"] }
hyper1 = { package = "hyper", version = "1", features = ["http1", "server"] }
hyper-util = { version = "0.1", features = ["tokio"] }
http-body-util = "0.1"
bytes = "1"
```

- [ ] **Step 2: Raise the declared MSRV**

Change `rust-version = "1.81"` (line 12) to:

```toml
rust-version = "1.88"
```

`rmcp 3.5.1` requires 1.88 and uses edition 2024, so the old value is not buildable.

- [ ] **Step 3: Remove the workspace member list**

Delete the trailing block:

```toml
[workspace]
members = [".", "mcp"]
resolver = "2"
```

Edition 2021 already selects resolver 2, so nothing replaces it. Leave `mcp/` on disk for now; it simply stops being a member.

- [ ] **Step 4: Verify the whole tree still builds with both http majors present**

Run: `pwsh -File dev-build.ps1 build`
Expected: `Finished dev [unoptimized + debuginfo] target(s)` with no version-conflict error. First run compiles BoringSSL — allow several minutes.

- [ ] **Step 5: Verify the existing tests still pass**

Run: `pwsh -File dev-build.ps1 test --lib -- --skip test_rate_limit_check --skip test_generic_web_backend`
Expected: `test result: ok.` — the 30 `api::` tests plus upstream tests, 0 failed. (Those two skipped tests hit live Reddit and already fail at `46cb2f5`; see the ledger.)

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock
git commit -m "build: link rmcp and hyper 1 alongside hyper 0.14"
```

---

### Task 3: Unified error taxonomy in `src/errors.rs`

`mcp/src/error.rs` and the private copy in `src/api.rs:97-157` differ in three ways that must be reconciled: api's `map_client_err` omits the `dns` heuristic, api's struct is `ApiError { kind, message, status }` while MCP's is `FetchError { kind, detail }`, and only MCP has `hint()`.

**Files:**
- Create: `src/errors.rs`
- Modify: `src/lib.rs` (add `pub mod errors;` in alphabetical position, before `instance_info`)
- Test: inline `#[cfg(test)] mod tests` in `src/errors.rs`

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `pub enum ErrorKind { BadInput, Unauthorized, RateLimited, TokenStale, Down, Unavailable, Parse }` — `#[derive(Debug, Clone, Copy, PartialEq, Eq)]`
  - `pub struct RedditError { pub kind: ErrorKind, pub message: String }`
  - `impl RedditError { pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self; pub fn status(&self) -> u16; pub fn to_json(&self) -> serde_json::Value; pub fn to_error_data(&self) -> rmcp::model::ErrorData }`
  - `pub fn map_client_err(message: String) -> RedditError`
  - `impl ErrorKind { pub fn as_str(self) -> &'static str; pub fn hint(self) -> &'static str }`

- [ ] **Step 1: Write the failing test module**

Moved assertions from both crates, plus the new variant:

```rust
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
fn maps_dns_text_to_down() {
    assert_eq!(map_client_err("dns error: failed to lookup address".into()).kind, ErrorKind::Down);
}
```

Keep these eleven tests. Six are written above; add the four `map_client_err` tests from `mcp/src/error.rs:79-97` (`maps_rate_limit_text`, `maps_token_stale_text`, `maps_transport_text`, `unknown_text_falls_back_to_parse`) and `maps_unavailable_text` from `src/api.rs:626-629`, verbatim except `FetchError`/`ApiError` → `RedditError`. Do not re-add `bad_input_maps_to_invalid_params` or `internal_kinds_map_to_internal_error` from `mcp/src/error.rs:99-109` — the two above already cover them.

- [ ] **Step 2: Run the test to verify it fails**

Run: `pwsh -File dev-build.ps1 test --lib errors::`
Expected: FAIL — `unresolved import crate::errors`.

- [ ] **Step 3: Implement `src/errors.rs`**

`status()` maps `BadInput`→400, `Unauthorized`→401, `RateLimited`→429, `Unavailable`→404, everything else→502. `as_str()` returns the six existing strings plus `"UNAUTHORIZED"`. `hint()` keeps MCP's two hints (`TokenStale`, `RateLimited`) and returns `""` otherwise. `to_json()` is api's envelope. `to_error_data()` is MCP's: `invalid_params` for `BadInput`, `internal_error` for everything else, message shaped `"KIND: message"` or `"KIND: message — hint"` when a hint exists. `map_client_err` is MCP's version, including the `dns` clause, with one correction: `client::json` appends ` | {path}` to every message (`src/client.rs:327`, `:421`), so scanning the raw string makes a post id or a subreddit like `/r/401k` classify as `TokenStale`/`Unavailable`. Classify on the text before the last `" | "`; return the untouched `message` in the error. This is a deliberate, reviewed deviation from "verbatim".

- [ ] **Step 4: Run the tests to verify they pass**

Run: `pwsh -File dev-build.ps1 test --lib errors::`
Expected: PASS, 11 passed.

- [ ] **Step 5: Commit**

```bash
git add src/errors.rs src/lib.rs
git commit -m "refactor: single error taxonomy in src/errors.rs"
```

### Task 4: Single copy of the normalizers in `src/normalize.rs`

`mcp/src/normalize.rs` and `src/api.rs:159-345` are the same code; the only differences are `pub` markers, the `JsonSchema` derive, and api's `listing_out` helper returning `Value`.

**Files:**
- Create: `src/normalize.rs`
- Modify: `src/lib.rs` (add `pub mod normalize;`)
- Test: inline `#[cfg(test)] mod tests` in `src/normalize.rs`

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `pub const COMMENT_BODY_CAP: usize = 1200; pub const POST_BODY_CAP: usize = 4000;`
  - `pub struct PostOut`, `pub struct CommentOut`, `pub struct ListingOut`, `pub struct PostPage` — all `#[derive(Debug, Clone, Serialize, Deserialize, rmcp::schemars::JsonSchema)]`, fields exactly as in `mcp/src/normalize.rs:13-56`
  - `pub fn normalize_post(data: &Value) -> Option<PostOut>`
  - `pub fn listing_posts(listing: &Value) -> Vec<PostOut>`
  - `pub fn cursor(listing: &Value) -> Option<String>`
  - `pub fn post_and_comments(page: &Value, budget: usize) -> Option<PostPage>`
  - `pub fn listing_out(listing: &Value) -> ListingOut` — **new**: builds `ListingOut { ok: true, kind: "listing", result_count, cursor, posts }`, replacing api's `Value`-returning helper so both consumers share one envelope

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn listing_out_builds_the_shared_envelope() {
    let v = json!({"kind": "Listing", "data": {"after": "t3_x", "children": [
        {"kind": "t3", "data": {"id": "abc123", "title": "Hello", "subreddit": "rust",
                                "permalink": "/r/rust/comments/abc123/hello/"}}
    ]}});
    let out = listing_out(&v);
    assert!(out.ok);
    assert_eq!(out.kind, "listing");
    assert_eq!(out.result_count, 1);
    assert_eq!(out.cursor.as_deref(), Some("t3_x"));
    assert_eq!(out.posts[0].permalink, "https://www.reddit.com/r/rust/comments/abc123/hello/");
}
```

Then copy the eight tests from `mcp/src/normalize.rs:196-320` unchanged (`normalizes_post_and_cursor`, `flair_null_and_empty_both_become_none`, `skips_children_without_id_or_title`, `builds_nested_comment_tree_with_more_ids`, `marks_deleted_comments`, `bfs_budget_keeps_shallow_comments_first`, `body_caps_apply`, `returns_none_when_first_listing_has_no_post`).

- [ ] **Step 2: Run the test to verify it fails**

Run: `pwsh -File dev-build.ps1 test --lib normalize::`
Expected: FAIL — `unresolved import crate::normalize`.

- [ ] **Step 3: Create `src/normalize.rs`**

Copy `mcp/src/normalize.rs` verbatim (all 321 lines), keeping `pub` on the types and functions and the `JsonSchema` derive. Add `listing_out` at the end of the non-test section:

```rust
pub fn listing_out(listing: &Value) -> ListingOut {
    let posts = listing_posts(listing);
    ListingOut { ok: true, kind: "listing".into(), result_count: posts.len(), cursor: cursor(listing), posts }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `pwsh -File dev-build.ps1 test --lib normalize::`
Expected: PASS, 9 passed.

- [ ] **Step 5: Commit**

```bash
git add src/normalize.rs src/lib.rs
git commit -m "refactor: single normalizer copy in src/normalize.rs"
```

---

### Task 5: Single copy of the param allowlists in `src/params.rs`

`mcp/src/params.rs` and `src/api.rs:39-66` are the same logic with different error types. After Task 3 both can return `RedditError`.

**Files:**
- Create: `src/params.rs`
- Modify: `src/lib.rs` (add `pub mod params;`)
- Test: inline `#[cfg(test)] mod tests` in `src/params.rs`

**Interfaces:**
- Consumes: `crate::errors::{ErrorKind, RedditError}`.
- Produces:
  - `pub const MAX_QUERY_LEN: usize = 120; pub const MAX_LIMIT: u32 = 100; pub const DEFAULT_LIMIT: u32 = 25;`
  - `pub fn clamp_limit(limit: Option<u32>) -> u32`
  - `pub fn validate_query(query: &str) -> Result<String, RedditError>`
  - `pub fn sanitize_name(name: &str) -> Result<String, RedditError>`
  - `pub fn require_allowed<'a>(value: &str, allowed: &[&'a str]) -> Result<&'a str, RedditError>`
  - `pub fn parse_post_url(url: &str) -> Result<(String, String), RedditError>`

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn clamps_limit_into_range() {
    assert_eq!(clamp_limit(None), 25);
    assert_eq!(clamp_limit(Some(0)), 1);
    assert_eq!(clamp_limit(Some(500)), 100);
    assert_eq!(clamp_limit(Some(42)), 42);
}

#[test]
fn parses_post_urls() {
    assert_eq!(parse_post_url("https://www.reddit.com/r/rust/comments/abc123/title/").unwrap(), ("rust".into(), "abc123".into()));
    assert_eq!(parse_post_url("https://old.reddit.com/r/rust/comments/abc123/title/").unwrap(), ("rust".into(), "abc123".into()));
    assert!(parse_post_url("https://example.com/nope").is_err());
}
```

Copy the remaining five from `mcp/src/params.rs:76-111` (`rejects_empty_query`, `rejects_overlong_query`, `trims_query`, `sanitizes_subreddit_names`, `requires_allowlisted_value`) with `ErrorKind::BadInput` unchanged. The source misspells that last one as `requires_allisted_value`; use the corrected name.

- [ ] **Step 2: Run the test to verify it fails**

Run: `pwsh -File dev-build.ps1 test --lib params::`
Expected: FAIL — `unresolved import crate::params`.

- [ ] **Step 3: Create `src/params.rs`**

Copy `mcp/src/params.rs` verbatim; change `use crate::error::{ErrorKind, FetchError}` to `use crate::errors::{ErrorKind, RedditError}` and every `FetchError` to `RedditError`. The test module has its own `use crate::error::ErrorKind;` — change that to `crate::errors::ErrorKind` as well, or the tests will not compile.

One deliberate deviation, ruled during review: the source extracts the path with `url.split("reddit.com/").nth(1)`, which silently requires that literal substring, so it rejects redlib-mirror URLs while its error message claims to validate the host. Replace that with host-agnostic extraction — split off the scheme, take everything from the first `/` — and document that the host is ignored because only `/r/<sub>/comments/<id>` is used and both values are revalidated. Add an id length cap (≤ 16 bytes) while there: the id is otherwise the only unbounded value that reaches the upstream path.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `pwsh -File dev-build.ps1 test --lib params::`
Expected: PASS, 7 passed.

- [ ] **Step 5: Commit**

```bash
git add src/params.rs src/lib.rs
git commit -m "refactor: single param allowlist copy in src/params.rs"
```

---

### Task 6: Move the MCP implementation to `src/mcp/` (tools, fetch, pacer)

Mechanical relocation. Nothing about tool behaviour changes: the four `#[tool]` functions, their descriptions and their argument structs stay byte-identical.

**Files:**
- Create: `src/mcp/mod.rs`, `src/mcp/tools.rs`, `src/mcp/fetch.rs`, `src/mcp/pace.rs`
- Modify: `src/lib.rs` (add `pub mod mcp;`)
- Reference (delete in Task 12): `mcp/src/tools.rs`, `mcp/src/fetch.rs`, `mcp/src/pace.rs`, `mcp/src/normalize.rs`

**Interfaces:**
- Consumes: `crate::errors::{ErrorKind, RedditError, map_client_err}`, `crate::normalize::{cursor, listing_posts, listing_out, post_and_comments, ListingOut, PostPage}`, `crate::params::*`.
- Produces:
  - `src/mcp/mod.rs`: `pub mod fetch; pub mod http; pub mod pace; pub mod stdio; pub mod tools; pub use tools::RedlibServer;` — declare `http` and `stdio` in Task 7/10; for this task declare only `fetch`, `pace`, `tools` and re-export `RedlibServer`.
  - `src/mcp/tools.rs`: `pub struct RedlibServer` with `pub fn new() -> Self`, and the four private tool methods `reddit_search`, `reddit_subreddit`, `reddit_post`, `reddit_user`; argument structs `SearchArgs`, `SubredditArgs`, `PostArgs`, `UserArgs`.
  - `src/mcp/fetch.rs`: `pub fn encode(value: &str) -> String`, `pub async fn get(path: &str) -> Result<Value, RedditError>`.
  - `src/mcp/pace.rs`: `pub struct Pacer`, `pub fn new(min_ms: u64) -> Self`, `pub async fn wait(&self)`, `pub fn from_env() -> Pacer`.

- [ ] **Step 1: Create the four files**

Copy each source file verbatim, then rewrite only the import paths:

| File | Old imports | New imports |
|---|---|---|
| `tools.rs` | `crate::error::{ErrorKind, FetchError}`, `crate::fetch`, `crate::normalize::...`, `crate::params::*` | `crate::errors::{ErrorKind, RedditError}`, `crate::mcp::fetch`, `crate::normalize::{listing_out, post_and_comments, ListingOut, PostPage}`, `crate::params::*` |
| `fetch.rs` | `crate::error::{map_client_err, FetchError}`, `crate::pace::Pacer` | `crate::errors::{map_client_err, RedditError}`, `crate::mcp::pace::Pacer` |
| `pace.rs` | none | none |

Two rewrites in `fetch.rs` are not imports, so the table misses them:

- `PACER`'s initialiser calls `crate::pace::from_env` — change to `crate::mcp::pace::from_env`.
- `get()` calls `redlib::client::json(...)` — a crate cannot name itself, so change to `crate::client::json(...)`.

After `listing_result` delegates to `listing_out`, `cursor` and `listing_posts` have no remaining callers, so drop them from `tools.rs`'s import list (otherwise `cargo clippy -- -D warnings` fails on unused imports).

In `tools.rs` also replace every `FetchError` with `RedditError`, replace the body of the private `listing_result` helper with `listing_out(listing)` (it already returns `ListingOut`), and keep `sorted_path` unchanged.

- [ ] **Step 2: Verify the moved unit tests compile and pass**

Run: `pwsh -File dev-build.ps1 test --lib "mcp::"`
Expected: PASS — `fetch::tests` 2 passed, `pace::tests` 2 passed.

- [ ] **Step 3: Verify the whole crate still builds with the MCP types linked in**

Run: `pwsh -File dev-build.ps1 build`
Expected: success, no unused-import warnings for `rmcp`.

- [ ] **Step 4: Commit**

```bash
git add src/mcp src/lib.rs
git commit -m "refactor: move MCP tools, fetch and pacer into src/mcp"
```

---

### Task 7: `redlib mcp` serves stdio

**Files:**
- Create: `src/mcp/stdio.rs`
- Modify: `src/mcp/mod.rs` (add `pub mod stdio;`)
- Modify: `src/main.rs:117-158` (add the subcommand) and after the `get_matches()` call
- Create: `tests/handshake.rs` (moved from `mcp/tests/handshake.rs`)
- Create: `tests/live.rs` (moved from `mcp/tests/live.rs`)

**Interfaces:**
- Consumes: `crate::mcp::tools::RedlibServer`.
- Produces: `pub async fn serve() -> Result<(), Box<dyn std::error::Error + Send + Sync>>` in `src/mcp/stdio.rs`; `redlib mcp` CLI path.

- [ ] **Step 1: Write the failing integration test**

Copy `mcp/tests/handshake.rs` to `tests/handshake.rs` and change the spawn call, then harden the harness. The original blocks forever in `read_line` if the server never answers, and never drains `stderr`, so a startup panic is reported as an unexplained hang. Write `tests/handshake.rs` as:

```rust
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

const READ_TIMEOUT: Duration = Duration::from_secs(60);

pub struct Harness {
	pub child: Child,
	pub stdin: ChildStdin,
	lines: Receiver<String>,
	stderr_buf: Arc<Mutex<String>>,
	stderr_thread: Option<JoinHandle<()>>,
}

impl Harness {
	pub fn spawn() -> Self {
		let exe = env!("CARGO_BIN_EXE_redlib");
		let mut child = Command::new(exe)
			.arg("mcp")
			.stdin(Stdio::piped())
			.stdout(Stdio::piped())
			.stderr(Stdio::piped())
			.spawn()
			.expect("spawn redlib mcp");
		let stdin = child.stdin.take().expect("stdin");

		let (tx, lines) = mpsc::channel();
		let stdout = child.stdout.take().expect("stdout");
		thread::spawn(move || {
			for line in BufReader::new(stdout).lines() {
				let Ok(line) = line else { break };
				if tx.send(line).is_err() {
					break;
				}
			}
		});

		let stderr_buf = Arc::new(Mutex::new(String::new()));
		let sink = Arc::clone(&stderr_buf);
		let mut stderr = child.stderr.take().expect("stderr");
		let stderr_thread = thread::spawn(move || {
			let mut buf = Vec::new();
			let _ = stderr.read_to_end(&mut buf);
			sink.lock().unwrap_or_else(|e| e.into_inner()).push_str(&String::from_utf8_lossy(&buf));
		});

		Self {
			child,
			stdin,
			lines,
			stderr_buf,
			stderr_thread: Some(stderr_thread),
		}
	}

	pub fn send(&mut self, payload: &str) {
		self.stdin.write_all(payload.as_bytes()).expect("write request");
		self.stdin.write_all(b"\n").expect("newline");
		self.stdin.flush().expect("flush");
	}

	pub fn read_line(&mut self) -> String {
		match self.lines.recv_timeout(READ_TIMEOUT) {
			Ok(line) => line,
			Err(RecvTimeoutError::Timeout) => panic!("no response within {READ_TIMEOUT:?}; stderr:\n{}", self.stderr_text()),
			Err(RecvTimeoutError::Disconnected) => panic!("server closed stdout before responding; stderr:\n{}", self.stderr_text()),
		}
	}

	/// Kill the child so its stderr pipe closes, then return whatever it wrote.
	pub fn stderr_text(&mut self) -> String {
		let _ = self.child.kill();
		let _ = self.child.wait();
		if let Some(handle) = self.stderr_thread.take() {
			let _ = handle.join();
		}
		self.stderr_buf.lock().unwrap_or_else(|e| e.into_inner()).clone()
	}

	pub fn initialize(&mut self) -> String {
		self.send(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"test","version":"0.0.1"}}}"#);
		let line = self.read_line();
		self.send(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#);
		line
	}
}

impl Drop for Harness {
	fn drop(&mut self) {
		let _ = self.child.kill();
		let _ = self.child.wait();
	}
}

#[test]
fn initialize_handshake_and_stdout_is_pure() {
	let mut h = Harness::spawn();
	let line = h.initialize();
	assert!(line.contains("protocolVersion"), "unexpected response: {line}");
	assert!(!line.contains("Starting Redlib"), "stdout carried a non-protocol line: {line}");
}
```

Copy `mcp/tests/live.rs` to `tests/live.rs` unchanged (it declares `mod handshake;`).

- [ ] **Step 2: Run the test to verify it fails**

Run: `pwsh -File dev-build.ps1 test --test handshake`
Expected: FAIL — `unexpected argument 'mcp' found` or a non-protocol line on stdout.

- [ ] **Step 3: Create `src/mcp/stdio.rs`**

```rust
pub async fn serve() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use rmcp::ServiceExt;
    std::sync::LazyLock::force(&crate::config::CONFIG);
    // Do NOT force OAUTH_CLIENT here: its initialiser does a live Reddit round
    // trip (and can take tens of seconds), which would delay `initialize` past a
    // client's timeout. The first tool call forces it lazily instead — safe
    // because `client.rs` uses `futures_lite::block_on`, which can run inside a
    // tokio task.
    // stdout carries MCP protocol frames only. Never println! on this path.
    let service = match crate::mcp::tools::RedlibServer::new().serve((tokio::io::stdin(), tokio::io::stdout())).await {
        Ok(service) => service,
        // A client that closes stdin before initialising is a normal shutdown,
        // not a server error (e.g. `redlib mcp < /dev/null`).
        Err(rmcp::service::ServerInitializeError::ConnectionClosed(_)) => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    service.waiting().await?;
    Ok(())
}
```

- [ ] **Step 4: Add the subcommand in `src/main.rs`**

Add `.subcommand(Command::new("mcp").about("Serve MCP over stdio").version(env!("CARGO_PKG_VERSION")))` to the `Command::new("Redlib")` builder chain. `.version` is what makes `redlib mcp --version` work; `.about` is inert because clap's `help`/`usage` features are off, but keep it for the day they are enabled. Immediately after `let matches = ...get_matches();`, before the rate-limit check:

```rust
if matches.subcommand_matches("mcp").is_some() {
    if let Err(e) = redlib::mcp::stdio::serve().await {
        eprintln!("mcp server error: {e}");
        std::process::exit(1);
    }
    return;
}
```

This branch must run before `rate_limit_check()` (no OAuth work for a stdio server) and before the `Starting Redlib...` line. `pretty_env_logger::init()` already ran and writes to stderr, so stdout stays protocol-pure.

- [ ] **Step 5: Run the test to verify it passes**

Run: `pwsh -File dev-build.ps1 test --test handshake`
Expected: PASS — `initialize_handshake_and_stdout_is_pure`.

- [ ] **Step 6: Verify bare `redlib` still parses its flags**

Run: `pwsh -File dev-build.ps1 build; .\target\debug\redlib.exe --ipv4-only --version; $null | .\target\debug\redlib.exe mcp`

Expected:
- `Redlib 0.36.0` — the web flags still parse alongside the new subcommand (`--version` short-circuits after parsing).
- The `mcp` invocation no longer reports `unexpected argument`; closing stdin before `initialize` is treated as a clean shutdown: exit code 0 and empty stderr.

`--help` is not available: `clap` is built with `default-features = false` and only `std`, `env`, `derive`, so the `help`/`usage` features are off. Do not add them in this task.

- [ ] **Step 7: Commit**

```bash
git add src/mcp/stdio.rs src/mcp/mod.rs src/main.rs tests/handshake.rs tests/live.rs
git commit -m "feat: serve MCP over stdio from the redlib binary"
```

### Task 8: `src/api.rs` consumes the shared modules

Delete the three duplicated copies from `api.rs` so the REST surface and the MCP tools provably run the same code.

**Files:**
- Modify: `src/api.rs` (delete lines 39-66 params, 97-157 error taxonomy, 159-345 normalizers; rewrite imports)
- Test: the existing 30 tests in `src/api.rs:526-810` must pass unchanged except the deletions noted below

**Interfaces:**
- Consumes: `crate::errors::{ErrorKind, RedditError, map_client_err}`, `crate::normalize::{cursor, listing_out, listing_posts, post_and_comments, PostOut, PostPage, COMMENT_BODY_CAP}`, `crate::params::{clamp_limit, require_allowed, sanitize_name, validate_query}`.
- Produces: `api.rs` keeps only the HTTP surface — `enabled`, `Bucket`, `rate_limit_take`, the four path builders, the request helpers, `json_response`, `err_response`, the five handlers — and re-exports nothing.

- [ ] **Step 1: Confirm the current test count before touching anything**

Run: `pwsh -File dev-build.ps1 test --lib api::`
Expected: PASS, 30 passed. Record the number; it must be identical afterwards.

- [ ] **Step 2: Delete the duplicated blocks**

Remove from `src/api.rs`:

- `clamp_limit`, `validate_query`, `sanitize_name`, `require_allowed` (lines 39-66) and the constants `DEFAULT_LIMIT`, `MAX_LIMIT`, `MAX_QUERY_LEN` (lines 16-18)
- `ErrorKind`, its `impl`, `ApiError`, its `impl`, `map_client_err` (lines 97-157)
- `PostOut`, `CommentOut`, `ListingOut`, `PostPage` (lines 159-203), `str_field`, `int_field`, `flair`, `cap` (lines 205-225), `normalize_post`, `listing_posts`, `cursor`, `listing_out` (lines 227-268), `normalize_comment`, `prune_bfs`, `post_and_comments` (lines 270-345)
- `COMMENT_BODY_CAP`, `POST_BODY_CAP`, `TRUNCATION_MARK` (lines 13-15)

Keep `encode` (line 347) — it is api's own and differs from MCP's only in being private; leave it.

- [ ] **Step 3: Rewrite the imports and call sites**

Replace the header imports with:

```rust
use crate::errors::{map_client_err, ErrorKind, RedditError};
use crate::normalize::{listing_out, post_and_comments};
use crate::params::{clamp_limit, require_allowed, sanitize_name, validate_query};
```

Also delete `use serde::{Deserialize, Serialize};` — its only users are the four structs removed in Step 2, so it would be an unused import. The shared `normalize` types carry their own derives.

Then: `ApiError` → `RedditError`; `e.status` → `e.status()`; `err_response(e: RedditError)` uses `e.status()`. This affects three call sites beyond the handlers: `gate()` and `not_found()` construct errors, and every handler forwards `map_client_err(e)`. `listing_out(&value)` now returns `ListingOut`, and the existing `post_and_comments` call site already wraps a struct, so both need serialising. Handlers return `Result<Response<Body>, String>`, so propagate the failure instead of swallowing it — `unwrap_or_default()` would answer HTTP 200 with the body `null`:

```rust
Ok(v) => json_response(200, serde_json::to_value(listing_out(&v)).map_err(|e| e.to_string())?),
```

Apply that to all three `listing_out` sites (`subreddit`, `search`, `user`) and the same shape to the `post_and_comments` site in `comments`, replacing its current `unwrap_or_else(|_| json!({ "ok": false, "kind": "PARSE_ERROR" }))`. `json!` is still needed by `health`, so keep the `serde_json::{json, Value}` import.

- [ ] **Step 4: Update the tests that referenced deleted private helpers**

`error_kinds_map_to_status`, `error_json_envelope`, `maps_*`, `unknown_text_falls_back_to_parse`, `normalizes_post_and_cursor`, `flair_*`, `skips_children_*`, `builds_nested_*`, `marks_deleted_*`, `bfs_budget_*`, `body_caps_apply`, `returns_none_*`, `clamps_limit_into_range`, `rejects_empty_query`, `rejects_overlong_query`, `trims_query`, `sanitizes_subreddit_names`, `requires_allowlisted_value` are now duplicated by Tasks 3-5. Delete those copies from `src/api.rs` (the shared modules own them). Keep in `api.rs`: `rate_bucket_allows_capacity_then_blocks`, `rate_bucket_refills_over_window`, `subreddit_path_defaults_to_hot`, `subreddit_path_honors_top_time_and_limit`, `subreddit_path_rejects_bad_input`, `user_path_defaults_to_new`, `search_path_encodes_query_and_restricts_to_subreddit`, `post_path_includes_sort_and_comment` — 8 tests. Leave `parses_enabled_flag` in place for now; Task 9 deletes it.

- [ ] **Step 5: Run the tests to verify they pass**

Run: `pwsh -File dev-build.ps1 test --lib -- --skip test_rate_limit_check --skip test_generic_web_backend`
Expected: PASS, 0 failed. `api::` now reports 9 passed (8 HTTP-surface tests plus `parses_enabled_flag`, which Task 9 replaces); `errors::` 14, `normalize::` 12, `params::` 7, `mcp::` 5.

- [ ] **Step 6: Commit**

```bash
git add src/api.rs
git commit -m "refactor(api): use shared normalize, params and errors modules"
```

---

### Task 9: `REDLIB_API_TOKEN` is the master switch

Retire `REDLIB_API_ENABLED`. A non-empty token turns `/api/v1` on and requires a bearer credential on every route except `/health`.

**Files:**
- Modify: `src/api.rs` (`enabled`, delete `parse_enabled`, `gate`, `err_response`, the five handler signatures' gate calls, module doc comment lines 1-3)
- Test: inline `#[cfg(test)] mod tests` in `src/api.rs`

**Interfaces:**
- Consumes: `crate::errors::{ErrorKind, RedditError}`.
- Produces:
  - `pub fn token() -> Option<String>` — `REDLIB_API_TOKEN`, trimmed; `None` when unset or empty after trim
  - `pub fn enabled() -> bool` — `token().is_some()`
  - `pub(crate) fn bearer_matches(header: Option<&str>, expected: &str) -> bool` — constant-time, scheme case-insensitive; `pub(crate)` because the MCP listener in Task 10 uses it
  - `fn gate(req: &Request<Body>) -> Result<(), RedditError>`
  - `pub(crate) fn rate_limit_take() -> bool` — visibility raised for the MCP HTTP listener in Task 10
  - `err_response` adds `WWW-Authenticate: Bearer` whenever the status is 401

- [ ] **Step 1: Write the failing tests**

Replace `parses_enabled_flag` with:

```rust
use sealed_test::prelude::*;

#[sealed_test]
fn token_absent_means_disabled() {
    std::env::remove_var("REDLIB_API_TOKEN");
    assert!(token().is_none());
    assert!(!enabled());
}

#[sealed_test]
fn token_present_means_enabled() {
    std::env::set_var("REDLIB_API_TOKEN", "s3cret");
    assert_eq!(token().as_deref(), Some("s3cret"));
    assert!(enabled());
}

#[sealed_test]
fn whitespace_only_token_behaves_as_unset() {
    std::env::set_var("REDLIB_API_TOKEN", "   ");
    assert!(token().is_none(), "whitespace-only token must not enable the surfaces");
    assert!(!enabled());
}

#[sealed_test]
fn empty_token_behaves_as_unset() {
    std::env::set_var("REDLIB_API_TOKEN", "");
    assert!(token().is_none());
    assert!(!enabled());
}

#[test]
fn bearer_matches_accepts_exact_token() {
    assert!(bearer_matches(Some("Bearer s3cret"), "s3cret"));
    assert!(bearer_matches(Some("bearer s3cret"), "s3cret"), "scheme is case-insensitive");
}

#[test]
fn bearer_matches_rejects_malformed_headers() {
    assert!(!bearer_matches(None, "s3cret"));
    assert!(!bearer_matches(Some("s3cret"), "s3cret"), "scheme is mandatory");
    assert!(!bearer_matches(Some("Basic s3cret"), "s3cret"), "wrong scheme");
    assert!(!bearer_matches(Some("Bearer wrong"), "s3cret"), "wrong token");
    assert!(!bearer_matches(Some("Bearer  s3cret"), "s3cret"), "double space is not the token");
    assert!(!bearer_matches(Some("Bearer s3cret "), "s3cret"), "trailing space is not the token");
    assert!(!bearer_matches(Some("Bearer "), "s3cret"), "empty credential");
}

#[test]
fn unauthorized_error_carries_www_authenticate() {
    let res = err_response(RedditError::new(ErrorKind::Unauthorized, "missing bearer token")).unwrap();
    assert_eq!(res.status(), 401);
    assert_eq!(res.headers().get("WWW-Authenticate").unwrap(), "Bearer");
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `pwsh -File dev-build.ps1 test --lib api::`
Expected: FAIL — `cannot find function token`, `bearer_matches`, and `sealed_test` unresolved.

- [ ] **Step 3: Implement the switch**

Add `use hyper::header::HeaderValue;` to the imports (the challenge header below needs it).

```rust
pub fn token() -> Option<String> {
    std::env::var("REDLIB_API_TOKEN").ok().map(|v| v.trim().to_owned()).filter(|v| !v.is_empty())
}

pub fn enabled() -> bool { token().is_some() }
```

`bearer_matches` splits the header once on a single space, compares the scheme with `eq_ignore_ascii_case("bearer")`, then compares bytes with an XOR accumulator over equal-length slices and rejects differing lengths without early exit on content:

```rust
pub(crate) fn bearer_matches(header: Option<&str>, expected: &str) -> bool {
    let Some((scheme, credential)) = header.and_then(|h| h.split_once(' ')) else { return false };
    if !scheme.eq_ignore_ascii_case("bearer") { return false }
    let (a, b) = (credential.as_bytes(), expected.as_bytes());
    let mut diff = (a.len() ^ b.len()) as u8;
    for i in 0..a.len().min(b.len()) { diff |= a[i] ^ b[i]; }
    diff == 0
}
```

`gate` becomes:

```rust
fn gate(req: &Request<Body>) -> Result<(), RedditError> {
    let expected = token().ok_or_else(|| RedditError::new(ErrorKind::Unauthorized, "API is disabled"))?;
    let header = req.headers().get(hyper::header::AUTHORIZATION).and_then(|v| v.to_str().ok());
    if !bearer_matches(header, &expected) {
        return Err(RedditError::new(ErrorKind::Unauthorized, "missing or invalid bearer token"));
    }
    if !rate_limit_take() {
        return Err(RedditError::new(ErrorKind::RateLimited, "rate limit exceeded: 30 requests/minute"));
    }
    Ok(())
}
```

Every handler changes `gate()` to `gate(&req)`. `err_response` adds the challenge header:

```rust
fn err_response(e: RedditError) -> Result<Response<Body>, String> {
    let status = e.status();
    let mut res = json_response(status, e.to_json())?;
    if status == 401 {
        if let Ok(v) = HeaderValue::from_str("Bearer") { res.headers_mut().insert("WWW-Authenticate", v); }
    }
    Ok(res)
}
```

`health` and `not_found` do not call `gate`. Update the module doc comment to say the switch is `REDLIB_API_TOKEN` and that auth is required.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `pwsh -File dev-build.ps1 test --lib -- --skip test_rate_limit_check --skip test_generic_web_backend`
Expected: PASS, 0 failed. `api::` reports 15 passed — Task 8's 9 minus `parses_enabled_flag` plus the 7 new tests. Full lib: 108 passed.

- [ ] **Step 5: Verify the retired variable is gone**

Run: `git grep -n "REDLIB_API_ENABLED" -- src`
Expected: no output. (There is no `.env.example` in the repo. The retired name survives only in `docs/superpowers/specs/2026-10-09-redlib-mcp-design.md`, a historical design document; leave it.)

- [ ] **Step 6: Commit**

```bash
git add src/api.rs
git commit -m "feat(api): gate /api/v1 behind REDLIB_API_TOKEN bearer auth"
```

---

### Task 10: MCP Streamable HTTP listener with bearer auth

`rmcp` 3.5.1 API, verified against the vendored source: `rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService}`, `rmcp::transport::streamable_http_server::session::local::LocalSessionManager` (`#[derive(Default)]`), and `StreamableHttpService::new(factory, Arc<LocalSessionManager>, config)`; the service exposes `pub async fn handle<B>(&self, request: Request<B>) -> Response<BoxBody<Bytes, Infallible>>`, so no `tower::Service` plumbing is needed.

**Files:**
- Create: `src/mcp/http.rs`
- Modify: `src/mcp/mod.rs` (add `pub mod http;`)

**Interfaces:**
- Consumes: `crate::api::{rate_limit_take, token}`, `crate::errors::{ErrorKind, RedditError}`, `crate::mcp::tools::RedlibServer`.
- Produces:
  - `pub fn resolve_port(raw: &str) -> Result<Option<u16>, String>` — `"off"` (case-insensitive, trimmed) → `Ok(None)`; a valid `u16` → `Ok(Some(n))`; anything else → `Err`
  - `pub fn parse_allowed_hosts(raw: Option<&str>) -> Vec<String>` — comma-split, trimmed, empties dropped; empty result falls back to `["localhost", "127.0.0.1", "::1"]`
  - `pub async fn serve(bind: String, token: String) -> Result<(), Box<dyn std::error::Error + Send + Sync>>`

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn resolves_off_to_no_listener() {
    assert_eq!(resolve_port("off").unwrap(), None);
    assert_eq!(resolve_port(" OFF ").unwrap(), None);
}

#[test]
fn resolves_numeric_port() {
    assert_eq!(resolve_port("9090").unwrap(), Some(9090));
    assert_eq!(resolve_port("0").unwrap(), Some(0));
}

#[test]
fn rejects_non_numeric_port_instead_of_defaulting() {
    assert!(resolve_port("90o0").is_err(), "a typo must not silently become 9090");
    assert!(resolve_port("").is_err());
    assert!(resolve_port("70000").is_err(), "out of u16 range");
}

#[test]
fn allowed_hosts_default_to_loopback() {
    assert_eq!(parse_allowed_hosts(None), vec!["localhost", "127.0.0.1", "::1"]);
    assert_eq!(parse_allowed_hosts(Some("   ")), vec!["localhost", "127.0.0.1", "::1"]);
}

#[test]
fn allowed_hosts_parse_a_comma_separated_list() {
    assert_eq!(parse_allowed_hosts(Some("example.com, example.com:8080 ,,")),
               vec!["example.com", "example.com:8080"]);
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `pwsh -File dev-build.ps1 test --lib "mcp::http"`
Expected: FAIL — `unresolved import crate::mcp::http`.

- [ ] **Step 3: Implement `src/mcp/http.rs`**

```rust
pub async fn serve(bind: String, token: String) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let config = StreamableHttpServerConfig {
        allowed_hosts: parse_allowed_hosts(std::env::var("REDLIB_MCP_ALLOWED_HOSTS").ok().as_deref()),
        ..Default::default()
    };
    let service = StreamableHttpService::new(
        || Ok(crate::mcp::tools::RedlibServer::new()),
        std::sync::Arc::new(LocalSessionManager::default()),
        config,
    );
    let listener = tokio::net::TcpListener::bind(&bind).await?;
    loop {
        let (stream, _) = listener.accept().await?;
        let io = hyper_util::rt::TokioIo::new(stream);
        let svc = service.clone();
        let token = token.clone();
        tokio::spawn(async move {
            let conn = hyper1::server::conn::http1::Builder::new().serve_connection(io, hyper1::service::service_fn(move |req| {
                let svc = svc.clone();
                let token = token.clone();
                async move { Ok::<_, std::convert::Infallible>(dispatch(svc, token, req).await) }
            }));
            let _ = conn.await;
        });
    }
}
```

`dispatch` returns `hyper1::Response<BoxBody<Bytes, Infallible>>` and performs, in order: 404 unless `req.uri().path() == "/mcp"`; 401 unless `crate::api::bearer_matches(header, &token)` holds, where `header` is `req.headers().get(hyper1::header::AUTHORIZATION).and_then(|v| v.to_str().ok())`; 429 unless `crate::api::rate_limit_take()`; otherwise `svc.handle(req).await`. Build the three error responses with `Full::new(Bytes::from(...)).map_err(|e| match e {}).boxed()`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `pwsh -File dev-build.ps1 test --lib "mcp::http"`
Expected: PASS, 5 passed.

- [ ] **Step 5: Commit**

```bash
git add src/mcp/http.rs src/mcp/mod.rs
git commit -m "feat(mcp): Streamable HTTP listener with bearer auth"
```

---

### Task 11: Wire both listeners in `main.rs`

**Files:**
- Modify: `src/main.rs` (clap args around lines 117-158; listener startup around lines 174-187 and 422-430)

**Interfaces:**
- Consumes: `redlib::api::{enabled, token}`, `redlib::mcp::http::{resolve_port, serve}`.
- Produces: bare `redlib` with a token starts both listeners; `--mcp-http` / `REDLIB_MCP_HTTP_PORT` select the MCP port.

- [ ] **Step 1: Write the failing test**

Add to `src/main.rs` a pure helper and test it (the binary target has no test module yet; add one at the bottom):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_mcp_port_equal_to_web_port() {
        assert!(ensure_distinct(9090, Some(9090)).is_err());
        assert!(ensure_distinct(8080, Some(9090)).is_ok());
        assert!(ensure_distinct(8080, None).is_ok());
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `pwsh -File dev-build.ps1 test --bin redlib`
Expected: FAIL — `cannot find function ensure_distinct`.

- [ ] **Step 3: Add the CLI arg and the helper**

Add to the builder chain, next to `port`:

```rust
.arg(
    Arg::new("mcp-http")
        .long("mcp-http")
        .value_name("PORT")
        .env("REDLIB_MCP_HTTP_PORT")
        .help("Port for the MCP Streamable HTTP listener, or 'off' to disable it")
        .default_value("9090")
        .action(ArgAction::Set)
        .num_args(1),
)
```

And a pure helper:

```rust
fn ensure_distinct(web: u16, mcp: Option<u16>) -> Result<(), String> {
    match mcp {
        Some(p) if p == web => Err(format!("MCP port {p} equals the web port; set REDLIB_MCP_HTTP_PORT to a different port or 'off'")),
        _ => Ok(()),
    }
}
```

- [ ] **Step 4: Start both listeners**

Replace the tail of `main` (lines 422-430). After the routes are registered and before `app.listen`:

```rust
let web_port: u16 = port.parse().map_err(|_| format!("invalid web port: {port}")).unwrap_or_else(|e| { eprintln!("{e}"); std::process::exit(1) });
let mcp_raw = matches.get_one::<String>("mcp-http").map(String::as_str).unwrap_or("9090");
let mcp_port = match redlib::api::token() {
    None => None,
    Some(_) => match redlib::mcp::http::resolve_port(mcp_raw) {
        Ok(p) => p,
        Err(e) => { eprintln!("invalid REDLIB_MCP_HTTP_PORT: {e}"); std::process::exit(1); }
    },
};
if let Err(e) = ensure_distinct(web_port, mcp_port) {
    eprintln!("{e}");
    std::process::exit(1);
}
```

Both checks run before any bind, so a collision exits with status 1 and no half-started process.

Then start:

```rust
let web = app.listen(&listener);
match mcp_port {
    None => { if let Err(e) = web.await { eprintln!("Server error: {e}"); } }
    Some(p) => {
        let bind = if ipv4_only { format!("0.0.0.0:{p}") } else if ipv6_only { format!("[::]:{p}") } else { format!("{address}:{p}") };
        let token = redlib::api::token().expect("token present when mcp_port is Some");
        println!("MCP Streamable HTTP on {bind}/mcp");
        tokio::select! {
            r = web => { if let Err(e) = r { eprintln!("Server error: {e}"); } }
            r = redlib::mcp::http::serve(bind, token) => { if let Err(e) = r { eprintln!("MCP server error: {e}"); } }
        }
    }
}
```

Keep the existing `println!("Running Redlib v{} on {listener}!", ...)` line above the select.

- [ ] **Step 5: Run the test to verify it passes**

Run: `pwsh -File dev-build.ps1 test --bin redlib`
Expected: PASS, 1 passed.

- [ ] **Step 6: Verify no-token behaviour is unchanged**

```powershell
pwsh -File dev-build.ps1 build
$env:REDLIB_API_TOKEN = $null
Start-Process .\target\debug\redlib.exe -ArgumentList '--port','8080'
Start-Sleep 3
(Invoke-WebRequest http://127.0.0.1:8080/api/v1/r/rust -SkipHttpErrorCheck).StatusCode
(Test-NetConnection 127.0.0.1 -Port 9090 -InformationLevel Quiet)
Stop-Process -Name redlib
```

Expected: `/api/v1/r/rust` → `404` (route not registered), `9090` → `False`, web UI at `/` → `200`.

- [ ] **Step 7: Verify the token path**

```powershell
$env:REDLIB_API_TOKEN = 'testtoken'
Start-Process .\target\debug\redlib.exe -ArgumentList '--port','8080'
Start-Sleep 3
(Invoke-WebRequest http://127.0.0.1:8080/api/v1/health -SkipHttpErrorCheck).StatusCode      # 200, no token
(Invoke-WebRequest http://127.0.0.1:8080/api/v1/r/rust -SkipHttpErrorCheck).StatusCode      # 401
(Invoke-WebRequest http://127.0.0.1:8080/api/v1/r/rust -Headers @{Authorization='Bearer testtoken'} -SkipHttpErrorCheck).StatusCode   # 200
(Test-NetConnection 127.0.0.1 -Port 9090 -InformationLevel Quiet)                            # True
Stop-Process -Name redlib
```

- [ ] **Step 8: Verify the off switch and the collision error**

```powershell
$env:REDLIB_API_TOKEN='testtoken'; $env:REDLIB_MCP_HTTP_PORT='off'
Start-Process .\target\debug\redlib.exe -ArgumentList '--port','8080'; Start-Sleep 3
(Test-NetConnection 127.0.0.1 -Port 9090 -InformationLevel Quiet)   # False
Stop-Process -Name redlib
$env:REDLIB_MCP_HTTP_PORT='8080'
.\target\debug\redlib.exe --port 8080; $LASTEXITCODE              # non-zero, message about the MCP port
```

- [ ] **Step 9: Commit**

```bash
git add src/main.rs
git commit -m "feat(cli): serve web, REST and MCP HTTP from one process"
```

---

### Task 12: Delete `mcp/` and finish the migration

**Files:**
- Delete: `mcp/` (whole directory)
- Modify: `.env.example` (append the three new vars)
- Modify: `docs/superpowers/specs/2026-10-09-redlib-mcp-design.md:201-210` (mark the A3 deferral superseded)
- Modify: `opencode.json` (repo-root file, points at `redlib-mcp.exe`)

**Interfaces:**
- Consumes: everything above.
- Produces: a tree with one crate and one binary.

- [ ] **Step 1: Confirm nothing outside `mcp/` still needs it**

Run: `git grep -n -I "redlib-mcp" -- . ":(exclude)mcp" ":(exclude)docs/superpowers/plans" ":(exclude)docs/superpowers/specs"`
Expected: only `Cargo.lock` (regenerated) and `opencode.json` (edited in Step 4).

- [ ] **Step 2: Delete the crate and rebuild**

```bash
git rm -r mcp
pwsh -File dev-build.ps1 build --release
```

Expected: success. `Get-ChildItem target\release\*.exe` lists exactly one file, `redlib.exe`.

- [ ] **Step 3: Append the new variables to `.env.example`**

```sh
# Master switch for /api/v1 and the MCP HTTP listener. Unset or empty = upstream behaviour.
# Generate with: openssl rand -hex 32
REDLIB_API_TOKEN=
# Port for MCP Streamable HTTP (default 9090), or 'off' to serve REST only.
REDLIB_MCP_HTTP_PORT=9090
# Comma-separated Host authorities allowed to reach the MCP listener.
# Default localhost,127.0.0.1,::1 - a public hostname MUST be added here.
REDLIB_MCP_ALLOWED_HOSTS=localhost,127.0.0.1,::1
```

- [ ] **Step 3b: Reword the stale-token hint now that there is one binary**

In `src/errors.rs`, `ErrorKind::hint()` tells the user to "Restart the MCP server". After this task there is no separate MCP server. Change that hint to "Reddit rejected the OAuth token. Restart redlib to obtain a fresh one." and update the two assertions that embed the old text (the `to_error_data` message tests added in Task 3).

- [ ] **Step 4: Point the repo `opencode.json` at the single binary**

If `opencode.json` exists at the repo root (it is untracked, so it may be absent in this worktree — skip this step when it is), change the `redlib` server entry's command to:

```json
"command": ["C:\\Users\\ajink\\projects\\redlib\\target\\release\\redlib.exe", "mcp"]
```

- [ ] **Step 5: Mark the A3 deferral note superseded**

At `docs/superpowers/specs/2026-10-09-redlib-mcp-design.md:201-210`, replace the deferral paragraph's opening with a line reading: `> **Superseded by `2026-10-17-unified-cli-design.md` (A4).** The deferral below is historical; the merge, the dedupe and the HTTP transport all land in A4.` Leave the original text beneath it.

- [ ] **Step 6: Run the whole suite**

Run: `pwsh -File dev-build.ps1 test -- --skip test_rate_limit_check --skip test_generic_web_backend`
Expected: all targets pass, including `tests/handshake.rs`. `tests/live.rs` remains `#[ignore]`d.

- [ ] **Step 7: Run the stdio regression by hand**

Run: `.\target\release\redlib.exe mcp` then send an `initialize` frame; also confirm no token is needed and stdout carries only the JSON-RPC response.

- [ ] **Step 8: Commit**

```bash
git add -A
git commit -m "refactor: delete the mcp crate; one binary, one command"
```

---

## Self-Review

**Spec coverage.** Every section maps to a task: one artifact (12), one env switch (9), `redlib mcp` stdio (7), `normalize` once (4), params/errors once (3, 5), fail-closed auth (9, 10), shared rate limit (10), build wrapper survives (1), tests move (3-8, 12). Decisions table: binary count 12, enable rule 9, `REDLIB_API_ENABLED` retired 9, bare `redlib` unchanged 11, stdio no token 7, Streamable HTTP 10, separate port 10-11, bearer + constant time 9, env-only secret 9, allowed hosts 10, origin default untouched 10, `/health` exempt 9, pacer MCP-only 6, wrapper moved 1.

**Open spec items resolved here.** `axum` is not used: hyper 1 + `hyper_util` serve the service directly via `handle()`, which is fewer dependencies than `axum` and avoids an `IntoResponse` shim. Listener 2 reuses listener 1's address resolution. `redlib mcp` is a real clap subcommand. The MCP listener draws on `api::rate_limit_take()` rather than a second bucket. `REDLIB_MCP_ALLOWED_HOSTS` is comma-separated. Moved tests land in `tests/`.

**Type consistency.** `RedditError` (not `ApiError`, not `FetchError`) is the only error type after Task 3; `status()` is a method, not a field; `listing_out` returns `ListingOut` everywhere after Task 8; `rate_limit_take` is `pub(crate)` from Task 9 and consumed in Task 10.

**Review Focus.** All five items have a named test: (1) `allowed_hosts_parse_a_comma_separated_list` in Task 10, (2) `whitespace_only_token_behaves_as_unset` in Task 9, (3) `rejects_non_numeric_port_instead_of_defaulting` in Task 10, (4) `rejects_mcp_port_equal_to_web_port` in Task 11, (5) `bearer_matches_rejects_malformed_headers` in Task 9.

**Known limit.** Item (1) proves the parser, not rmcp's 403 behaviour. The end-to-end check is manual and belongs to the hosted deployment: run `REDLIB_MCP_ALLOWED_HOSTS=ajinkya.duckdns.org redlib` and confirm a request through the public hostname reaches `initialize` instead of 403.

