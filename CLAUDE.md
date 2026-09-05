# Herdr Remote

@CLAUDE.local.md

## Overview

A web application that connects to the herdr session's execution environment via Cloudflare Tunnel to send instructions, and that reads each agent pane's own transcript file to show its history — a pane running on the alternate screen keeps no scrollback, so the file is the only place its finished answers still exist.

## Tech Stack

Cloudflare Access + Cloudflare Tunnel + Terraform + cloudflared + systemd + nftables + Rust + SQLite + pulldown-cmark + TypeScript + Aube + Astro + Vitest + Biome

## Development Rules

1. Follow `$artful-simplicity:artful-simplicity` to keep the implementation to the bare minimum and pursue art-level simplicity.
2. The `any` type is prohibited in TypeScript.
