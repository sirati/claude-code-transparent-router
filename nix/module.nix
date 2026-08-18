{ config, lib, pkgs, ... }:
let
  cfg = config.services.claude-router;
  settingsFormat = pkgs.formats.toml { };

  # The config file is generated from this module and never contains
  # credentials; those arrive via systemd LoadCredential only.
  configFile = settingsFormat.generate "claude-router.toml" {
    listen = "127.0.0.1:${toString cfg.port}";
    anthropic_upstream = cfg.anthropicUpstream;
    providers = lib.mapAttrs (_: p: {
      base_url = p.baseUrl;
      api = p.api;
      models = map (m: if builtins.isString m then m else { inherit (m) id name; }) p.models;
    } // lib.optionalAttrs (p.effort != null) { effort = p.effort; }) cfg.providers;
  };

  providersWithKey = lib.filterAttrs (_: p: p.apiKeyFile != null) cfg.providers;
in
{
  options.services.claude-router = {
    enable = lib.mkEnableOption "transparent loopback router for Claude Code";

    package = lib.mkOption {
      type = lib.types.package;
      description = "The claude-code-transparent-router package to run.";
    };

    claudeRoutedPackage = lib.mkOption {
      type = lib.types.package;
      description = "Claude Code wrapper (claude-routed) pointed at the router.";
    };

    installClaudeRouted = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = "Install the claude-routed wrapper system-wide.";
    };

    port = lib.mkOption {
      type = lib.types.port;
      default = 8787;
      description = "Loopback port the router listens on.";
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
          baseUrl = lib.mkOption {
            type = lib.types.str;
            example = "https://api.deepseek.com/anthropic";
            description = "Provider API base URL.";
          };

          api = lib.mkOption {
            type = lib.types.enum [ "openai" "anthropic" ];
            default = "openai";
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
    environment.systemPackages = lib.mkIf cfg.installClaudeRouted [ cfg.claudeRoutedPackage ];

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
