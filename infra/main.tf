# OWNERSHIP: the state for these objects currently lives in ../portable-dev,
# which is the `self` side (see TUNNEL_OWNER in the Makefile). This file is kept
# so this repository can take ownership back, but `make setup` is refused while
# TUNNEL_OWNER=external — and taking it back means moving infra/terraform.tfstate
# here as well. Applying without that state creates a SECOND tunnel with a new
# token, and the route apply then fails against the surviving /32.

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
  # Pinned: left unset, apply nulls the account's existing value to "".
  tunnel_protocol     = "masque"
  dns_search_suffixes = []
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

# Account-level "Cloudflare One Client Authentication" (Access -> Settings):
# the app's allow_authenticate_via_warp is rejected outright until a session
# duration exists here. Another account SINGLETON — imported, never created —
# so name/auth_domain are pinned to the values the tenant already has rather
# than left null, which the PUT would write back as empty.
resource "cloudflare_zero_trust_organization" "this" {
  account_id                  = var.account_id
  name                        = "${var.team_name}.cloudflareaccess.com"
  auth_domain                 = "${var.team_name}.cloudflareaccess.com"
  allow_authenticate_via_warp = true
  warp_auth_session_duration  = "24h"
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
# session_duration: the provider rejects it on type = "warp". Cloudflare
# auto-creates this app on every Zero Trust account, so it is always imported,
# never created (make setup looks up its id); its name is fixed by Cloudflare
# and pinned here, or every plan reports a rename that silently never lands.
resource "cloudflare_zero_trust_access_application" "enrollment" {
  account_id = var.account_id
  type       = "warp"
  name       = "Warp Login App"
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
  account_id                  = var.account_id
  type                        = "self_hosted"
  name                        = "herdr-remote"
  session_duration            = "24h"
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

  depends_on = [cloudflare_zero_trust_organization.this]
}

output "tunnel_token" {
  value     = data.cloudflare_zero_trust_tunnel_cloudflared_token.herdr.token
  sensitive = true
}
