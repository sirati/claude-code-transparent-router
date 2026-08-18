{ config, lib, pkgs, ... }:
let
  cfg = config.services.claude-router;
  settingsFormat = pkgs.formats.toml { };

  # The config file is generated from this module and never contains
  # credentials; those arrive via systemd LoadCredential only.
  configFile = settingsFormat.generate "claude-router.toml" {
    listen = "127.0.0.1:${toString cfg.port}";
    anthropic_upstream = cfg.anthropicUpstream;
    allowed_uids = cfg.allowedUids;
    per_user_credentials = cfg.perUserCredentials;
    providers = lib.mapAttrs (_: p:
      lib.optionalAttrs (p.preset != null) { preset = p.preset; }
      // lib.optionalAttrs (p.baseUrl != null) { base_url = p.baseUrl; }
      // lib.optionalAttrs (p.api != null) { api = p.api; }
      // lib.optionalAttrs (p.models != [ ]) {
        models = map (m: if builtins.isString m then m else { inherit (m) id name; }) p.models;
      }
      // lib.optionalAttrs (p.effort != null) { effort = p.effort; }
    ) cfg.providers;
  } // lib.optionalAttrs (cfg.pickerModel != null) { picker_model = cfg.pickerModel; };

  providersWithKey = lib.filterAttrs (_: p: p.apiKeyFile != null) cfg.providers;

  wrapper = pkgs.callPackage ./wrapper.nix {
    claude-code = cfg.claudeCodePackage;
    name = cfg.wrapperName;
    routerUrl = "http://127.0.0.1:${toString cfg.port}";
  };
