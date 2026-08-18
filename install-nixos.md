# Installing on NixOS

## 1. Add the flake input

```nix
inputs.claude-router = {
  url = "github:sirati/claude-code-transparent-router";
  inputs.nixpkgs.follows = "nixpkgs";
};
```

## 2. Install the service

Pick one. Both are socket-activated on `127.0.0.1:8787` and exit after
`idleTimeout` seconds idle (default 300).

|  | User service | System-wide |
| --- | --- | --- |
| Module | `homeManagerModules.default` | `nixosModules.default` |
| Providers configured in | your home config | each user's `~/.config/claude-router/config.toml` |
| Users per machine | one per port | any number, one port |

System-wide is just as secure — every user's credentials stay in their own
files, and the daemon reads them by the uid the kernel reports for the
connection — and avoids giving each user a separate port.

### User service

```nix
home-manager.sharedModules = [ inputs.claude-router.homeManagerModules.default ];
```

```nix
services.claude-router = {
  enable = true;
  wrapperName = "claude";        # install the wrapper as `claude`
  pickerModel = "deepseek/pro";

  providers = {
    deepseek.preset = "deepseek";
    codex.preset = "codex";
  };
};
```

### System-wide

```nix
imports = [ inputs.claude-router.nixosModules.default ];

services.claude-router = {
  enable = true;
  wrapperName = "claude";
  # allowedUids = [ 1000 1001 ];   # optional: refuse everyone else
};
```

Providers are not configured here: each user writes their own
`~/.config/claude-router/config.toml`, in the format shown in
[install-systemd-linux.md](install-systemd-linux.md#3-configure-providers).
A provider given an `apiKeyFile` gets a machine-wide key instead, which
outranks any user's own.

## 3. Use it

```console
$ claude-router      # TUI: [s]et an API key, [l]og in
$ claude             # Claude Code, routed
```

Remove `claude-code` from `environment.systemPackages` — with `wrapperName =
"claude"` the two would compete on `PATH`. The wrapper launches
`pkgs.claude-code` including your overlays, or `claudeCodePackage` if set.

## Agents

Each agent names a model and effort. Written into every directory in
`agentDirs`, so a machine with more than one Claude Code setup gets them all.

```nix
services.claude-router = {
  agentDirs = [ ".claude" ".claudeB" ];
  agents.flash = {
    provider = "deepseek";
    model = "flash";               # ID or a preset shorthand
    description = "Cheap, fast helper for mechanical edits.";
    effort = "low";
    tools = [ "Read" "Grep" "Glob" ];
    prompt = "You make small mechanical changes exactly as asked.";
  };
};
```

`provider` and `model` are checked when you build, so a typo fails the
rebuild rather than a conversation. With the system-wide service, write the
agent files yourself.

## Troubleshooting

`journalctl --user -u claude-router` — or without `--user` for the system
service — logs one line per request with the provider and model it went to.
