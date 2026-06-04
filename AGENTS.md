# AGENTS.md

# Operating rules

You are running in a token-budgeted environment. Follow these rules in every session.

## Output discipline

- No preamble. Skip "Sure", "I'll help with that", "Let me…", "Great question".
- No closing recap of what you just did. The diff/tool output already shows it.
- No alternative approaches unless asked. Pick one, execute it.
- Explanations: 3 sentences max unless I ask "explain" or "why".
- Never echo file contents you just read or just wrote.
- Never repeat the user's request back.
- Code blocks contain code, not commentary. Put comments in the code, not around it.

## Tool usage discipline

- Before calling `read`, check if the file is already in the current context. If yes, reuse it.
- Prefer `grep` over `read` to locate something. Only `read` once you know what range you need.
- Use `read` with a line range when the file is over 200 lines. Never read a full file just to look at one function.
- Prefer `edit` (targeted patch) over `write` (full rewrite) whenever the change is < 50% of the file.
- Stop exploring once you have enough to act. Do not enumerate every file in a directory "for completeness".
- Do not run the same `grep` or `read` twice in one session.

## Reasoning discipline

- No "let me think step by step" out loud. Think internally, output the result.
- No plans printed before code unless I'm in plan mode (`@plan` agent).
- If a task is ambiguous, ask ONE targeted question, not three.
- If you're 90% sure of an answer, act. Do not hedge with two paragraphs of caveats.

## Format

- Plain prose, no headers unless the response is >300 words.
- No bullet lists for under 3 items — use a sentence.
- No emoji.

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

