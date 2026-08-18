# claude-code-transparent-router

Loopback HTTP router for Claude Code. Anthropic models pass through to
`api.anthropic.com` unchanged; models aliased as `anthropic/<id>` are
translated to OpenAI-compatible providers (GLM, DeepSeek, ...).

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

```console
$ nix run github:sirati/claude-code-transparent-router                 # router on 127.0.0.1:8787
$ nix run github:sirati/claude-code-transparent-router#claude-routed   # Claude Code pointed at it
```

Run from a terminal, the router opens a TUI listing the configured providers
and their credential status, with actions to set or clear credentials. Key
entry is masked: a short prefix stays visible, the rest shows as `****`.
`--headless` (or no TTY) runs the plain server.

## Configuration

`--config <path>`, `$CLAUDE_ROUTER_CONFIG`, or
`~/.config/claude-router/config.toml`. Without a config file the router is
pure passthrough.

```toml
listen = "127.0.0.1:8787"                         # optional
anthropic_upstream = "https://api.anthropic.com"  # optional

[providers.glm]
base_url = "https://api.z.ai/api/paas/v4"
models = ["glm-4.7"]

[providers.deepseek]
base_url = "https://api.deepseek.com/v1"
models = ["deepseek-chat"]
```

Model IDs must be unique across providers, since aliases don't carry the
provider name.

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
