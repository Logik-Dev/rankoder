# rankoder

Re-encodes video files (h264 and other codecs) to **HEVC** to reduce storage —
but not blindly. Each file is scored on three criteria before a decision is
made, and the decision is submitted for **human approval over MQTT** (Home
Assistant) before any transcode starts.

Scoring criteria:

1. **TMDB rating** — high-rated content warrants better quality preservation.
2. **Current bitrate** (from ffprobe) — already-efficient files may not benefit.
3. **Compression potential** — estimated h264→HEVC gain for this file.

After a successful transcode, the original is moved to a retention directory
(reaped after a configurable delay) and the relevant media manager is refreshed:
**Radarr** for movies (`RescanMovie`), **Sonarr** for series (`RescanSeries`).

## Architecture

Two pipelines share a `MediaStore` (Postgres):

- **Sync** (once at startup): `JellyfinProvider` → `SyncOrchestrator` →
  `MediaStore`. Fetches series, episodes and movies and upserts them.
- **Event** (daemon): a row in the `events` table fires `pg_notify` →
  `PostgresListener` → `WorkflowOrchestrator` → ffprobe → analysis → MQTT
  approval → transcode → done.

Workflow states:

```
discovered → probed → analyzed → pending_approval → transcoding → done
                                                                 → skipped
                                                                 → failed
```

## Development

Uses **devenv** (not raw flakes) for the dev shell:

```sh
direnv allow        # or: devenv shell
```

It starts a local PostgreSQL on port 5433 and sets `DATABASE_URL` /
`AUTO_MIGRATE=1`. Secrets come from `secretspec` (`secretspec.toml`).

```sh
cargo build
cargo run
cargo clippy --all-targets
cargo test
```

Rust edition 2024 (rustc ≥ 1.85). sqlx checks queries at compile time: in dev
against the live DB, in sandboxed builds against the committed `.sqlx/` offline
data. **After changing any query or migration, run `cargo sqlx prepare` and
commit `.sqlx/`.**

This project uses **jj** (Jujutsu) for version control; a `.git` directory
exists for compatibility. Note that jj does not run git hooks.

## Deployment (NixOS)

The `flake.nix` at the repo root is for packaging/deployment and is independent
of devenv. It exposes:

- `packages.default` — the binary, built with crane (`SQLX_OFFLINE=true`,
  ffmpeg wrapped into `PATH`).
- `nixosModules.default` — a `services.rankoder` systemd service.

> Build on a **Linux** host or remote builder. A macOS build is not
> representative (reqwest uses a different TLS backend there).

### Example configuration

Add the flake as an input and import the module:

```nix
{
  inputs.rankoder.url = "github:youruser/rankoder";

  outputs = { nixpkgs, rankoder, ... }: {
    nixosConfigurations.myhost = nixpkgs.lib.nixosSystem {
      modules = [
        rankoder.nixosModules.default
        ({ config, ... }: {
          services.rankoder = {
            enable = true;

            # KEY=VALUE file kept out of the Nix store (sops-nix / agenix / …):
            #   JELLYFIN_API_KEY=…
            #   RADARR_API_KEY=…      (optional)
            #   SONARR_API_KEY=…      (optional)
            environmentFile = config.sops.secrets.rankoder-env.path;

            jellyfinUrl = "https://jellyfin.example.com";
            mqtt.host   = "localhost";

            radarrUrl = "https://radarr.example.com";   # optional
            sonarrUrl = "https://sonarr.example.com";   # optional

            tmpDir       = "/srv/media/.rankoder-tmp";   # same FS as library
            retentionDir = "/srv/rankoder-retention";    # OUTSIDE Radarr/Sonarr libs

            # hardwareAcceleration = true;  # grant /dev/dri for VAAPI/QSV
          };
        })
      ];
    };
  };
}
```

### What the module does

- Provisions the `rankoder` database and a peer-authenticated role (named after
  `services.rankoder.user`) on the host's **existing** PostgreSQL via
  `ensureDatabases`/`ensureUsers`. It does **not** re-enable PostgreSQL — it
  assumes you already run it. The schema is created at first start by the app's
  migrations (`AUTO_MIGRATE=1`).
- Creates the system user, the tmp/retention directories (tmpfiles), and a
  hardened systemd service with `WorkingDirectory=/var/lib/rankoder` (so the
  app's relative `logs/` dir is writable; the compact log layer also goes to
  journald).
- Reads all secrets from `environmentFile`; non-secret config (URLs, MQTT host,
  etc.) is passed as plain environment.

### Requirements

- The host's PostgreSQL must allow local **peer** authentication
  (`local all all peer`, the NixOS default) so the system user `rankoder` maps
  to the `rankoder` role.
- `DATABASE_URL` defaults to the local Unix socket
  (`postgresql:///rankoder?host=/run/postgresql`); override
  `services.rankoder.database.url` for a remote DB (and set
  `database.provision = false`).

### Key options

| Option | Default | Notes |
| --- | --- | --- |
| `environmentFile` | — (required) | Secrets file (`JELLYFIN_API_KEY`, …) |
| `jellyfinUrl` | — (required) | Jellyfin base URL |
| `tmpDir` / `retentionDir` | — (required) | Scratch / originals retention |
| `mqtt.host` / `mqtt.port` | `localhost` / `1883` | Approval broker |
| `radarrUrl` / `sonarrUrl` | `null` | Enable per-manager refresh |
| `database.url` | local socket | `DATABASE_URL` |
| `database.provision` | `true` | Create DB + role on existing Postgres |
| `retentionDays` | `7` | Days before originals are reaped |
| `autoMigrate` | `true` | Run migrations at startup |
| `hardwareAcceleration` | `false` | Grant `/dev/dri` + video/render groups |
| `logLevel` | `info` | `RUST_LOG` / tracing filter |
| `settings` | `{}` | Extra env vars (override the above) |
