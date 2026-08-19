# Herdr Remote

Send prompts to [Herdr](https://herdr.dev) panes from a phone browser.

```
phone ── HTTPS ──> Cloudflare Access ──> Cloudflare Tunnel ──> herdr-remote (127.0.0.1:8787) ──> Herdr socket
```

Single Rust binary (Axum) serving the API and the static Astro UI from `web/dist`.

## API

```
GET  /api/health
GET  /api/session                  # tabs/panes snapshot
POST /api/panes/:pane_id/prompt    # {"text": "..."}
```

## Development

```bash
cd web && aube install && aube run build   # build UI
cargo run                                  # serve on 127.0.0.1:8787
```

Checks: `cargo clippy` / `cd web && aube run check` / `aube run test`

## Deployment

Expose `http://127.0.0.1:8787` via `cloudflared` and put Cloudflare Access in front of the public hostname. The server binds loopback only; never bind `0.0.0.0`.
