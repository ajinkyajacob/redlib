# Deploying the unified binary

How the `feat/unified-cli` build is run on a small VPS (2 vCPU / 1GB) that has
neither a Rust toolchain nor the RAM for a release build. Only the binary is
built elsewhere; everything else happens on the host.

## 1. Build the Linux binary

The release profile uses `lto = true` and `codegen-units = 1`, which needs
several GB of RAM. On a small host that OOM-kills the compiler, so build it
somewhere roomy — a Linux box, or a container on a workstation:

```bash
docker build -t redlib-builder -f - <<'EOF'
FROM rust:bookworm
RUN apt-get update && apt-get install -y --no-install-recommends \
      cmake clang libclang-dev perl nasm pkg-config ca-certificates \
 && rm -rf /var/lib/apt/lists/*
WORKDIR /src
EOF

git archive --format=tar HEAD -o /tmp/redlib-src.tar
docker volume create redlib-src
docker volume create redlib-target
docker run --rm -v redlib-src:/src \
  -v /tmp/redlib-src.tar:/tmp/src.tar:ro alpine \
  sh -c 'tar -xf /tmp/src.tar -C /src'
docker run --rm -v redlib-src:/src -v redlib-target:/src/target \
  -w /src redlib-builder cargo build --release
```

Pull the artifact out:

```bash
id=$(docker create -v redlib-target:/t debian:bookworm-slim)
docker cp "$id:/t/release/redlib" ./redlib
docker rm "$id"
```

`cmake`, `clang` and `perl` are there for BoringSSL (`wreq` → `btls`), which
compiles C++ and C during the build.

## 2. Lay it out on the host

```
/opt/redlib/
├── redlib                 # the binary, mode 0755
├── Dockerfile.runtime     # contrib/docker/Dockerfile.runtime
├── compose.yaml           # contrib/docker/compose.yaml
└── redlib.env             # secrets, mode 0600
```

```bash
sudo install -d -o "$USER" /opt/redlib
scp redlib Dockerfile.runtime compose.yaml host:/opt/redlib/
```

`/opt/redlib/redlib.env`:

```ini
REDLIB_API_TOKEN=<openssl rand -hex 24>
REDLIB_MCP_ALLOWED_HOSTS=example.org
REDLIB_ROBOTS_DISABLE_INDEXING=on
```

- `REDLIB_API_TOKEN` gates `/api/v1` and MCP. Without it both answer `401`.
- `REDLIB_MCP_ALLOWED_HOSTS` **must** name the public hostname. `rmcp` rejects
  any other `Host` with `403` (DNS-rebinding guard), and it defaults to
  loopback only, so leaving it unset breaks a proxied deployment silently.
- RSS stays off unless `REDLIB_ENABLE_RSS=on` is added; `GET /r/x.rss` answers
  `404` and renders "RSS is disabled on this instance".

## 3. Start it

```bash
cd /opt/redlib
sudo docker compose up -d --build
sudo docker logs -f redlib
```

Expected start-up:

```
INFO  redlib::oauth  > [✅] Successfully created OAuth client
INFO  redlib         > [✅] Rate limit check passed
Running Redlib v0.40.0 on 0.0.0.0:8080!
MCP Streamable HTTP on 0.0.0.0:9091/mcp
```

The container joins the proxy network and publishes no host ports, so nginx is
the only way in. `mem_limit: 512m` is generous: the web UI and MCP listener
together idle around 15MB.

## 4. Reverse proxy

Use `contrib/nginx/redlib-subpath.conf` to serve it under `/redlib/` on a
vhost that already hosts something else. It rewrites redlib's root-relative
URLs with `sub_filter`, proxies `/redlib/mcp` to the MCP listener, and leaves
`/api/v1` alone (JSON needs no rewriting).

Two gotchas that bite on reload:

- **`sub_filter` needs an uncompressed body.** Hence
  `proxy_set_header Accept-Encoding ""`; nginx compresses for the client.
- **Single-file bind mounts pin the inode.** If `default.conf` is mounted as a
  *file*, editing it with an editor that replaces the file (`sed -i`, most
  editors' safe-write) leaves the running container reading the old inode even
  after `nginx -s reload`. Rewrite in place (`python -c`, `tee`) or restart the
  container.

Video needs one extra rewrite. Posters are emitted as `poster="/preview/..."`
and appear in no other attribute, so they need their own `sub_filter` rule. Miss
it and every poster 404s on the parent vhost, the `<video>` falls back to its
100px `min-width`, and the native control bar no longer has room for a play
button — the video looks broken and only the context menu's *Play* works.

## 5. Verify

```bash
curl -s -o /dev/null -w '%{http_code}\n' https://example.org/redlib/          # 200
curl -s https://example.org/redlib/r/rust | grep -c 'Weekly visitors'         # 1
curl -s -o /dev/null -w '%{http_code}\n' https://example.org/redlib/api/v1/health   # 200
curl -s -o /dev/null -w '%{http_code}\n' https://example.org/redlib/api/v1/r/rust   # 401
curl -s -o /dev/null -w '%{http_code}\n' \
  -H "Authorization: Bearer $TOKEN" https://example.org/redlib/api/v1/r/rust        # 200
```

MCP handshake:

```bash
curl -s -o /dev/null -w '%{http_code}\n' https://example.org/redlib/mcp \
  -X POST -H "Authorization: Bearer $TOKEN" \
  -H 'Content-Type: application/json' \
  -H 'Accept: application/json, text/event-stream' \
  -d '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"probe","version":"1"}}}'
```

Also check that every link the page emits carries the prefix:

```bash
curl -s https://example.org/redlib/r/rust \
  | grep -oE '(href|src|action|poster)="/(redlib|#)?' | sort -u
```

With `use_hls=on` (Settings → *Use HLS for videos*) each Reddit video is an
`<video>` with no `src` of its own: `playHLSVideo.js` swaps in a clone, waits for
the first `play` or `click`, then hands the manifest to hls.js. If a video shows
a poster but will not start, check that `poster="/` is rewritten (above) and
that `/redlib/hls/<id>/HLSPlaylist.m3u8?...` answers `200
application/x-mpegurl` — the variant playlists and segments below it need no
query string.

## 6. Updating

```bash
cd /opt/redlib
scp new-redlib host:/opt/redlib/redlib
sudo docker compose up -d --build     # rebuilds the 17MB runtime layer only
```

`restart: unless-stopped` and the compose health of the proxy network keep it
up across reboots.
