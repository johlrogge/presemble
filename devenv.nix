{ inputs, ... }:
let
  system = builtins.currentSystem;
  pkgs = inputs.nixpkgs.legacyPackages.${system};
in
{
  claude.code.enable = true;
  env.METADEV_PROJECT = "presemble";

  git-hooks.enable = true;
  git-hooks.hooks.polylith-clippy = {
    enable = true;
    name = "Clippy (polylith)";
    entry = "cargo polylith cargo --profile dev clippy -- -D warnings";
    files = "\\.(rs|toml)$";
    language = "system";
    pass_filenames = false;
  };

  packages = [
    pkgs.helix
    pkgs.neovim
    pkgs.rep
  ];

  claude.code.mcpServers.presemble = {
    type = "stdio";
    command = "cargo";
    args = [ "polylith" "cargo" "--profile" "dev" "run" "--bin" "presemble" "--" "mcp" "site/" ];
  };

  enterShell = ''
    if [ -z "''${CI:-}" ]; then
      cargo polylith cargo --profile live build -q --release --bin presemble 2>/dev/null \
        && export PATH="$PWD/target/release:$PATH" \
        && echo "presemble ready (target/debug/presemble)" \
        || echo "presemble not built — run: cargo polylith cargo build --bin presemble"
    fi
  '';
}
