{ config, lib, pkgs, ... }:
let
  cfg = config.services.claude-router;
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

    glm = {
      apiKeyFile = lib.mkOption {
        type = lib.types.nullOr lib.types.path;
        default = null;
        example = "/run/secrets/glm-api-key";
        description = ''
          File containing the second provider's API key, passed to the service
          via systemd LoadCredential (never the Nix store). Null disables the
          second-provider path entirely; the router is then pure passthrough.
        '';
      };

      baseUrl = lib.mkOption {
        type = lib.types.str;
        default = "https://api.z.ai/api/paas/v4";
        description = "OpenAI-compatible base URL of the second provider.";
      };

      models = lib.mkOption {
        type = lib.types.listOf lib.types.str;
        default = [ "glm-4.7" ];
        description = "Upstream model IDs served as anthropic/<id> aliases.";
      };
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
      environment = {
        GLM_BASE_URL = cfg.glm.baseUrl;
        GLM_MODELS = lib.concatStringsSep "," cfg.glm.models;
      };
      serviceConfig = {
        ExecStart = lib.getExe cfg.package;
        DynamicUser = true;
        LoadCredential =
          lib.optional (cfg.glm.apiKeyFile != null) "glm:${cfg.glm.apiKeyFile}";
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
