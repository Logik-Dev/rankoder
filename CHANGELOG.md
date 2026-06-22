# Changelog

All notable changes to rankoder are documented here.
This project adheres to [Semantic Versioning](https://semver.org), applied to
its *operational contract* (NixOS module options, environment variables, MQTT
topics/payloads, DB migrations) rather than a Rust API.


## [0.1.0] - 2026-06-22

### Features

- **sync:** Periodic + webhook-triggered library re-sync
- **nix:** Expose minVmaf module option for the VMAF quality gate
- **analysis:** Detect Dolby Vision and skip it instead of degrading
- **nix:** Expose backfillVmaf and requeueQualitySkips module options
- **vmaf:** Add backfill and requeue maintenance ops for quality-gate calibration
- **transcode:** Measure VMAF as a quality gate (observe-first)
- **analysis:** Re-encode over-bitrate HEVC, not just h264
- **nix:** Grant the media library read-write for in-place swaps
- **mqtt:** Retain approval requests so Home Assistant doesn't miss them
- **nix:** Grant NVENC device access for hardware HEVC encoding
- **notification:** Surface failures and a status snapshot over MQTT
- **transcode:** Re-enqueue stalled transcoding files periodically
- **nix:** Package as a flake and add a NixOS module
- **transcode:** Refresh Sonarr after a completed episode transcode
- **transcode:** Refresh Radarr after a completed transcode
- **approval:** Gate new requests on in-flight work, not just pending
- **store:** Add apply_event helper using state machine next_on
- **transcode:** Pass-through HDR color metadata during encode
- **transcode:** Add post-crash swap reconciliation
- **transcode:** Extract atomic swap and add retention reaper
- **transcode:** Orchestrator with ffmpeg encode, validation, swap, and recovery
- **transcode:** Migration, models, and store methods for transcode lifecycle
- **transcode:** Encoder detection and ffmpeg argument generation
- **approval:** Approve by batch (seasons + movies) instead of per-file
- **approval:** Add bounded approval queue with pull-based feeder
- **approval:** Add stale approval checker with periodic re-publisher
- **shutdown:** Graceful shutdown with CancellationToken + JoinSet
- **probe:** Ffprobe video analysis and metadata persistence

### Bug Fixes

- **sync:** Stop logging expected episode skips at ERROR
- **vmaf:** Write libvmaf log to a safe temp path, not the media filename
- **vmaf:** Align frame PTS before libvmaf to stop spurious low scores
- **transcode:** Fall back to copy+remove when the swap crosses filesystems
- **approval:** Estimate space saved from a real ratio, not the decision score
- **sync:** Don't overwrite probe-owned size_bytes on resync
- **nix:** Use a placeholder host in the socket DATABASE_URL
- **nix:** Don't run tests in the build, and set the DB user for peer auth
- Omit test on crane build to fix sqlx prepared queries
- **sqlx:** Prepare tests requests too
- **transcode:** Make audio/subtitle stream maps optional
- **validation:** Skip attached_pic streams when locating video stream
- **probe:** Skip attached_pic streams when resolving video properties
- **transcode:** Use -map 0:V to exclude cover art from video mapping
- **transcode:** Replace blocking I/O in async contexts
- **transcode:** Fail on missing transcode_spec/crf instead of defaulting
- **transcode:** Use explicit stream mapping instead of -map 0
- **transcode:** Graceful error handling for swap and DB commit
- **transcode:** Parse bitrate from ffprobe validation output
- **transcode:** Remove guaranteed panic on successful encode validation
- **approval:** Make stale checker resilient to store errors
- **mqtt:** Add backoff sleep on persisting connection error in driver loop
- **mqtt:** Resubscribe on ConnAck after reconnection
- **tracing:** Return WorkerGuard from init_tracing instead of forgetting it

### Performance

- **vmaf:** Thread libvmaf to ~3x the measurement speed
- **workflow:** Parallelize processing with JoinSet bounded by Semaphore

### Refactor

- **transcode:** Carry original_size in CompletedTranscode
- **transcode:** Replace repeated temp file cleanup with ScopedTemp guard
- **transcode:** Separate transcode logic from store side-effects
- **transcode:** Add TranscodeOutcome and restructure TranscodeError
- **transcode:** Extract compute_swap_paths shared helper
- Use as_uuid() for filenames instead of Debug format
- **transcode:** Wire encoder override from config instead of env
- **approval:** Simplify ApprovalRequest payload and pre-compute size metrics
- **workflow:** Decouple recv from acquire_owned in select! loop

### Other

- Use EventNotification struct for pg_notify parsing
- Unify into single AppConfig with fail-fast parsing
- Decouple events audit log from transcode_spec operational data
- Extract Prober trait behind Box<dyn> for testability
- Transition to Failed when ffprobe errors
- Extract eventloop into dedicated driver task
- Catch up lost pg_notify events on startup and reconnect
- Compare-and-swap on state transitions + centralise state machine

