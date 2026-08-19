SHELL := /bin/bash
.SHELLFLAGS := -eo pipefail -c

-include .env
export

TMP := .tmp
PORT ?= 8787
BIN = $(shell cargo metadata --format-version 1 --no-deps | sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p')/release/herdr-remote
WEB_SRC = $(shell find web/src web/package.json web/astro.config.mjs -type f 2>/dev/null)

.PHONY: help run test format check deploy

help: ## list targets
	@grep -hE '^[a-z-]+:.*##' $(MAKEFILE_LIST) | sed -E 's/:[^#]*## /\t/'

run: web/dist ## serve on 127.0.0.1:8787, or PORT from .env
	cargo run

test: | $(TMP) ## cargo test + vitest, logged to .tmp/test.log
	{ cargo test; cd web && aube run test; } 2>&1 | tee $(TMP)/test.log

format: | $(TMP) ## cargo fmt + biome write, logged to .tmp/format.log
	{ cargo fmt; cd web && aube exec biome check --write .; } 2>&1 | tee $(TMP)/format.log

check: web/node_modules ## clippy + astro check + biome check
	cargo clippy --all-targets -- -D warnings
	cd web && aube run check

deploy: web/dist ## build release, expose it through the cloudflare tunnel
	@test -n "$$CLOUDFLARE_TUNNEL_TOKEN" || { echo "CLOUDFLARE_TUNNEL_TOKEN unset — cp .env.example .env and fill it"; exit 1; }
	cargo build --release
	@$(BIN) & trap "kill $$!" EXIT; cloudflared tunnel run --token "$$CLOUDFLARE_TUNNEL_TOKEN"

web/node_modules: web/package.json
	cd web && aube install
	@touch $@

web/dist: web/node_modules $(WEB_SRC)
	cd web && aube run build

$(TMP):
	@mkdir -p $@
