SHELL := /bin/bash
.SHELLFLAGS := -eo pipefail -c

-include .env
# Only what the server reads. A bare `export` would hand every recipe —
# cargo, aube, build scripts — whatever secrets .env holds.
export BIND_ADDR PORT ALLOWED_HOSTS

TMP := .tmp
# A blank PORT= in .env DEFINES the variable, so ?= would not rescue it, and an
# empty port must never reach the sed/nft renders below.
ifeq ($(strip $(PORT)),)
override PORT := 8787
endif
# nftables ships in sbin, which is not on a non-root PATH, and every nft
# operation here needs root anyway — so resolve it by path rather than asking
# `command -v` whether the user can see it.
NFT = $(shell command -v nft 2>/dev/null || for p in /usr/sbin/nft /sbin/nft; do test -x $$p && echo $$p && break; done)

BIN = $(shell cargo metadata --format-version 1 --no-deps | sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p')/release/herdr-remote

.PHONY: help run deps web bind-addr firewall setup services test format lint check deploy

help: ## list targets
	@grep -hE '^[a-z-]+:.*##' $(MAKEFILE_LIST) | sed -E 's/:[^#]*## /\t/'

deps: ## install rust + web dependencies
	cargo fetch
	cd web && aube install

web: deps ## build the UI into web/dist
	cd web && aube run build

run: web bind-addr firewall ## serve on BIND_ADDR:PORT from .env, default 127.0.0.1:8787
	cargo run

# A private-network route needs an address WARP can be routed to, and an alias on
# `lo` does not survive a reboot — so re-add it here rather than relying on the
# operator having persisted it. Skipped entirely when BIND_ADDR is unset.
bind-addr: ## add BIND_ADDR to lo if missing (needs sudo)
	@case "$$BIND_ADDR" in 0.0.0.0|::|::0|0:0:0:0:0:0:0:0) echo "BIND_ADDR=$$BIND_ADDR would listen on every interface — refusing"; exit 1;; esac
	@test -z "$$BIND_ADDR" \
	  || ip -4 -o addr show dev lo | grep -qFw "$$BIND_ADDR" \
	  || { echo "adding $$BIND_ADDR/32 to lo (sudo)"; sudo ip addr add "$$BIND_ADDR/32" dev lo; }

firewall: | $(TMP) ## drop BIND_ADDR:PORT arriving off-loopback (needs sudo)
	@test -z "$$BIND_ADDR" \
	  || { test -n "$(NFT)" || { echo "nft not found — install nftables"; exit 1; }; }
	@test -z "$$BIND_ADDR" \
	  || { sudo -n $(NFT) list table inet herdr-remote 2>/dev/null || true; } \
	     | grep -qF "ip daddr $$BIND_ADDR tcp dport $$PORT" \
	  || { echo "installing nft drop rule (sudo)"; \
	       sed -e "s/@BIND_ADDR@/$$BIND_ADDR/" -e "s/@PORT@/$$PORT/" deploy/herdr.nft > $(TMP)/herdr.nft; \
	       sudo $(NFT) -f $(TMP)/herdr.nft; }

