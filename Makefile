SHELL := /bin/bash
.SHELLFLAGS := -eo pipefail -c

-include .env
export

TMP := .tmp
PORT ?= 8787
BIN = $(shell cargo metadata --format-version 1 --no-deps | sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p')/release/herdr-remote

.PHONY: help run deps web bind-addr test format lint check deploy

help: ## list targets
	@grep -hE '^[a-z-]+:.*##' $(MAKEFILE_LIST) | sed -E 's/:[^#]*## /\t/'

deps: ## install rust + web dependencies
	cargo fetch
	cd web && aube install

web: deps ## build the UI into web/dist
	cd web && aube run build

run: web bind-addr ## serve on BIND_ADDR:PORT from .env, default 127.0.0.1:8787
	cargo run

# A private-network route needs an address WARP can be routed to, and an alias on
# `lo` does not survive a reboot — so re-add it here rather than relying on the
# operator having persisted it. Skipped entirely when BIND_ADDR is unset.
bind-addr: ## add BIND_ADDR to lo if missing (needs sudo)
	@test -z "$$BIND_ADDR" -o "$$BIND_ADDR" = "0.0.0.0" \
	  || ip -4 -o addr show dev lo | grep -qFw "$$BIND_ADDR" \
	  || { echo "adding $$BIND_ADDR/32 to lo (sudo)"; sudo ip addr add "$$BIND_ADDR/32" dev lo; }

test: | $(TMP) ## cargo test + vitest, logged to .tmp/test.log
	{ cargo test; cd web && aube run test; } 2>&1 | tee $(TMP)/test.log

format: | $(TMP) ## cargo fmt + biome write, logged to .tmp/format.log
	{ cargo fmt; cd web && aube exec biome check --write .; } 2>&1 | tee $(TMP)/format.log

lint: | $(TMP) ## clippy + astro check + biome check, logged to .tmp/lint.log
	{ cargo clippy --all-targets -- -D warnings; cd web && aube run check; } 2>&1 | tee $(TMP)/lint.log

check: deps ## everything: deps, test, lint, format — logged to .tmp/*.log
	$(MAKE) test
	$(MAKE) lint
	$(MAKE) format

deploy: web bind-addr ## build release, expose it through the cloudflare tunnel
	@test -n "$$CLOUDFLARE_TUNNEL_TOKEN" || { echo "CLOUDFLARE_TUNNEL_TOKEN unset — cp .env.example .env and fill it"; exit 1; }
	@test -n "$$ALLOWED_HOSTS" -o -n "$$BIND_ADDR" || { echo "Set ALLOWED_HOSTS to your public hostname, or BIND_ADDR for a private-network route — otherwise every tunnel request gets 403"; exit 1; }
	cargo build --release
	@$(BIN) & trap "kill $$!" EXIT; wrangler tunnel run --token "$$CLOUDFLARE_TUNNEL_TOKEN"

$(TMP):
	@mkdir -p $@
