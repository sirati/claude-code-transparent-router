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
          # Defaults to this nixpkgs' claude-code; no third-party flake is
          # pulled in. Override it per nix/wrapper.nix to use another source.
          claude-routed = pkgs.callPackage ./nix/wrapper.nix { };
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

      # The wrapper as a plain package function, for callers who want it
      # outside the NixOS module: pkgs.callPackage flake.lib.wrapper { … }.
      lib.wrapper = ./nix/wrapper.nix;

      nixosModules.default =
        { pkgs, lib, ... }:
        {
          imports = [ ./nix/module.nix ];
          services.claude-router.package =
            lib.mkDefault self.packages.${pkgs.stdenv.hostPlatform.system}.claude-code-transparent-router;
        };
    };
}
