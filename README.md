# claude-code-transparent-router

A loopback HTTP router that sits in front of Claude Code, so models from
other providers appear in its model picker while Anthropic traffic passes
through untouched.

Requests for Anthropic models go to `api.anthropic.com` with the original
bytes and headers. Requests for models aliased as `anthropic/<id>` are routed
to a configured provider, translating between the Anthropic Messages API and
whatever that provider speaks.

## Features

**Verbatim passthrough.** Anthropic requests keep their exact body bytes and
headers; only hop-by-hop headers are stripped, nothing is added, and
responses stream back unbuffered. No decompression, no retries, no response
timeout — Claude Code keeps owning backoff, and reads `retry-after` and the
rate-limit headers as sent. Failures from the router itself are 502s marked
`x-proxy-origin: claude-code-transparent-router`, so a transcript never
confuses the two.

**Three provider dialects.** `anthropic` endpoints get near-passthrough (only
the model ID is rewritten and the credential swapped). `openai` goes through
a Messages ↔ chat-completions translator. `responses` goes through a Messages
↔ Responses translator, including tool calls, reasoning blocks and streaming.

**Presets.** `preset = "deepseek"` supplies a provider's endpoint, dialect,
model list and effort mapping from a file in [`presets/`](presets); anything
you write beside it wins. Ships `deepseek` and `codex`.

**Credential isolation.** Your Anthropic credential is never logged, stored,
or forwarded to a provider — provider requests are built from a fresh header
map, so it cannot leak by construction. Provider keys live outside the config
file, in a 0600 store, resolved from systemd credentials, the store, or the
environment.

**Browser logins.** Providers with an `[oauth]` block sign in with
`claude-router login <name>` (authorization code + PKCE, tokens refreshed
automatically). Issuer, client id, scopes, callback port and claim paths are
all config, so a new OAuth provider is a preset file.

**Access control.** The daemon can verify the uid behind each loopback
connection and refuse everyone else, so a shared machine cannot spend your
credentials. In multi-user mode one daemon serves everybody, filing each
user's keys under their own uid.

**Reasoning effort.** Claude Code's effort level is translated per provider:
target field, level mapping, and a default, all from config.

**Model picker.** Claude Code accepts exactly one custom picker entry, so you
choose which routed model fills it. The rest stay reachable through `--model`
and `/model <id>` — or through generated subagents, whose frontmatter can name
any routed model.

**TUI.** Running `claude-router` in a terminal opens a client for the running
daemon: provider list, credential status, and masked key entry.

## Installation

- **NixOS** — [install-nixos.md](install-nixos.md): flake input, a
  home-manager user service or a system-wide service, and the Claude Code
  wrapper.
- **Other Linux** — [install-systemd-linux.md](install-systemd-linux.md):
  cargo build, a systemd user or system unit, and the wrapper script.

## Configuration

Config lives at `--config <path>`, `$CLAUDE_ROUTER_CONFIG`, or
`~/.config/claude-router/config.toml`. Without a file the router is pure
passthrough. It never contains credentials.

```toml
listen = "127.0.0.1:8787"                   # optional
picker_model = "anthropic/deepseek-v4-pro"  # which model fills /model
restrict_to_owner = true                    # only this user may connect

[providers.deepseek]
preset = "deepseek"                         # endpoint, dialect, models, effort

[providers.glm]
base_url = "https://api.z.ai/api/paas/v4"   # OpenAI-format -> translated
models = ["glm-4.7"]
```

Model IDs must be unique across providers, since an alias does not carry the
provider name. The install guides cover the rest: credentials, effort
mapping, agents, and multi-user setups.

## Development

`nix develop` provides the toolchain. `cargo test` runs the unit and
integration tests, including the SSE translators and the credential rules.

## License

MIT — see [LICENSE](LICENSE).
