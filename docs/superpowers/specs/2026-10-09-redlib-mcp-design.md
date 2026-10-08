# redlib-mcp: in-process Reddit MCP server (A2)

Date: 2026-10-09
Status: approved
Repo: `ajinkyajacob/redlib` (fork of `redlib-org/redlib`), branch `feat/mcp-server`

## Problem

The fork runs redlib locally (Docker `:18080`, native exe `:18081`) for private browsing. opencode has no way to *read* Reddit through it. The goal is a Model Context Protocol server so opencode can search and read Reddit without logging in, without Node, and without scraping HTML.

## Decisions (locked)

| Decision | Choice | Why |
|---|---|---|
| Implementation language | Rust, rmcp 3.5.1 | User rejected Node; single native binary, no toolchain beyond cargo |
| Data path | In-process `redlib::client::json()` | No HTTP hop, one OAuth session, automatic token refresh |
| Architecture | **A2** — MCP only, no `/api/v1` HTTP routes | opencode is the only consumer; HTTP surface deferred to A3 |
| Auth | None | Local single-user trust; no bearer enforcement |
| Scope | Read-only, 4 tools | No vote/comment/submit (needs Reddit OAuth app creds + session cookies) |

## Non-goals

- No writes of any kind to Reddit.
- No public HTTP API (deferred as A3).
- No instance discovery/rotation — one local instance.
- No changes to upstream `src/` — keeps rebases cheap.

## Architecture

```
opencode ──stdio──> redlib-mcp.exe ──> redlib::client::json() ──> Reddit
                       (mcp/ crate)         (in-process, OAuth spoofed client)
```

The MCP binary links the fork's lib target. Startup forces the same two statics that
`src/main.rs:196-200` forces:

```rust
LazyLock::force(&redlib::config::CONFIG);
LazyLock::force(&redlib::client::OAUTH_CLIENT);
```

After that, `client::json()` handles OAuth lazily, tracks `x-ratelimit-remaining`, and
triggers `force_refresh_token()` on its own (`src/client.rs:323-338`). No listener, no
port, no `REDLIB_URL`.

Consequence: the MCP works with **no redlib server process running**. Stopping the Docker
container does not affect it. This is verified by a test.

## Crate layout

```
redlib/
├── Cargo.toml          # + [workspace] members = [".", "mcp"]
├── src/                # untouched
└── mcp/
    ├── Cargo.toml      # redlib = { path = ".." }, rmcp 3.5.1
    └── src/
        ├── main.rs     # CLI flags, init, stdio serve
        ├── normalize.rs# pure fns, unit-tested, no I/O
        ├── params.rs   # allowlists + clamps
        ├── fetch.rs    # Reddit path builder + client::json caller
        ├── error.rs    # ErrorKind taxonomy
        ├── pace.rs     # min-interval limiter
        └── tools.rs    # rmcp #[tool_router(server_handler)]
```

`[workspace]` exists so `mcp/` shares `target/` with the root crate and BoringSSL
builds once instead of twice.

## Tool surface

| Tool | Params | Reddit path |
|---|---|---|
| `reddit_search` | `query` (required, ≤120 chars), `subreddit?`, `sort?`, `time?`, `limit?` | `/search.json` or `/r/{sub}/search.json?restrict_sr=on` |
| `reddit_subreddit` | `subreddit`, `sort?`, `time?`, `after?`, `limit?` | `/r/{sub}/{sort}.json` |
| `reddit_post` | `subreddit` + `post_id` **or** `url`, `comment_sort?`, `max_comments?` (50), `comment_id?` | `/comments/{id}.json?sort=&depth=10` |
| `reddit_user` | `username`, `sort?`, `after?`, `limit?` | `/user/{name}/{sort}.json` |

Outputs use rmcp `Json<T>` (structured content, not stringified JSON):

```
listing → { ok, kind: "listing", result_count, cursor?, posts: [...] }
post    → { ok, kind: "post", post: {...}, comments_returned, comments: [...] }
```

`limit` clamps to 1..=100. `sort`/`time` are enums in the schema, so invalid values are
structurally impossible from the agent side; the allowlist still guards internal callers.

### Normalization rules

- Post: `id, title, subreddit, author, score, comment_count, created_utc, permalink, url, flair, over_18, is_self, selftext`
- Comment: `id, author, score, body, is_deleted, more_id?, replies[]`. No `is_op`/`is_mod` —
  those markers exist only in redlib's rendered HTML, never in Reddit's JSON, so they are
  omitted rather than faked
- Drop: `preview.images`, audit fields (`*_by`), everything not listed above
- `link_flair_text` collapses `null` and `""` to `None`
- Unknown keys are tolerated (Reddit renames fields); only `id` + `title` are required
- Comment tree: recursive over `replies` where `kind == "t1"`, `more` nodes emit
  `more_id` so the agent can expand via `comment_id`
- Cursor: `data.after` → `cursor`
- Empty results are `ok: true, result_count: 0`, not an error

### Token discipline

Carried from `grazomarin/redlib-mcp`:

- `UNTRUSTED` preamble prepended to every tool description (their `index.ts:29`) — post
  titles, bodies and comments are user-generated and must be treated as data
