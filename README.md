# Herdr Remote

Send prompts to [Herdr](https://herdr.dev) panes from a phone browser.

```
phone ── HTTPS ──> Cloudflare Access ──> Cloudflare Tunnel ──> herdr-remote (127.0.0.1:8787) ──> Herdr socket
```

Single Rust binary (Axum) serving the API and the static Astro UI from `web/dist`.

## API

```
GET  /api/health                   # "ok"
GET  /api/session                  # {"tabs":[{"id","label","panes":[{"id","label","agent","state"}]}]}
POST /api/panes/{pane_id}/prompt   # {"text": "..."} -> 204, or 404 for an unknown pane
```

`/api/session` reshapes Herdr's `session.snapshot` rather than forwarding it, so a
schema change on the Herdr side stops at the server instead of reaching the phone.
`prompt` picks its Herdr call per pane: `agent.prompt` where an agent is attached,
otherwise `pane.send_input` with a trailing `enter`.

## Development

```bash
cp .env.example .env   # once
make run               # build the UI, serve on 127.0.0.1:8787
```

`make check` is the one to run before committing: it installs dependencies, then tests, lints, and formats, stopping at the first failure. Each stage tees its output to `.tmp/` (`test.log`, `lint.log`, `format.log`), and each is also runnable on its own. Because formatting runs last, freshly written code trips the lint stage — run `make format` first, or reorder the target if you would rather it self-heal. `make help` lists everything.

`aube` is the only package manager — never `npm`, `pnpm`, or `yarn`.

## Deployment

Create a remotely-managed tunnel in the Cloudflare Zero Trust dashboard, point its public hostname at `http://127.0.0.1:8787`, put Access in front of it, and put the tunnel token in `.env`. Then:

```bash
make deploy   # release build, then wrangler serving the tunnel
```

Needs `wrangler` on PATH — it ships its own `cloudflared`, so that does not need installing separately. The server binds loopback only; never bind `0.0.0.0`.

`wrangler tunnel quick-start http://127.0.0.1:8787` gives a throwaway public URL with no Cloudflare Access in front of it. Handy for a one-off check, never for leaving running: anyone with the URL can drive your panes.
