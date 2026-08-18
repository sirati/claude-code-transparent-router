{
  description = "Transparent loopback router for Claude Code: verbatim Anthropic passthrough plus second-provider translation";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, nixpkgs, rust-overlay, ... }:
    let
      systems = [
        "aarch64-linux"
        "x86_64-linux"
      ];
      forAllSystems = nixpkgs.lib.genAttrs systems;
      dev = import ./devel.nix { inherit nixpkgs rust-overlay; };
    in
    dev // {
      packages = forAllSystems (
        system:
        let
          pkgs = import nixpkgs {
            inherit system;
            overlays = [ rust-overlay.overlays.default ];
            # claude-code is unfree; nothing else needs the exception.
            config.allowUnfreePredicate =
              pkg: nixpkgs.lib.getName pkg == "claude-code";
          };
          rustPlatform = pkgs.makeRustPlatform {
            cargo = pkgs.rust-bin.stable.latest.minimal;
            rustc = pkgs.rust-bin.stable.latest.minimal;
          };
          claude-code-transparent-router = rustPlatform.buildRustPackage {
            pname = "claude-code-transparent-router";
            version = "0.1.0";
            src = ./.;
            cargoLock.lockFile = ./Cargo.lock;
            meta.mainProgram = "claude-router";
          };
          # Claude Code pointed at the router; the credential flow stays
          # Claude Code's own. Gateway model discovery makes it fetch
          # GET /v1/models from the router at startup, so the providers'
          # anthropic/<id> aliases appear in the /model picker.
          claude-routed = pkgs.writeShellScriptBin "claude-routed" ''
            export ANTHROPIC_BASE_URL="''${CLAUDE_ROUTER_URL:-http://127.0.0.1:8787}"
            export CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY=1
            exec ${pkgs.claude-code}/bin/claude "$@"
          '';
        in
        {
          inherit claude-code-transparent-router claude-routed;
          default = claude-code-transparent-router;
        }
      );

      apps = forAllSystems (system: {
        default = {
          type = "app";
          program = "${self.packages.${system}.claude-code-transparent-router}/bin/claude-router";
        };
        claude-routed = {
          type = "app";
          program = "${self.packages.${system}.claude-routed}/bin/claude-routed";
        };
      });

      nixosModules.default =
        { pkgs, lib, ... }:
        {
          imports = [ ./nix/module.nix ];
          services.claude-router.package =
            lib.mkDefault self.packages.${pkgs.stdenv.hostPlatform.system}.claude-code-transparent-router;
          services.claude-router.claudeRoutedPackage =
            lib.mkDefault self.packages.${pkgs.stdenv.hostPlatform.system}.claude-routed;
        };
    };
}
