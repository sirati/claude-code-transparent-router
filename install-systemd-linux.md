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

System-wide is just as secure — every user's credentials stay in their own
files, and the daemon reads them by the uid the kernel reports for the
connection — and avoids giving each user a separate port.

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

`--user-config` resolves each connecting uid to its home. Leave
`restrict_to_owner` out of the config here; to limit who may connect, use
`allowed_uids = [1000, 1001]`.

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
models = [{ id = "glm-4.7", name = "GLM 4.7", aliases = ["glm"], context_window = 200000 }]

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

### Signing in from another machine

On a headless box the browser runs elsewhere, so the provider's redirect
lands on a `localhost` address that does not exist there. Paste that address
back — into the TUI's sign-in screen, or at the prompt `claude-router login`
leaves open — and the login finishes. It is checked exactly as the callback
would be, so a URL from a different attempt is refused.

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

## Context windows

Claude Code assumes 200k tokens for a model it does not recognise and
compacts against that. Declare `context_window` per model and the router
handles the rest:

- models at 1M or more are named with Claude Code's `[1m]` marker, which is
  the only per-model way to state a window;
- the smallest window among the remaining models is served at
  `/__router/context-window`, and the wrapper exports it as
  `CLAUDE_CODE_MAX_CONTEXT_TOKENS` — that setting is per session, not per
  model, so the smallest is the only value safe for all of them.

Set `force_max_context_window` in the config to pin that number. Anthropic models
ignore the variable, so the main session is unaffected either way.

## Agents

Each agent names a model and effort — `~/.claude/agents/flash.md`:

```markdown
---
name: flash
description: Cheap, fast helper for mechanical edits.
model: deepseek/flash[1m]
effort: low
tools: Read, Grep, Glob
---

You make small mechanical changes exactly as asked.
```

Name a model by full ID, by shorthand, or qualified: `sol`, `codex/sol`,
`gpt-5.6-sol`.

## Troubleshooting

```console
$ curl -s http://127.0.0.1:8787/__router/providers | jq
$ journalctl --user -u claude-router -f
```

The log has one line per request with the provider and model it went to.
