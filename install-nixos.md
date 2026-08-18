# Installing on NixOS

Two deployments, and the choice is about *whose* credentials the router
holds:

- **User service** (home-manager) — the router belongs to one person. Their
  keys, their logins, their `/model` picker. This is the right choice on a
  workstation.
- **System service** — one daemon for the whole machine. Everyone reaches it
  on the same port and, in the default multi-user mode, each person's
  credentials stay in their own directory keyed by the uid the kernel reports
  for the connection.

Several people each running their *own* user service on one machine is also
possible, but they must be given different `port`s — the first to start wins
`8787` and the rest fail to bind. If you find yourself doing that, the system
service is the tidier answer.

## The flake input

```nix
inputs.claude-router = {
  url = "github:sirati/claude-code-transparent-router";
  inputs.nixpkgs.follows = "nixpkgs";
};
```

## User service (home-manager)

Pass the module into home-manager — as a NixOS module that is:

```nix
home-manager.sharedModules = [ inputs.claude-router.homeManagerModules.default ];
```

Then, in your home configuration:

```nix
services.claude-router = {
  enable = true;

  # Install the wrapper as `claude` so the routed CLI is the one on PATH.
  # Then remove claude-code from environment.systemPackages, or two `claude`
  # binaries compete and which you get depends on profile ordering.
  wrapperName = "claude";

  # Which Claude Code the wrapper launches. Defaults to pkgs.claude-code,
  # including any overlay you already apply.
  # claudeCodePackage = inputs.claude-code-nix.packages.${system}.default;

  providers = {
    deepseek.preset = "deepseek";
    codex.preset = "codex";
  };

  # Claude Code shows exactly one custom entry in /model; this picks it.
  pickerModel = "anthropic/deepseek-v4-pro";
};
```

This writes `~/.config/claude-router/config.toml`, starts a systemd **user**
service, and puts `claude` and `claude-router` in your profile. Because the
daemon runs as you, `claude-router login` and the TUI write exactly where it
reads. `restrictToOwner` defaults to true, so no other local user can reach
the port.

### Subagents for the other models

Only one routed model fits in the picker, but a subagent's frontmatter can
name any model — so agents are how the rest stay one keystroke away. Files
are written into each directory in `agentDirs`, which is a list because a
machine may carry more than one Claude Code setup:

```nix
services.claude-router = {
  agentDirs = [ ".claude" ".claudeB" ];
  agents.flash = {
    model = "anthropic/deepseek-v4-flash";
    description = "Cheap, fast helper for mechanical edits.";
    effort = "low";
    tools = [ "Read" "Grep" "Glob" ];
    prompt = "You make small mechanical changes exactly as asked.";
  };
};
```

## System service

```nix
imports = [ inputs.claude-router.nixosModules.default ];

services.claude-router = {
  enable = true;
  wrapperName = "claude";

  providers.deepseek = {
    preset = "deepseek";
    # Optional: a machine-level key from systemd LoadCredential, never the
    # Nix store. Leave it out and each user sets their own in the TUI.
    apiKeyFile = "/run/secrets/deepseek-api-key";
  };

  # Default: each user's own keys and logins are filed under their uid.
  # perUserCredentials = true;

  # Optional: refuse connections from anyone not listed.
  # allowedUids = [ 1000 1001 ];
};
```

The unit is socket-activated on `127.0.0.1:8787`, runs as `DynamicUser` in a
hardened sandbox, and gets a `StateDirectory` for credentials set at runtime.
`claude-router login` hands the finished session to the daemon over the admin
API, so it lands in the right per-user directory even though the daemon runs
as a different user.

Note the precedence: a provider's `apiKeyFile` is a systemd credential, and
those outrank a user's own stored key for that provider. Use it for keys that
genuinely belong to the machine, not to a person.

## After the rebuild

```console
$ claude-router               # TUI: pick a provider, [s]et its API key
$ claude-router login codex   # browser sign-in for OAuth providers
$ claude                      # Claude Code, routed
```

Check `/model` for your chosen entry, and `journalctl --user -u claude-router`
(or `journalctl -u claude-router` for the system service) if a request does
not go where you expect — every routed request logs its provider and model.
