# claude-code-transparent-router

Loopback HTTP router for Claude Code. Anthropic models pass through to
`api.anthropic.com` unchanged; models aliased as `anthropic/<id>` are routed
to second providers — either their Anthropic-compatible endpoints
(near-passthrough) or OpenAI-compatible ones (full Messages translation).

## Behavior

- Requests to Anthropic are forwarded with the original body bytes and
  headers; only hop-by-hop headers are stripped, nothing is added.
- Responses stream through unbuffered, including SSE. No decompression, no
  retries, no response timeout.
- Errors originating in the router itself (as opposed to upstream) are 502s
  marked with `x-proxy-origin: claude-code-transparent-router`.
- Unknown paths are proxied to Anthropic too; only `/v1/messages`,
  `/v1/messages/count_tokens`, and `/v1/models` are handled specially.
- `/v1/models` returns Anthropic's catalog with the configured provider
  models appended as `anthropic/<id>` (Claude Code's model picker drops IDs
  that don't start with `claude` or `anthropic`).
- Provider requests are built from scratch; inbound headers — including the
  Anthropic credential — never reach a provider.

## Usage

The binary is both the daemon and its control CLI:

- `claude-router --daemon` (or any run without a TTY, e.g. under systemd)
  is the daemon — the only thing that listens.
- `claude-router` from a terminal opens a TUI that configures the *running*
  daemon over its loopback admin API (`/__router/*`); it does not listen
  itself. It shows the configured providers and their credential status, and
  can set or clear credentials — pasted keys are masked (short prefix
  visible, the rest `****`). Changes apply to the daemon immediately.

```console
$ nix run github:sirati/claude-code-transparent-router -- --daemon   # daemon on 127.0.0.1:8787
$ nix run github:sirati/claude-code-transparent-router                 # TUI for the running daemon
$ nix run github:sirati/claude-code-transparent-router#claude-routed   # Claude Code pointed at it
```

State lives in the standard locations: config in
`~/.config/claude-router/config.toml`, credentials in
`~/.local/state/claude-router/credentials/` (under systemd:
`/var/lib/claude-router/credentials` via `StateDirectory`), logs on stderr
(journald).

## Configuration

`--config <path>`, `$CLAUDE_ROUTER_CONFIG`, or
`~/.config/claude-router/config.toml`. Without a config file the router is
pure passthrough.

```toml
listen = "127.0.0.1:8787"                         # optional
anthropic_upstream = "https://api.anthropic.com"  # optional

[providers.deepseek]
preset = "deepseek"                         # endpoint, dialect, models, effort

[providers.glm]
base_url = "https://api.z.ai/api/paas/v4"   # OpenAI-format endpoint -> translated
models = ["glm-4.7"]
```

### Presets

`preset = "<name>"` fills in a provider's defaults from a TOML file shipped
in [`presets/`](presets) — endpoint, API dialect, model list, and effort
mapping. Anything you write alongside it wins, key by key (arrays replace
rather than merge), so `preset` plus a `base_url` override points the same
model set at a different host. Credentials never come from a preset.

Available: `deepseek` (Anthropic-format endpoint, both `deepseek-v4-pro` and
`deepseek-v4-flash`). Adding one is a new file in `presets/` plus a line in
the registry.

`api` selects the provider's dialect: `"anthropic"` endpoints get
near-passthrough (only the model ID is rewritten and the credential swapped),
`"openai"` (the default) goes through the Messages ↔ chat-completions
translator. A model entry is a bare upstream ID or `{ id, name }`; `name` is
the display name shown in Claude Code's model switcher.

Model IDs must be unique across providers, since aliases don't carry the
provider name.

### Reasoning effort

Claude Code sends the session (or subagent) effort level as
`output_config.effort`. Providers spell that field differently and accept
different levels, so an optional per-provider `effort` block translates it —
the router itself knows no provider-specific levels:

```toml
[providers.example.effort]
field = "reasoning_effort"          # dotted path in the outgoing body
default = "high"                    # used when unset or unmapped
remove = ["output_config"]          # keys to drop before sending
map = { low = "low", medium = "high", high = "high", xhigh = "high", max = "max" }
```

Without an `effort` block the field is forwarded untouched — correct for
Anthropic-format endpoints that already accept Anthropic's own spelling.
Levels absent from `map` fall back to `default`; with no default the field is
left alone. Use it to pin a level (`default` with an empty `map`), collapse
levels a provider doesn't distinguish, or move the value to an
OpenAI-style `reasoning_effort`.

Credentials are not part of the config file. They resolve per provider, in
order:

1. systemd credential `$CREDENTIALS_DIRECTORY/<provider>` (`LoadCredential`)
2. `~/.local/state/claude-router/credentials/<provider>` (managed by the TUI)
3. `<PROVIDER>_API_KEY` environment variable

Requests to a provider without a credential fail with an error saying which
credential is missing and how to set it.

`count_tokens` for provider models returns a local estimate (`bytes / 4`);
these providers have no count endpoint.

## NixOS

```nix
{
  imports = [ claude-code-transparent-router.nixosModules.default ];
  services.claude-router = {
    enable = true;
    providers.glm = {
      baseUrl = "https://api.z.ai/api/paas/v4";
      models = [ "glm-4.7" ];
      apiKeyFile = "/run/secrets/glm-api-key";
    };
  };
}
```

The module generates the TOML config, wires one `LoadCredential` per
provider, and socket-activates the service on `127.0.0.1:8787` as
`DynamicUser` with a hardened sandbox. `claude-routed` is installed
system-wide unless `installClaudeRouted = false`.

## Development

`nix develop` provides the toolchain. `cargo test` runs unit and integration
tests, including the SSE translation state machine.

## License

MIT — see [LICENSE](LICENSE).
