# Installing on other Linux distributions

Everything here is plain systemd and a config file; nothing is NixOS-specific.

## Build and install

Needs a Rust toolchain (1.85+) and a C linker:

```console
$ git clone https://github.com/sirati/claude-code-transparent-router
$ cd claude-code-transparent-router
$ cargo build --release
$ install -Dm755 target/release/claude-router ~/.local/bin/claude-router
```

For a system-wide install put it in `/usr/local/bin` instead.

## Configure

Write `~/.config/claude-router/config.toml`. It never holds credentials:

```toml
listen = "127.0.0.1:8787"
restrict_to_owner = true                    # only your uid may connect
picker_model = "anthropic/deepseek-v4-pro"  # fills Claude Code's one custom row

[providers.deepseek]
preset = "deepseek"

[providers.codex]
preset = "codex"
```

`preset` fills in the endpoint, API dialect, model list and effort mapping;
anything you write beside it overrides that. Without a preset, spell it out:

```toml
[providers.glm]
base_url = "https://api.z.ai/api/paas/v4"
api = "openai"                              # openai | anthropic | responses
models = [{ id = "glm-4.7", name = "GLM 4.7" }]

[providers.glm.effort]                      # optional
field = "reasoning_effort"
map = { low = "low", medium = "medium", high = "high" }
```

## Run it as a user service

The daemon belongs to one person, so a user unit is the natural fit — it
shares a state directory with `claude-router login` and the TUI, and starts
when you log in. Write `~/.config/systemd/user/claude-router.service`:

```ini
[Unit]
Description=Claude Code transparent router
After=network.target

[Service]
ExecStart=%h/.local/bin/claude-router --daemon
Restart=on-failure
RestartSec=2

[Install]
WantedBy=default.target
```

```console
$ systemctl --user daemon-reload
$ systemctl --user enable --now claude-router
$ systemctl --user status claude-router
```

If the router should keep running while you are logged out:
`sudo loginctl enable-linger $USER`.

### Or as a system service

One daemon for everyone on the machine. Add `per_user_credentials = true` to
the config so each person's keys are filed under the uid the kernel reports
for their connection, then use `/etc/claude-router/config.toml`:

```ini
[Unit]
Description=Claude Code transparent router
After=network.target

[Service]
ExecStart=/usr/local/bin/claude-router --daemon --config /etc/claude-router/config.toml
DynamicUser=true
StateDirectory=claude-router
Restart=on-failure
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
PrivateTmp=true
RestrictAddressFamilies=AF_INET AF_INET6 AF_UNIX
SystemCallFilter=@system-service
MemoryDenyWriteExecute=true
LockPersonality=true
RestrictNamespaces=true
CapabilityBoundingSet=

[Install]
WantedBy=multi-user.target
```

Leave `restrict_to_owner` out here — the daemon's own uid is not the users'.
To limit who may connect, list them: `allowed_uids = [1000, 1001]`.

Note that several people each running their own *user* service on one machine
need different `listen` ports; the first to start takes 8787 and the others
fail to bind. A single system service avoids that.

## Credentials

Never in the config file. Per provider, in order:

1. a systemd credential — `LoadCredential=deepseek:/path/to/key`
2. the store at `~/.local/state/claude-router/credentials/<provider>`
   (`$STATE_DIRECTORY/credentials` under a system unit), written by the TUI
3. the `<PROVIDER>_API_KEY` environment variable

```console
$ claude-router               # TUI: select a provider, [s]et its key
$ claude-router login codex   # browser sign-in for OAuth providers
$ claude-router logout codex
```

A request to a provider with no credential fails with an error naming it and
saying how to fix it.

## The Claude Code wrapper

Claude Code needs two things: the base URL, and the picker entry. Save this
as `~/.local/bin/claude-routed` (or `claude`, if you would rather it shadow
the plain CLI — then make sure it comes first on `PATH`):

```bash
#!/usr/bin/env bash
export ANTHROPIC_BASE_URL="${CLAUDE_ROUTER_URL:-http://127.0.0.1:8787}"
# Adds every routed model to the picker, but only under API-key auth.
export CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY=1
# Under a claude.ai login discovery never runs, so expose the chosen model as
# the one custom entry Claude Code supports.
entry=$(curl -sf --max-time 2 "$ANTHROPIC_BASE_URL/__router/picker" | head -n1) || entry=""
if [ -n "$entry" ]; then
  export ANTHROPIC_CUSTOM_MODEL_OPTION=$(printf '%s' "$entry" | cut -f1)
  name=$(printf '%s' "$entry" | cut -f2)
  [ -n "$name" ] && export ANTHROPIC_CUSTOM_MODEL_OPTION_NAME="$name"
fi
exec claude "$@"
```

`chmod +x` it, and if it is named `claude`, have `exec` point at the real
binary's full path so it does not call itself.

## Subagents for the other models

Only one routed model fits the picker, but a subagent's frontmatter can name
any model. Write `~/.claude/agents/flash.md`:

```markdown
---
name: flash
description: Cheap, fast helper for mechanical edits.
model: anthropic/deepseek-v4-flash
effort: low
tools: Read, Grep, Glob
---

You make small mechanical changes exactly as asked.
```

## Checking it works

```console
$ curl -s http://127.0.0.1:8787/__router/providers | jq
$ claude-routed
```

`journalctl --user -u claude-router -f` logs one line per request with the
provider and model it went to, so it is obvious whether a turn was routed or
passed through to Anthropic.
