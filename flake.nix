{
  description = "Rank and encode";
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };
  outputs =
    {
      self,
      nixpkgs,
      flake-utils,
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = import nixpkgs { inherit system; };
        dbName = "rankoder";
        pgData = "./pgdata";
        pgPort = "5433";
      in
      {
        packages.default = pkgs.rustPlatform.buildRustPackage {
          pname = "rankoder";
          version = "0.0.1";
          src = ./.;
        };

        devShells.default = pkgs.mkShell {
          buildInputs = [
            pkgs.cargo
            pkgs.rustc
            pkgs.clippy
            pkgs.ffmpeg
            pkgs.postgresql_16
            pkgs.rustfmt
            pkgs.rust-analyzer
            pkgs.tree-sitter
            pkgs.pkg-config
            pkgs.openssl
            pkgs.sqlx-cli
          ];

          shellHook = ''
            export PGDATA="${pgData}"
            export PGPORT="${pgPort}"
            export PGHOST="/tmp"
            export AUTO_MIGRATE=1
            export DB_SOCKET_DIR="/tmp"
            export DB_PORT="${pgPort}"
            export DB_NAME="${dbName}"

            # Initialise le cluster si nécessaire
            if [ ! -d "$PGDATA" ]; then
              echo "Initialisation du cluster PostgreSQL..."
              initdb --auth=trust --no-locale --encoding=UTF8
            fi

            # Démarre PostgreSQL en tâche de fond si pas déjà en cours
            if ! pg_isready -q 2>/dev/null; then
              echo "Démarrage de PostgreSQL..."
              pg_ctl start -l "$PGDATA/postgresql.log" -o "-k /tmp" --no-wait
              # Attend que postgres soit prêt
              until pg_isready -q 2>/dev/null; do sleep 0.2; done
              createdb ${dbName} 2>/dev/null || true
              echo "PostgreSQL prêt sur le port ${pgPort}"
            fi
          '';
        };
      }
    );
}
