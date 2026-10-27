# Unified redlib: one binary, four surfaces (A4)

Date: 2026-10-17
Status: draft — pending approval
Repo: `ajinkyajacob/redlib` (fork of `redlib-org/redlib` @ `a4d36e9`, redlib v0.36.0)
Supersedes the A3 deferral note in `2026-10-09-redlib-mcp-design.md:201-210`

## Problem

The fork now has three ways to read Reddit, split across two artifacts:

| Surface | Lives in | Needs | Consumed by |
|---|---|---|---|
| Web UI | `redlib.exe` (root crate) | `src/main.rs` + `src/server.rs` | browser |
| `/api/v1` JSON | `redlib.exe` | `src/api.rs` (gated by `REDLIB_API_ENABLED`) | `curl`, any HTTP client |
| MCP | `redlib-mcp.exe` (workspace member `mcp/`) | stdio transport | opencode |

Two artifacts means two things to build, ship, version and document. The user wants to **host this on a server and reach it from anywhere** — with MCP served over HTTP rather than spawned locally. A second binary that only speaks stdio cannot be reached over the network at all.

The real maintenance cost is not the second binary (35 lines of `main.rs`). It is the **duplicated code between the two crates**:

| Duplicated thing | Copy 1 | Copy 2 |
|---|---|---|
| Normalizers (`PostOut`, `CommentOut`, `ListingOut`, `PostPage`, `listing_posts`, `post_and_comments`, `cursor`, `cap`, `flair`) | `mcp/src/normalize.rs` (321 lines) | transcribed into `src/api.rs` |
| Allowlists + `clamp_limit`, `validate_query`, `sanitize_name`, `require_allowed`, `parse_post_url` | `mcp/src/params.rs` (112 lines) | private fns in `src/api.rs` |
| Error taxonomy | `mcp/src/error.rs` `ErrorKind`/`FetchError` | `src/api.rs` `ErrorKind`/`ApiError` |

Merging the crate into the root is what makes a single copy possible. That is the actual goal.

## Goals

1. **One artifact.** `cargo build` produces `redlib` only. `redlib-mcp.exe` and the `mcp/` crate are gone.
2. **One env var turns on the hosted surfaces.** With `REDLIB_API_TOKEN` set, bare `redlib` serves web + `/api/v1` + MCP over HTTP in one process.
3. **`redlib mcp` serves MCP over stdio**, preserving the existing local consumption path with no token.
4. **`normalize.rs` exists once**, used by both the HTTP API and the MCP tools.
5. **Params/allowlists exist once.** Same for the error taxonomy.
6. **Neither network surface can run without a token.** Fail-closed by construction, not by a check.
7. **Rate limiting is shared** between `/api/v1` and MCP HTTP.
8. **The MSVC build wrapper survives** the deletion of `mcp/`.
9. Existing tests move and pass; MCP tool behaviour is unchanged.

## Non-goals

- No authentication on the **web UI**. It keeps upstream behaviour.
- No OAuth/OIDC for MCP. Bearer token only.
- No write tools (vote/comment/submit). Read-only, as A2/A3.
- No single-port multiplexing. The MCP endpoint gets its own listener.
- No cargo feature flag for a lean web-only build. `rmcp` is always linked.
- No behavioural changes to upstream `src/` beyond additive modules.
- No A3 Phase 2 work beyond the dedupe described here.

## Anti-goals

Behaviour that would make this a failure **even if it technically works**:

- Breaking bare `redlib` for someone who never sets a token. With no token, the binary must behave exactly as upstream.
- Exposing `/api/v1` or MCP HTTP **unauthenticated** in any configuration.
- Opening the MCP port as a side effect the operator did not ask for.
- Regressing the 30 `api::` unit tests or any MCP tool's output shape.
- Making upstream rebases expensive: keep new code in new files, keep edits to upstream files minimal and localised.

## Decisions (locked)

