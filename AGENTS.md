# AGENTS.md

## Dev environment

- Use Nix flakes: `nix develop` or `direnv allow` (`.envrc` calls `use flake`).
- The shell auto-starts a local PostgreSQL 16 cluster on **port 5433** (not 5432) with socket dir `/tmp`. Database name: `rankoder`.
- `DATABASE_URL` is set by the flake shell hook.

## Build & run

```sh
cargo build
cargo run
```

- Rust edition 2024 (requires rustc ≥ 1.85+).
- No CI, no tests defined yet.

## Lint & format

```sh
cargo clippy
cargo fmt -- --check
```

Clang and ffmpeg are system deps provided by the Nix shell.

## Required env vars

- `JELLYFIN_URL`, `JELLYFIN_API_KEY` — Jellyfin media server credentials.
- `DATABASE_URL` — set automatically by flake; do not override unless you want a different DB.

## Architecture

```
src/
  main.rs         — entrypoint, wires providers, prints series list
  config.rs       — loads JELLYFIN_URL + JELLYFIN_API_KEY from env
  models/         — domain types: Series, TmdbId, Rating, SeriesId
  providers/      — external API adapters
    mod.rs        — SeriesProvider trait (async_trait)
    jellyfin.rs   — Jellyfin HTTP client (reqwest + axum headers)
    error.rs      — ProviderError enum
```

- `SeriesProvider` trait is the integration boundary for media sources.
- Jellyfin auth uses `X-Emby-Token` header (Emby-compatible API).

## Dependencies

- **rmcp** — MCP server (likely for OpenCode agent communication).
- **axum** — web framework.
- **sqlx** — postgres ORM with compile-time migration support.
- **rumqttc** — MQTT client.
- **ffmpeg** — pinned via Nix; no Rust wrapper yet.

## VCS

Uses **jj** (`.jj/`), not plain git. Use `jj` commands instead of `git`.
