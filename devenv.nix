{ inputs, ... }:
let
  system = builtins.currentSystem;
  pkgs = inputs.nixpkgs.legacyPackages.${system};
in
{
  claude.code.enable = true;
  env.METADEV_PROJECT = "presemble";

  # The one source of the Rust toolchain: pinned, so local clippy/rustc match
  # CI (which runs `devenv shell`). Bump it deliberately; the rust-overlay input
  # in devenv.yaml must be new enough to know the version (`devenv update
  # rust-overlay`).
  languages.rust = {
    enable = true;
    channel = "stable";
    version = "1.99.0";
    components = [ "rustc" "cargo" "clippy" "rustfmt" "rust-analyzer" ];
  };

  git-hooks.enable = true;
  git-hooks.hooks.polylith-clippy = {
    enable = true;
    name = "Clippy (polylith)";
    entry = "cargo polylith cargo --profile dev clippy -- -D warnings";
    files = "\\.(rs|toml)$";
    language = "system";
    pass_filenames = false;
  };
  git-hooks.hooks.rustfmt-check = {
    enable = true;
    name = "rustfmt (check)";
    entry = "just fmt-check";
    files = "\\.rs$";
    language = "system";
    pass_filenames = false;
  };

  packages = [
    pkgs.helix
    pkgs.neovim
    pkgs.rep
    pkgs.jet
    pkgs.deno
    pkgs.just
    pkgs.openstackclient
    pkgs.opentofu
    pkgs.tofu-ls
    pkgs.dnsutils
    # Conductor cross-compile to the Bahnhof box (Debian 13, glibc 2.41).
    # cargo-zigbuild pins the glibc version with a standard interpreter; zig cc
    # cross-compiles nng-sys's C; cmake drives the bundled libnng build.
    pkgs.zig
    pkgs.cargo-zigbuild
    pkgs.cmake
  ];

  claude.code.mcpServers.presemble = {
    type = "stdio";
    command = "./target/release/presemble";
    args = [ "mcp" ];
  };

  enterShell = ''
    if [ -z "''${CI:-}" ]; then
      cargo polylith cargo --profile live build -q --release --bin presemble 2>/dev/null \
        && export PATH="$PWD/target/release:$PATH" \
        && echo "presemble ready (target/release/presemble)" \
        || echo "presemble not built — run: cargo polylith cargo build --bin presemble"
    fi
  '';
}
