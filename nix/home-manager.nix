# Home-manager module: the router as a user service.
#
# This is the natural scope for a workstation — the credentials belong to one
# human, Claude Code runs as that human, and the port is loopback-only. The
# login flow, the TUI and the daemon then all share one state directory, so a
# `claude-router login` is immediately visible to the daemon.
#
# Use nix/module.nix instead when the router is a machine-level service
# (multi-user hosts, servers, credentials from systemd LoadCredential).
{ config, lib, pkgs, ... }:
let
  cfg = config.services.claude-router;
  settingsFormat = pkgs.formats.toml { };

  configFile = settingsFormat.generate "claude-router.toml" (
    {
      listen = "127.0.0.1:${toString cfg.port}";
      anthropic_upstream = cfg.anthropicUpstream;
      restrict_to_owner = cfg.restrictToOwner;
      providers = lib.mapAttrs (_: p:
        lib.optionalAttrs (p.preset != null) { preset = p.preset; }
        // lib.optionalAttrs (p.baseUrl != null) { base_url = p.baseUrl; }
        // lib.optionalAttrs (p.api != null) { api = p.api; }
        // lib.optionalAttrs (p.models != [ ]) {
          models = map (m: if builtins.isString m then m else { inherit (m) id name; }) p.models;
        }
        // lib.optionalAttrs (p.effort != null) { effort = p.effort; }
      ) cfg.providers;
    }
    // cfg.extraSettings
  );

  wrapper = pkgs.callPackage ./wrapper.nix {
    claude-code = cfg.claudeCodePackage;
    name = cfg.wrapperName;
    routerUrl = "http://127.0.0.1:${toString cfg.port}";
  };
in
{
  options.services.claude-router = {
    enable = lib.mkEnableOption "the Claude Code router as a user service";

    package = lib.mkOption {
      type = lib.types.package;
      description = "The claude-code-transparent-router package to run.";
    };

    claudeCodePackage = lib.mkOption {
      type = lib.types.package;
      default = pkgs.claude-code;
      defaultText = lib.literalExpression "pkgs.claude-code";
      example = lib.literalExpression "inputs.claude-code-nix.packages.\${system}.default";
      description = ''
        Claude Code package the wrapper launches. Defaults to the one in the
        nixpkgs this module is evaluated with, including any overlay already
        applied; set it to use a different source.
      '';
    };

    wrapperName = lib.mkOption {
      type = lib.types.str;
      default = "claude-routed";
      example = "claude";
      description = ''
        Command name the wrapper is installed as. Use "claude" to have the
        routed CLI be the one on PATH — then do not also install claude-code
        into this profile, or the two collide over bin/claude.
      '';
    };

    installWrapper = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = "Add the wrapper and the router CLI to home.packages.";
    };

    wrapperPackage = lib.mkOption {
      type = lib.types.package;
      readOnly = true;
      default = wrapper;
      defaultText = lib.literalExpression "<wrapper built from claudeCodePackage>";
      description = "The built wrapper, exposed for inspection or reuse.";
    };

    settingsFile = lib.mkOption {
      type = lib.types.path;
      readOnly = true;
      default = configFile;
      defaultText = lib.literalExpression "<generated claude-router.toml>";
      description = "The generated router config, exposed for inspection.";
    };

    port = lib.mkOption {
      type = lib.types.port;
      default = 8787;
      description = "Loopback port the daemon listens on.";
    };

    anthropicUpstream = lib.mkOption {
      type = lib.types.str;
      default = "https://api.anthropic.com";
      description = "Upstream that passthrough requests are forwarded to verbatim.";
    };

    restrictToOwner = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = ''
        Accept connections only from the user the daemon runs as. A loopback
        port is otherwise reachable by every local process, any of which
        could then spend this user's credentials.
      '';
    };

    extraSettings = lib.mkOption {
      type = settingsFormat.type;
      default = { };
      description = "Extra top-level keys merged into the generated config.";
    };

    providers = lib.mkOption {
      default = { };
      description = ''
        Second providers, keyed by name. Their models are offered to Claude
        Code as anthropic/<model-id> aliases; model IDs must be unique across
        providers. Credentials are never written here — set an API key in the
        router TUI, or run `claude-router login <name>` for OAuth providers.
      '';
      type = lib.types.attrsOf (lib.types.submodule {
        options = {
          preset = lib.mkOption {
            type = lib.types.nullOr lib.types.str;
            default = null;
            example = "deepseek";
            description = ''
              Named preset supplying this provider's defaults (endpoint, API
              dialect, models, effort mapping). Options set here override it.
            '';
          };

          baseUrl = lib.mkOption {
            type = lib.types.nullOr lib.types.str;
            default = null;
            description = "Provider API base URL. Required unless a preset supplies it.";
          };

          api = lib.mkOption {
            type = lib.types.nullOr (lib.types.enum [ "openai" "anthropic" "responses" ]);
            default = null;
            description = "API dialect the provider speaks.";
          };

          models = lib.mkOption {
            type = lib.types.listOf (
              lib.types.either lib.types.str (
                lib.types.submodule {
                  options = {
                    id = lib.mkOption {
                      type = lib.types.str;
                      description = "Upstream model ID sent to the provider.";
                    };
                    name = lib.mkOption {
                      type = lib.types.str;
                      description = "Display name shown in Claude Code's model switcher.";
                    };
                  };
                }
              )
            );
            default = [ ];
            description = "Upstream models to expose: bare IDs or { id, name }.";
          };

          effort = lib.mkOption {
            type = lib.types.nullOr settingsFormat.type;
            default = null;
            example = {
              field = "reasoning.effort";
              default = "high";
              map = { low = "low"; medium = "high"; high = "high"; };
            };
            description = ''
              How to translate Claude Code's reasoning effort
              (`output_config.effort`) for this provider. Null forwards it
              unchanged.
            '';
          };
        };
      });
    };
  };

  config = lib.mkIf cfg.enable {
    home.packages = lib.optionals cfg.installWrapper [ wrapper cfg.package ];

    # The daemon reads this path by default, so the TUI shows the same file.
    xdg.configFile."claude-router/config.toml".source = configFile;

    systemd.user.services.claude-router = {
      Unit = {
        Description = "Claude Code transparent router";
        After = [ "network.target" ];
        # The config is a store path, so a changed config is a changed unit
        # and home-manager restarts the daemon on switch.
        X-Config = "${configFile}";
      };
      Service = {
        ExecStart = "${lib.getExe cfg.package} --daemon --config ${configFile}";
        Restart = "on-failure";
        RestartSec = 2;
      };
      Install.WantedBy = [ "default.target" ];
    };
  };
}
