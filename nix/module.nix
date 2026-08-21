{ config, lib, pkgs, ... }:
let
  cfg = config.services.claude-router;
  settingsFormat = pkgs.formats.toml { };

  # The stable supervisor owns the socket-activated listener and forks workers.
  # A Nix switch calls --reload: unchanged binaries reload config in place, while
  # changed binaries receive a duplicated listener before the old worker drains.
  supervisorStart = lib.concatStringsSep " " (
    [
      (lib.getExe cfg.package)
      "--supervisor"
      "--listen"
      "127.0.0.1:${toString cfg.port}"
      "--idle-timeout"
      (toString cfg.idleTimeout)
    ]
    ++ lib.optional cfg.perUserConfig "--user-config"
  );

  reloadStart = lib.concatStringsSep " " (
    [
      (lib.getExe cfg.package)
      "--reload"
      "--target"
      (lib.getExe cfg.package)
      "--listen"
      "127.0.0.1:${toString cfg.port}"
      "--idle-timeout"
      (toString cfg.idleTimeout)
    ]
    ++ lib.optional cfg.perUserConfig "--user-config"
  );

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
        Claude Code package the wrapper launches. The default follows any
        overlay you already apply.
      '';
    };

    wrapperName = lib.mkOption {
      type = lib.types.str;
      default = "claude-routed";
      example = "claude";
      description = ''
        Command the wrapper is installed as. With "claude", drop claude-code
        from environment.systemPackages: two `claude` binaries would compete
        on PATH.
      '';
    };

    installWrapper = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = "Install the Claude Code wrapper system-wide.";
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

    idleTimeout = lib.mkOption {
      type = lib.types.int;
      default = 300;
      description = ''
        Seconds without a request before the daemon exits; the socket starts
        it again. A streaming turn counts as activity throughout. 0 keeps it
        resident.
      '';
    };

    perUserConfig = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = ''
        Serve each connecting user from their own config and credentials in
        their home, resolved from the uid the kernel reports. Nothing personal
        is then configured machine-wide.

        A provider's `apiKeyFile` still applies to everyone: systemd
        credentials outrank a user's own key.
      '';
    };

    allowedUids = lib.mkOption {
      type = lib.types.listOf lib.types.int;
      default = [ ];
      example = [ 1000 ];
      description = ''
        Uids allowed to connect; empty means any local user may. The daemon
        runs as a DynamicUser, so list the people who should reach it.
      '';
    };

    pickerModel = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = null;
      example = "deepseek/pro";
      description = ''
        Which routed model fills Claude Code's single custom `/model` entry;
        the rest stay reachable by name and through agents. Takes an ID or
        shorthand, optionally provider-qualified. Defaults to the first
        configured model.
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
        Providers to route to, keyed by name. Model IDs and shorthands must
        be unique across them.
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

    # Socket activation: systemd holds the port, the daemon starts on the
    # first connection and exits again once idle.
    systemd.sockets.claude-router = {
      wantedBy = [ "sockets.target" ];
      socketConfig.ListenStream = "127.0.0.1:${toString cfg.port}";
    };

    systemd.services.claude-router = {
      requires = [ "claude-router.socket" ];
      after = [ "network.target" "claude-router.socket" ];
      # Started by the socket, not at boot.
      wantedBy = [ ];
      serviceConfig = {
        ExecStart = supervisorStart;
        ExecReload = reloadStart;
        Environment = "CLAUDE_ROUTER_CONTROL_SOCKET=%t/claude-router/control.sock";
        RuntimeDirectory = "claude-router";
        RuntimeDirectoryMode = "0700";
        DynamicUser = true;
        # Users' own credentials live in their homes and are written by the
        # CLI running as them; the daemon only reads. The state directory is
        # just for a daemon with no per-user resolution.
        StateDirectory = "claude-router";
        LoadCredential =
          lib.mapAttrsToList (name: p: "${name}:${p.apiKeyFile}") providersWithKey;
        NoNewPrivileges = true;
        ProtectSystem = "strict";
        # Read-only rather than off: the daemon must read each user's config
        # and credentials, and must never write into their home.
        ProtectHome = if cfg.perUserConfig then "read-only" else true;
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
        ExitType = "cgroup";
      };
      reloadIfChanged = true;
      restartIfChanged = false;
    };
  };
}
