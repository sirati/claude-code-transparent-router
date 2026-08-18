# claude-code-transparent-router

Mix providers inside a single Claude Code session: keep Claude as the model
you talk to and hand work to subagents running on DeepSeek or GPT, or make
one of those the main model instead. Anthropic models keep working exactly as
before.

A loopback HTTP router — point Claude Code at it instead of
`api.anthropic.com`.

## Features

- Run subagents on other providers, started straight from the conversation.
- Use a ChatGPT subscription for GPT instead of metered API billing.
- Add any Anthropic- or OpenAI-compatible endpoint as a provider.

## Agents

An agent names its own model and effort, which is what makes a session mixed.
`/model` lists every configured model, via Claude Code's gateway discovery;
agents can name any of them directly.

```markdown
---
name: flash
description: Cheap, fast helper for mechanical edits.
model: deepseek/flash
effort: low
---
```

On NixOS these are generated from your configuration.

## Presets

`preset = "<name>"` configures a provider in one line.

| Preset | Provider | Models | Credential |
| --- | --- | --- | --- |
| `deepseek` | DeepSeek | `pro`, `flash` | API key |
| `codex` | OpenAI | `sol`, `terra`, `luna` | ChatGPT login |

Shorthands follow the newest model of a line; `pro-v4` and `sol-5.6` pin a
version.

## Provider APIs

Providers without a preset are configured by hand, in one of three dialects.

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

On NixOS through the module options; elsewhere
`~/.config/claude-router/config.toml`, which the NixOS module generates for
you. Credentials are set with `claude-router`, never in the configuration.

```toml
picker_model = "deepseek/pro"  # the extra custom /model entry, on top of discovery

[providers.deepseek]
preset = "deepseek"

[providers.glm]
base_url = "https://api.z.ai/api/paas/v4"
api = "openai"
models = [{ id = "glm-4.7", name = "GLM 4.7", aliases = ["glm"] }]
```

Name a model as `sol`, `codex/sol`, or `gpt-5.6-sol`; names must be unique
across providers.

## Compaction

`/compact` reaches the router as an ordinary request whose last user message
carries Claude Code's summarisation instruction, so the router recognises it
and providers that compact their own way can be told to:

| Key | Effect |
| --- | --- |
| `compact_patterns` | Extra wordings that mark a compaction, for phrasings a release changes |
| `[providers.X.compaction]` | `path`, `trigger_item`, `request_extra`, `request_remove`, applied only to a compaction |

Sending `OVERRIDE_SHOULD_COMPACT` on its own accepts the next request whatever
it looks like.

## Development

`nix develop` for the toolchain, `cargo test` for the suite.

## License

MIT — see [LICENSE](LICENSE).
