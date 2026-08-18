# Installing on other Linux distributions

> Unsupported. NixOS is what the author runs and tests; these instructions
> should work, but you are on your own if they do not.

## Build

Needs a Rust toolchain (1.85+) and a C linker.

```console
$ git clone https://github.com/sirati/claude-code-transparent-router
$ cd claude-code-transparent-router
$ cargo build --release
$ install -Dm755 target/release/claude-router ~/.local/bin/claude-router
```

## Configure

`~/.config/claude-router/config.toml`, which never holds credentials:

```toml
restrict_to_owner = true                    # only your uid may connect
picker_model = "deepseek/pro"                # which model fills /model

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
models = [{ id = "glm-4.7", name = "GLM 4.7" }]

[providers.glm.effort]                      # optional
field = "reasoning_effort"
map = { low = "low", medium = "medium", high = "high" }
```

## Run it

A user service, socket-activated so it starts on Claude Code's first request
and exits when unused.

`~/.config/systemd/user/claude-router.socket`:

```ini
[Socket]
ListenStream=127.0.0.1:8787

[Install]
WantedBy=sockets.target
```

`~/.config/systemd/user/claude-router.service`:

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

### Machine-wide instead

One daemon for everyone: it resolves each connecting uid to that user's home
and reads their own config and credentials, so nothing personal is configured
system-wide. Use `/etc/systemd/system/claude-router.{socket,service}` with the
same socket, and:

```ini
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

`ProtectHome=read-only` is required: the daemon reads users' configs and
credentials and never writes to them. Leave `restrict_to_owner` out here — the
daemon's own uid is not the users'. To limit who may connect, list them with
`allowed_uids = [1000, 1001]`.

Several *user* services on one machine would each need a different port; a
single system service avoids that.

## Credentials

Per provider, in order: a systemd credential
(`LoadCredential=deepseek:/path/to/key`), the store at
`~/.local/state/claude-router/credentials/<provider>`, then
`<PROVIDER>_API_KEY`.

```console
$ claude-router               # TUI: [s]et an API key, [l]og in, [c]lear
$ claude-router login codex   # same browser sign-in, from the shell
```

A request to a provider with no credential fails with an error naming it.

## Point Claude Code at the router

Save as `~/.local/bin/claude-routed`, or as `claude` if you would rather it
shadow the plain CLI:

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

If you name it `claude`, point `exec` at the real binary's full path.

## Agents

The models that did not win the picker slot stay selectable as subagents.
`~/.claude/agents/flash.md`:

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

## Checking it

```console
$ curl -s http://127.0.0.1:8787/__router/providers | jq
$ journalctl --user -u claude-router -f
```

The log has one line per request with the provider and model it went to.
