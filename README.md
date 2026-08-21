# Herdr Remote

Send prompts to [Herdr](https://herdr.dev) panes from a phone browser.

```
phone (Cloudflare One client) ── WARP ──> Cloudflare Access (private app)
    ──> Cloudflare Tunnel ──> host firewall ──> herdr-remote (10.99.99.1:8787) ──> Herdr socket
```

Single Rust binary (Axum) serving the API and the static Astro UI from `web/dist`.

## Tunnel ownership

`TUNNEL_OWNER` in `.env` decides who owns the Cloudflare Tunnel, the `lo` alias and the
nftables rule.

- `self` — the default. This repository does it all: `make setup`, `make deploy`,
  `make services` work as documented below.
- `external` — another repository does. This one is only a server: it takes `BIND_ADDR`
  and `PORT` from the environment, and `bind-addr` / `firewall` / `setup` / `deploy` /
  `services` refuse. Injected `BIND_ADDR` and `PORT` beat the values in `.env`.

Cloudflare gives an account exactly one device profile, one Zero Trust organization and
one WARP enrollment application, so exactly one side may be `self`; two applying against
the same objects flip-flop, each reverting the other. Switching sides is not a flag flip —
`infra/terraform.tfstate` and `.tunnel-token` must move with the ownership.


## API

```
GET  /api/health                     # "ok"
GET  /api/session                    # {"workspaces":[{"id","label","tabs":[{"id","label","panes":[{"id","label","agent","state"}]}]}]}
POST /api/panes/{pane_id}/prompt     # {"text": "..."} -> 204; 404 for an unknown pane
POST /api/panes/{pane_id}/interrupt  # Esc to an agent's turn -> 204; 403 for a shell pane, 404 unknown
POST /api/panes/{pane_id}/enter      # Enter, for the question an agent is showing; same rules
POST /api/panes/{pane_id}/up         # Up, to move the selection in it; same rules
POST /api/panes/{pane_id}/down       # Down, likewise
GET  /api/panes/{pane_id}/output     # plain text; ?lines=1..20000 (default 300),
                                     # ?source=scrollback|screen; x-truncated: true when more remains
```

Cross-site mutating requests are refused by `Origin`, API responses are `no-store`, and the
UI cannot be framed (`frame-ancestors 'none'`).

`/api/session` reshapes Herdr's `session.snapshot` rather than forwarding it, so a
schema change on the Herdr side stops at the server instead of reaching the phone.
The UI keeps its state in the path, one segment per level of Herdr's own
hierarchy — `/` lists workspaces, `/w/<workspace>` lists that workspace's tabs,
`/w/<workspace>/t/<tab>` lists that tab's panes, and
`/w/<workspace>/t/<tab>/p/<pane>` is one pane's log, polled every 3 seconds
while that page is open and in the foreground, with the composer aimed at it. So anything
outside `/api` that is not a file in `web/dist` is answered with `index.html`,
and an unknown `/api` path still 404s instead of handing a fetch a page of HTML.

`prompt` picks its Herdr call per pane: `agent.prompt` where an agent is attached,
otherwise `pane.send_input` with a trailing `enter`.

The bare-key routes are agent-only, enforced server-side: a shell pane would
execute whatever sits on its command line, so the server answers 403 rather
than trusting the UI's disabled buttons.

## Development

```bash
install -m 600 .env.example .env   # once
make run               # build the UI, serve on 127.0.0.1:8787
```

`make check` is the one to run before committing: it installs dependencies, then tests, lints, and formats, stopping at the first failure. Each stage tees its output to `.tmp/` (`test.log`, `lint.log`, `format.log`), and each is also runnable on its own. Because formatting runs last, freshly written code trips the lint stage — run `make format` first, or reorder the target if you would rather it self-heal. `make help` lists everything.

`aube` is the only package manager — never `npm`, `pnpm`, or `yarn`.

## Deployment

Two routes in. The private network below is the one this repo automates and was
set up on. The public hostname route needs a domain on your Cloudflare account
and is written from Cloudflare's docs, not from a run here.

### Private network (no domain)

Zero Trust routes `10.99.99.1/32` to devices running the Cloudflare One Client
(formerly WARP) instead of publishing a hostname: no domain, no DNS. Both sides
dial out to Cloudflare's edge, so the phone does not share a network with this
machine. Terraform builds all of it — tunnel, route, include-mode split tunnel
(only that /32 rides WARP; the rest of the phone's traffic goes direct), a
device-enrollment policy pinned to one email, and a private Access application
on `$BIND_ADDR:$PORT` requiring that same identity. Enrollment alone would let
any process on an enrolled device drive your panes; the Access application is
the boundary that says *who*, not just *which device*.

The phone reaches the server *by address*, and `127.0.0.1` is the phone's own
loopback — it never enters the tunnel. Binding a LAN interface would publish
the server to everyone on that LAN, so `make` binds an alias on `lo` instead
and enforces it with a firewall rule: Linux's weak host model would otherwise
deliver a LAN packet aimed at a local address whatever interface it arrived on,
so "on `lo`" is topology, and the nft rule (drop `$BIND_ADDR:$PORT` arriving
off-loopback) is the enforcement. Both are re-applied by `make` when missing;
neither needs to survive a reboot by itself (`make services` handles boot).

Setup, once:

1. Create an API token at dash.cloudflare.com -> My Profile -> API Tokens with
   Account / Cloudflare Tunnel : Edit, Account / Access: Apps and Policies :
   Edit, Account / Zero Trust : Edit.
2. `install -m 600 .env.example .env` and fill `CLOUDFLARE_API_TOKEN`,
   `CLOUDFLARE_ACCOUNT_ID`, `USER_EMAIL`, `TEAM_NAME`; set
   `BIND_ADDR=10.99.99.1`.
3. A private-network CIDR belongs to exactly one tunnel, so delete the old
   tunnel's `10.99.99.1/32` route (or the whole tunnel) in the dashboard —
   remote control is down from here until the final step.
4. `make setup` (needs `terraform`, `cloudflared`, `curl`, `jq` on PATH) —
   Terraform shows its plan and asks to confirm, then writes the
   tunnel's connector token to `.tunnel-token` (0600, git-ignored, also present
   in `infra/terraform.tfstate` — both stay local). Terraform now owns the
   Split Tunnels list (include mode: only the /32 and the team's own
   `.cloudflareaccess.com` domain ride WARP) and device enrollment. That device
   profile is **tenant-global**: any device enrolled later gets the same
   include-mode list. Account singletons Cloudflare creates for you — the
   device profile, the Zero Trust organization settings, and the WARP
   enrollment application — are imported, not created, so a hand-made setup
   needs no manual import step.
5. Install Cloudflare One Agent on the phone (the app formerly published as
   1.1.1.1 / WARP), log in with `TEAM_NAME`, and enrol as `USER_EMAIL`.
6. `make deploy`, then browse to `http://$BIND_ADDR:$PORT` on the phone — the
   address and port from your `.env`, which is what `make deploy` prints and
   what Terraform gave the Access application. `PORT` is not always `8787`.
   The Access prompt authenticates the user, not just the device.

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
4. Paste the token into `.tunnel-token` and `chmod 600` it, then put the
   hostname in `.env`:

```
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
make deploy    # release build + cloudflared, foreground, dies with the terminal
make services  # or: systemd user units + boot-time net setup, restarts on failure and reboot
```

Both need `cloudflared` >= 2025.4.0 on PATH (for `--token-file`) and a
`.tunnel-token` from `make setup`. `make services` is private-route only (it
requires `BIND_ADDR`); the public-hostname route runs `make deploy`. The server
refuses wildcard, IPv6, and non-literal binds outright; `bind-addr` asks for
sudo only when the alias is missing, and `firewall` whenever it cannot confirm
the rule is already loaded (reading nftables usually needs root, so expect it
after a reboot).

`cloudflared tunnel --url http://127.0.0.1:8787` gives a throwaway public URL with no Cloudflare Access in front of it. Handy for a one-off check, never for leaving running: anyone with the URL can drive your panes.
