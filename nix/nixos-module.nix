self:
{
  config,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.services.rankoder;
in
{
  options.services.rankoder = {
    enable = lib.mkEnableOption "rankoder HEVC re-encoding service";

    package = lib.mkOption {
      type = lib.types.package;
      default = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
      defaultText = lib.literalExpression "rankoder.packages.\${system}.default";
      description = "The rankoder package to run.";
    };

    user = lib.mkOption {
      type = lib.types.str;
      default = "rankoder";
      description = ''
        System user the service runs as. Also the PostgreSQL role used for
        Unix-socket peer authentication, so it must match a role that owns the
        database (see {option}`services.rankoder.database.provision`).
      '';
    };

    group = lib.mkOption {
      type = lib.types.str;
      default = "rankoder";
      description = "System group the service runs as.";
    };

    environmentFile = lib.mkOption {
      type = lib.types.path;
      example = "/run/secrets/rankoder.env";
      description = ''
        Path to an environment file (kept out of the Nix store) holding the
        secrets as `KEY=VALUE` lines. Required: `JELLYFIN_API_KEY`. Optional:
        `RADARR_API_KEY`, `SONARR_API_KEY`, plus any MQTT credentials. Works
        with sops-nix / agenix by pointing at the decrypted file.
      '';
    };

    jellyfinUrl = lib.mkOption {
      type = lib.types.str;
      example = "https://jellyfin.example.com";
      description = "Base URL of the Jellyfin server (JELLYFIN_URL).";
    };

    mqtt = {
      host = lib.mkOption {
        type = lib.types.str;
        default = "localhost";
        description = "MQTT broker host (Home Assistant) for approval messages.";
      };
      port = lib.mkOption {
        type = lib.types.port;
        default = 1883;
        description = "MQTT broker port.";
      };
    };

    radarrUrl = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = null;
      example = "https://radarr.example.com";
      description = ''
        Radarr base URL. When set (with `RADARR_API_KEY` in the environment
        file), a completed movie transcode triggers a Radarr RescanMovie.
      '';
    };

    sonarrUrl = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = null;
      example = "https://sonarr.example.com";
      description = ''
        Sonarr base URL. When set (with `SONARR_API_KEY` in the environment
        file), a completed episode transcode triggers a Sonarr RescanSeries.
      '';
    };

    database = {
      url = lib.mkOption {
        type = lib.types.str;
        default = "postgresql://${cfg.user}@localhost/${cfg.database.name}?host=/run/postgresql";
        defaultText = lib.literalExpression
          ''"postgresql://''${user}@localhost/''${database.name}?host=/run/postgresql"'';
        description = ''
          DATABASE_URL passed to the app. Connects to the local shared
          PostgreSQL over its Unix socket: the `host=` query parameter points at
          the socket directory (sqlx uses it because it starts with `/`), while
          the `localhost` authority is only a placeholder — sqlx rejects an empty
          host. The username is set explicitly (to {option}`user`) so peer
          authentication maps it to the role of the same name.
        '';
      };
      name = lib.mkOption {
        type = lib.types.str;
        default = "rankoder";
        description = "Database name to provision when {option}`provision` is true.";
      };
      provision = lib.mkOption {
        type = lib.types.bool;
        default = true;
        description = ''
          Add the database and a peer-authenticated role (named after
          {option}`user`) to the host's PostgreSQL via
          `services.postgresql.ensureDatabases`/`ensureUsers`. Assumes
          PostgreSQL is already enabled on this host. The schema itself is
          created at first start by the app's migrations
          (see {option}`autoMigrate`).
        '';
      };
    };

    tmpDir = lib.mkOption {
      type = lib.types.str;
      example = "/srv/media/.rankoder-tmp";
      description = ''
        Scratch directory for in-progress encodes (TRANSCODE_TMP_DIR). Should
        be on the same filesystem as the library for atomic moves.
      '';
    };

    retentionDir = lib.mkOption {
      type = lib.types.str;
      example = "/srv/rankoder-retention";
      description = ''
        Directory where originals are moved after a successful transcode
        (TRANSCODE_RETENTION_DIR). Must live OUTSIDE the Radarr/Sonarr library
        folders, otherwise the rescan re-imports the moved original.
      '';
    };

    retentionDays = lib.mkOption {
      type = lib.types.int;
      default = 7;
      description = "Days to keep originals before the reaper deletes them.";
    };

    mediaPaths = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = [ ];
      example = [ "/mnt/storage/medias" ];
      description = ''
        Library directories whose files rankoder rewrites in place. With the
        hardened `ProtectSystem = "strict"` the whole filesystem is read-only
        except the service's writable paths, so the roots that hold the
        movies/episodes MUST be listed here — otherwise the final swap fails
        with EROFS ("Read-only file system"). List the common root(s), e.g. the
        Jellyfin library mount.
      '';
    };

    autoMigrate = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = "Run database migrations automatically at startup (AUTO_MIGRATE).";
    };

    logLevel = lib.mkOption {
      type = lib.types.str;
      default = "info";
      description = "Value for RUST_LOG / the tracing EnvFilter.";
    };

    syncInterval = lib.mkOption {
      type = lib.types.int;
      default = 3600;
      description = ''
        Periodic library re-sync cadence in seconds (SYNC_INTERVAL_SECS). The
        sync also runs once immediately at startup and on each webhook trigger;
        this timer is the safety net that guarantees eventual convergence even
        if a webhook is missed. `0` disables the timer (startup + webhook only).
      '';
    };

    webhook = {
      enable = lib.mkOption {
        type = lib.types.bool;
        default = false;
        description = ''
          Run the webhook HTTP server so Radarr/Sonarr (Connect → Webhook,
          "On Import") and Jellyfin (Webhook plugin) can trigger a library
          re-sync on demand. Triggers are debounced and coalesced into a single
          sync. Requires `WEBHOOK_TOKEN` in {option}`environmentFile`; callers
          must send it in the `X-Rankoder-Token` header.
        '';
      };
      address = lib.mkOption {
        type = lib.types.str;
        default = "127.0.0.1";
        description = ''
          Address to bind the webhook server. Defaults to loopback, which is
          correct when Radarr/Sonarr/Jellyfin run on this host; no firewall hole
          is opened. Use a LAN address only if the callers are remote (and open
          the port yourself).
        '';
      };
      port = lib.mkOption {
        type = lib.types.port;
        default = 8765;
        description = "Port for the webhook server (combined with {option}`address` into WEBHOOK_BIND).";
      };
    };

    hardwareAcceleration = lib.mkOption {
      type = lib.types.bool;
      default = false;
      description = ''
        Grant the service access to the GPU (`/dev/dri` for VAAPI/QSV and the
        `/dev/nvidia*` nodes for NVENC, plus the video/render groups) for
        hardware HEVC encoding. Leave off for software-only encoding.
      '';
    };

    minVmaf = lib.mkOption {
      type = lib.types.float;
      default = 0.0;
      example = 92.0;
      description = ''
        Post-encode VMAF quality gate (MIN_VMAF). `0.0` (the default) is
        observe-only: the score is always measured and stored under
        `transcode_spec.vmaf`, but never rejects an encode. Set above `0` to
        reject encodes scoring below it (`skipped` / `QualityTooLow`). Calibrate
        from the recorded distribution before enforcing; a healthy h264->HEVC
        encode pools around 95-97, so a threshold near 92 gates real regressions
        without false rejects. Lowering it later pairs with
        {option}`requeueQualitySkips` to re-encode files that now clear the bar.
      '';
    };

    backfillVmaf = lib.mkOption {
      type = lib.types.bool;
      default = false;
      description = ''
        One-shot maintenance pass (BACKFILL_VMAF): on startup, measure VMAF for
        already-transcoded (`done`) files that predate the quality gate, while
        their original is still in retention. Idempotent — already-scored files
        are skipped — so leaving it on is harmless, but the intended workflow is
        enable -> deploy once -> disable. Time-limited by
        {option}`retentionDays`: only files whose original has not yet been
        reaped can be scored.
      '';
    };

    requeueQualitySkips = lib.mkOption {
      type = lib.types.bool;
      default = false;
      description = ''
        One-shot maintenance pass (REQUEUE_QUALITY_SKIPS): on startup, re-encode
        files previously rejected as `QualityTooLow` whose recorded VMAF now
        clears `MIN_VMAF`. Use after lowering `MIN_VMAF`. Safe and idempotent;
        enable -> deploy once -> disable.
      '';
    };

    settings = lib.mkOption {
      type = lib.types.attrsOf lib.types.str;
      default = { };
      example = lib.literalExpression ''{ MIN_ANALYSIS_BPP = "0.05"; }'';
      description = "Extra environment variables, merged last (can override the above).";
    };
  };

  config = lib.mkIf cfg.enable {
    users.users = lib.mkIf (cfg.user == "rankoder") {
      rankoder = {
        isSystemUser = true;
        group = cfg.group;
        description = "rankoder service user";
      };
    };

    users.groups = lib.mkIf (cfg.group == "rankoder") {
      rankoder = { };
    };

    services.postgresql = lib.mkIf cfg.database.provision {
      ensureDatabases = [ cfg.database.name ];
      ensureUsers = [
        {
          name = cfg.user;
          ensureDBOwnership = true;
        }
      ];
    };

    systemd.tmpfiles.rules = [
      "d ${cfg.tmpDir} 0750 ${cfg.user} ${cfg.group} - -"
      "d ${cfg.retentionDir} 0750 ${cfg.user} ${cfg.group} - -"
    ];

    systemd.services.rankoder = {
      description = "rankoder HEVC re-encoder";
      wantedBy = [ "multi-user.target" ];
      # Ordering only: a non-existent postgresql.service (remote DB) is ignored.
      after = [
        "network-online.target"
        "postgresql.service"
      ];
      wants = [ "network-online.target" ];

      environment = {
        DATABASE_URL = cfg.database.url;
        AUTO_MIGRATE = if cfg.autoMigrate then "1" else "0";
        JELLYFIN_URL = cfg.jellyfinUrl;
        MQTT_HOST = cfg.mqtt.host;
        MQTT_PORT = toString cfg.mqtt.port;
        TRANSCODE_TMP_DIR = cfg.tmpDir;
        TRANSCODE_RETENTION_DIR = cfg.retentionDir;
        TRANSCODE_RETENTION_DAYS = toString cfg.retentionDays;
        MIN_VMAF = toString cfg.minVmaf;
        SYNC_INTERVAL_SECS = toString cfg.syncInterval;
        RUST_LOG = cfg.logLevel;
      }
      // lib.optionalAttrs (cfg.radarrUrl != null) { RADARR_URL = cfg.radarrUrl; }
      // lib.optionalAttrs (cfg.sonarrUrl != null) { SONARR_URL = cfg.sonarrUrl; }
      // lib.optionalAttrs cfg.webhook.enable {
        WEBHOOK_BIND = "${cfg.webhook.address}:${toString cfg.webhook.port}";
      }
      // lib.optionalAttrs cfg.backfillVmaf { BACKFILL_VMAF = "1"; }
      // lib.optionalAttrs cfg.requeueQualitySkips { REQUEUE_QUALITY_SKIPS = "1"; }
      // cfg.settings;

      serviceConfig = {
        ExecStart = lib.getExe cfg.package;
        User = cfg.user;
        Group = cfg.group;
        EnvironmentFile = cfg.environmentFile;

        # /var/lib/rankoder; also the WorkingDirectory so the app's relative
        # `logs/` directory lands there.
        StateDirectory = "rankoder";
        StateDirectoryMode = "0750";
        WorkingDirectory = "/var/lib/rankoder";

        Restart = "on-failure";
        RestartSec = 10;

        # Hardening.
        NoNewPrivileges = true;
        ProtectSystem = "strict";
        ProtectHome = true;
        PrivateTmp = true;
        ProtectKernelTunables = true;
        ProtectKernelModules = true;
        ProtectControlGroups = true;
        RestrictNamespaces = true;
        RestrictRealtime = true;
        LockPersonality = true;
        # The media library roots are needed read-write: the swap replaces the
        # original in place. Without them ProtectSystem=strict makes the library
        # read-only and the swap fails with EROFS.
        ReadWritePaths = [
          cfg.tmpDir
          cfg.retentionDir
        ]
        ++ cfg.mediaPaths;
      }
      // lib.optionalAttrs cfg.hardwareAcceleration {
        PrivateDevices = false;
        # /dev/dri covers VAAPI/QSV; the /dev/nvidia* nodes are required for
        # NVENC (hevc_nvenc). Without them an explicit DeviceAllow forces a
        # "closed" device policy that hides the GPU, so the encoder probe falls
        # back to libx265 (CPU).
        DeviceAllow = [
          "/dev/dri rw"
          "/dev/nvidia0 rw"
          "/dev/nvidiactl rw"
          "/dev/nvidia-uvm rw"
          "/dev/nvidia-uvm-tools rw"
          "/dev/nvidia-modeset rw"
        ];
        SupplementaryGroups = [
          "video"
          "render"
        ];
      };
    };
  };
}
