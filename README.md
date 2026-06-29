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

- **Sync** (at startup, then periodically and on demand): `JellyfinProvider` →
  `SyncOrchestrator` → `MediaStore`. Fetches series, episodes and movies and
  upserts them. A `SyncScheduler` runs it once immediately at startup, then on a
  timer and on webhook triggers — see [Library sync](#library-sync).
- **Event** (daemon): a row in the `events` table fires `pg_notify` →
  `PostgresListener` → `WorkflowOrchestrator` → ffprobe → analysis → MQTT
  approval → transcode → done.

Workflow states:

```
discovered → probed → analyzed → pending_approval → transcoding → done
                                                                 → skipped
                                                                 → failed
```

## Library sync

The `SyncScheduler` owns the library sync and runs it from three sources, all
coalesced behind a single-flight loop (two syncs never overlap):

1. **Startup** — one immediate, non-blocking sync. A failure is non-fatal: the
   daemon keeps serving work already in the DB (the listener reconciles active
   files from DB state, independent of the sync), and the next tick/trigger
   retries. So Jellyfin being briefly down at boot no longer crash-loops the
   service.
2. **Periodic** (`syncInterval`, default 1h) — the safety net that guarantees
   eventual convergence even if a trigger is missed. `0` disables it.
3. **Webhook** (`http.enable` + `WEBHOOK_TOKEN`) — on-demand, event-driven.
   Radarr, Sonarr and Jellyfin POST to `/sync` when the library changes; bursts
   (e.g. importing a season) are **debounced** (`SYNC_DEBOUNCE_SECS`, default
   15s) and collapsed into one sync.

The HTTP server (`http.enable`) exposes `POST /sync` (requires the
`X-Rankoder-Token` header), `GET /healthz`, and the operator
[dashboard](#dashboard) at `GET /`. It binds to loopback by default — correct
when the *arr stack and Jellyfin run on the same host, with no firewall hole.
The `/sync` body is ignored: any call just nudges a full re-sync. The webhook is
mounted only when `WEBHOOK_TOKEN` is set; without it the UI is still served.

Configure the callers to `POST http://127.0.0.1:8765/sync` with header
`X-Rankoder-Token: <token>`:

- **Radarr / Sonarr** — *Settings → Connect → + → Webhook*: URL above, method
  `POST`, add the header, tick *On Import* / *On Upgrade*.
- **Jellyfin** — the *Webhook* plugin: add a destination pointing at the URL,
  with the header, for the `ItemAdded` notification.

## Dashboard

When the HTTP server is enabled (`http.enable`), `GET /` serves an operator
dashboard: per-state file counts, total space saved, the outstanding **backlog**
(files decided but not yet transcoded, with projected savings), the batches
**pending approval**, a **codec × state** breakdown, the VMAF score
distribution, the **quality skips** (files rejected on the VMAF gate),
**failures grouped by cause** and the most recent failure. It is
server-rendered (no JavaScript, no build step) and
reads straight through the app's database pool, so there is nothing extra to
deploy — it ships inside the binary. The page refreshes itself every 60s.

The UI has **no authentication of its own**: keep the bind on loopback
(`http.address`, default `127.0.0.1`) and put it behind your reverse proxy,
which handles auth. The `/sync` webhook keeps its own `X-Rankoder-Token` gate
for machine callers.

### Remediation actions

The dashboard is read-only by default. Setting **`UI_CONTROL_TOKEN`** (in
`environmentFile`) unlocks the mutating actions:

- **Approve / reject a pending batch.** The *Pending approval* panel lists each
  batch (season or movie) awaiting a decision — title, file count, current size,
  projected savings, TMDB rating — with **Approve** (→ `transcoding`) and
  **Reject** (→ `skipped`) buttons. This funnels into the *same* approval
  chokepoint the MQTT round-trip uses, so it **coexists** with Home Assistant
  rather than replacing it: either source can decide a batch, and a race or
  double-submit is a safe no-op (the batch transition is an atomic
  compare-and-swap). The per-batch in-flight cap (`approvalMaxPending`) still
  applies — approving frees a slot and the feeder tops the queue back up.

- **Requeue failed (per cause).** On the failures panel, a button per cause
  moves the failed files of a given class back to `discovered` so the pipeline
  re-probes and re-drives them. The panel labels each cause: `missing video
  properties` is *requeue-safe* (a re-probe repopulates it), while swap I/O
  errors (permission denied, read-only filesystem, cross-device) are
  environmental — **fix the host first** (e.g. directory permissions), otherwise
  the file just re-encodes and fails again at the swap.

- **Re-verify quality skips.** The *Quality skips* panel counts the files left
  in `skipped` because their post-encode VMAF was below `MIN_VMAF`, with the
  originals' size (what a successful re-verify reclaims). A **Re-verify** button
  flips *all* of them back to `transcoding` so the orchestrator re-encodes them,
  **recomputes the VMAF from scratch** and re-applies `MIN_VMAF` — the ones that
  now clear the bar complete, genuine rejects simply re-skip. Unlike the
  threshold-based `REQUEUE_QUALITY_SKIPS` flag it deliberately **ignores the
  stored score**, so it recovers good encodes whose recorded VMAF was a
  measurement artefact (e.g. a framesync misalignment) rather than a real
  quality loss. The transcode orchestrator's stale re-queue picks the rows up on
  its own — no extra plumbing.

- **Delete confirmed originals.** After a successful transcode the original is
  held in retention for a safety window (`retentionDays`) before the reaper
  prunes it. The retention panel splits held originals into *quality-confirmed*
  (`done` **and** VMAF ≥ `MIN_VMAF`) versus held, and a **Delete originals**
  button reclaims the confirmed set's disk space immediately. Originals with no
  recorded VMAF or a score below the bar are kept — so under `MIN_VMAF=0`
  (observe-only) nothing is offered for deletion until a real bar is set.

When `UI_CONTROL_TOKEN` is unset the `/actions/*` routes are not mounted and the
dashboard stays strictly read-only. When set, the token is embedded as a hidden
field in each action form and verified on POST: defence in depth on top of the
proxy, and a same-origin guard (a cross-origin page cannot read the token to
forge the request). Actions are plain HTML forms with POST-redirect-GET, so the
zero-JS, no-build property holds.

## Monitoring (MQTT / Home Assistant)

Everything operator-facing goes through one MQTT connection, under `rankoder/`:

| Topic | Dir | QoS | Retained | Payload |
| --- | --- | --- | --- | --- |
| `rankoder/approval/request` | out | 1 | no | `{ batch_id, title, file_count, total_size_gb, total_space_saved_gb, tmdb_rating }` |
| `rankoder/approval/response` | in | 1 | no | `{ batch_id, approved }` |
| `rankoder/failure` | out | 1 | no | `{ media_file_id, kind, title, reason }` |
| `rankoder/status` | out | 1 | **yes** | `{ version, discovered, probed, analyzed, pending_approval, transcoding, done, skipped, failed, space_saved_gb, last_failure }` |

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

### Versioning & releases

[Semantic Versioning](https://semver.org), applied to the **operational
contract** — NixOS module options, environment variables, MQTT topics/payloads,
DB migrations — not a Rust API (rankoder ships a binary, not a library):

- `fix:` → **patch**, `feat:` → **minor**, `feat!:` / `BREAKING CHANGE:` →
  **major**. In `0.x`, a breaking change bumps the **minor** (`0.1 → 0.2`).
- Commits follow [Conventional Commits](https://www.conventionalcommits.org);
  [`git-cliff`](https://git-cliff.org) (in the dev shell) turns them into
  `CHANGELOG.md`.

Cutting a release (jj cannot create tags itself — use the colocated `.git`):

```sh
# 1. bump version in Cargo.toml, then regenerate the changelog for the new tag
git-cliff --tag vX.Y.Z -o CHANGELOG.md
# 2. commit the bump + changelog
jj commit -m "chore(release): X.Y.Z"
# 3. tag it via git, then push the tag
git tag -a vX.Y.Z -m "rankoder X.Y.Z"
```

Deployment stays on a rolling `main`; tags are milestones / rollback points
(`nix flake update` to a specific rev or tag if a release misbehaves).

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
            #   MQTT_PASSWORD=…       (optional, pairs with mqtt.username)
            environmentFile = config.sops.secrets.rankoder-env.path;

            jellyfinUrl = "https://jellyfin.example.com";
            mqtt.host   = "localhost";
            # mqtt.username = "rankoder";   # optional, needs MQTT_PASSWORD

            radarrUrl = "https://radarr.example.com";   # optional
            sonarrUrl = "https://sonarr.example.com";   # optional

            tmpDir       = "/srv/media/.rankoder-tmp";   # same FS as library
            retentionDir = "/srv/rankoder-retention";    # OUTSIDE Radarr/Sonarr libs

            # Library roots to grant read-write so the in-place swap can
            # replace originals (required under ProtectSystem=strict):
            mediaPaths = [ "/srv/media" ];

            # hardwareAcceleration = true;  # grant /dev/dri (VAAPI/QSV) + /dev/nvidia* (NVENC)
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
| `mediaPaths` | `[]` | Library roots to grant **read-write** so the in-place swap can replace originals (e.g. `[ "/mnt/storage/medias" ]`). Required under `ProtectSystem=strict`, else the swap fails with EROFS. |
| `mqtt.host` / `mqtt.port` | `localhost` / `1883` | Approval broker |
| `mqtt.username` | `null` | MQTT auth username; pair with `MQTT_PASSWORD` in `environmentFile` |
| `radarrUrl` / `sonarrUrl` | `null` | Enable per-manager refresh |
| `database.url` | local socket | `DATABASE_URL` |
| `database.provision` | `true` | Create DB + role on existing Postgres |
| `retentionDays` | `7` | Days before originals are reaped |
| `autoMigrate` | `true` | Run migrations at startup |
| `hardwareAcceleration` | `false` | Grant the GPU: `/dev/dri` (VAAPI/QSV) + `/dev/nvidia*` (NVENC) + video/render groups |
| `logLevel` | `info` | `RUST_LOG` / tracing filter |
| `syncInterval` | `3600` | Periodic library re-sync cadence in seconds (`SYNC_INTERVAL_SECS`). `0` = startup + webhook only |
| `http.enable` | `false` | Run the HTTP server: operator [dashboard](#dashboard) (`GET /`) + sync webhook (`POST /sync`). The webhook needs `WEBHOOK_TOKEN` in `environmentFile`; without it the UI is still served |
| `http.address` / `http.port` | `127.0.0.1` / `8765` | Bind address for the HTTP server (`HTTP_BIND`). Keep on loopback behind a reverse proxy |
| `UI_CONTROL_TOKEN` | unset | **Secret** (set in `environmentFile`, not a module option): unlocks the dashboard's mutating [remediation actions](#remediation-actions) (approve/reject pending batches, re-verify quality skips, failure requeue, delete confirmed originals). Unset = strictly read-only UI |
| `minVmaf` | `0.0` | Post-encode VMAF quality gate (`MIN_VMAF`). `0` = observe only (measure + record, never reject); set > 0 (e.g. `92`) to reject encodes below it |
| `backfillVmaf` | `false` | One-shot: score `done` files that predate the VMAF gate (`BACKFILL_VMAF`). Enable → deploy once → disable |
| `requeueQualitySkips` | `false` | One-shot: re-encode `QualityTooLow` skips that now clear `MIN_VMAF` (`REQUEUE_QUALITY_SKIPS`). Enable → deploy once → disable |
| `settings` | `{}` | Extra env vars (override the above) — see Analysis & quality tuning |

### Analysis & quality tuning

Non-secret knobs passed via `settings` (env vars). All have defaults; override
only what you need:

| Env var | Default | Notes |
| --- | --- | --- |
| `MIN_ANALYSIS_BPP` | `0.04` | h264 bits-per-pixel baseline |
| `MIN_ANALYSIS_BPP_HEVC` | `0.15` | HEVC baseline — only re-encode clearly over-bitrate (remux-tier) HEVC. AV1 is never re-encoded |
| `MIN_COMPRESSION_POTENTIAL` | `1.0` | Resolution-aware headroom gate |
| `MIN_ANALYSIS_SIZE_PER_HOUR_GB` | `2.0` | Skip files below this size/hour |
| `TRANSCODE_MIN_SIZE_REDUCTION` | `0.1` | Reject an encode that isn't at least this much smaller |
| `MIN_VMAF` | `0.0` | Post-encode VMAF gate. `0` = observe only (measure + record, never reject); set > 0 to reject encodes below it. Also exposed as the first-class `minVmaf` option above |
| `VMAF_N_SUBSAMPLE` | `5` | Evaluate 1 frame out of N for VMAF (cost vs precision) |
| `VMAF_N_THREADS` | `6` | Threads for libvmaf (single-threaded otherwise → ~3x faster). Capped to leave cores for the host; `0` lets libvmaf decide |

The VMAF score is recorded under `transcode_spec.vmaf` for **every** attempt
(accepted or rejected), so the threshold can be calibrated from the real
distribution before enforcing:

```sql
SELECT round((transcode_spec->>'vmaf')::float8) AS vmaf, count(*)
FROM media_files WHERE transcode_spec ? 'vmaf' GROUP BY 1 ORDER BY 1;
```

VMAF requires ffmpeg built with libvmaf; the flake's `packages.default` wraps
`ffmpeg.override { withVmaf = true; }` automatically.

#### One-shot VMAF maintenance flags

Two startup flags help calibrate and operate the gate. Both are idempotent, but
the intended workflow is **set → run once → unset**.

| Env var | Default | Notes |
| --- | --- | --- |
| `BACKFILL_VMAF` | `0` | On startup, measure VMAF for `done` files that predate the gate, **while their original is still in retention**. Idempotent (scored files are skipped), runs sequentially in the background so it doesn't starve live transcodes |
| `REQUEUE_QUALITY_SKIPS` | `0` | On startup, re-encode files previously rejected as `QualityTooLow` whose recorded score now clears the current `MIN_VMAF`. Use after **lowering** `MIN_VMAF` |

`BACKFILL_VMAF` is time-limited: only files whose original is still within
`TRANSCODE_RETENTION_DAYS` (7 by default) can be scored — the rest are already
reaped. Count the backfillable population first:

```sql
SELECT count(*)
FROM media_files mf JOIN retention_files rf ON rf.media_file_id = mf.id
WHERE mf.workflow_state = 'done' AND NOT (mf.transcode_spec ? 'vmaf');
```

## Roadmap

Ordered by priority. The first items close out the current VMAF work; the later
ones open new fronts.

### Near-term

1. **Enable the VMAF quality gate.** The measurement is now trustworthy (frame
   PTS aligned, log path made filename-safe) and the recorded distribution is
   clean (~94–97). Finish the backfill rattrapage, then set `minVmaf ≈ 92` to
   move the gate from *observe* to *protect* for new transcodes.
2. **Stats UI (phase 1).** ✅ Done — a server-rendered [dashboard](#dashboard)
   inside the binary (axum + maud, zero JS): per-state counts, space saved,
   backlog with projected savings, codec × state breakdown, failures by cause,
   VMAF distribution.
3. **Maintenance UI (phase 2).** In progress — mutating [remediation
   actions](#remediation-actions) on the same dashboard (plain HTML forms, gated
   by `UI_CONTROL_TOKEN`), replacing the one-shot env flags and NixOS rebuilds.
   Done: **approve/reject pending batches** from the UI (coexists with MQTT —
   unblocks the h264 backlog stuck behind the approval gate), per-cause failure
   **requeue**, **delete quality-confirmed originals** from retention,
   **re-verify quality skips** (re-encode + recompute VMAF, ignoring the stored
   score, to recover encodes mis-rejected by a measurement artefact). Next:
   on-demand VMAF backfill, re-analyse skips.

### Codec coverage

4. **Dolby Vision (RPU handling).** DV is currently *skipped* (`dv_profile` set,
   `SkipReason::DolbyVision`) because a naive re-encode strips the RPU. Extract
   and re-inject the RPU with `dovi_tool` to bring this population — counted via
   `SELECT dv_profile, count(*) … WHERE dv_profile IS NOT NULL` — back in scope.
   The only item that *grows* the addressable savings.
5. **HDR10+ dynamic metadata.** Today it falls back to static HDR10. Preserving
   the dynamic metadata is complex for a small population — backlog.

### Ideas / backlog

Not yet prioritised — candidates to make rankoder more useful, resilient or
interesting:

- **Quality-targeted encoding.** Replace the fixed CRF + post-hoc gate with a
  short CRF search that lands on a *target* VMAF. Maximises savings while
  guaranteeing quality, instead of discovering a bad encode only after the fact.
- **Transcode scheduling / quiet hours.** Restrict encoding to off-peak windows
  (or pause via an MQTT command topic) so it never competes with Jellyfin
  playback for CPU/GPU.
- **Auto-retry transient failures.** Distinguish transient (I/O, NFS, GPU busy)
  from permanent failures and retry with backoff instead of leaving the file
  `failed`.
- **Pre-flight free-space check.** Verify `tmpDir` has room for the encode
  before starting; fail fast and clearly instead of mid-encode.
- **systemd watchdog.** `sd_notify` heartbeat + `WatchdogSec` so a hung ffmpeg
  restarts the service rather than stalling the pipeline silently.
- **AV1 output.** Optionally target AV1 (SVT-AV1 / NVENC AV1) for extra savings
  on capable content, alongside HEVC.
- **Per-frame VMAF floor.** Record the min / low-percentile VMAF, not just the
  pooled mean, to catch localised quality dips that a healthy average hides.
- **Dry-run savings report.** Analyse the whole library and emit a
  projected-savings report without transcoding — to size the opportunity before
  committing.
