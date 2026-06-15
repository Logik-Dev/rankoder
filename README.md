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

## Monitoring (MQTT / Home Assistant)

Everything operator-facing goes through one MQTT connection, under `rankoder/`:

| Topic | Dir | QoS | Retained | Payload |
| --- | --- | --- | --- | --- |
| `rankoder/approval/request` | out | 1 | no | `{ batch_id, title, file_count, total_size_gb, total_space_saved_gb, tmdb_rating }` |
| `rankoder/approval/response` | in | 1 | no | `{ batch_id, approved }` |
| `rankoder/failure` | out | 1 | no | `{ media_file_id, kind, title, reason }` |
| `rankoder/status` | out | 1 | **yes** | `{ discovered, probed, analyzed, pending_approval, transcoding, done, skipped, failed, space_saved_gb, last_failure }` |

`rankoder/status` is retained and republished every 60s, so a fresh subscriber
(e.g. Home Assistant restarting) immediately gets the current state.
`rankoder/failure` fires once per failure; on a rankoder restart, past failures
are **not** re-sent (they remain visible in the status counts).

The snippets below assume the [MQTT integration](https://www.home-assistant.io/integrations/mqtt/)
is configured and use the modern (`triggers`/`actions`) automation syntax.
Replace `mobile_app_your_phone` with your `notify` service.

### Status sensors

```yaml
# configuration.yaml
mqtt:
  sensor:
    - name: "rankoder done"
      unique_id: rankoder_done
      state_topic: "rankoder/status"
      value_template: "{{ value_json.done }}"
      icon: mdi:check-circle
    - name: "rankoder failed"
      unique_id: rankoder_failed
      state_topic: "rankoder/status"
      value_template: "{{ value_json.failed }}"
      icon: mdi:alert-circle
    - name: "rankoder transcoding"
      unique_id: rankoder_transcoding
      state_topic: "rankoder/status"
      value_template: "{{ value_json.transcoding }}"
      icon: mdi:cog
    - name: "rankoder pending approval"
      unique_id: rankoder_pending_approval
      state_topic: "rankoder/status"
      value_template: "{{ value_json.pending_approval }}"
      icon: mdi:account-clock
    - name: "rankoder skipped"
      unique_id: rankoder_skipped
      state_topic: "rankoder/status"
      value_template: "{{ value_json.skipped }}"
      icon: mdi:debug-step-over
    - name: "rankoder space saved"
      unique_id: rankoder_space_saved
      state_topic: "rankoder/status"
      value_template: "{{ value_json.space_saved_gb | round(1) }}"
      unit_of_measurement: "GB"
      device_class: data_size
      state_class: total_increasing
    - name: "rankoder last failure"
      unique_id: rankoder_last_failure
      state_topic: "rankoder/status"
      value_template: >-
        {{ value_json.last_failure.title if value_json.last_failure
           else 'none' }}
      json_attributes_topic: "rankoder/status"
      json_attributes_template: "{{ value_json.last_failure | tojson }}"
```

### Failure alerts

```yaml
# automations.yaml
- alias: "rankoder — transcode failure alert"
  mode: queued
  triggers:
    - trigger: mqtt
      topic: "rankoder/failure"
  actions:
    - action: notify.mobile_app_your_phone
      data:
        title: "rankoder: transcode failed"
        message: >-
          {{ trigger.payload_json.title or trigger.payload_json.media_file_id }}
          ({{ trigger.payload_json.kind }}) — {{ trigger.payload_json.reason }}
```

### Approval (actionable notification)

A request triggers a notification with Approve/Reject buttons; tapping one
publishes the response back to rankoder. The batch id is carried in the action
string and reconstructed on the way back (robust even if it contains
underscores).

```yaml
# automations.yaml
- alias: "rankoder — approval request"
  mode: queued
  triggers:
    - trigger: mqtt
      topic: "rankoder/approval/request"
  actions:
    - action: notify.mobile_app_your_phone
      data:
        title: "rankoder: approve transcode?"
        message: >-
          {{ trigger.payload_json.title }} —
          {{ trigger.payload_json.file_count }} file(s),
          {{ trigger.payload_json.total_size_gb | round(1) }} GB,
          ~{{ trigger.payload_json.total_space_saved_gb | round(1) }} GB saved
        data:
          actions:
            - action: "RANKODER_APPROVE_{{ trigger.payload_json.batch_id }}"
              title: "Approve"
            - action: "RANKODER_REJECT_{{ trigger.payload_json.batch_id }}"
              title: "Reject"

- alias: "rankoder — approval response"
  mode: queued
  triggers:
    - trigger: event
      event_type: mobile_app_notification_action
  conditions:
    - condition: template
      value_template: "{{ trigger.event.data.action.startswith('RANKODER_') }}"
  actions:
    - variables:
        parts: "{{ trigger.event.data.action.split('_') }}"
        # parts[0]=RANKODER, parts[1]=APPROVE|REJECT, parts[2:]=batch_id
        approved: "{{ parts[1] == 'APPROVE' }}"
        batch_id: "{{ parts[2:] | join('_') }}"
    - action: mqtt.publish
      data:
        topic: "rankoder/approval/response"
        payload: "{{ {'batch_id': batch_id, 'approved': approved} | to_json }}"
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
