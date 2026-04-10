{
  description = "Presemble — semantic site publisher";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    cargo-polylith-src = {
      url = "github:johlrogge/cargo-polylith";
      flake = false;
    };
  };

  outputs = { self, nixpkgs, rust-overlay, cargo-polylith-src }:
    let
      supportedSystems = [ "x86_64-linux" "aarch64-linux" "aarch64-darwin" ];
      forAllSystems = nixpkgs.lib.genAttrs supportedSystems;

      pkgsFor = system: import nixpkgs {
        inherit system;
        overlays = [ rust-overlay.overlays.default ];
      };
    in
    {
      packages = forAllSystems (system:
        let
          pkgs = pkgsFor system;
          rustToolchain = pkgs.rust-bin.stable.latest.default;

          cargo-polylith = pkgs.rustPlatform.buildRustPackage {
            pname = "cargo-polylith";
            version = "0.10.1";
            src = cargo-polylith-src;
            cargoLock.lockFile = "${cargo-polylith-src}/Cargo.lock";
            cargoBuildFlags = [ "-p" "cargo-polylith" ];
          };

          presemble = pkgs.stdenv.mkDerivation {
            pname = "presemble";
            version = self.shortRev or self.dirtyShortRev or "dev";
            src = self;

            nativeBuildInputs = [ rustToolchain cargo-polylith pkgs.pkg-config ];
            buildInputs = pkgs.lib.optionals pkgs.stdenv.isDarwin [
              pkgs.darwin.apple_sdk.frameworks.Security
              pkgs.darwin.apple_sdk.frameworks.SystemConfiguration
            ];

            buildPhase = ''
              cargo polylith cargo --profile live build --release -p publisher
            '';

            installPhase = ''
              mkdir -p $out/bin
              cp target/release/presemble $out/bin/
            '';
          };
        in
        {
          inherit presemble;
          default = presemble;
        }
      );

      overlays.default = final: prev: {
        presemble = self.packages.${final.system}.presemble;
      };
    };
}
