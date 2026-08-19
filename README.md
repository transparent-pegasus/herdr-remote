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
cp .env.example .env   # once
make run               # build the UI, serve on 127.0.0.1:8787
```

`make help` lists everything. `make test` and `make format` also write their output to `.tmp/test.log` and `.tmp/format.log`; `make check` runs clippy, `astro check`, and biome.

`aube` is the only package manager — never `npm`, `pnpm`, or `yarn`.

## Deployment

Create a remotely-managed tunnel in the Cloudflare Zero Trust dashboard, point its public hostname at `http://127.0.0.1:8787`, put Access in front of it, and put the tunnel token in `.env`. Then:

```bash
make deploy   # release build, then cloudflared serving the tunnel
```

Needs `cloudflared` on PATH. The server binds loopback only; never bind `0.0.0.0`.
