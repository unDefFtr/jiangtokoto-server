{
  description = "Jiangtokoto Server — High-performance Axum web server";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    crane.url = "github:ipetkov/crane";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, crane, flake-utils, ... }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = nixpkgs.legacyPackages.${system};

        craneLib = crane.mkLib pkgs;

        # System libraries needed by the `image` crate (JPEG decoding)
        nativeBuildInputs = with pkgs; [
          pkg-config
        ];

        buildInputs = with pkgs; [
          libjpeg
        ];

        # Filter source to include only build-relevant files
        src = pkgs.lib.cleanSourceWith {
          src = ./.;
          filter = path: type:
            (pkgs.lib.hasSuffix ".rs" path)
            || (pkgs.lib.hasSuffix ".toml" path)
            || (pkgs.lib.hasSuffix ".lock" path)
            || (pkgs.lib.hasSuffix ".yml" path)
            || (pkgs.lib.hasSuffix ".yaml" path)
            || (pkgs.lib.hasInfix "/assets/" path)
            || (type == "directory");
        };

        # Build dependencies separately for caching
        cargoArtifacts = craneLib.buildDepsOnly {
          inherit src nativeBuildInputs buildInputs;
        };

        jiangtokoto-server = craneLib.buildPackage {
          inherit src cargoArtifacts nativeBuildInputs buildInputs;

          # Bundle assets and example config with the binary
          postInstall = ''
            mkdir -p $out/share/jiangtokoto-server/assets
            if [ -d assets ]; then
              cp -r assets/. $out/share/jiangtokoto-server/assets/
            fi
            if [ -f config.yml.example ]; then
              cp config.yml.example $out/share/jiangtokoto-server/
            fi
          '';

          meta = with pkgs.lib; {
            description = "Jiangtokoto high-performance web server";
            license = licenses.mit;
            mainProgram = "jiangtokoto-server";
            platforms = platforms.linux ++ platforms.darwin;
          };
        };
      in
      {
        packages = {
          default = jiangtokoto-server;
          inherit jiangtokoto-server;
        };

        apps.default = flake-utils.lib.mkApp {
          drv = jiangtokoto-server;
        };

        devShells.default = craneLib.devShell {
          inputsFrom = [ jiangtokoto-server ];
          packages = with pkgs; [
            cargo
            rustc
            rustfmt
            clippy
            cargo-watch
          ];
        };
      });
}
