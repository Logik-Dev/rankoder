{
  description = "rankoder — re-encode video to HEVC with rating-aware, human-approved decisions";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    crane.url = "github:ipetkov/crane";
  };

  outputs =
    {
      self,
      nixpkgs,
      flake-utils,
      rust-overlay,
      crane,
    }:
    let
      perSystem = flake-utils.lib.eachDefaultSystem (
        system:
        let
          pkgs = import nixpkgs {
            inherit system;
            overlays = [ (import rust-overlay) ];
          };

          # Pin a stable toolchain (edition 2024 needs rustc >= 1.85).
          rustToolchain = pkgs.rust-bin.stable.latest.default;
          craneLib = (crane.mkLib pkgs).overrideToolchain rustToolchain;

          # crane's default source filter keeps only Cargo/Rust files; we also
          # need the migrations (embedded by sqlx::migrate!) and the committed
          # .sqlx/ offline query data (so the build needs no live database).
          src = pkgs.lib.cleanSourceWith {
            src = ./.;
            name = "rankoder-source";
            filter =
              path: type:
              (craneLib.filterCargoSources path type)
              || (builtins.match ".*/migrations/.*\\.sql$" path != null)
              || (builtins.match ".*/\\.sqlx/.*\\.json$" path != null);
          };

          commonArgs = {
            inherit src;
            strictDeps = true;

            # Build queries against the committed .sqlx/ data, never a DB.
            SQLX_OFFLINE = "true";

            nativeBuildInputs = [ pkgs.pkg-config ];
            # reqwest's default TLS backend links against OpenSSL.
            buildInputs = [ pkgs.openssl ];
          };

          # Build all dependencies once and cache them separately so changing
          # only the crate's own sources triggers a fast incremental rebuild.
          cargoArtifacts = craneLib.buildDepsOnly commonArgs;

          rankoder = craneLib.buildPackage (
            commonArgs
            // {
              inherit cargoArtifacts;
              # The integration tests need a live PostgreSQL + ffmpeg, neither of
              # which exists in the hermetic build sandbox (they skip at runtime
              # but still have to compile, which would require offline data for
              # every test-only query). Tests are a dev-time concern: run them
              # with `cargo test` against the devenv database, not in the build.
              doCheck = false;
              nativeBuildInputs = commonArgs.nativeBuildInputs ++ [ pkgs.makeWrapper ];
              # ffprobe/ffmpeg are runtime dependencies shelled out to at run time.
              postInstall = ''
                wrapProgram $out/bin/rankoder \
                  --prefix PATH : ${pkgs.lib.makeBinPath [ pkgs.ffmpeg ]}
              '';
              meta = {
                description = "Rating-aware, human-approved HEVC re-encoder";
                mainProgram = "rankoder";
              };
            }
          );
        in
        {
          packages = {
            default = rankoder;
            rankoder = rankoder;
          };

          # `nix flake check` builds the crate.
          checks.rankoder = rankoder;
        }
      );
    in
    perSystem
    // {
      nixosModules.default = import ./nix/nixos-module.nix self;

      overlays.default = final: _prev: {
        rankoder = self.packages.${final.system}.default;
      };
    };
}
