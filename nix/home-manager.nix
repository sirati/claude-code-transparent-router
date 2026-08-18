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
    # TOML has no null, so an unset choice is an absent key.
    // lib.optionalAttrs (cfg.pickerModel != null) { picker_model = cfg.pickerModel; }
    // cfg.extraSettings
  );

  wrapper = pkgs.callPackage ./wrapper.nix {
    claude-code = cfg.claudeCodePackage;
    name = cfg.wrapperName;
    routerUrl = "http://127.0.0.1:${toString cfg.port}";
  };

  # Agents name a provider and a model; the router accepts that spelling
  # directly, so no `anthropic/` prefix is involved.
  agentModel = agent:
    if agent.provider == null then agent.model else "${agent.provider}/${agent.model}";

  # A preset's model list lives in its TOML, so read it back to validate
  # agents against providers that were configured with nothing but a preset.
  presetModels =
    preset:
    let
      file = ../presets + "/${preset}.toml";
      parsed = builtins.fromTOML (builtins.readFile file);
    in
    if builtins.pathExists file then modelNames (parsed.models or [ ]) else [ ];

  # Every name a model answers to: its ID and its shorthands.
  modelNames = lib.concatMap (
    m: if builtins.isString m then [ m ] else [ m.id ] ++ (m.aliases or [ ])
  );

  providerModels =
    p: modelNames p.models ++ lib.optionals (p.preset != null) (presetModels p.preset);

  # Claude Code's picker holds exactly one custom entry, but a subagent's
  # frontmatter can name any model — so agents are how the other routed
  # models stay reachable.
  agentFile =
    name: agent:
    let
      frontmatter = [
        "name: ${name}"
        "description: ${agent.description}"
        "model: ${agentModel agent}"
      ]
      ++ lib.optional (agent.effort != null) "effort: ${agent.effort}"
      ++ lib.optional (agent.tools != null) "tools: ${lib.concatStringsSep ", " agent.tools}";
    in
    ''
      ---
      ${lib.concatStringsSep "\n" frontmatter}
      ---

      ${agent.prompt}
    '';

  # One file per agent per configured Claude Code directory, since a machine
  # may carry several (.claude, .claudeB, ...).
  agentFiles = lib.listToAttrs (
    lib.concatMap (
      dir:
      lib.mapAttrsToList (name: agent: {
        name = "${dir}/agents/${name}.md";
        value = { text = agentFile name agent; };
      }) cfg.agents
    ) cfg.agentDirs
  );
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

    idleTimeout = lib.mkOption {
      type = lib.types.int;
      default = 300;
      description = ''
        Seconds without a request before the daemon exits; the socket starts
        it again on the next connection. A streaming turn counts as activity
        for its whole duration. 0 keeps it resident.
      '';
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

    pickerModel = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = null;
      example = "deepseek/pro";
      description = ''
        Which routed model fills Claude Code's `/model` picker. Claude Code
        supports one custom entry, so this picks it; the rest stay reachable
        through `--model`, `/model <name>`, and agents. Accepts an ID, a
        shorthand, or either qualified by provider. Defaults to the first
        configured model.
      '';
    };

    agentDirs = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = [ ".claude" ];
      example = [ ".claude" ".claudeB" ];
      description = ''
        Claude Code configuration directories, relative to $HOME, that
        `agents` are written into. Several entries suit a machine carrying
        more than one Claude Code setup.
      '';
    };

    agents = lib.mkOption {
      default = { };
      example = lib.literalExpression ''
        {
          flash = {
            provider = "deepseek";
            model = "deepseek-v4-flash";
            description = "Cheap, fast helper for mechanical edits.";
            effort = "low";
            prompt = "You make small mechanical changes exactly as asked.";
          };
        }
      '';
      description = ''
        Subagents written to each of `agentDirs`. An agent's frontmatter can
        name any model, so this is how routed models that did not win the
        single `/model` slot are still selectable.
      '';
      type = lib.types.attrsOf (lib.types.submodule {
        options = {
          provider = lib.mkOption {
            type = lib.types.nullOr lib.types.str;
            example = "deepseek";
            description = ''
              Configured provider serving this agent's model. The routing
              alias is assembled from it, so `model` stays a plain upstream
              ID. Null means an Anthropic model that passes straight through,
              and `model` is then used verbatim.
            '';
          };

          model = lib.mkOption {
            type = lib.types.str;
            example = "flash";
            description = ''
              The provider's model: its upstream ID, or a shorthand the
              provider defines (`flash` for `deepseek-v4-flash`).
            '';
          };

          description = lib.mkOption {
            type = lib.types.str;
            description = "When to use this agent; Claude Code reads it to decide.";
          };

          prompt = lib.mkOption {
            type = lib.types.lines;
            default = "";
            description = "The agent's system prompt, i.e. the body of its file.";
          };

          effort = lib.mkOption {
            type = lib.types.nullOr (lib.types.enum [ "low" "medium" "high" "xhigh" "max" ]);
            default = null;
            description = "Reasoning effort while this agent is active; null inherits the session.";
          };

          tools = lib.mkOption {
            type = lib.types.nullOr (lib.types.listOf lib.types.str);
            default = null;
            example = [ "Read" "Grep" "Glob" ];
            description = "Tools the agent may use; null inherits all of them.";
          };
        };
      });
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
    # A mistyped provider or model would otherwise surface as a failed turn
    # in the middle of a conversation.
    assertions = lib.concatLists (
      lib.mapAttrsToList (
        name: agent:
        lib.optionals (agent.provider != null) [
          {
            assertion = cfg.providers ? ${agent.provider};
            message = ''
              services.claude-router.agents.${name}.provider is "${agent.provider}",
              which is not a configured provider (have: ${
                lib.concatStringsSep ", " (lib.attrNames cfg.providers)
              }).
            '';
          }
          {
            assertion =
              !(cfg.providers ? ${agent.provider})
              || lib.elem agent.model (providerModels cfg.providers.${agent.provider});
            message = ''
              services.claude-router.agents.${name}.model is "${agent.model}", which
              provider "${agent.provider}" does not offer (it has: ${
                lib.concatStringsSep ", " (providerModels cfg.providers.${agent.provider})
              }).
            '';
          }
        ]
      ) cfg.agents
    );

    home.packages = lib.optionals cfg.installWrapper [ wrapper cfg.package ];

    home.file = agentFiles;

    # The daemon reads this path by default, so the TUI shows the same file.
    xdg.configFile."claude-router/config.toml".source = configFile;

    # Socket activation: systemd holds the port, so the daemon starts on the
    # first request from Claude Code and exits again once idle.
    systemd.user.sockets.claude-router = {
      Socket.ListenStream = "127.0.0.1:${toString cfg.port}";
      Install.WantedBy = [ "sockets.target" ];
    };

    systemd.user.services.claude-router = {
      Unit = {
        Description = "Claude Code transparent router";
        After = [ "network.target" "claude-router.socket" ];
        Requires = [ "claude-router.socket" ];
        # The config is a store path, so a changed config is a changed unit
        # and home-manager restarts the daemon on switch.
        X-Config = "${configFile}";
      };
      Service = {
        ExecStart = "${lib.getExe cfg.package} --daemon --config ${configFile} --idle-timeout ${toString cfg.idleTimeout}";
        Restart = "on-failure";
        RestartSec = 2;
      };
    };
  };
}
