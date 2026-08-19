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
GET  /api/panes/{pane_id}/output   # the pane's last 300 lines, as plain text
```

`/api/session` reshapes Herdr's `session.snapshot` rather than forwarding it, so a
schema change on the Herdr side stops at the server instead of reaching the phone.
The UI keeps its state in the path — `/` lists tabs, `/t/<tab>` lists that tab's
panes, `/t/<tab>/p/<pane>` is one pane's log, polled every 3 seconds while that
page is open and in the foreground, with the composer aimed at it. So anything
outside `/api` that is not a file in `web/dist` is answered with `index.html`,
and an unknown `/api` path still 404s instead of handing a fetch a page of HTML.

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

Create the Access application **before** the tunnel's public hostname: between
publishing a hostname and protecting it, anyone holding the URL can drive your
panes. Access apps can be created for a hostname that does not resolve yet.

1. Zero Trust -> Access -> Applications -> Add an application -> Self-hosted.
   Domain `herdr.example.com`, policy Allow / Emails / your address.
2. Zero Trust -> Networks -> Tunnels -> Create a tunnel -> Cloudflared. Copy the
   token out of the install command.
3. That tunnel's Public Hostname tab: `herdr.example.com` -> HTTP ->
   `127.0.0.1:8787`. The CNAME is created for you.
4. Put the token and the hostname in `.env`:

```
CLOUDFLARE_TUNNEL_TOKEN=<token>
ALLOWED_HOSTS=herdr.example.com
```

`ALLOWED_HOSTS` is a Host-header allowlist. Access guards the tunnel, not this
socket: a page using DNS rebinding resolves its own hostname to `127.0.0.1` and
is then same-origin, so CORS does not apply — but it still sends that hostname
as `Host`, and an unlisted `Host` gets 403. Loopback is always allowed, so
`make run` works whether or not this is set. `make deploy` refuses to start
without it, since every tunnel request would otherwise 403.

A public hostname needs a domain on your Cloudflare account; `*.trycloudflare.com`
cannot carry Access. Without one, use Zero Trust's Private Network with the WARP
client on the phone instead.

Then:

```bash
make deploy   # release build, then wrangler serving the tunnel
```

Needs `wrangler` on PATH — it ships its own `cloudflared`, so that does not need installing separately. The server binds loopback only; never bind `0.0.0.0`.

`wrangler tunnel quick-start http://127.0.0.1:8787` gives a throwaway public URL with no Cloudflare Access in front of it. Handy for a one-off check, never for leaving running: anyone with the URL can drive your panes.
