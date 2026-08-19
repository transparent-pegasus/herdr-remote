# Automated Deploy + Security Hardening Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use pane-driven-development (recommended) or executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `make setup` builds the whole Cloudflare Zero Trust side with Terraform; `make deploy` stays the one foreground command that serves everything (now via cloudflared, not wrangler); `make services` is the selectable persistent mode — while closing security-review findings 1-6 (7 via services).

**Architecture:** New `infra/` Terraform root (tunnel, /32 route, include-mode split tunnel, enrollment + private Access apps) and new `deploy/` templates (nft rule, two user units, one root net oneshot), orchestrated by the existing Makefile. `src/main.rs` gains fail-closed bind validation and an Origin/headers middleware. Token moves from a blanket-exported env var to a 0600 `.tunnel-token` file.

**Tech Stack:** Terraform + provider `cloudflare/cloudflare` ~> 5, cloudflared, systemd (user units + one system oneshot), nftables, Rust (axum 0.8, anyhow), GNU make.

**Spec:** `docs/designs/2026-08-20-automated-deploy.md` — read it first; every task argues from it.

**Finding numbers** throughout this plan use `.tmp/security-review-chatgpt-pro.md`'s own numbering: 1 = Access app, 2 = wildcard bind, 3 = split tunnel, 4 = firewall, 5 = token, 6 = request-level, 7 = supervision.

## Global Constraints

- `aube` is the only web package manager — never `npm`/`pnpm`/`yarn`.
- TypeScript `any` prohibited (no web/ changes planned, but applies if touched).
- Artful simplicity per `.claude/skills/artful-simplicity/SKILL.md`: no speculative flexibility.
- Design/plan docs under `docs/designs/`, `docs/plans/` stay **uncommitted**. `CLAUDE.md`, `README.md`, code all commit normally.
- `CLAUDE.md` Tech Stack line must keep the order infra → low-level → high-level → application layer (user directive).
- Rust edition 2024, axum 0.8 (`{param}` route syntax, `middleware::map_response`).
- Comment style: sparse, constraint-stating, matching existing `src/main.rs` voice.
- Secrets never in argv or blanket env: recipes needing `.env` secrets source it inside the recipe shell (`set -a && . ./.env`), never via `$(VAR)` expansion into a command line.

## Tracks

| Track | Goal | Tasks | Owned files | Depends on |
|---|---|---|---|---|
| `rust` | fail-closed bind + origin gate + headers | 1-3 | `src/main.rs`, `Cargo.toml` | — |
| `infra` | Terraform root, deploy templates, Makefile rewrite | 4-6 | `infra/**`, `deploy/**`, `Makefile`, `.env.example`, `.gitignore` | — |
| `docs` | README + CLAUDE.md reflect the new deploy | 7 | `README.md`, `CLAUDE.md` | `rust`, `infra` |

**Integration order:** merge `rust` before `infra`. Task 6's `export BIND_ADDR PORT ALLOWED_HOSTS` exports empty strings for unset `.env` keys, and only Task 1's `env_nonempty` makes the server treat empty as unset — a tree holding Task 6 without Task 1 crashes `make run` when `.env` leaves `BIND_ADDR=` blank (the current code turns `""` into a `":8787"` bind string that fails resolution). The tracks still implement in parallel; only the merge is ordered.

**Post-integration follow-ups (never parallel tracks):** Task 8 — `make check`, `make run` smoke, and the operator-run cutover (`make setup`, `make deploy`, retire the hand-made tunnel/route/split-tunnel edit).

---

### Task 1: Fail-closed bind validation (`env_nonempty` + `parse_bind`)

**Track:** `rust`

