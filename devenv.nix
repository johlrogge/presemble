{ inputs, ... }:
let
  system = builtins.currentSystem;
  pkgs = inputs.nixpkgs.legacyPackages.${system};
in
{
  claude.code.enable = true;

  git-hooks.enable = true;
  git-hooks.hooks.clippy = {
    enable = true;
    name = "clippy";
    entry = "cargo polylith cargo --profile dev clippy -- -D warnings";
    pass_filenames = false;
    types = ["rust"];
  };

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
