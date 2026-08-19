# Herdr Remote

## Overview

A web application that connects to the herdr session's execution environment via Cloudflare Tunnel to send instructions.

## Tech Stack

Cloudflare Access + Cloudflare Tunnel + Rust + TypeScript + Aube + Astro + Vitest + Biome

## Development Rules

1. Follow `.claude/skills/artful-simplicity/SKILL.md` to keep the implementation to the bare minimum and pursue art-level simplicity.
2. Unless otherwise specified, implement according to the `herdrpowers:full_cycle` workflow.
3. The `any` type is prohibited in TypeScript.

## Herdrpowers Configuration

herdrpowers skills and commands resolve `<KEY>` placeholders from this section.

- `REPO_INSTRUCTION_FILES`: `CLAUDE.md` (`AGENTS.md` is a symlink to it — one file, edit `CLAUDE.md` only)
- `BASE_BRANCH`: `main`
- `REPORT_DIRECTORY`: `.tmp/`
- `DESIGN_DOC_PATH_PATTERN`: `docs/designs/YYYY-MM-DD-feature.md`
- `PLAN_PATH_PATTERN`: `docs/plans/YYYY-MM-DD-feature.md`
- `PLAN_DIRECTORY`: `docs/plans/`
- `BASELINE_VERIFICATION_COMMAND`: `make check test`
- `SUPPLEMENTAL_VERIFICATION_COMMANDS`: `make run` as a smoke check when `src/` or `web/` changes (it rebuilds `web/dist`, which the server serves)
- `TEST_FRAMEWORK_AND_COMMANDS`: Rust `#[test]` via `cargo test`; Vitest via `aube run test` in `web/`. `aube` is the only package manager — never `npm`/`pnpm`/`yarn`.
- `TEST_FILE_LOCATIONS`: Rust unit tests in `#[cfg(test)]` modules beside the code in `src/`, integration tests in `tests/`. TypeScript tests colocated as `web/src/**/*.test.ts`.
- `TARGETED_TEST_COMMAND`: `cargo test <filter>` (Rust) / `cd web && aube exec vitest run <path>` (TypeScript)
- `FULL_TEST_SUITE_COMMAND`: `make test` (logs to `.tmp/test.log`)
