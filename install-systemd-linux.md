# Installing on other Linux distributions

> Unsupported. NixOS is what the author runs and tests; these instructions
> should work, but you are on your own if they do not.

## 1. Build

Needs a Rust toolchain (1.85+) and a C linker.

```console
$ git clone https://github.com/sirati/claude-code-transparent-router
$ cd claude-code-transparent-router
$ cargo build --release
$ install -Dm755 target/release/claude-router ~/.local/bin/claude-router
```

## 2. Install the service

Pick one. Both are socket-activated: systemd holds `127.0.0.1:8787`, starts
the daemon on Claude Code's first request, and it exits after 300 idle
seconds.

|  | User service | System-wide |
| --- | --- | --- |
| Units in | `~/.config/systemd/user` | `/etc/systemd/system` |
| Users per machine | one per port | any number, one port |

The two are equally secure. In both, the port is loopback-only, credentials
live in one user's own files, and the daemon identifies its caller by the uid
the kernel reports for the connection rather than anything the client claims.
The system-wide unit additionally runs as `DynamicUser` with
`ProtectHome=read-only`, so it reads each user's config and credentials and
can never write to them.

Prefer system-wide on a machine with more than one user: a user service per
person would need a distinct port each, since the first to start takes 8787
and the rest fail to bind.

Both use the same socket unit:

```ini
[Socket]
ListenStream=127.0.0.1:8787

[Install]
WantedBy=sockets.target
```

### User service

`claude-router.service`:

```ini
[Unit]
Requires=claude-router.socket
After=claude-router.socket

[Service]
ExecStart=%h/.local/bin/claude-router --daemon --idle-timeout 300
Restart=on-failure
RestartSec=2
```

```console
$ systemctl --user daemon-reload
$ systemctl --user enable --now claude-router.socket
```

`loginctl enable-linger $USER` keeps it available while you are logged out.

### System-wide

`claude-router.service`, with the binary in `/usr/local/bin`:

```ini
[Unit]
Requires=claude-router.socket
After=claude-router.socket

[Service]
ExecStart=/usr/local/bin/claude-router --daemon --listen 127.0.0.1:8787 \
          --user-config --idle-timeout 300
DynamicUser=true
StateDirectory=claude-router
ProtectHome=read-only
NoNewPrivileges=true
ProtectSystem=strict
PrivateTmp=true
RestrictAddressFamilies=AF_INET AF_INET6 AF_UNIX
SystemCallFilter=@system-service
MemoryDenyWriteExecute=true
LockPersonality=true
RestrictNamespaces=true
CapabilityBoundingSet=
```

`--user-config` is what resolves each connecting uid to its home. Leave
`restrict_to_owner` out of the config here — the daemon's own uid is not the
users'; to limit who may connect, use `allowed_uids = [1000, 1001]`.

## 3. Configure providers

`~/.config/claude-router/config.toml`, per user in both deployments:

```toml
restrict_to_owner = true       # user service only
picker_model = "deepseek/pro"  # fills Claude Code's one custom /model entry

[providers.deepseek]
preset = "deepseek"

[providers.codex]
preset = "codex"
```

A preset supplies the endpoint, API dialect, models and effort mapping;
anything beside it overrides that. Without one, spell it out:

```toml
[providers.glm]
base_url = "https://api.z.ai/api/paas/v4"
api = "openai"                              # openai | anthropic | responses
models = [{ id = "glm-4.7", name = "GLM 4.7", aliases = ["glm"] }]

[providers.glm.effort]                      # optional
field = "reasoning_effort"
map = { low = "low", medium = "medium", high = "high" }
```

## 4. Set credentials

Never in the config file. Per provider, first match wins:

| Source | Where |
| --- | --- |
| systemd credential | `LoadCredential=<provider>:/path/to/key` |
| credential store | `~/.local/state/claude-router/credentials/<provider>` |
| environment | `<PROVIDER>_API_KEY` |

```console
$ claude-router               # TUI: [s]et an API key, [l]og in, [c]lear
$ claude-router login codex   # the same browser sign-in, from the shell
```

A request to a provider with no credential fails with an error naming it.

## 5. Point Claude Code at the router

Save as `~/.local/bin/claude-routed`, or as `claude` to shadow the plain CLI
— then point `exec` at the real binary's full path so it does not call
itself.

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

## Agents

Agents are what mix providers inside a session: each names its own model and
effort, so a conversation can delegate to another provider and carry on.
Claude Code shows only one custom model in `/model` (`picker_model` chooses
it); agents have no such limit. `~/.claude/agents/flash.md`:

```markdown
---
name: flash
description: Cheap, fast helper for mechanical edits.
model: deepseek/flash
effort: low
tools: Read, Grep, Glob
---

You make small mechanical changes exactly as asked.
```

Models can be named by full ID, by shorthand, or qualified: `sol`,
`codex/sol`, `gpt-5.6-sol`.

## Troubleshooting

```console
$ curl -s http://127.0.0.1:8787/__router/providers | jq
$ journalctl --user -u claude-router -f
```

The log has one line per request with the provider and model it went to.
