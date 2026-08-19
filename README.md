# Herdr Remote

Send prompts to [Herdr](https://herdr.dev) panes from a phone browser.

```
phone ── HTTPS ──> Cloudflare Access ──> Cloudflare Tunnel ──> herdr-remote (127.0.0.1:8787) ──> Herdr socket
```

Single Rust binary (Axum) serving the API and the static Astro UI from `web/dist`.

## API

```
GET  /api/health                     # "ok"
GET  /api/session                    # {"tabs":[{"id","label","panes":[{"id","label","agent","state"}]}]}
POST /api/panes/{pane_id}/prompt     # {"text": "..."} -> 204; 404 for an unknown pane
POST /api/panes/{pane_id}/interrupt  # Esc to an agent's turn -> 204; 403 for a shell pane, 404 unknown
POST /api/panes/{pane_id}/enter      # Enter, for the question an agent is showing; same rules
POST /api/panes/{pane_id}/up         # Up, to move the selection in it; same rules
POST /api/panes/{pane_id}/down       # Down, likewise
GET  /api/panes/{pane_id}/output     # plain text; ?lines=1..20000 (default 300),
                                     # ?source=scrollback|screen; x-truncated: true when more remains
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

The bare-key routes are agent-only, enforced server-side: a shell pane would
execute whatever sits on its command line, so the server answers 403 rather
than trusting the UI's disabled buttons.

## Development

```bash
cp .env.example .env   # once
make run               # build the UI, serve on 127.0.0.1:8787
```

`make check` is the one to run before committing: it installs dependencies, then tests, lints, and formats, stopping at the first failure. Each stage tees its output to `.tmp/` (`test.log`, `lint.log`, `format.log`), and each is also runnable on its own. Because formatting runs last, freshly written code trips the lint stage — run `make format` first, or reorder the target if you would rather it self-heal. `make help` lists everything.

`aube` is the only package manager — never `npm`, `pnpm`, or `yarn`.

## Deployment

Two routes in. The private network below is the one this repo was set up and
tested on. The public hostname route needs a domain on your Cloudflare account
and is written from Cloudflare's docs, not from a run here.

### Private network (no domain)

Zero Trust can route an IP range to devices running the Cloudflare One Client
(formerly WARP) instead of publishing a hostname, which needs no domain, no
DNS, and no Access application. Both sides still dial out to Cloudflare's edge,
so the phone does not have to share a network with this machine.

No Access application sits in front of the server here and Gateway network
policies default to allow, so device enrollment is the whole access boundary —
restrict it first, before the tunnel exists.

The catch is that the phone reaches the server *by address*, and `127.0.0.1` is the
phone's own loopback — it never enters the tunnel. Binding a LAN interface would
work but publishes the server to everyone on that LAN, where nothing authenticates.
Bind an alias on `lo` instead: reachable from this host, and so from cloudflared,
but never from the LAN.

```
BIND_ADDR=10.99.99.1
```

`make run` and `make deploy` add that address to `lo` themselves when it is
missing, asking for sudo only then — an alias does not survive a reboot, so
persisting it separately is optional. Pick a range that collides with neither this
machine's networks nor whatever network the phone is on. `BIND_ADDR` joins the
Host allowlist automatically, so `ALLOWED_HOSTS` stays empty here.

Then, in the Cloudflare dashboard:

1. Zero Trust -> Team & Resources -> Devices -> Management -> Device enrollment
   -> Manage -> Create new policy, selector Emails, value your address.
   **This is the real access boundary.** Narrow it further under Zero Trust ->
   Traffic policies -> Firewall policies -> Network only if you want to.
2. Networking -> Tunnels -> Create Tunnel, name it, and copy the token out of
   the install command into `.env` as `CLOUDFLARE_TUNNEL_TOKEN`.
3. That tunnel -> Routes -> Add route -> Private CIDR, network CIDR
   `10.99.99.1/32`.
4. Zero Trust -> Team & Resources -> Devices -> Device profiles -> Default ->
   Edit -> Split Tunnels -> Manage -> delete `10.0.0.0/4`. The client excludes
   that range by default, so the route is unreachable until the entry goes —
   and everything else in it now travels the tunnel too.
5. Install Cloudflare One Agent on the phone (the app formerly published as
   1.1.1.1 / WARP), log in with the team name from Zero Trust -> Settings ->
   Team name and domain, and enrol.
6. `make deploy` below, then browse to `http://10.99.99.1:8787`.

The trade against a public hostname: the client must stay connected on the
phone, and it occupies the device's VPN slot.

### Public hostname (not run here)

Needs a domain on your Cloudflare account; `*.trycloudflare.com` cannot carry
Access. Create the Access application **before** the tunnel's public hostname:
between publishing a hostname and protecting it, anyone holding the URL can
drive your panes. Access apps can be created for a hostname that does not
resolve yet.

1. Zero Trust -> Access controls -> Applications -> Create new application ->
   Self-hosted and private -> Add public hostname `herdr.example.com`, policy
   Allow / Emails / your address.
2. Networking -> Tunnels -> Create Tunnel, name it, copy the token out of the
   install command.
3. That tunnel -> Routes -> Add route -> Published application:
   `herdr.example.com`, service URL `http://127.0.0.1:8787`. The CNAME is
   created for you.
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

### Running it

```bash
make deploy   # release build, then wrangler serving the tunnel
```

Needs `wrangler` on PATH — it ships its own `cloudflared`, so that does not need installing separately. The server binds loopback, or the `lo` alias above, only; never bind `0.0.0.0`.

`wrangler tunnel quick-start http://127.0.0.1:8787` gives a throwaway public URL with no Cloudflare Access in front of it. Handy for a one-off check, never for leaving running: anyone with the URL can drive your panes.
