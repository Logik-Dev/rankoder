{
  pkgs,
  lib,
  config,
  inputs,
  ...
}:
let
  pgPort = 5433;
  pgSocketDir = "/tmp";
in
{

  env = {

    RADARR_URL = "https://radarr.hyper.logikdev.fr";
    RADARR_API_KEY = config.secretspec.secrets.RADARR_API_KEY;

    JELLYFIN_URL = "https://jellyfin.hyper.logikdev.fr";
    JELLYFIN_API_KEY = config.secretspec.secrets.JELLYFIN_API_KEY;
    DATABASE_URL = "postgresql://logikdev@localhost:${toString pgPort}/rankoder";

    PGPORT = lib.mkForce pgPort;
    PGHOST = pgSocketDir;

    MQTT_HOST = "localhost";
    MQTT_PORT = "1883";
  };

  packages = [
    pkgs.ffmpeg
    pkgs.jujutsu
    pkgs.sqlx-cli
    pkgs.secretspec
    pkgs.cargo-watch
  ];

  languages = {
    rust = {
      enable = true;
      channel = "stable";
    };
  };

  services.postgres = {
    enable = true;
    port = pgPort;
    initialDatabases = [
      {
        name = "rankoder";
      }
    ];
  };

  processes.watch.exec = "cargo watch -x run";

  enterShell = ''
    pg_data="${config.env.DEVENV_STATE}/postgres"
    pg_pidfile="$pg_data/postmaster.pid"
    if [ -f "$pg_pidfile" ]; then
      pg_pid=$(head -1 "$pg_pidfile")
      if ! kill -0 "$pg_pid" 2>/dev/null; then
        echo "devenv: removing stale PostgreSQL lock file (pid $pg_pid no longer running)"
        rm -f "$pg_pidfile"
      else
        echo "devenv: stopping orphaned PostgreSQL (pid $pg_pid) to allow clean restart"
        pg_ctl stop -D "$pg_data" -m fast -w 2>/dev/null || kill "$pg_pid"
      fi
    fi
  '';

  dotenv.enable = true;

  scripts = {
    "reset-db".exec = ''
      sqlx database drop
      #psql -h localhost -p ${toString pgPort} postgres \
      #  -c "DROP DATABASE IF EXISTS rankoder WITH (FORCE);"
      sqlx database create
      sqlx migrate run
    '';

  };
  git-hooks.hooks = {
    rustfmt.enable = true;
    clippy.enable = true;
  };
}
