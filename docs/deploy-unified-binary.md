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
└── redlib.env             # secrets, mode 0600, root:root
```

```bash
sudo install -d -o "$USER" /opt/redlib
scp redlib host:/opt/redlib/
```

`/opt/redlib/redlib.env`:

```ini
REDLIB_API_TOKEN=<openssl rand -hex 24>
REDLIB_MCP_ALLOWED_HOSTS=example.org
REDLIB_ROBOTS_DISABLE_INDEXING=on
RUST_LOG=info
```

- `REDLIB_API_TOKEN` gates `/api/v1` and MCP. Without it both answer `401`.
- `REDLIB_MCP_ALLOWED_HOSTS` **must** name the public hostname. `rmcp` rejects
  any other `Host` with `403` (DNS-rebinding guard), and it defaults to
  loopback only, so leaving it unset breaks a proxied deployment silently.
- RSS stays off unless `REDLIB_ENABLE_RSS=on` is added; `GET /r/x.rss` answers
  `404` and renders "RSS is disabled on this instance".

## 3. Start it

Install `contrib/systemd/redlib.service` as `/etc/systemd/system/redlib.service`
and start it:

```bash
sudo install -m 0644 contrib/systemd/redlib.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now redlib
sudo journalctl -fu redlib
```

Expected start-up:

```
INFO  redlib::oauth  > [✅] Successfully created OAuth client
INFO  redlib         > [✅] Rate limit check passed
Running Redlib v0.40.0 on 0.0.0.0:8090!
MCP Streamable HTTP on 0.0.0.0:9091/mcp
```

The unit runs the binary as a `DynamicUser` under `ProtectSystem=strict`,
restarts on failure and caps memory at 512M (it idles around 12MB). Nothing is
written to the filesystem, so no state directory is needed.

Ports 8090/9091 are the host ports; the reverse proxy reaches them through the
docker bridge. If the host's `INPUT` chain ends in a REJECT (Oracle Cloud images
ship `-A INPUT -j REJECT --reject-with icmp-host-prohibited`, which drops
everything but SSH), traffic from a container to the host gateway is dropped
even though the socket is listening. The unit therefore inserts

```
-A INPUT -s 172.18.0.0/16 -p tcp --dport 8090 -j ACCEPT
-A INPUT -s 172.18.0.0/16 -p tcp --dport 9091 -j ACCEPT
```

on start, so the rules survive a reboot without an `iptables-persistent`
package. Adjust the subnet to the docker network nginx lives on.

A container image is also provided (`contrib/docker/`) for hosts that cannot run
the binary directly; it joins the proxy network with no published ports and is
reached as `redlib:8080` / `redlib:9091` instead. Pick one — they serve the same
ports and would collide.

## 4. Reverse proxy

Use `contrib/nginx/redlib-subpath.conf` to serve it under `/redlib/` on a
vhost that already hosts something else. It rewrites redlib's root-relative
URLs with `sub_filter`, proxies `/redlib/mcp` to the MCP listener, and leaves
`/api/v1` alone (JSON needs no rewriting).

Point the upstreams at wherever nginx can reach the process: `127.0.0.1:8090`
and `127.0.0.1:9091` when nginx runs on the host, or the docker bridge gateway
(`172.18.0.1:8090`, `172.18.0.1:9091`) when nginx itself is a container.

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
scp new-redlib host:/opt/redlib/redlib
ssh host 'sudo systemctl restart redlib'
```

`Restart=always` and `systemctl enable` keep it up across reboots. Replacing the
binary under a running service is safe on Linux — the old inode stays mapped
until the restart.