**Files:**
- Modify: `src/main.rs:177-193` (main's env reading + the warning block), tests module at end
- Test: `src/main.rs` `#[cfg(test)] mod tests`

**Interfaces:**
- Produces: `fn env_nonempty(name: &str) -> Option<String>`; `fn parse_bind(bind: &str) -> anyhow::Result<std::net::Ipv4Addr>` (IPv4 only, wildcard fatal). Task 6's Makefile `export BIND_ADDR PORT ALLOWED_HOSTS` relies on empty-string env being treated as unset here — see the Tracks table's integration order.

- [ ] **Step 1: Write the failing tests** (append inside the existing `mod tests`)

```rust
    #[test]
    fn wildcard_and_non_literal_binds_are_fatal() {
        assert!(parse_bind("0.0.0.0").is_err());
        assert!(parse_bind("::").is_err());
        assert!(parse_bind("0:0:0:0:0:0:0:0").is_err());
        // IPv4 only: everything downstream — the host:port string, the lo
        // alias, the nft rule — is IPv4-shaped, so accepting ::1 here would
        // just move the failure somewhere less legible.
        assert!(parse_bind("::1").is_err());
        assert!(parse_bind("example.com").is_err());
        assert!(parse_bind("").is_err());
        assert!(parse_bind("10.99.99.1").is_ok());
        assert!(parse_bind("127.0.0.1").is_ok());
    }

    #[test]
    fn empty_env_is_unset() {
        // The Makefile exports BIND_ADDR/PORT/ALLOWED_HOSTS even when .env
        // leaves them blank; blank must behave exactly like absent.
        unsafe { std::env::set_var("HERDR_REMOTE_TEST_EMPTY", " ") };
        assert_eq!(env_nonempty("HERDR_REMOTE_TEST_EMPTY"), None);
        unsafe { std::env::set_var("HERDR_REMOTE_TEST_EMPTY", "x") };
        assert_eq!(env_nonempty("HERDR_REMOTE_TEST_EMPTY"), Some("x".into()));
    }
```

(`set_var` is `unsafe` in edition 2024; a dedicated var name keeps the two
assertions immune to test parallelism.)

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test wildcard_and_non_literal_binds_are_fatal`
Expected: FAIL — `parse_bind` not found.

- [ ] **Step 3: Implement** (above `main`, near the host-allowlist section)

```rust
/// Empty is unset: the Makefile `export`s BIND_ADDR/PORT/ALLOWED_HOSTS even
/// when .env leaves them blank, and a blank value must not change behavior.
fn env_nonempty(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
}

/// Refuse to listen where nothing authenticates: the bind must be one literal
/// IPv4 address, and a wildcard is fatal rather than a warning — the tunnel is
/// the only intended ingress, so there is no legitimate all-interfaces
/// deployment. IPv4 only because the whole path (host:port strings, the lo
/// alias, the nft rule) is IPv4-shaped.
fn parse_bind(bind: &str) -> anyhow::Result<std::net::Ipv4Addr> {
    let addr: std::net::Ipv4Addr = bind.parse().map_err(|_| {
        anyhow::anyhow!("BIND_ADDR must be a literal IPv4 address, got {bind:?}")
    })?;
    anyhow::ensure!(
        !addr.is_unspecified(),
        "BIND_ADDR={bind} would listen on every interface; nothing in front of \
         this socket authenticates. Bind 127.0.0.1 or the lo alias instead."
    );
    Ok(addr)
}
```

(`"::".parse::<Ipv4Addr>()` and the expanded IPv6 forms fail the parse, so the
unspecified check only has the IPv4 wildcard left to catch.)

In `main`, replace the env reads and the whole `if bind == "0.0.0.0" ...` warning block (`src/main.rs:179-190`) with:

```rust
    let port = env_nonempty("PORT").unwrap_or_else(|| "8787".into());
    // Default loopback. A Zero Trust private-network route needs an address the
    // WARP client can be routed to, so bind an alias on `lo` (see README) rather
    // than a LAN interface, which would also publish the server to the LAN.
    let bind = env_nonempty("BIND_ADDR").unwrap_or_else(|| "127.0.0.1".into());
    parse_bind(&bind)?;
    let extra = env_nonempty("ALLOWED_HOSTS");
```

(The later `let addr = format!("{bind}:{port}")` and `allowed_hosts(&bind, &port, extra.as_deref())` lines keep working unchanged — `parse_bind` is validation only, and with IPv4 enforced the unbracketed `host:port` string is always well-formed.)

- [ ] **Step 4: Run the tests**

Run: `cargo test`
Expected: all pass, including the existing host-allowlist tests.

- [ ] **Step 5: Commit**

```bash
git add src/main.rs
git commit -m "fix: refuse wildcard and non-literal binds"
```

---

### Task 2: Origin gate for mutating requests

**Track:** `rust`

**Files:**
- Modify: `src/main.rs` (new pure fn + middleware beside `guard_host`; wire in Task 3's router step is NOT needed here — this task wires it too)

**Interfaces:**
- Consumes: `Allowed`, `host_allowed` (existing), `allowed_hosts` (existing).
- Produces: `fn origin_allowed(origin: &str, allowed: &[String]) -> bool`; `async fn guard_origin(State<Allowed>, Request, Next) -> Response`. Task 3 layers it into the final router; this task layers it into the current one.

- [ ] **Step 1: Write the failing test**

```rust
    #[test]
    fn origins_match_the_host_allowlist() {
        let allowed = allowed_hosts("10.99.99.1", "8787", Some("herdr.example.com"));
        assert!(origin_allowed("http://10.99.99.1:8787", &allowed));
        assert!(origin_allowed("http://127.0.0.1:8787", &allowed));
        assert!(origin_allowed("https://herdr.example.com", &allowed));
        assert!(!origin_allowed("https://evil.example.com", &allowed));
        // A sandboxed iframe or data: page sends the literal string "null".
        assert!(!origin_allowed("null", &allowed));
        assert!(!origin_allowed("", &allowed));
        // Scheme is required — a bare host is not an Origin.
        assert!(!origin_allowed("10.99.99.1:8787", &allowed));
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test origins_match_the_host_allowlist`
Expected: FAIL — `origin_allowed` not found.

- [ ] **Step 3: Implement** (below the `guard_host` block, same section)

```rust
// --- Origin gate --------------------------------------------------------------
//
// The bodyless POSTs (interrupt, enter) are CORS "simple" requests: a malicious
// page anywhere can fire them without a preflight, and the browser sets Host to
// the target's own name, so the Host allowlist passes. The Origin header is the
// one thing such a page cannot forge; require it to match the same allowlist.

/// Origin is scheme://host[:port]. The scheme carries no identity here (the
/// tunnel terminates TLS), so match on host[:port] against the Host allowlist.
fn origin_allowed(origin: &str, allowed: &[String]) -> bool {
    origin
        .strip_prefix("http://")
        .or_else(|| origin.strip_prefix("https://"))
        .is_some_and(|host| host_allowed(host, allowed))
}

/// No Origin header (curl, native clients) passes: the gate is against
/// browser-ambient authority, not against someone who can already reach the
/// socket. An unreadable or unlisted Origin on a POST is refused.
async fn guard_origin(State(allowed): State<Allowed>, request: Request, next: Next) -> Response {
    let cross_site = request.method() == axum::http::Method::POST
        && match request.headers().get(header::ORIGIN) {
            None => false,
            Some(value) => !value
                .to_str()
                .is_ok_and(|origin| origin_allowed(origin, &allowed.0)),
        };
    if cross_site {
        (StatusCode::FORBIDDEN, "cross-site request refused").into_response()
    } else {
        next.run(request).await
    }
}
```

Wire it in `main` — the router's layer stack becomes (order matters, last is outermost):

```rust
        .layer(middleware::from_fn_with_state(allowed.clone(), guard_origin))
        .layer(middleware::from_fn_with_state(allowed, guard_host));
```

(`allowed.clone()` — `Allowed` is `Clone` already.)

- [ ] **Step 4: Run the tests**

Run: `cargo test`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/main.rs
git commit -m "feat: refuse cross-site mutating requests by Origin"
```

---

### Task 3: Response headers + `/api` nest + router extraction

**Track:** `rust`

**Files:**
- Modify: `src/main.rs` (router construction), `Cargo.toml` (dev-dependency)

**Interfaces:**
- Consumes: `guard_origin`, `guard_host`, `Allowed`.
- Produces: `fn app(allowed: Allowed) -> Router` — main() and the header tests both build the router through it.

- [ ] **Step 1: Add the dev-dependency** (`Cargo.toml`)

```toml
[dev-dependencies]
tower = { version = "0.5", features = ["util"] }
```

- [ ] **Step 2: Write the failing test**

```rust
    #[tokio::test]
    async fn responses_are_hardened() {
        use tower::ServiceExt;
        let allowed = Allowed(Arc::new(allowed_hosts("127.0.0.1", "8787", None)));
        let request = |method: &str, path: &str, origin: Option<&str>| {
            let mut builder = axum::http::Request::builder()
                .method(method)
                .uri(path)
                .header(header::HOST, "127.0.0.1:8787");
            if let Some(origin) = origin {
                builder = builder.header(header::ORIGIN, origin);
            }
            builder.body(axum::body::Body::empty()).unwrap()
        };

        // API responses must never be cached, and every response names its type.
        let response = app(allowed.clone())
            .oneshot(request("GET", "/api/health", None))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["cache-control"], "no-store");
        assert_eq!(response.headers()["x-content-type-options"], "nosniff");
        assert_eq!(
            response.headers()["content-security-policy"],
            "frame-ancestors 'none'"
        );

        // A cross-site bodyless POST is refused before it reaches herdr.
        let response = app(allowed.clone())
            .oneshot(request("POST", "/api/panes/x/enter", Some("https://evil.example.com")))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);

        // The refusal itself still carries the hardening headers.
        assert_eq!(response.headers()["x-content-type-options"], "nosniff");

        // A same-origin POST passes the gate: it reaches the handler, which
        // fails on the absent herdr socket — anything but 403 proves passage.
        let response = app(allowed.clone())
            .oneshot(request("POST", "/api/panes/x/enter", Some("http://127.0.0.1:8787")))
            .await
            .unwrap();
        assert_ne!(response.status(), StatusCode::FORBIDDEN);

        // An unknown or bare /api path is an API 404, never the UI's HTML.
        for path in ["/api/nope", "/api", "/api/"] {
            let response = app(allowed.clone()).oneshot(request("GET", path, None)).await.unwrap();
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
        }

        // A rejected Host is refused whatever the path.
        let mut bad_host = request("GET", "/api/health", None);
        bad_host.headers_mut().insert(header::HOST, "evil.example.com".parse().unwrap());
        let response = app(allowed).oneshot(bad_host).await.unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }
```

- [ ] **Step 3: Run to verify it fails**

Run: `cargo test responses_are_hardened`
Expected: FAIL — `app` not found.

- [ ] **Step 4: Implement** — extract and extend the router (replaces the `let app = Router::new()...` block in `main`):

```rust
async fn no_store(mut response: Response) -> Response {
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-store"),
    );
    response
}

/// Applied outermost so even a guard's 403 carries the headers. frame-ancestors
/// replaces X-Frame-Options; a page that cannot frame the UI cannot clickjack
/// its buttons.
async fn harden(mut response: Response) -> Response {
    let headers = response.headers_mut();
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        axum::http::HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        axum::http::HeaderValue::from_static("frame-ancestors 'none'"),
    );
    response
}

fn app(allowed: Allowed) -> Router {
    // no-store on /api only: pane output and session state are live, but the
    // hashed UI assets should stay cacheable for a phone on mobile data.
    let api = Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/session", get(session))
        .route("/panes/{pane_id}/prompt", post(prompt))
        .route("/panes/{pane_id}/interrupt", post(interrupt))
        .route("/panes/{pane_id}/enter", post(enter))
        .route("/panes/{pane_id}/output", get(output))
        // Without this, an unknown /api path — including bare /api and /api/,
        // which a {*rest} route would NOT match — falls through the nest to the
        // UI and answers a fetch with 200 and a page of HTML.
        .fallback(|| async { StatusCode::NOT_FOUND })
        .layer(middleware::map_response(no_store));

    Router::new()
        .nest("/api", api)
        // The UI routes on the path (/t/<tab>/p/<pane>), so a deep link or a
        // reload asks for a file that does not exist; hand back the app.
        .fallback_service(ServeDir::new("web/dist").fallback(ServeFile::new("web/dist/index.html")))
        .layer(middleware::from_fn_with_state(allowed.clone(), guard_origin))
        .layer(middleware::from_fn_with_state(allowed, guard_host))
        .layer(middleware::map_response(harden))
}
```

`main` then does `let app = app(allowed);` where the old block stood. The `any` import (`axum::routing::any`) loses its last user — remove it from the `use` line.

- [ ] **Step 5: Run the tests**

Run: `cargo test`
Expected: PASS — including `responses_are_hardened`.

- [ ] **Step 6: Commit**

```bash
git add src/main.rs Cargo.toml Cargo.lock
git commit -m "feat: no-store on the API, nosniff and frame-ancestors everywhere"
```

---

### Task 4: Terraform root (`infra/`) + env schema

**Track:** `infra`

**Files:**
- Create: `infra/main.tf`
- Modify: `.gitignore`, `.env.example`

**Interfaces:**
- Produces: `terraform output -raw tunnel_token` (sensitive), consumed by Task 6's `make setup`; variables `account_id`, `user_email`, `team_name`, `bind_addr` (default `10.99.99.1`), `port` (default `8787`); resource address `cloudflare_zero_trust_device_default_profile.warp`, imported by Task 6's guarded `terraform import`.

- [ ] **Step 1: Write `infra/main.tf`** — one file; splitting vars/outputs into more files adds nothing at this size:

```hcl
terraform {
  required_providers {
    cloudflare = {
      source  = "cloudflare/cloudflare"
      version = "~> 5"
    }
  }
}

# Reads CLOUDFLARE_API_TOKEN from the environment; make setup injects it
# from .env inside the recipe shell so it never appears in argv.
provider "cloudflare" {}

variable "account_id" { type = string }
variable "user_email" { type = string }

# Zero Trust team name (dash.cloudflare.com -> Zero Trust -> Settings): the
# include-mode split tunnel must carry <team>.cloudflareaccess.com or the
# Access/WARP session flows have no route to their own control plane.
variable "team_name" { type = string }

variable "bind_addr" {
  type    = string
  default = "10.99.99.1"
}

variable "port" {
  type    = number
  default = 8787
}

resource "cloudflare_zero_trust_tunnel_cloudflared" "herdr" {
  account_id = var.account_id
  name       = "herdr-remote"
  config_src = "cloudflare"
}

data "cloudflare_zero_trust_tunnel_cloudflared_token" "herdr" {
  account_id = var.account_id
  tunnel_id  = cloudflare_zero_trust_tunnel_cloudflared.herdr.id
}

resource "cloudflare_zero_trust_tunnel_cloudflared_route" "herdr" {
  account_id = var.account_id
  tunnel_id  = cloudflare_zero_trust_tunnel_cloudflared.herdr.id
  network    = "${var.bind_addr}/32"
  comment    = "herdr-remote"
}

# Include mode: the /32 plus Cloudflare's own required entries ride WARP;
# every other destination on the phone goes direct. This replaces the README's
# old "delete the default exclude entry" step, which sent all of 10/8 through
# the tunnel. The default profile is an account SINGLETON — imported in step 2,
# never created — and include-mode entries must also cover whatever the current
# include-mode doc lists as required for Access login and WARP sessions.
resource "cloudflare_zero_trust_device_default_profile" "warp" {
  account_id = var.account_id
  include = [
    {
      address     = "${var.bind_addr}/32"
      description = "herdr-remote"
    },
    {
      host        = "${var.team_name}.cloudflareaccess.com"
      description = "Access login + session"
    },
  ]
}

# One reusable policy: the same single identity gates enrollment and the app.
# (MFA/posture deliberately out — the IdP is one-time PIN; see the design's
# non-goals.)
resource "cloudflare_zero_trust_access_policy" "user" {
  account_id = var.account_id
  name       = "herdr-remote user"
  decision   = "allow"
  include    = [{ email = { email = var.user_email } }]
}

# Device enrollment permission — the review's "restrict it first". No
# session_duration: the provider rejects it on type = "warp". The tenant may
# already have a warp app from the manual setup — imported on conflict, step 2.
resource "cloudflare_zero_trust_access_application" "enrollment" {
  account_id = var.account_id
  type       = "warp"
  name       = "herdr-remote device enrollment"
  policies = [{
    id         = cloudflare_zero_trust_access_policy.user.id
    precedence = 1
  }]
}

# The application boundary itself: enrollment alone admits every process on an
# enrolled device; this scopes access to one user on one TCP destination.
# allow_authenticate_via_warp: the origin is plain HTTP on a non-web port, so
# there is no browser 302 — authentication rides the One Client session.
resource "cloudflare_zero_trust_access_application" "herdr" {
  account_id                 = var.account_id
  type                       = "self_hosted"
  name                       = "herdr-remote"
  session_duration           = "24h"
  allow_authenticate_via_warp = true
  destinations = [{
    type        = "private"
    cidr        = "${var.bind_addr}/32"
    l4_protocol = "tcp"
    port_range  = tostring(var.port)
  }]
  policies = [{
    id         = cloudflare_zero_trust_access_policy.user.id
    precedence = 1
  }]
}

output "tunnel_token" {
  value     = data.cloudflare_zero_trust_tunnel_cloudflared_token.herdr.token
  sensitive = true
}
```

- [ ] **Step 2: Validate schema, then plan against the real tenant**

First, schema only (no credentials):

```bash
cd infra && terraform init -backend=false -input=false && terraform validate
```

Expected: `Success!`. The attribute shapes asserted from docs, not from a run, are the default-profile `include` list, the private-app `destinations`, and `allow_authenticate_via_warp`. If validate rejects any, fix the attribute names against https://registry.terraform.io/providers/cloudflare/cloudflare/latest/docs (resources `zero_trust_device_default_profile`, `zero_trust_access_application`) **keeping the intent fixed**: import-not-create default profile in include mode carrying the /32 + team domain; warp enrollment app without `session_duration`; a self-hosted app whose destination is the private /32, TCP, one port, One Client auth, allow policy attached. While there, confirm against Cloudflare's include-mode doc (https://developers.cloudflare.com/cloudflare-one/team-and-resources/devices/cloudflare-one-client/configure/route-traffic/split-tunnels/) whether entries beyond the team domain are currently required, and add any that are.

Then, with real credentials (schema cannot see singletons, CIDR exclusivity, or tier entitlement) — `.env` must be filled first:

```bash
set -a && . ../.env && set +a
terraform init -input=false
terraform import cloudflare_zero_trust_device_default_profile.warp "$CLOUDFLARE_ACCOUNT_ID"
TF_VAR_account_id="$CLOUDFLARE_ACCOUNT_ID" TF_VAR_user_email="$USER_EMAIL" TF_VAR_team_name="$TEAM_NAME" terraform plan
```

(Export the same `TF_VAR_*` values before the import too if terraform prompts for variables.) Expected: a clean plan creating tunnel/token/route/apps and *updating* the imported default profile. Two known conflicts and their fixes:

- The tenant's existing warp enrollment app makes the plan or apply conflict → `terraform import cloudflare_zero_trust_access_application.enrollment "$CLOUDFLARE_ACCOUNT_ID/<app-id>"`, app id from Zero Trust → Access → Applications.
- The private-app `destinations` are rejected (tier, feature flag, or the provider demanding a `domain` — a known wart, provider issue #5529) → **stop and report**; per the design's non-goals, no Gateway-policy substitute gets improvised here.

The route itself cannot conflict if the README cutover was followed (the hand-made `10.99.99.1/32` route is deleted *before* setup — a CIDR belongs to exactly one tunnel per virtual network).

- [ ] **Step 3: Update `.gitignore`** — state contains the tunnel secret; wrangler is gone:

```diff
-.wrangler/
+.tunnel-token*
+infra/.terraform/
+infra/*.tfstate*
+infra/terraform.tfstate.d/
```

(`.tunnel-token*` also covers the atomic-write temp file; the tfstate glob covers backups and any workspace remnants.)

(Keep `infra/.terraform.lock.hcl` tracked — it pins the provider build.)

- [ ] **Step 4: Rewrite `.env.example`**:

```bash
# cp .env.example .env and fill in. .env is git-ignored.

# API token for `make setup` (Terraform). Create at dash.cloudflare.com ->
# My Profile -> API Tokens with: Account / Cloudflare Tunnel : Edit,
# Account / Access: Apps and Policies : Edit, Account / Zero Trust : Edit.
CLOUDFLARE_API_TOKEN=

# Zero Trust account id (dashboard URL: dash.cloudflare.com/<account-id>).
CLOUDFLARE_ACCOUNT_ID=

# The one identity allowed to enrol a device and reach the app.
USER_EMAIL=

# Zero Trust team name (Zero Trust -> Settings -> Team name and domain).
TEAM_NAME=

# Address to listen on. Blank means 127.0.0.1 — right for development and for a
# public hostname, where cloudflared reaches the server over loopback. The
# private-network route needs an address the WARP client can be routed to: set
# 10.99.99.1, and make adds the `lo` alias when missing (sudo). A wildcard or
# IPv6 bind is fatal — nothing in front of this socket authenticates.
BIND_ADDR=

# Extra Host headers to answer to, comma-separated. Only needed for a public
# hostname; loopback and BIND_ADDR are always accepted.
ALLOWED_HOSTS=

# Port the server listens on.
PORT=8787
```

- [ ] **Step 5: Commit**

```bash
git add infra/main.tf infra/.terraform.lock.hcl .gitignore .env.example
git commit -m "feat: terraform the cloudflare side"
```

---

### Task 5: Deploy templates (`deploy/`)

**Track:** `infra`

**Files:**
- Create: `deploy/herdr.nft`, `deploy/herdr-remote.service`, `deploy/cloudflared.service`, `deploy/herdr-remote-net.service`

**Interfaces:**
- Produces: `@BIND_ADDR@ @PORT@ @BIN@ @REPO@ @CLOUDFLARED@ @IP@ @NFT@` placeholder contract, consumed by Task 6's `sed` renders. nft table name `inet herdr-remote` — Task 6's staleness grep depends on the rule text `ip daddr <addr> tcp dport <port>`.

- [ ] **Step 1: Write `deploy/herdr.nft`** — flush-then-define makes re-runs idempotent and BIND_ADDR/PORT changes self-correcting:

```nft
# Rendered by make (sed): @BIND_ADDR@ @PORT@. The lo alias is topology, this is
# enforcement — Linux's weak host model delivers a packet for a local address
# whatever interface it arrived on, so a LAN peer with a static route could
# otherwise reach the socket.
table inet herdr-remote
flush table inet herdr-remote
table inet herdr-remote {
  chain input {
    type filter hook input priority filter; policy accept;
    iifname != "lo" ip daddr @BIND_ADDR@ tcp dport @PORT@ drop
  }
}
```

- [ ] **Step 2: Write `deploy/herdr-remote.service`** (user unit). The 2s retry loop IS the boot-ordering strategy: a user unit cannot `After=` the system-manager net oneshot, so the server retries until the `lo` alias exists. `StartLimitIntervalSec=0` keeps systemd from giving up while it waits:

```ini
[Unit]
Description=herdr-remote

[Service]
ExecStart=@BIN@
WorkingDirectory=@REPO@
Environment=BIND_ADDR=@BIND_ADDR@ PORT=@PORT@
Restart=on-failure
RestartSec=2
StartLimitIntervalSec=0
NoNewPrivileges=yes
PrivateTmp=yes

[Install]
WantedBy=default.target
```

- [ ] **Step 3: Write `deploy/cloudflared.service`** (user unit). No `After=network-online.target` — that target is system-scoped, a no-op in the user manager; cloudflared retries its own dial-out and the restart policy covers early exits:

```ini
[Unit]
Description=cloudflared tunnel for herdr-remote

[Service]
ExecStart=@CLOUDFLARED@ tunnel run --token-file @REPO@/.tunnel-token
Restart=on-failure
RestartSec=2
StartLimitIntervalSec=0
NoNewPrivileges=yes
PrivateTmp=yes

[Install]
WantedBy=default.target
```

- [ ] **Step 4: Write `deploy/herdr-remote-net.service`** (system oneshot; the user units cannot run privileged setup, so boot-time alias + firewall live here):

```ini
[Unit]
Description=herdr-remote net setup (lo alias + nft rule)

[Service]
Type=oneshot
RemainAfterExit=yes
ExecStart=@IP@ addr replace @BIND_ADDR@/32 dev lo
ExecStart=@NFT@ -f /etc/herdr-remote.nft

[Install]
WantedBy=multi-user.target
```

- [ ] **Step 5: Verify the templates render and parse** — all three units, real executable paths so verify checks something true, and no error suppression:

```bash
mkdir -p .tmp
sed -e 's/@BIND_ADDR@/10.99.99.1/' -e 's/@PORT@/8787/' deploy/herdr.nft > .tmp/herdr.nft
sudo nft -c -f .tmp/herdr.nft
sed -e "s|@BIN@|$(command -v true)|" -e "s|@REPO@|$PWD|" -e 's/@BIND_ADDR@/10.99.99.1/' -e 's/@PORT@/8787/' deploy/herdr-remote.service > .tmp/herdr-remote.service
sed -e "s|@CLOUDFLARED@|$(command -v true)|" -e "s|@REPO@|$PWD|" deploy/cloudflared.service > .tmp/cloudflared.service
sed -e "s|@IP@|$(command -v true)|" -e "s|@NFT@|$(command -v true)|" -e 's/@BIND_ADDR@/10.99.99.1/' deploy/herdr-remote-net.service > .tmp/herdr-remote-net.service
systemd-analyze --user verify .tmp/herdr-remote.service .tmp/cloudflared.service
systemd-analyze verify .tmp/herdr-remote-net.service
```

(`--user` is a global option and comes before the verb; `/bin/true` stands in for the executables so path-existence checks pass while syntax errors still fail the command.) Expected: `nft -c` silent, both `systemd-analyze` calls exit 0.

- [ ] **Step 6: Commit**

```bash
git add deploy/
git commit -m "feat: nft rule and systemd unit templates"
```

---

### Task 6: Makefile — `setup`, `deploy` via cloudflared, `services`, scoped exports

**Track:** `infra`

**Files:**
- Modify: `Makefile`

**Interfaces:**
- Consumes: `infra/main.tf` outputs (Task 4), `deploy/*` templates + placeholder names (Task 5), `env_nonempty` empty-is-unset behavior (Task 1).
- Produces: `.tunnel-token` (0600) contract consumed by `deploy`/`services`; targets `setup`, `firewall`, `services`.

- [ ] **Step 1: Scope the exports and normalize PORT** — replace `Makefile:4-5`, and replace the `PORT ?= 8787` line:

```make
-include .env
# Only what the server reads. A bare `export` would hand every recipe —
# cargo, aube, build scripts — whatever secrets .env holds.
export BIND_ADDR PORT ALLOWED_HOSTS
```

```make
# A blank PORT= in .env DEFINES the variable, so ?= would not rescue it, and an
# empty port must never reach the sed/nft renders below.
ifeq ($(strip $(PORT)),)
override PORT := 8787
endif
```

(The `override` also normalizes an empty `PORT=` given on the command line. `BIND_ADDR` needs no equivalent: empty legitimately means "loopback, skip alias and firewall", and the server treats empty as unset.)

- [ ] **Step 2: Make the wildcard bind fatal in `bind-addr`** — replace the target body:

```make
bind-addr: ## add BIND_ADDR to lo if missing (needs sudo)
	@case "$$BIND_ADDR" in 0.0.0.0|::|::0|0:0:0:0:0:0:0:0) echo "BIND_ADDR=$$BIND_ADDR would listen on every interface — refusing"; exit 1;; esac
	@test -z "$$BIND_ADDR" \
	  || ip -4 -o addr show dev lo | grep -qFw "$$BIND_ADDR" \
	  || { echo "adding $$BIND_ADDR/32 to lo (sudo)"; sudo ip addr add "$$BIND_ADDR/32" dev lo; }
```

(The `case` is a convenience gate for the obvious wildcards; `parse_bind` in the server is the real one — a hostname or IPv6 literal passes here and dies there, before listening.)

- [ ] **Step 3: Add `firewall`** (after `bind-addr`; the grep keeps re-runs sudo-free and catches a changed address or port):

```make
firewall: | $(TMP) ## drop BIND_ADDR:PORT arriving off-loopback (needs sudo)
	@test -z "$$BIND_ADDR" \
	  || { nft list table inet herdr-remote 2>/dev/null || sudo -n nft list table inet herdr-remote 2>/dev/null || true; } \
	     | grep -qF "ip daddr $$BIND_ADDR tcp dport $$PORT" \
	  || { echo "installing nft drop rule (sudo)"; \
	       command -v nft >/dev/null || { echo "nft not found — install nftables"; exit 1; }; \
	       sed -e "s/@BIND_ADDR@/$$BIND_ADDR/" -e "s/@PORT@/$$PORT/" deploy/herdr.nft > $(TMP)/herdr.nft; \
	       sudo nft -f $(TMP)/herdr.nft; }
```

(Listing a ruleset usually needs CAP_NET_ADMIN, so the unprivileged check often fails: the `sudo -n` retry uses cached credentials without prompting, and when neither can confirm the rule the apply path runs — re-applying the flush+define file is idempotent, so the worst case is a sudo prompt, not a wrong ruleset. "Sudo only when stale" is best-effort, stated as such in the README.)

- [ ] **Step 4: Add `setup`** — secrets are sourced inside the recipe shell, never expanded into argv; `terraform output` reads local state and needs no token. The `{ …; } 2>/dev/null || true` on the first source exists because `.SHELLFLAGS` carries `-e`: a missing `.env` must reach the friendly message, not die on the failed source:

```make
setup: ## terraform the cloudflare side, write .tunnel-token
	@command -v terraform >/dev/null || { echo "terraform not found — https://developer.hashicorp.com/terraform/install"; exit 1; }
	@command -v cloudflared >/dev/null || { echo "cloudflared not found (needs >= 2025.4.0 for --token-file) — https://developers.cloudflare.com/cloudflare-one/connections/connect-networks/downloads/"; exit 1; }
	@{ set -a && . ./.env; } 2>/dev/null || true; test -n "$$CLOUDFLARE_API_TOKEN" -a -n "$$CLOUDFLARE_ACCOUNT_ID" -a -n "$$USER_EMAIL" -a -n "$$TEAM_NAME" \
	  || { echo "Set CLOUDFLARE_API_TOKEN, CLOUDFLARE_ACCOUNT_ID, USER_EMAIL, TEAM_NAME in .env"; exit 1; }
	@set -a && . ./.env && set +a \
	  && export TF_VAR_account_id="$$CLOUDFLARE_ACCOUNT_ID" TF_VAR_user_email="$$USER_EMAIL" \
	            TF_VAR_team_name="$$TEAM_NAME" TF_VAR_bind_addr="$${BIND_ADDR:-10.99.99.1}" TF_VAR_port="$$PORT" \
	  && terraform -chdir=infra init -input=false \
	  && terraform -chdir=infra validate \
	  && { terraform -chdir=infra state show cloudflare_zero_trust_device_default_profile.warp >/dev/null 2>&1 \
	       || terraform -chdir=infra import cloudflare_zero_trust_device_default_profile.warp "$$CLOUDFLARE_ACCOUNT_ID"; } \
	  && terraform -chdir=infra apply
	@umask 077 && terraform -chdir=infra output -raw tunnel_token > .tunnel-token.tmp \
	  && test -s .tunnel-token.tmp \
	  || { rm -f .tunnel-token.tmp; echo "terraform output produced no token"; exit 1; }
	@chmod 600 .tunnel-token.tmp && mv .tunnel-token.tmp .tunnel-token
	@echo "wrote .tunnel-token — make deploy (foreground) or make services (persistent)"
```

Three properties to keep: the default-profile **import is guarded by `state show`** so re-running setup stays idempotent; `terraform apply` itself shows the plan and asks for confirmation (that IS the authenticated plan gate at run time — Task 4 step 2 already exercised a standalone `plan` during implementation); the token write is **atomic and permission-correct** (temp file under `umask 077`, non-empty check, `chmod 600`, `mv`) because a plain `>` redirect would truncate a good token before terraform runs and umask cannot fix a pre-existing 0644 file.

- [ ] **Step 5: Rewrite `deploy`** — same shape, cloudflared instead of wrangler, token from the file:

```make
deploy: web bind-addr firewall ## build release, serve through the tunnel (foreground)
	@test -s .tunnel-token || { echo ".tunnel-token missing or empty — make setup first"; exit 1; }
	@command -v cloudflared >/dev/null || { echo "cloudflared not found (needs >= 2025.4.0 for --token-file)"; exit 1; }
	@test -n "$$ALLOWED_HOSTS" -o -n "$$BIND_ADDR" || { echo "Set ALLOWED_HOSTS to your public hostname, or BIND_ADDR for a private-network route — otherwise every tunnel request gets 403"; exit 1; }
	cargo build --release
	@$(BIN) & cloudflared tunnel run --token-file .tunnel-token & trap 'kill $$(jobs -p) 2>/dev/null' EXIT; wait -n
```

(Both processes are children and `wait -n` returns when *either* exits, so a dying server ends the deploy visibly instead of leaving cloudflared fronting a corpse; the trap then reaps the survivor.)

Also add `firewall` to `run`'s prerequisites: `run: web bind-addr firewall`.

- [ ] **Step 6: Add `services`** — the selectable persistent mode:

```make
services: web | $(TMP) ## persistent alternative: systemd user units + boot net setup (private route only)
	@test -s .tunnel-token || { echo ".tunnel-token missing or empty — make setup first"; exit 1; }
	@test -n "$$BIND_ADDR" || { echo "services mode is private-route only — set BIND_ADDR"; exit 1; }
	@for tool in cloudflared ip nft; do command -v "$$tool" >/dev/null || { echo "$$tool not found on PATH — needed to render the units"; exit 1; }; done
	cargo build --release
	mkdir -p ~/.config/systemd/user
	@sed -e "s|@BIN@|$(BIN)|" -e "s|@REPO@|$(CURDIR)|" -e "s/@BIND_ADDR@/$$BIND_ADDR/" -e "s/@PORT@/$$PORT/" \
	  deploy/herdr-remote.service > ~/.config/systemd/user/herdr-remote.service
	@sed -e "s|@CLOUDFLARED@|$$(command -v cloudflared)|" -e "s|@REPO@|$(CURDIR)|" \
	  deploy/cloudflared.service > ~/.config/systemd/user/cloudflared.service
	@sed -e "s/@BIND_ADDR@/$$BIND_ADDR/" -e "s/@PORT@/$$PORT/" deploy/herdr.nft > $(TMP)/herdr.nft
	@sed -e "s|@IP@|$$(command -v ip)|" -e "s|@NFT@|$$(command -v nft)|" -e "s/@BIND_ADDR@/$$BIND_ADDR/" \
	  deploy/herdr-remote-net.service > $(TMP)/herdr-remote-net.service
	sudo install -m 644 $(TMP)/herdr.nft /etc/herdr-remote.nft
	sudo install -m 644 $(TMP)/herdr-remote-net.service /etc/systemd/system/herdr-remote-net.service
	sudo systemctl daemon-reload
	sudo systemctl enable herdr-remote-net.service
	sudo systemctl restart herdr-remote-net.service
	systemctl --user daemon-reload
	systemctl --user enable herdr-remote.service cloudflared.service
	systemctl --user restart herdr-remote.service cloudflared.service
	loginctl enable-linger $$USER
	@echo "persistent: server + tunnel restart on failure and after reboot"
```

(`restart` rather than `enable --now`: a re-run of `make services` must pick up a rebuilt binary, a re-rendered unit, or a changed address — `--now` does nothing to an already-active service, and the `RemainAfterExit` oneshot would otherwise skip re-applying the alias and nft file. `restart` also starts a stopped unit, so the first run works identically.)

- [ ] **Step 7: Update `.PHONY` and sweep the corpse**

`.PHONY: help run deps web bind-addr firewall setup services test format lint check deploy` — and confirm no `CLOUDFLARE_TUNNEL_TOKEN` or `wrangler` reference survives: `grep -n 'wrangler\|CLOUDFLARE_TUNNEL_TOKEN' Makefile` must output nothing.

- [ ] **Step 8: Verify without touching the tenant**

```bash
make -n deploy BIND_ADDR=10.99.99.1 | head -20        # recipe shape, no execution
make bind-addr BIND_ADDR=0.0.0.0                      # expect: refusing, exit 1
make bind-addr                                        # BIND_ADDR empty: silent no-op
make setup                                            # with .env unfilled: expect the "Set CLOUDFLARE_API_TOKEN..." refusal (or the terraform-not-found hint on a machine without it)
```

Expected: first prints build + cloudflared lines; second fails with the refusal; third exits 0 silently; fourth refuses before any terraform runs.

- [ ] **Step 9: Commit**

```bash
git add Makefile
git commit -m "feat: make setup/deploy/services — terraform in, wrangler out"
```

---

### Task 7: README + CLAUDE.md

**Track:** `docs` (after `rust` and `infra` — it documents their final behavior)

**Files:**
- Modify: `README.md`, `CLAUDE.md`

- [ ] **Step 1: Update the `CLAUDE.md` Tech Stack line** — order is infra → low-level → high-level → application layer (user directive; `AGENTS.md` is a symlink, edit `CLAUDE.md` only):

```markdown
Cloudflare Access + Cloudflare Tunnel + Terraform + cloudflared + systemd + nftables + Rust + TypeScript + Aube + Astro + Vitest + Biome
```

- [ ] **Step 2: Fix the README diagram** (`README.md:5-7`) to describe the deployment that is actually recommended:

```markdown
```
phone (Cloudflare One client) ── WARP ──> Cloudflare Access (private app)
    ──> Cloudflare Tunnel ──> host firewall ──> herdr-remote (10.99.99.1:8787) ──> Herdr socket
```
```

- [ ] **Step 3: Rewrite the Deployment section.** Replace everything from `## Deployment` through the end of `### Private network (no domain)` (currently `README.md:49-102`) with:

````markdown
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
on `10.99.99.1:8787` requiring that same identity. Enrollment alone would let
any process on an enrolled device drive your panes; the Access application is
the boundary that says *who*, not just *which device*.

The phone reaches the server *by address*, and `127.0.0.1` is the phone's own
loopback — it never enters the tunnel. Binding a LAN interface would publish
the server to everyone on that LAN, so `make` binds an alias on `lo` instead
and enforces it with a firewall rule: Linux's weak host model would otherwise
deliver a LAN packet aimed at a local address whatever interface it arrived on,
so "on `lo`" is topology, and the nft rule (drop `10.99.99.1:8787` arriving
off-loopback) is the enforcement. Both are re-applied by `make` when missing;
neither needs to survive a reboot by itself (`make services` handles boot).

Setup, once:

1. Create an API token at dash.cloudflare.com -> My Profile -> API Tokens with
   Account / Cloudflare Tunnel : Edit, Account / Access: Apps and Policies :
   Edit, Account / Zero Trust : Edit.
2. `cp .env.example .env` and fill `CLOUDFLARE_API_TOKEN`,
   `CLOUDFLARE_ACCOUNT_ID`, `USER_EMAIL`, `TEAM_NAME`; set
   `BIND_ADDR=10.99.99.1`.
3. Migrating from a hand-made setup? Do it now, not after: a private-network
   CIDR belongs to exactly one tunnel, so the old route must die before
   Terraform can create the new one. In the dashboard, delete the old tunnel's
   `10.99.99.1/32` route (or the whole tunnel) — remote control is down from
   here until step 5.
4. `make setup` — Terraform shows its plan and asks to confirm, then writes the
   tunnel's connector token to `.tunnel-token` (0600, git-ignored, also present
   in `infra/terraform.tfstate` — both stay local). Terraform now owns the
   Split Tunnels list (include mode: only the /32 and the team's own
   `.cloudflareaccess.com` domain ride WARP) and device enrollment. That device
   profile is **tenant-global**: any device enrolled later gets the same
   include-mode list.
5. Install Cloudflare One Agent on the phone (the app formerly published as
   1.1.1.1 / WARP), log in with `TEAM_NAME`, and enrol as `USER_EMAIL`.
6. `make deploy`, then browse to `http://10.99.99.1:8787`. The Access prompt
   for the private app authenticates the user, not just the device.

The trade against a public hostname: the client must stay connected on the
phone, and it occupies the device's VPN slot.
````

- [ ] **Step 4: Rewrite `### Running it`** (`README.md:134-142`) — wrangler is gone, services mode exists:

````markdown
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
````

- [ ] **Step 5: Public-hostname section touch-up** (`README.md:104-132`): swap the token instructions — token goes into `.tunnel-token` (paste it there, `chmod 600`) instead of `.env`'s deleted `CLOUDFLARE_TUNNEL_TOKEN`; keep `ALLOWED_HOSTS`. The quick-tunnel warning paragraph swaps `wrangler tunnel quick-start` for `cloudflared tunnel --url http://127.0.0.1:8787`.

- [ ] **Step 6: API section** — after the route list (`README.md:13-21`), add one line: cross-site mutating requests are refused by `Origin`, API responses are `no-store`, and the UI cannot be framed (`frame-ancestors 'none'`).

- [ ] **Step 7: Commit**

```bash
git add README.md CLAUDE.md
git commit -m "docs: terraform-automated deployment"
```

---

### Task 8: Post-integration verification (after all tracks merge)

**Track:** post-integration follow-up

- [ ] **Step 1:** `make check` — deps, tests, lint, format; logs under `.tmp/`. Expected: green.
- [ ] **Step 2: server smoke, pinned to loopback and cleaned up deterministically.** The user's real `.env` sets `BIND_ADDR=10.99.99.1`, so pin the address on the command line (command-line make vars beat `.env`); the build happened in step 1's deps, so start the binary directly rather than through the blocking `make run`:

```bash
cargo build --release
BIND_ADDR=127.0.0.1 PORT=8787 ./target/release/herdr-remote & SERVER=$!
trap 'kill $SERVER 2>/dev/null' EXIT
until curl -sf http://127.0.0.1:8787/api/health >/dev/null; do sleep 0.2; done
curl -si -X POST -H 'Origin: https://evil.example.com' http://127.0.0.1:8787/api/panes/x/enter | head -1   # expect 403
curl -si http://127.0.0.1:8787/api/health | grep -i '^cache-control'                                        # expect no-store
curl -s -o /dev/null -w '%{http_code}\n' http://127.0.0.1:8787/api/nope                                     # expect 404
kill $SERVER
```

(If `cargo metadata` points the target dir elsewhere, use `$(BIN)`'s path from `make -n deploy` output.)
- [ ] **Step 3: services rehearsal (host, no tenant needed):** with the real `.env` (`BIND_ADDR=10.99.99.1`) and a `.tunnel-token` present, `make services`, then: `systemctl status herdr-remote-net --no-pager` (active/exited, alias + nft applied), `systemctl --user status herdr-remote cloudflared --no-pager` (both active or restarting-until-tunnel-exists), `ip -4 addr show dev lo | grep 10.99.99.1`, `sudo nft list table inet herdr-remote`. Re-run `make services` once and confirm both user services restarted (new `Active:` timestamps). Stop cleanly afterwards if persistent mode is not wanted yet: `systemctl --user disable --now herdr-remote cloudflared && sudo systemctl disable --now herdr-remote-net`. A reboot check is the operator's call; the units are built for it but it is not asserted here. The nft rule's *behavioral* test (a packet arriving off-loopback being dropped) needs a second LAN host or a netns rig — deferred, named in the final report.
- [ ] **Step 4 (operator, real tenant):** follow the README order — delete the old `10.99.99.1/32` route first (downtime starts), `make setup` (review the Terraform plan before approving; the default-profile import is built into the target), `make deploy`, phone check via `http://10.99.99.1:8787` including the Access authentication prompt. If this step is deferred, say so in the final report — the repo change is complete without it, the cutover is not.