| Decision | Choice | Why |
|---|---|---|
| Binary count | **One**: `redlib` | kills the duplicated modules above |
| Enable rule | **`REDLIB_API_TOKEN` is the master switch** | one env turns on `/api/v1` + MCP HTTP; unset ⇒ upstream behaviour |
| `REDLIB_API_ENABLED` | **retired** | redundant once the token is the switch; supersedes A3 Phase 1 |
| Bare `redlib`, no token | web UI only, exactly upstream | preserves the anti-goal |
| `redlib mcp` | stdio MCP, **no token required** | it is client-spawned, no network surface |
| Remote MCP transport | **Streamable HTTP** (`rmcp` `transport-streamable-http-server`) | MCP standard; rmcp supports it |
| MCP HTTP listener | separate port | `rmcp` is `http` 1.x, redlib is hyper 0.14 = `http` 0.2 |
| Token transport | `Authorization: Bearer <token>` | constant-time compare, 401 + `WWW-Authenticate` |
| Token source | env only — never `redlib.toml`, never a flag | a secret must not land in a repo file or `argv` |
| Host validation | `REDLIB_MCP_ALLOWED_HOSTS`, default loopback | rmcp rejects non-loopback `Host` by default |
| Origin validation | **left disabled** (rmcp's default) | non-browser clients send no `Origin` |
| Health | `/api/v1/health` token-exempt | no Reddit data; needed for probes |
| Pacer | **MCP-only**, unchanged | the shared bucket is stricter than the pacer |
| Build wrapper | moves to repo root `dev-build.ps1` | `mcp/` disappears |

### Decisions I made that the user has not explicitly seen

Flagging these so they can be rejected while reading:

1. **Default MCP port is 9090**, and `REDLIB_MCP_HTTP_PORT=off` disables the MCP listener while leaving REST on. Without the escape hatch, setting the token would always open a second port — which is the "silent port" anti-goal, one step removed.
2. **A token set to an empty string counts as unset.** Only a non-empty value enables the surfaces.
3. **MCP port equal to the web port is a startup error.** Two listeners cannot share one port; fail loudly rather than half-start.
4. **No feature flag.** `rmcp` + its tree always link into `redlib`. Simpler; costs bytes.
5. **`/api/v1/health` stays token-exempt.**
6. **Shared `params` lives in a neutral `src/params.rs`**, not inside `src/api/` as the old deferral note suggested — MCP is now a second consumer, not just the HTTP surface.
7. **No separate health route on the MCP listener.** The web listener always runs, so probes target it.

## Architecture

### Before

```
redlib.exe ──> web UI
           └─> /api/v1            (src/api.rs; own copies of normalize/params/errors)

redlib-mcp.exe ──stdio──> MCP    (mcp/src/*; own copies of normalize/params/errors)
```

### After

```
                    ┌─ web UI              (upstream src/, unchanged)
redlib (one binary) ├─ /api/v1             (src/api.rs)      ─┐
                    └─ MCP Streamable HTTP (src/mcp/http.rs)  ─┴─> src/normalize.rs
                                                                   src/params.rs
                                                                   src/errors.rs
                                                           ──> redlib::client::json()
redlib mcp ──stdio──> MCP                (same src/mcp/, stdio transport)
```

All four surfaces share one process, one tokio runtime (`src/main.rs:109` is already `#[tokio::main]`) and one in-process `redlib::client::json()` path. Nothing talks to Reddit over the loopback.

### Two listeners, not one

`rmcp`'s `StreamableHttpService` is a `tower` service over `http` **1.x** requests (`rmcp-3.5.1/src/transport/streamable_http_server/tower.rs`, importing `http_body`, `http_body_util::BodyExt`, `Full`, `BoxBody`). Redlib's server is hyper **0.14** (`Cargo.toml`: `hyper = "0.14.31"`), i.e. `http` **0.2**, with `route_recognizer` routing (`src/server.rs:172-303`).

The two `http` majors cannot be bridged inside one router without conversion glue, so:

- **Listener 1** — unchanged: `app.listen(&listener)` at `src/main.rs:424`, serving web + `/api/v1`.
- **Listener 2** — new: a hyper 1.x / `axum` server hosting `StreamableHttpService` at `/mcp`.

Both run on the one tokio runtime. One process, one systemd unit, two ports.

### Host validation — must be configured, not left at default

rmcp defaults `allowed_hosts` to **loopback only** — `localhost`, `127.0.0.1`, `::1` (`tower.rs:203`). A hosted deployment reached through a public hostname would get **403 on every request**, which would defeat the entire purpose of this work. Therefore:

- New knob `REDLIB_MCP_ALLOWED_HOSTS` — comma-separated authorities (`example.com`, `example.com:8080`), default `localhost,127.0.0.1,::1`. Hosted setups add their public host.
- Do **not** call `disable_allowed_hosts()`; keep DNS-rebinding protection.
- Origin validation stays at rmcp's default (**disabled**). Missing-`Origin` requests pass, which is what non-browser clients like opencode send. The bearer token is the real gate.

### Proposed layout

```
redlib/
├── Cargo.toml            # [workspace] block removed; rmcp moves here
├── dev-build.ps1         # ← moved from mcp/dev-build.ps1 (all cargo invocations)
├── src/
│   ├── main.rs           # clap: bare = web+api+mcp-http (token-gated); `mcp` = stdio
│   ├── api.rs            # HTTP JSON surface; uses crate::normalize/params/errors
│   ├── normalize.rs      # ← mcp/src/normalize.rs, single copy
│   ├── params.rs         # ← mcp/src/params.rs, single copy
│   ├── errors.rs         # ← unified ErrorKind + RedditError (2 renderers)
│   └── mcp/
│       ├── mod.rs        # rmcp server glue, tool router
│       ├── tools.rs      # the 4 #[tool] fns
│       ├── fetch.rs      # in-process client::json + percent-encoding
│       ├── http.rs       # Streamable HTTP listener + bearer layer
│       └── stdio.rs      # stdio serve (what `redlib mcp` runs)
└── (mcp/ deleted)
```

### Error taxonomy

Today two types with two renderers:

| Crate | Type | Rendered as |
|---|---|---|
| `mcp` | `FetchError` (`mcp/src/error.rs`) | `ErrorData` for rmcp |
| root | `ApiError` (`src/api.rs`) | JSON `{ok, kind, message}` |

After the merge: one `ErrorKind` enum + one error struct in `src/errors.rs`, with two render methods — `to_json()` for `/api/v1` and `to_error_data()` for MCP. `map_client_err` (substring heuristics over `client::json()`'s `String` error) moves there and stops existing twice.

## CLI

| Invocation | Surfaces | Ports |
|---|---|---|
| `redlib` (no token) | web UI | `--address`/`--port` as today |
| `redlib` + `REDLIB_API_TOKEN` | web + `/api/v1` + MCP HTTP | web port + `REDLIB_MCP_HTTP_PORT` (default 9090) |
| `redlib` + token + `REDLIB_MCP_HTTP_PORT=off` | web + `/api/v1` | web port |
| `redlib mcp` | stdio MCP only, no token needed | none |

Clap keeps bare `redlib` identical to today; `mcp` becomes a subcommand. `--mcp-http <PORT>` mirrors `REDLIB_MCP_HTTP_PORT` the way `-p` mirrors `PORT` (`src/main.rs:138-148`).

## Enabling

```
REDLIB_API_TOKEN set (non-empty)?
├─ no  → web UI only. Identical to upstream. No error, no extra port, no auth.
└─ yes → /api/v1 mounted (token required, /health exempt)
         └─ REDLIB_MCP_HTTP_PORT
            ├─ "off" → REST only
            └─ unset → 9090; MCP Streamable HTTP at /mcp (token required)
```

Consequences:

- There is no configuration in which `/api/v1` or `/mcp` answers without a valid token. Fail-closed falls out of the design rather than from a validation check.
- An operator who never sets the token gets an unmodified redlib.

## Auth

- Secret: `REDLIB_API_TOKEN` (env only). Required on every `/api/v1/*` request except `/health`, and on every MCP HTTP request.
- Header: `Authorization: Bearer <token>`, compared in constant time.
- Failure: `401` + `WWW-Authenticate: Bearer`.
- MCP clients must set `oauth: false` (see Consumption). OpenCode defaults remote servers to OAuth; without that flag it attempts OAuth discovery against our endpoint.
- TLS is **not** redlib's job. The token crosses the wire in plaintext unless a reverse proxy terminates TLS. Document that.

## Rate limiting

The two limiters protect different things and both stay:

| Limiter | Where | Limits | Protects |
|---|---|---|---|
| `Bucket` (`src/api.rs`) | every `/api/v1` call, and now MCP HTTP | cap 30, 30 per 60 s, `LazyLock<Mutex>`, health-exempt | **our** surfaces |
| `Pacer` (`mcp/src/pace.rs`, `REDLIB_MCP_MIN_INTERVAL_MS`, default 300) | MCP → Reddit calls only | 1 per 300 ms | **Reddit** |

Not routing `/api/v1` through the pacer is deliberate: the bucket already caps it at 30/min, which is stricter than the pacer's ~200/min. `/api/v1` is therefore better protected than stdio MCP is. The MCP HTTP layer draws from the existing bucket rather than adding a second one.

## Consumption

### Hosted (the point of this work)

```sh
REDLIB_API_TOKEN=$(openssl rand -hex 32) redlib --port 8080
# MCP HTTP comes up on :9090 because the token is set
```

| Surface | URL |
|---|---|
| Web UI | `http://host:8080/` |
| REST | `http://host:8080/api/v1/...` |
| MCP | `http://host:9090/mcp` |

OpenCode, `~/.config/opencode/opencode.json` (V2 shape — note `mcp.servers`, not `mcp`):

```jsonc
{
  "mcp": {
    "servers": {
      "redlib": {
        "type": "remote",
        "url": "http://host:9090/mcp",
        "oauth": false,
        "headers": { "Authorization": "Bearer {env:REDLIB_API_TOKEN}" }
      }
    }
  }
}
```

or per CLI:

```sh
opencode mcp add redlib --url http://host:9090/mcp --header 'Authorization=Bearer {env:REDLIB_API_TOKEN}'
```

### Local (as today)

```jsonc
{
  "mcp": {
    "servers": {
      "redlib": {
        "type": "local",
        "command": ["C:\\Users\\ajink\\projects\\redlib\\target\\release\\redlib.exe", "mcp"]
      }
    }
  }
}
```

Tool names are unchanged: `redlib_reddit_search`, and under Code Mode `tools.redlib.reddit_search(...)`.

### Migration on the user's machine

- `~/.config/opencode/opencode.json` points `redlib` at `target\release\redlib-mcp.exe`; that path dies with this change.
- That file already uses the V2 `mcp.servers` shape, so no reshaping is needed.
- `~/.config/opencode/opencode.jsonc` (same directory) still uses the **V1** `mcp.<name>` shape, so V2 ignores it entirely, and it contains a **plaintext context7 API key**. Out of scope, but it should be rotated and moved to `{env:CONTEXT7_API_KEY}`.
- `.env.example` must lose `REDLIB_API_ENABLED` (if present) and gain `REDLIB_API_TOKEN`, `REDLIB_MCP_HTTP_PORT`, `REDLIB_MCP_ALLOWED_HOSTS`.

## Testing

| Layer | Coverage |
|---|---|
| Moved unit tests | `normalize`, `params`, error mapping — same assertions, new module paths |
| Existing | the 30 `api::` tests still pass unchanged |
| No token | bare `redlib` with `REDLIB_API_TOKEN` unset: web serves, `/api/v1/*` 404s, no MCP port listens |
| Auth positive | token set → `/api/v1/r/rust` and `POST /mcp` succeed with `Authorization: Bearer <token>` |
| Auth negative | missing header → 401; wrong token → 401; `/api/v1/health` → 200 without a token |
| Empty token | `REDLIB_API_TOKEN=""` behaves as unset (surfaces absent) |
| Port collision | MCP port == web port → startup error, no half-started process |
| Allowed hosts | default loopback config → non-loopback `Host` → 403; adding the host to `REDLIB_MCP_ALLOWED_HOSTS` → 200 |
| `REDLIB_MCP_HTTP_PORT=off` | REST serves, MCP port closed |
| MCP HTTP integration | a real Streamable HTTP client does `initialize` → `tools/list` returns exactly 4 → `tools/call` per tool against live Reddit |
| MCP stdio regression | `redlib mcp` handshake works with no token set; stdout purity preserved |
| Rate limit | 40 rapid `/api/v1` calls → 429 after 30; MCP HTTP draws from the same bucket |
| Build | `pwsh -File dev-build.ps1 build` produces exactly one exe |

## Risks

| Risk | Mitigation |
|---|---|
| rmcp's loopback-only `allowed_hosts` default 403s the hosted flow | `REDLIB_MCP_ALLOWED_HOSTS` knob; explicit test above |
| `rmcp` protocol revision vs OpenCode's default `legacy` handshake | verify `initialize` end-to-end; pin `=3.5.1` |
| `StreamableHttpService` needs hyper 1.x / `axum` alongside hyper 0.14 | both majors coexist as separate crates; verify `cargo build` first |
| Binary size / MSRV rise to 1.81 (`mcp/Cargo.toml`) | accepted; root already declares `rust-version = "1.81"` |
| Bearer token over plaintext HTTP | document reverse-proxy TLS termination |
| Upstream rebase cost | one new file per concern; edits to `src/main.rs` localised to clap + listener startup + the enable block at `src/main.rs:411-418` |
| `mcp/dev-build.ps1` is load-bearing for MSVC builds | move it first, before deleting `mcp/` |
| Something else in the tree references `mcp/` | grep the tree before deletion (CI, compose, docs, `.gitignore`) |
| Retiring `REDLIB_API_ENABLED` breaks A3 Phase 1 docs/tests | update `.env.example`, the A3 design doc's deferral note, and the `api::enabled()` call site |

## Let to the builder

- Exact `axum` version and how the bearer layer attaches (tower middleware vs in-handler check).
- Whether Listener 2 shares the resolved IPv4/IPv6 logic from `src/main.rs:178-187` or always binds the same address as Listener 1.
- Shape of the `clap` subcommand (`redlib mcp` as a real subcommand vs a positional), preserving bare-`redlib` invocation.
- Whether `src/mcp/http.rs` gets its own bucket instance or reaches into `api.rs`'s static.
- How `REDLIB_MCP_ALLOWED_HOSTS` is parsed (comma-separated string vs repeatable var).
- Test-file placement for the moved MCP tests.
- Split of work into commits.

## Not decided here

- Any change to upstream Reddit-facing behaviour.
- Phase 2 of the original A3 list (there is no remaining Phase 2 once the dedupe lands in this spec).
