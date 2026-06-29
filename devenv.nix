{
  pkgs,
  lib,
  config,
  ...
}:
let
  pgPort = 5433;
in
{

  env = {

    RADARR_URL = "https://radarr.hyper.logikdev.fr";
    RADARR_API_KEY = config.secretspec.secrets.RADARR_API_KEY;

    JELLYFIN_URL = "https://jellyfin.hyper.logikdev.fr";
    JELLYFIN_API_KEY = config.secretspec.secrets.JELLYFIN_API_KEY;

    PGPORT = lib.mkForce pgPort;

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
    pkgs.cargo-edit # `cargo set-version` (bumps Cargo.toml + Cargo.lock together)
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
    initialDatabases = [
      {
        name = "rankoder";
      }
    ];
  };

  processes.watch.exec = "cargo watch -x run";

  dotenv.enable = true;

  scripts = {
    "reset-db".exec = ''
      sqlx database drop
      #psql -h localhost -p ${toString pgPort} postgres \
      #  -c "DROP DATABASE IF EXISTS rankoder WITH (FORCE);"
      sqlx database create
      sqlx migrate run
    '';

    # Show the next version git-cliff would pick from the Conventional Commits
    # since the last tag, plus the changelog entry it would generate — without
    # touching any file. Run this before `release` to sanity-check.
    "release-preview".exec = ''
      set -euo pipefail
      next="$(git-cliff --bumped-version)"
      echo "next version: $next"
      echo
      git-cliff --unreleased --tag "$next"
    '';

    # One-shot release: bump Cargo.toml + Cargo.lock, regenerate CHANGELOG.md,
    # record a `chore(release): X.Y.Z` commit, create the signed v* tag, and
    # push main + the tag. Mirrors the manual flow in a single command.
    #
    #   release             # version inferred from Conventional Commits
    #   release 0.3.0       # explicit version override
    #   release --no-push   # do everything except publish (push manually later)
    "release".exec = ''
      set -euo pipefail

      push=1
      args=()
      for a in "$@"; do
        case "$a" in
          --no-push) push=0 ;;
          *) args+=("$a") ;;
        esac
      done

      # Start a fresh working copy for the release artifacts (bump + changelog)
      # *before* anything else. This is what keeps a described feature on @ from
      # being clobbered: without it, the later `jj describe -m "chore(release)"`
      # overwrote the feature's own message and merged it into the release
      # commit. As a bonus, the feature commit is now @- (in HEAD's ancestry), so
      # `git-cliff --bumped-version` sees it and infers feat→minor correctly.
      jj new

      if [ -n "''${args[0]:-}" ]; then
        ver="''${args[0]#v}"
      else
        ver="$(git-cliff --bumped-version)"
        ver="''${ver#v}"
      fi

      if git rev-parse -q --verify "refs/tags/v$ver" >/dev/null; then
        echo "release: tag v$ver already exists" >&2
        exit 1
      fi

      echo "Releasing rankoder $ver"

      # Bump the manifest + lockfile in lockstep.
      cargo set-version "$ver"

      # Regenerate the changelog with the new version as the release header.
      git-cliff --tag "v$ver" -o CHANGELOG.md

      # Record the release commit (the current jj working copy holds the bump +
      # changelog), advance main onto it, then leave an empty working copy.
      jj describe -m "chore(release): $ver"
      rel="$(jj log --no-graph -r @ -T 'commit_id')"
      jj bookmark set main -r @
      jj new
      jj git export # ensure the git ref exists before tagging

      # Signed annotated tag, matching the existing v* tags ("rankoder X.Y.Z").
      git tag -s -m "rankoder $ver" "v$ver" "$rel"

      if [ "$push" -eq 1 ]; then
        # jj pushes the bookmark; tags are git-only, push them separately.
        jj git push --bookmark main
        git push origin "v$ver"
        echo "Published rankoder $ver (main + v$ver)."
      else
        echo
        echo "Tagged v$ver (not pushed). To publish:"
        echo "  jj git push --bookmark main && git push origin v$ver"
      fi
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

  enterShell = ''
    export DATABASE_URL="postgresql://logikdev@localhost/rankoder?host=$PGHOST"
  '';
}
