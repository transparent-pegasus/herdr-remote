# Automated Deploy + Security Hardening — Design

Status: approved in chat 2026-08-20; plan-review resolutions 2026-08-20.
Not committed (coordination artifact).
Source: security review `.tmp/security-review-chatgpt-pro.md` findings 1-7, accepted
with the residuals below (6 now, #7 folded in via the selectable service mode), plus
the user's directive: maximize deploy automation, Terraform allowed, ideal shape
`make setup` (Cloudflare side) + `make deploy` (serve everything, foreground, current
shape).

## Goal

One-time `make setup` builds the entire Cloudflare Zero Trust side with Terraform;
`make deploy` stays the single foreground command that serves everything; an optional
`make services` installs reboot-persistent systemd units. The review's two blockers
(enrollment-only boundary, wildcard bind failing open) are closed in the same change.

## Non-goals

- Public-hostname route stays manual documentation (needs a domain; not run here).
  Its README section only swaps wrangler for cloudflared.
- No remote Terraform state, no CI, no multi-user anything. Single operator, single host.
- No rate limiting / CSRF tokens / Sec-Fetch-Site / prompt-size cap / MFA / device
  posture (review #6 extras and the posture half of #1). A local process on an already
  enrolled-and-Access-authorized device can still POST; the Access app is that
  boundary, not a second app-level credential.
- `make services` is private-network only. Public-hostname stays `make deploy`.
- No Gateway-policy substitute invented at apply time. If the private Access app
  is rejected (plan tier, feature flag, or provider `domain` requirement), stop;
  do not half-apply a different architecture.

## Current → target

| | current | target |
|---|---|---|
| Cloudflare config | 6 manual dashboard steps | `make setup` (Terraform, fresh objects) |
| Access boundary | device enrollment only | + private Access application on 10.99.99.1:8787 |
| Split tunnel | delete default 10/8 exclude (README wrongly says /4) | include mode: `10.99.99.1/32` + `<team>.cloudflareaccess.com` |
| Connector | `wrangler tunnel run --token <argv>` (experimental) | `cloudflared tunnel run --token-file .tunnel-token` (≥ 2025.4.0) |
| Tunnel secret | `.env` var, blanket-exported to every recipe | 0600 `.tunnel-token` (atomic write) + tfstate only |
| Wildcard bind | warning, keeps listening | fatal in `main.rs` (non-literal / unspecified / IPv6) and in `bind-addr` |
| LAN exposure of lo alias | claimed impossible (wrong: weak host model) | nft drop rule, non-lo ingress to BIND_ADDR:PORT |
| CSRF on bodyless POSTs | open (no preflight, Host is target's own) | Origin gate middleware |
| Persistence | none, dies with the terminal | optional `make services` (systemd user units + net oneshot) |

## Components

### infra/ — Terraform (new)

Provider `cloudflare/cloudflare` v5, local state (`infra/terraform.tfstate`,
gitignored — it contains the tunnel secret). Resources:

- `cloudflare_zero_trust_tunnel_cloudflared` — the tunnel, remote-managed config.
- data `cloudflare_zero_trust_tunnel_cloudflared_token` — connector token, output
  `tunnel_token` (sensitive).
- `cloudflare_zero_trust_tunnel_cloudflared_route` — network `10.99.99.1/32` → tunnel.
  **The tenant's hand-made route already owns this CIDR** (one route per CIDR per
  virtual network), so the old route is deleted in the dashboard *before*
  `make setup` — brief downtime, ordered in the README cutover.
- Default device profile in **include** mode: `10.99.99.1/32` **plus the
  Cloudflare-required Zero Trust entries** — at minimum the team domain
  `<team>.cloudflareaccess.com` (host entry) and whatever else the current
  include-mode doc lists as required for Access/WARP session flows. The default
  profile is an account singleton: `terraform import` it (id = account id)
  rather than create.
- Access application `type = "warp"` (no `session_duration` — the provider
  rejects it on this type) + allow policy on `var.user_email` — device
  enrollment permission (review: "restrict it first"). The tenant's existing
  enrollment app, if apply conflicts with it, is imported, not duplicated.
- **Private self-hosted Access application** — `destinations` with
  `type = "private"`, `cidr = 10.99.99.1/32`, `l4_protocol = "tcp"`,
  `port_range = "8787"`; `allow_authenticate_via_warp = true` (auth rides the
  One Client session — the origin is plain HTTP on a non-web port, no browser
  302); the same reusable allow policy; `session_duration = "24h"` (#1).

Variables: `account_id`, `user_email`, `team_name`, `bind_addr` (default
`10.99.99.1`), `port` (default `8787`). Auth: provider reads
`CLOUDFLARE_API_TOKEN` from the environment natively; the token is sourced from
`.env` inside the one recipe shell, never exported globally, never in argv.
Required token scopes (README): Account → Cloudflare Tunnel:Edit, Access: Apps
and Policies:Edit, Zero Trust:Edit.

### Makefile

- `setup`: check `terraform`/`cloudflared` on PATH (fail with install hint), then
  inside one recipe shell that sources `.env`: `terraform -chdir=infra init`,
  `validate`, an **authenticated `terraform plan`** (schema alone cannot see
  apply-time conflicts), then `apply`. Token file written atomically: `umask 077`
  → temp file → `test -s` → `chmod 600` → `mv .tunnel-token` (a plain `>`
  redirect would truncate a good token before terraform runs, and umask cannot
  fix a pre-existing 0644 file).
- `deploy`: deps `web bind-addr firewall`; release build; server and cloudflared
  both as children, `wait -n` + kill-the-other — either process dying ends the
  deploy visibly (a lone backgrounded server could die silently behind a live
  cloudflared).
- `services`: prerequisite checks (`command -v` for cloudflared/ip/nft must be
  non-empty — sbin paths are not on every user PATH), render `deploy/*` via
  `sed`, install user units to `~/.config/systemd/user/`, root oneshot to
  `/etc/systemd/system/` (sudo), `daemon-reload`, `enable --now`, **explicit
  `restart`s** (a re-run must pick up rebuilt binaries and re-rendered units —
  `enable --now` alone does not restart an active service, and the oneshot's
  `RemainAfterExit` would skip a changed address), `loginctl enable-linger`.
- `bind-addr`: the `0.0.0.0` skip becomes a hard fail (`case` covering
  `0.0.0.0|::|::0|0:0:0:0:0:0:0:0`); Rust's `parse_bind` stays the real gate (#4).
- `firewall` (new): apply `deploy/herdr.nft` (own table `inet herdr-remote`,
  `flush`+define = idempotent), skipped when `BIND_ADDR` unset. Staleness check
  tries `nft list` unprivileged, then `sudo -n` (cached credentials), and applies
  via sudo when neither confirms the rendered rule — plain users usually lack
  CAP_NET_ADMIN to list rulesets, so "sudo only when stale" is best-effort, not
  guaranteed. `run` and `deploy` both depend on it (#3).
- `PORT` normalized once at make level (`ifeq` on the stripped value → 8787):
  `.env` with a blank `PORT=` defines-but-empties the variable, `?=` does not
  rescue it, and an empty port must not reach sed/nft renders.
- Line 5 bare `export` replaced by `export BIND_ADDR PORT ALLOWED_HOSTS` — the only
  vars the server reads (#5). `CLOUDFLARE_TUNNEL_TOKEN` is deleted everywhere.

### deploy/ — service + firewall files (new)

- `herdr.nft` — template: `table inet herdr-remote { chain input { type filter
  hook input priority filter; iifname != "lo" ip daddr @BIND_ADDR@ tcp dport
  @PORT@ drop } }` with flush preamble. nft does no env substitution, so each
  consumer renders it with `sed`: `make firewall` to `.tmp/herdr.nft` and
  `sudo nft -f` that; `make services` to `/etc/herdr-remote.nft` for the boot
  oneshot's `nft -f`.
- `herdr-remote.service` (user) — `ExecStart=<target>/release/herdr-remote`,
  `WorkingDirectory=<repo>`, `Environment=BIND_ADDR= PORT=` baked at install,
  `Restart=on-failure`, `RestartSec=2`, `StartLimitIntervalSec=0`,
  `NoNewPrivileges=yes`, `PrivateTmp=yes`. The retry loop is the boot-ordering
  strategy: the user manager cannot `After=` a system unit, so the server simply
  retries every 2s until the root oneshot has added the `lo` alias — bounded
  noise, no cross-manager dependency machinery.
- `cloudflared.service` (user) — `ExecStart=<abs cloudflared> tunnel run
  --token-file <repo>/.tunnel-token`, same hardening + restart policy. No
  `After=network-online.target` — that target is system-scoped and a no-op in
  the user manager; cloudflared retries its own dial-out.
- `herdr-remote-net.service` (system, root, oneshot) — `ip addr replace
  <BIND_ADDR>/32 dev lo` (values baked at render time) + `nft -f
  /etc/herdr-remote.nft`, `WantedBy=multi-user.target`.
  Only installed by `make services`; foreground mode keeps the sudo-on-demand
  make targets.

### src/main.rs

- Bind validation (#4): `BIND_ADDR` must parse as a literal `Ipv4Addr` — IPv6 is
  refused because every downstream shape (host:port strings, the `lo` alias, the
  nft rule) is IPv4 — and the IPv4 wildcard is fatal. Empty env vars are treated
  as unset (`env_nonempty` helper) so `export BIND_ADDR` of an undefined make
  var cannot change behavior.
- Origin gate + headers (#6), one middleware layer: on POST, an `Origin` header,
  when present, must carry `host[:port]` ∈ the existing allowed-hosts list after
  stripping the scheme (absent Origin — curl — passes; `null` fails). Responses:
  `Cache-Control: no-store` on `/api` only (hashed UI assets keep caching for
  mobile data), `X-Content-Type-Options: nosniff` and
  `Content-Security-Policy: frame-ancestors 'none'` on everything.
- Pure helpers (`origin_allowed`, bind validation) unit-tested beside
  `host_allowed`'s tests.

### README / .env.example

- Diagram shows the real private path: phone (Cloudflare One client) → Access
  private app → tunnel → host firewall → herdr-remote on 10.99.99.1:8787 →
  Herdr socket (#1).
- Deployment section: API-token scopes, `cp .env.example .env`, `make setup`,
  enrol the phone, `make deploy`; `make services` for persistence. Dashboard
  split-tunnel steps deleted — Terraform owns them; the erroneous `/4` text goes
  away with them (#2).
- "never from the LAN" softened to describe the weak host model + the firewall
  rule that enforces it (#3).
- Cutover, ordered (the /32 can belong to only one tunnel, so overlap is
  impossible and "verify new before deleting old" cannot happen): **1.** delete
  the hand-made route (and tunnel) in the dashboard — remote control is down
  from here, **2.** `make setup`, **3.** `make deploy` + phone check, **4.**
  Terraform now owns the split-tunnel list and enrollment. README also warns the
  default device profile is tenant-global: a second device enrolled later gets
  the include-mode list too.
- `.env.example`: `CLOUDFLARE_API_TOKEN`, `CLOUDFLARE_ACCOUNT_ID`, `USER_EMAIL`,
  `TEAM_NAME`, `BIND_ADDR=` (blank keeps `make run` on loopback with no sudo; the
  comment says to set `10.99.99.1` for the private route), optional
  `ALLOWED_HOSTS`/`PORT`. `.gitignore` gains `.tunnel-token*`, `infra/.terraform/`,
  `infra/*.tfstate*`, `infra/terraform.tfstate.d/`.

## Security-review traceability

1 private Access app → Terraform (identity + One Client auth; MFA/posture
deliberately out — the IdP is one-time PIN and a posture layer is not this
tool's ceiling). 2 wildcard bind → fatal in both layers for wildcard,
non-literal, and IPv6; **residual, accepted:** an operator who sets BIND_ADDR
to an address the host genuinely owns on a LAN interface is not detected
(interface-membership checks need netlink machinery; README forbids it, the
firewall does not cover it). 3 split tunnel → include-mode /32 + required Zero
Trust entries in Terraform. 4 firewall → `deploy/herdr.nft` + target + boot
oneshot. 5 token → tfstate + atomically-written 0600 token-file, no global
export, no argv. 6 CSRF/headers → origin gate middleware + header layers;
**residual, accepted:** a POST with no Origin passes — the gate is against
browser-ambient authority; a hostile native process on an enrolled device is
held back by the Access app, not by this server. 7 supervision → `make
services` + cloudflared everywhere; **narrowed:** no version pinning, resource
limits, or credential isolation between the two same-user processes — one
operator, one host, not worth the machinery.

## Risks / verification points

- **Provider v5 attribute shapes** (default-profile include list, private-app
  `destinations`, `allow_authenticate_via_warp`) are asserted from docs, not from
  a run. Gates, in order: `terraform validate` (schema), then an authenticated
  `terraform plan` against the tenant (imports/conflicts), and apply stays with
  the operator. If the private Access app is rejected at plan/apply (tier,
  feature flag, or the provider demanding a `domain` on private destinations —
  a known provider wart), **stop and report; no improvised fallback** (Non-goals).
- **Singletons**: the default device profile always exists (import, id = account
  id) and the tenant already has a warp enrollment app (import on conflict). A
  create-only apply would 409 or duplicate.
- **Include-mode list completeness** decides whether Access auth works at all
  from the phone; the required-entries list comes from Cloudflare's current
  include-mode doc at execution time, and the Task 8 phone check is the proof.
- **Include-mode profile is tenant-global** (default profile): acceptable — the
  phone is the only enrolled device; warned in README.
- **Cutover has downtime by necessity**: the /32 route is exclusive, so the old
  route dies before `make setup` can create the new one. Ordered in README.
- Baseline: `make check`; smoke: `make run BIND_ADDR=127.0.0.1` (web/src change
  rebuilds `web/dist`); infra gates are validate + authenticated plan, not
  validate alone.
