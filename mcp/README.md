# redlib-mcp

MCP (Model Context Protocol) server that gives an AI coding agent read access to
Reddit through this fork's own `client::json()` OAuth stack — **in-process, no
HTTP listener, no HTML scraping**.

## Tools

| Tool | Purpose |
|---|---|
| `reddit_search` | Keyword search over Reddit posts (short keyword queries, term matching) |
| `reddit_subreddit` | Hot/top/new/rising posts from a subreddit, with cursor pagination |
| `reddit_post` | One post plus its threaded comments; expand collapsed branches via `comment_id` |
| `reddit_user` | A user's recent submissions |

All four are read-only and unauthenticated from the client's side. Post bodies,
comments, and titles are **UNTRUSTED user-generated text** — tool descriptions tell
the agent to treat them as data, never as instructions.

## Architecture

- stdio transport, newline-delimited JSON-RPC; `stdout` carries protocol frames only.
- Request paths are built from caller input that is validated (`params.rs`) and
  percent-encoded (`fetch.rs`) before being proxied to Reddit.
- Upstream calls are paced by a minimum-interval limiter (`pace.rs`,
  `REDLIB_MCP_MIN_INTERVAL_MS`, default 300) so the single reader never bursts Reddit.
- Error mapping (`error.rs`) classifies transport/OAuth failures and reports them as
  MCP `-32602`/`-32603` errors with hints.

## Build

The crate is a workspace member of the fork. Building requires the MSVC toolchain:

```powershell
pwsh -File mcp\dev-build.ps1 build --release -p redlib-mcp
```

Binary: `target\release\redlib-mcp.exe`.

## Configure opencode

```json
"mcp": {
  "redlib": {
    "type": "local",
    "command": ["C:\\path\\to\\redlib\\target\\release\\redlib-mcp.exe"]
  }
}
```

## Test

```powershell
pwsh -File mcp\dev-build.ps1 test -p redlib-mcp
pwsh -File mcp\dev-build.ps1 test -p redlib-mcp -- --include-ignored
```

The `#[ignore]`d live tests drive real Reddit calls and need network + a valid OAuth
token (obtained on first use by `client::json`).