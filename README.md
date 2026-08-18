# claude-code-transparent-router

A loopback HTTP router for Claude Code. It puts other providers' models in
the model picker while Anthropic traffic passes through untouched.

Claude Code points at the router instead of `api.anthropic.com`. Requests for
Anthropic models are forwarded byte for byte. Requests for models aliased as
`anthropic/<id>` go to a configured provider, translating between the
Anthropic Messages API and that provider's own.

## What it does

- **Other providers in Claude Code.** DeepSeek, GLM, or any endpoint speaking
  the Anthropic, OpenAI chat-completions, or OpenAI Responses API. `preset =
  "deepseek"` configures one in a line.
- **Your ChatGPT subscription as a backend.** `preset = "codex"` reaches
  GPT-5.6 through the Codex OAuth flow, spending the subscription's allowance
  rather than a metered API key.
- **Anthropic untouched.** Original bytes and headers, unbuffered streaming,
  no retries and no added timeout, so Claude Code's own backoff and
  rate-limit handling still work.
- **Short model names.** Select a model as `/model sol`, `codex/sol`, or its
  full `gpt-5.6-sol` — shorthands come from the preset and follow the newest
  version of that line.
- **Agents beyond the picker.** Claude Code shows one custom model; you pick
  which. The rest stay reachable by name, or as generated subagents that name
  a routed model in their frontmatter.
- **Per-user credentials.** Run `claude-router` for a TUI to manage them; they
  are never shared between users and never written to the config file.
- **Effort translation.** Claude Code's reasoning effort is moved to each
  provider's own field, and its levels collapsed where a provider has fewer.
  With no level configured, the provider's canonical default applies.

## Installation

- **NixOS** — [install-nixos.md](install-nixos.md)
- **Other Linux** — [install-systemd-linux.md](install-systemd-linux.md)

Both cover the single-user and machine-wide setups, and the Claude Code
wrapper that points the CLI at the router.

## Configuration

`--config <path>`, `$CLAUDE_ROUTER_CONFIG`, or
`~/.config/claude-router/config.toml`. Without a file, the router is pure
passthrough.

```toml
listen = "127.0.0.1:8787"                   # optional
picker_model = "deepseek/pro"                # which model fills /model
restrict_to_owner = true                    # only this user may connect
idle_timeout_secs = 300                     # exit when unused; 0 to stay

[providers.deepseek]
preset = "deepseek"

[providers.glm]
base_url = "https://api.z.ai/api/paas/v4"
models = ["glm-4.7"]
```

Model IDs and shorthands must be unique across providers, so that a bare name
selects exactly one model; a mistake is reported at startup. Credentials go in
the TUI or `claude-router login <provider>`, never here.

## Development

`nix develop` for the toolchain, `cargo test` for the suite.

## License

MIT — see [LICENSE](LICENSE).
