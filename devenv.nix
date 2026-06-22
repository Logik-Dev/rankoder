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
  # Percent-encode the socket dir so it can sit in the host position of a URL
  # ("/tmp" -> "%2Ftmp"). libpq/sqlx read a host starting with "/" as a unix
  # socket directory; the empty-authority "?host=" form is rejected by sqlx.
  pgSocketHost = lib.replaceStrings [ "/" ] [ "%2F" ] pgSocketDir;
in
{

  env = {

    RADARR_URL = "https://radarr.hyper.logikdev.fr";
    RADARR_API_KEY = config.secretspec.secrets.RADARR_API_KEY;

    JELLYFIN_URL = "https://jellyfin.hyper.logikdev.fr";
    JELLYFIN_API_KEY = config.secretspec.secrets.JELLYFIN_API_KEY;
    # Connect over the unix socket (not TCP), so the URL is identical on Linux
    # and macOS. The socket lives in pgSocketDir (see services.postgres below).
    DATABASE_URL = "postgresql://logikdev@${pgSocketHost}:${toString pgPort}/rankoder";

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
    pkgs.git-cliff
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
    # Keep TCP disabled (devenv's default) and pin the unix socket to /tmp.
    # devenv otherwise defaults the socket to $DEVENV_RUNTIME/postgres, whose
    # path varies per machine; pinning it keeps DATABASE_URL/PGHOST stable and
    # identical on Linux and macOS.
    settings.unix_socket_directories = pgSocketDir;
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
    # Only clear a *stale* lock (process is dead). A live Postgres here is the
    # one `devenv up` is managing in the background — never stop it, or every
    # `devenv shell` entry would kill the running database. Use
    # `devenv processes down` to stop it deliberately.
    if [ -f "$pg_pidfile" ]; then
      pg_pid=$(head -1 "$pg_pidfile")
      if ! kill -0 "$pg_pid" 2>/dev/null; then
        echo "devenv: removing stale PostgreSQL lock file (pid $pg_pid no longer running)"
        rm -f "$pg_pidfile"
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

    # Fail the commit if the committed .sqlx/ offline data is out of sync with
    # the queries/migrations. Runs only when .rs or migration files change.
    # `--check` verifies without rewriting; regenerate with `cargo sqlx prepare`.
    # Relies on the devenv Postgres + DATABASE_URL being available.
    sqlx-prepare = {
      enable = true;
      name = "sqlx prepare check";
      entry = "cargo sqlx prepare --check";
      files = "(\\.rs$|^migrations/.*\\.sql$)";
      pass_filenames = false;
    };
  };
}