in
{
  options.services.claude-router = {
    enable = lib.mkEnableOption "transparent loopback router for Claude Code";

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
        nixpkgs this module is evaluated with (including any overlay you
        already apply); set it to use a different source.
      '';
    };

    wrapperName = lib.mkOption {
      type = lib.types.str;
      default = "claude-routed";
      example = "claude";
      description = ''
        Command name the wrapper is installed as. Use "claude" to have the
        routed CLI be the one on PATH — in that case do not also install
        claude-code itself system-wide, or the two collide over bin/claude.
      '';
    };

    installWrapper = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = "Install the Claude Code wrapper system-wide.";
    };

    settingsFile = lib.mkOption {
      type = lib.types.path;
      readOnly = true;
      default = configFile;
      defaultText = lib.literalExpression "<generated claude-router.toml>";
      description = "The generated router config, exposed for inspection.";
    };

    wrapperPackage = lib.mkOption {
      type = lib.types.package;
      readOnly = true;
      default = wrapper;
      defaultText = lib.literalExpression "<wrapper built from claudeCodePackage>";
      description = ''
        The built wrapper, exposed so it can be added to a user profile or
        home-manager configuration instead of (or besides) the system one.
      '';
    };

    port = lib.mkOption {
      type = lib.types.port;
      default = 8787;
      description = "Loopback port the router listens on.";
    };

    perUserCredentials = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = ''
        Keep each connecting user's credentials and logins in their own
        directory under the service's state directory, keyed by the uid the
        kernel reports for the connection. This is what lets one machine-wide
        daemon serve several people without them sharing keys.

        Machine-level keys from `apiKeyFile` still apply to everyone: a
        systemd credential outranks a user's own stored key for that provider.
        Set this to false to have every user share one credential store.
      '';
    };

    allowedUids = lib.mkOption {
      type = lib.types.listOf lib.types.int;
      default = [ ];
      example = [ 1000 ];
      description = ''
        Uids allowed to connect. Empty means any local user may, as with any
        loopback port. The daemon runs as a DynamicUser here, so its own uid
        is not a useful default — list the humans who should reach it.
      '';
    };

    pickerModel = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = null;
      example = "anthropic/deepseek-v4-pro";
      description = ''
        Which routed model fills Claude Code's `/model` picker. Claude Code
        supports exactly one custom entry, so this picks it; the others stay
        reachable through `--model`, `/model <id>`, and agents. Accepts the
        alias or the bare provider model ID. Defaults to the first configured
        model.
      '';
    };

    anthropicUpstream = lib.mkOption {
      type = lib.types.str;
      default = "https://api.anthropic.com";
      description = "Upstream requests are forwarded to verbatim.";
    };

    providers = lib.mkOption {
      default = { };
      description = ''
        Second providers, keyed by name. Their models are served to Claude
        Code as anthropic/<model-id> aliases; model IDs must therefore be
        unique across providers.
      '';
      type = lib.types.attrsOf (lib.types.submodule {
        options = {
          preset = lib.mkOption {
            type = lib.types.nullOr lib.types.str;
            default = null;
            example = "deepseek";
            description = ''
              Named preset supplying this provider's defaults (endpoint, API
              dialect, models, effort mapping). Any option set here overrides
              the preset.
            '';
          };

          baseUrl = lib.mkOption {
            type = lib.types.nullOr lib.types.str;
            default = null;
            example = "https://api.deepseek.com/anthropic";
            description = "Provider API base URL. Required unless a preset supplies it.";
          };

          api = lib.mkOption {
            type = lib.types.nullOr (lib.types.enum [ "openai" "anthropic" "responses" ]);
            default = null;
            description = ''
              API dialect the provider speaks. "anthropic" endpoints get
              near-passthrough (model rewrite only); "openai" endpoints go
              through the Messages <-> chat-completions translator.
            '';
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
            example = [ { id = "some-model-id"; name = "Some Model Pro"; } ];
            description = "Upstream models to expose: bare IDs or { id, name }.";
          };

          effort = lib.mkOption {
            default = null;
            example = {
              field = "reasoning.effort";
              default = "high";
              remove = [ "output_config" ];
              map = { low = "low"; medium = "high"; high = "high"; xhigh = "high"; max = "max"; };
            };
            description = ''
              How to translate Claude Code's reasoning effort
              (`output_config.effort`) for this provider. Null forwards the
              request unchanged.
            '';
            type = lib.types.nullOr (lib.types.submodule {
              options = {
                field = lib.mkOption {
                  type = lib.types.str;
                  example = "reasoning.effort";
                  description = "Dotted JSON path the level is written to.";
                };
                map = lib.mkOption {
                  type = lib.types.attrsOf lib.types.str;
                  default = { };
                  description = "Claude Code level -> provider level.";
                };
                default = lib.mkOption {
                  type = lib.types.nullOr lib.types.str;
                  default = null;
                  description = "Level used when the request has none, or an unmapped one.";
                };
                remove = lib.mkOption {
                  type = lib.types.listOf lib.types.str;
                  default = [ ];
                  description = "Top-level request keys to drop before sending.";
                };
              };
            });
          };

          apiKeyFile = lib.mkOption {
            type = lib.types.nullOr lib.types.path;
            default = null;
            example = "/run/secrets/glm-api-key";
            description = ''
              File containing the provider's API key, passed to the service
              via systemd LoadCredential (never the Nix store). Null leaves
              the provider configured but credential-less: requests to it
              return an error saying so.
            '';
          };
        };
      });
    };
  };

  config = lib.mkIf cfg.enable {
    environment.systemPackages =
      lib.optional cfg.installWrapper wrapper
      # The router binary is also the login/TUI client, so it belongs on PATH
      # wherever the wrapper does.
      ++ lib.optional cfg.installWrapper cfg.package;

    systemd.sockets.claude-router = {
      wantedBy = [ "sockets.target" ];
      socketConfig.ListenStream = "127.0.0.1:${toString cfg.port}";
    };

    systemd.services.claude-router = {
      requires = [ "claude-router.socket" ];
      after = [ "network.target" ];
      serviceConfig = {
        ExecStart = "${lib.getExe cfg.package} --daemon --config ${configFile}";
        DynamicUser = true;
        # Writable home for credentials set at runtime through the admin API.
        StateDirectory = "claude-router";
        LoadCredential =
          lib.mapAttrsToList (name: p: "${name}:${p.apiKeyFile}") providersWithKey;
        NoNewPrivileges = true;
        ProtectSystem = "strict";
        ProtectHome = true;
        PrivateTmp = true;
        # AF_UNIX is required for NSS/nscd name resolution on NixOS.
        RestrictAddressFamilies = [ "AF_INET" "AF_INET6" "AF_UNIX" ];
        SystemCallFilter = [ "@system-service" ];
        MemoryDenyWriteExecute = true;
        LockPersonality = true;
        RestrictNamespaces = true;
        ProtectKernelTunables = true;
        ProtectKernelModules = true;
        ProtectControlGroups = true;
        CapabilityBoundingSet = "";
      };
    };
  };
}