setup: ## terraform the cloudflare side, write .tunnel-token
	@command -v terraform >/dev/null || { echo "terraform not found — https://developer.hashicorp.com/terraform/install"; exit 1; }
	@command -v cloudflared >/dev/null || { echo "cloudflared not found (needs >= 2025.4.0 for --token-file) — https://developers.cloudflare.com/cloudflare-one/connections/connect-networks/downloads/"; exit 1; }
	@for tool in curl jq; do command -v "$$tool" >/dev/null || { echo "$$tool not found — needed to look up the account's existing WARP enrollment application"; exit 1; }; done
	@{ set -a && . ./.env; } 2>/dev/null || true; test -n "$$CLOUDFLARE_API_TOKEN" -a -n "$$CLOUDFLARE_ACCOUNT_ID" -a -n "$$USER_EMAIL" -a -n "$$TEAM_NAME" \
	  || { echo "Set CLOUDFLARE_API_TOKEN, CLOUDFLARE_ACCOUNT_ID, USER_EMAIL, TEAM_NAME in .env"; exit 1; }
	@umask 077 \
	  && set -a && . ./.env && set +a \
	  && export TF_VAR_account_id="$$CLOUDFLARE_ACCOUNT_ID" TF_VAR_user_email="$$USER_EMAIL" \
	            TF_VAR_team_name="$$TEAM_NAME" TF_VAR_bind_addr="$${BIND_ADDR:-10.99.99.1}" TF_VAR_port="$$PORT" \
	  && terraform -chdir=infra init -input=false \
	  && terraform -chdir=infra validate \
	  && { terraform -chdir=infra state show cloudflare_zero_trust_device_default_profile.warp >/dev/null 2>&1 \
	       || terraform -chdir=infra import cloudflare_zero_trust_device_default_profile.warp "$$CLOUDFLARE_ACCOUNT_ID"; } \
	  && { terraform -chdir=infra state show cloudflare_zero_trust_organization.this >/dev/null 2>&1 \
	       || terraform -chdir=infra import cloudflare_zero_trust_organization.this "$$CLOUDFLARE_ACCOUNT_ID"; } \
	  && { terraform -chdir=infra state show cloudflare_zero_trust_access_application.enrollment >/dev/null 2>&1 \
	       || { warp_app=$$(curl -fsS -H "Authorization: Bearer $$CLOUDFLARE_API_TOKEN" \
	                          "https://api.cloudflare.com/client/v4/accounts/$$CLOUDFLARE_ACCOUNT_ID/access/apps" \
	                        | jq -r 'first(.result[] | select(.type == "warp") | .id) // empty'); \
	            test -z "$$warp_app" \
	              || terraform -chdir=infra import cloudflare_zero_trust_access_application.enrollment "accounts/$$CLOUDFLARE_ACCOUNT_ID/$$warp_app"; }; } \
	  && terraform -chdir=infra apply \
	  && chmod 600 infra/terraform.tfstate \
	  && for backup in infra/*.backup; do \
	       test ! -e "$$backup" || chmod 600 "$$backup"; \
	     done
	@umask 077 && terraform -chdir=infra output -raw tunnel_token > .tunnel-token.tmp \
	  && test -s .tunnel-token.tmp \
	  || { rm -f .tunnel-token.tmp; echo "terraform output produced no token"; exit 1; }
	@chmod 600 .tunnel-token.tmp && mv .tunnel-token.tmp .tunnel-token
	@echo "wrote .tunnel-token — make deploy (foreground) or make services (persistent)"

test: web | $(TMP) ## cargo test + vitest, logged to .tmp/test.log
	{ cargo test; cd web && aube run test; } 2>&1 | tee $(TMP)/test.log

format: | $(TMP) ## cargo fmt + biome write, logged to .tmp/format.log
	{ cargo fmt; cd web && aube exec biome check --write .; } 2>&1 | tee $(TMP)/format.log

lint: | $(TMP) ## clippy + astro check + biome check, logged to .tmp/lint.log
	{ cargo clippy --all-targets -- -D warnings; cd web && aube run check; } 2>&1 | tee $(TMP)/lint.log

check: deps ## everything: deps, test, lint, format — logged to .tmp/*.log
	$(MAKE) test
	$(MAKE) lint
	$(MAKE) format

deploy: web bind-addr firewall ## build release, serve through the tunnel (foreground)
	@test -s .tunnel-token || { echo ".tunnel-token missing or empty — make setup first"; exit 1; }
	@command -v cloudflared >/dev/null || { echo "cloudflared not found (needs >= 2025.4.0 for --token-file)"; exit 1; }
	@test -n "$$ALLOWED_HOSTS" -o -n "$$BIND_ADDR" || { echo "Set ALLOWED_HOSTS to your public hostname, or BIND_ADDR for a private-network route — otherwise every tunnel request gets 403"; exit 1; }
	cargo build --release
	@test -z "$$BIND_ADDR" || echo "open http://$$BIND_ADDR:$$PORT on the enrolled device"
	@$(BIN) & cloudflared tunnel run --token-file .tunnel-token & trap 'kill $$(jobs -p) 2>/dev/null' EXIT; wait -n

services: web | $(TMP) ## persistent alternative: systemd user units + boot net setup (private route only)
	@test -s .tunnel-token || { echo ".tunnel-token missing or empty — make setup first"; exit 1; }
	@test -n "$$BIND_ADDR" || { echo "services mode is private-route only — set BIND_ADDR"; exit 1; }
	@for tool in cloudflared ip; do command -v "$$tool" >/dev/null || { echo "$$tool not found on PATH — needed to render the units"; exit 1; }; done
	@test -n "$(NFT)" || { echo "nft not found — install nftables"; exit 1; }
	cargo build --release
	mkdir -p ~/.config/systemd/user
	@sed -e "s|@BIN@|$(BIN)|" -e "s|@REPO@|$(CURDIR)|" -e "s/@BIND_ADDR@/$$BIND_ADDR/" -e "s/@PORT@/$$PORT/" \
	  deploy/herdr-remote.service > ~/.config/systemd/user/herdr-remote.service
	@sed -e "s|@CLOUDFLARED@|$$(command -v cloudflared)|" -e "s|@REPO@|$(CURDIR)|" \
	  deploy/cloudflared.service > ~/.config/systemd/user/cloudflared.service
	@sed -e "s/@BIND_ADDR@/$$BIND_ADDR/" -e "s/@PORT@/$$PORT/" deploy/herdr.nft > $(TMP)/herdr.nft
	@sed -e "s|@IP@|$$(command -v ip)|" -e "s|@NFT@|$(NFT)|" -e "s/@BIND_ADDR@/$$BIND_ADDR/" \
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
	@echo "open http://$$BIND_ADDR:$$PORT on the enrolled device"

$(TMP):
	@mkdir -p $@
