# claude-code-transparent-router

Mix providers inside a single Claude Code session. Keep Claude as the model
you talk to and hand work to subagents running on DeepSeek or GPT — or make
one of those the main model instead. It is a loopback HTTP router: point
Claude Code at it instead of `api.anthropic.com`.

## Features

- **Mix providers in one session.** Subagents run on whichever provider you
  give them, so a conversation can delegate to DeepSeek or GPT and carry on.
- **Start a subagent on another provider directly.** Each agent names its own
  model and reasoning effort.
- Use a ChatGPT subscription instead of metered API billing.
- Serve several users from one daemon, each with their own providers and
  credentials.
- Anthropic models keep working unchanged: same bytes, same streaming, same
  rate-limit handling.

## Presets

`preset = "<name>"` configures a provider in one line.

| Preset | Provider | Models | Credential |
| --- | --- | --- | --- |
| `deepseek` | DeepSeek | `pro`, `flash` | API key |
| `codex` | OpenAI | `sol`, `terra`, `luna` | ChatGPT login |

Shorthands follow the newest model of a line; `pro-v4` and `sol-5.6` pin a
version.

## Provider APIs

Any provider can also be configured by hand, in one of three dialects.

| `api` | Endpoint | Used by |
| --- | --- | --- |
| `anthropic` | `/v1/messages` | DeepSeek, Anthropic-compatible gateways |
| `openai` | `/chat/completions` | GLM, most OpenAI-compatible APIs |
| `responses` | `/responses` | OpenAI GPT-5.6 |

## Installation

| System | Guide |
| --- | --- |
| NixOS | [install-nixos.md](install-nixos.md) |
| Other Linux | [install-systemd-linux.md](install-systemd-linux.md) |

## Configuration

`--config <path>`, `$CLAUDE_ROUTER_CONFIG`, or
`~/.config/claude-router/config.toml`. Credentials never go here;
`claude-router` opens a TUI to manage them per user.

```toml
listen = "127.0.0.1:8787"      # optional
picker_model = "deepseek/pro"  # fills Claude Code's one custom /model entry
restrict_to_owner = true       # only this user may connect
idle_timeout_secs = 300        # exit when unused; 0 to stay resident

[providers.deepseek]
preset = "deepseek"

[providers.glm]
base_url = "https://api.z.ai/api/paas/v4"
api = "openai"
models = [{ id = "glm-4.7", name = "GLM 4.7", aliases = ["glm"] }]
```

Select a model as `sol`, `codex/sol`, or `gpt-5.6-sol`. Names must be unique
across providers, checked at startup.

Subagents are what make a session mixed: each names a model and, optionally,
an effort level. Claude Code shows only one custom model in `/model`
(`picker_model` chooses it), but an agent's frontmatter has no such limit.

```markdown
---
name: flash
description: Cheap, fast helper for mechanical edits.
model: deepseek/flash
effort: low
---
```

On NixOS these are generated from your configuration, including into several
Claude Code directories at once. Reasoning effort is translated to each
provider's own field and levels.

## Development

`nix develop` for the toolchain, `cargo test` for the suite.

## License

MIT — see [LICENSE](LICENSE).
