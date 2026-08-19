SHELL := /bin/bash
.SHELLFLAGS := -eo pipefail -c

-include .env
export

TMP := .tmp
PORT ?= 8787
BIN = $(shell cargo metadata --format-version 1 --no-deps | sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p')/release/herdr-remote

.PHONY: help run deps web test format lint check deploy

help: ## list targets
	@grep -hE '^[a-z-]+:.*##' $(MAKEFILE_LIST) | sed -E 's/:[^#]*## /\t/'

deps: ## install rust + web dependencies
	cargo fetch
	cd web && aube install

web: deps ## build the UI into web/dist
	cd web && aube run build

run: web ## serve on 127.0.0.1:8787, or PORT from .env
	cargo run

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

deploy: web ## build release, expose it through the cloudflare tunnel
	@test -n "$$CLOUDFLARE_TUNNEL_TOKEN" || { echo "CLOUDFLARE_TUNNEL_TOKEN unset — cp .env.example .env and fill it"; exit 1; }
	@test -n "$$ALLOWED_HOSTS" || { echo "ALLOWED_HOSTS unset — the tunnel would get 403 on every request; set it to your public hostname in .env"; exit 1; }
	cargo build --release
	@$(BIN) & trap "kill $$!" EXIT; wrangler tunnel run --token "$$CLOUDFLARE_TUNNEL_TOKEN"

$(TMP):
	@mkdir -p $@