- Comment body cap 1200 chars, post body cap 4000 chars (their `parse.ts:9`)
- Omit `false` flags and empty `replies` (their `parse.ts:106`)
- Breadth-first comment pruning with a `max_comments` budget so one deep thread cannot
  starve the rest (their `parse.ts:120`)

## Error taxonomy

| Kind | Cause |
|---|---|
| `BAD_INPUT` | missing/unknown/oversized params, unparseable URL |
| `RATE_LIMITED` | Reddit throttling text in the error string |
| `UPSTREAM_TOKEN_STALE` | OAuth/token errors; hint "restart the MCP server" |
| `REDLIB_DOWN` | transport failure reaching Reddit |
| `CONTENT_UNAVAILABLE` | removed/gated content |
| `PARSE_ERROR` | Reddit returned JSON we cannot shape |

`client::json()` returns `Result<Value, String>`; `fetch.rs` maps the message by
substring. All failures become `CallToolResult { isError: true, content: { kind, message } }`.

## Pacing

`REDLIB_MCP_MIN_INTERVAL_MS` (default 300) gates every upstream call. Single user over
stdio, so a min-interval limiter suffices — no token bucket in A2.

## Stdout discipline

stdout carries MCP protocol frames only. Logs go to stderr. Any stray `println!` corrupts
the stream; the handshake integration test is what catches it.

## opencode wiring

`~/.config/opencode/opencode.json`:

```json
"redlib": {
  "type": "local",
  "command": ["C:\\Users\\ajink\\projects\\redlib\\target\\release\\redlib-mcp.exe"],
  "environment": { "REDLIB_MCP_MIN_INTERVAL_MS": "300" },
  "enabled": true
}
```

Requires an opencode restart to load. No `REDLIB_URL` in A2.

## Testing

| Layer | Coverage |
|---|---|
| Unit | normalize (post/comment/tree/cursor, flair `null` vs `""`), param allowlists + clamps, error substring mapping, limiter timing. Hand-trimmed JSON snippets, no network. |
| Integration | Spawn exe → `initialize` → `tools/list` returns exactly 4 → `tools/call` each against live Reddit → assert `ok`, non-empty comments, cursor present |
| Negative | bad `sort` → BAD_INPUT; `limit > 100` clamps; malformed URL → BAD_INPUT; oversized `query` → BAD_INPUT |
| Isolation | stop the Docker redlib container, re-run a tool → still succeeds |
| Stdout purity | handshake fails if non-protocol bytes reach stdout |

## Build environment

MSVC toolchain required:

```powershell
$env:PATH = "$env:USERPROFILE\.cargo\bin;$env:PATH"   # rustup MSVC, not the GNU rustc on PATH
cmd /c "call `"C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvars64.bat`" && cargo build --release -p redlib-mcp"
```

A GNU `rustc` on PATH makes `boring-sys2` fail with
`could not find native static library 'crypto'` (CMake emits `crypto.lib`; the GNU linker
wants `libcrypto.a`). `build.rs` shells out to `git rev-parse HEAD`, so `.git` must exist.

## Risks

| Risk | Mitigation |
|---|---|
| rmcp 3.x API churn (newest major) | pin `=3.5.1`; macro signatures may shift on bump |
| schemars version mismatch (`JsonSchema` derive moved between 0.8 and 1.0) | take whatever rmcp 3.5.1 requires; verify at first compile |
| `build.rs` needs git | keep `.git`; do not build from a tarball copy |
| Reddit OAuth liveness (issue #551 class) | in-process auto-refresh; `UPSTREAM_TOKEN_STALE` error tells the user to restart |
| AGPL-3.0-only | in-repo binary inherits AGPL; fine for personal self-hosting |

## Learned from prior art

| Source | What was taken | What was rejected |
|---|---|---|
| `grazomarin/redlib-mcp` | tool surface, error taxonomy, BFS prune, token caps, UNTRUSTED preamble | HTML scraping (cheerio), HTTP transport mode, instance rotation, 27 KB CLI |
| `snapetech/redlibe` | `api.rs` structure, query allowlist, 64 KiB body cap | hardcoded `hot.json` (inert `sort`), plain-text 500s, no rate limit, Docker build that fetches upstream tarball |
| `levkropp/redlib-now` | complete selector inventory, Anubis detection idea | HTML/Jsoup entirely |
| `kernelzeroday/reddit-rs` | error classification, depth-tracking idea | regex parsing — its `<p class="post_flair">` pattern matches nothing on our build (flair renders as `<a>`) |

Empirically verified against our running build: selectors above are real; `limit=5`
returns exactly 5 posts; `after=t3_…` cursors exist; `#column_one` present and `#error`
absent on listing/post/search/user pages; `/user/:name` listings are thin (2 posts in the
smoke test) so small results are normal.

## Deferred: A3 (HTTP access)

1. Add `src/api.rs` + 5 routes to the fork (`/api/v1/{r,search,u,health}`), JSON errors on
   every path, 30 rpm token bucket, `REDLIB_API_ENABLED` kill switch, honoring redlibe's
   lessons (honor `sort`, sanitize every route)
2. `normalize.rs` moves from the MCP crate into `src/api/` — it already has unit tests
3. MCP swaps `fetch` to an HTTP client, gains `REDLIB_URL`
4. Payoff: `curl` access, non-Rust consumers, remote agents

Purely additive; A2 does not block it.