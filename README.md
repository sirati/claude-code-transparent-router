# claude-code-transparent-router

A loopback HTTP router for Claude Code. Point Claude Code at it instead of
`api.anthropic.com` and models from other providers appear in the model
picker, while Anthropic traffic is forwarded byte for byte.

## Features

- Use other providers' models in Claude Code.
- Use a ChatGPT subscription instead of metered API billing.
- Reach several routed models, despite Claude Code's single custom picker
  slot.
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
across providers, checked at startup. Models beyond the picker slot are
reachable by name or through generated subagents. Reasoning effort is
translated to each provider's own field and levels.

## Development

`nix develop` for the toolchain, `cargo test` for the suite.

## License

MIT — see [LICENSE](LICENSE).
