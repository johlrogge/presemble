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
      cargo polylith cargo --profile dev build -q --bin presemble 2>/dev/null \
        && export PATH="$PWD/target/debug:$PATH" \
        && echo "presemble ready (target/debug/presemble)" \
        || echo "presemble not built — run: cargo polylith cargo build --bin presemble"
    fi
  '';
}
