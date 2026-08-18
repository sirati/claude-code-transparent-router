# Installing on NixOS

## 1. Add the flake input

```nix
inputs.claude-router = {
  url = "github:sirati/claude-code-transparent-router";
  inputs.nixpkgs.follows = "nixpkgs";
};
```

## 2. Import the home-manager module

```nix
home-manager.sharedModules = [ inputs.claude-router.homeManagerModules.default ];
```

## 3. Configure it

```nix
services.claude-router = {
  enable = true;
  wrapperName = "claude";        # install the wrapper as `claude`
  pickerModel = "anthropic/deepseek-v4-pro";

  providers = {
    deepseek.preset = "deepseek";
    codex.preset = "codex";
  };
};
```

Also remove `claude-code` from `environment.systemPackages`: with
`wrapperName = "claude"` two `claude` binaries would otherwise compete on
`PATH`. The wrapper still launches it — by default `pkgs.claude-code`,
including any overlay you apply, or set `claudeCodePackage` to something
else.

## 4. Rebuild and use it

```console
$ claude-router      # TUI: [s]et an API key, [l]og in
$ claude             # Claude Code, routed
```

The daemon is a socket-activated user service: systemd holds
`127.0.0.1:8787`, starts it on the first request, and it exits after
`idleTimeout` seconds (default 300). Only your uid may connect.

---

## Agents

Claude Code shows one custom model in `/model`, chosen by `pickerModel`. The
others stay selectable as subagents:

```nix
services.claude-router = {
  agentDirs = [ ".claude" ".claudeB" ];   # a machine may carry several
  agents.flash = {
    provider = "deepseek";
    model = "deepseek-v4-flash";
    description = "Cheap, fast helper for mechanical edits.";
    effort = "low";
    tools = [ "Read" "Grep" "Glob" ];
    prompt = "You make small mechanical changes exactly as asked.";
  };
};
```

`provider` and `model` are checked against your configured providers when you
build, so a typo fails the rebuild rather than a conversation.

## Shared machines

For several users, run one system service instead of a user service each
(which would need a distinct port per person):

```nix
imports = [ inputs.claude-router.nixosModules.default ];

services.claude-router = {
  enable = true;
  wrapperName = "claude";
  # allowedUids = [ 1000 1001 ];   # optional: refuse everyone else
};
```

No providers are configured here. The daemon resolves each connecting uid to
its home and reads that user's own `~/.config/claude-router/config.toml`, so
everyone keeps their own providers and credentials and uses the same TUI. It
runs as `DynamicUser` with `ProtectHome = "read-only"`, so it reads those
files and never writes them.

A provider given an `apiKeyFile` gets a machine-level key from systemd
credentials, which outranks any user's own key for that provider.

## Troubleshooting

`journalctl --user -u claude-router` — or without `--user` for the system
service — logs one line per request with the provider and model it went to.
