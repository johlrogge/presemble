{ inputs, ... }:
let
  system = builtins.currentSystem;
  pkgs = inputs.nixpkgs.legacyPackages.${system};
in
{
  claude.code.enable = true;

  packages = [
    pkgs.helix
  ];

  enterShell = ''
    if [ -z "''${CI:-}" ]; then
      cargo polylith cargo --profile dev build -q --bin publisher 2>/dev/null \
        && export PATH="$PWD/target/debug:$PATH" \
        && echo "presemble publisher ready (target/debug/presemble)" \
        || echo "presemble publisher not built — run: cargo build -p publisher"
    fi
  '';
}
