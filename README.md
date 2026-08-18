# claude-code-transparent-router

A loopback HTTP router for Claude Code. Requests for Anthropic models are
forwarded to `api.anthropic.com` **semantically verbatim** — byte-identical
method, path, headers, and body in both directions. Requests for aliased
second-provider models (`anthropic/<id>`) are translated between the Anthropic
Messages API and an OpenAI-compatible provider (GLM by default).

There is deliberately **no TLS interception and no transport disguise**: the
inbound leg is plain HTTP on `127.0.0.1`, the outbound leg is this stack's own
normal TLS. Fidelity is semantic, not cosmetic — it's your credential, your
machine, your session.

## Design invariants

- **Verbatim passthrough.** The body is buffered, `model` is peeked with a
  shallow parse, and the *original bytes* are forwarded — never reserialized.
  Only hop-by-hop headers (`connection`, `keep-alive`, `te`, `trailer`,
  `transfer-encoding`, `upgrade`, `proxy-authenticate`, `proxy-authorization`)
  are stripped; multi-valued headers like `anthropic-beta` keep every value in
  order. Nothing is added — no `via`, no `x-forwarded-for`.
- **No content negotiation of our own.** reqwest's decompression is disabled,
  so `content-encoding` always tells the truth about the body it accompanies.
- **Streaming untouched.** SSE responses flow through chunk-by-chunk with no
  buffering, no re-framing, no compression layer.
- **No retries, no timeouts on responses.** Claude Code owns backoff (it reads
  `retry-after` / `anthropic-ratelimit-*`, which are forwarded verbatim).
  Proxy-origin failures return 502 with an `x-proxy-origin:
  claude-code-transparent-router` header so transcripts can tell the two apart.
- **Credential isolation.** `authorization` / `x-api-key` are never logged,
  inspected, or persisted, and *cannot* reach the second provider: the
  provider module's request signatures take body bytes and a model name only,
  and it builds its outbound headers from scratch.
- **Catch-all.** Every path that isn't explicitly owned (`/v1/models` merging)
  is proxied unchanged — telemetry, entitlements, future endpoints.

## Running

```console
$ nix run github:sirati/claude-code-transparent-router   # router on 127.0.0.1:8787
$ nix run github:sirati/claude-code-transparent-router#claude-routed   # Claude Code pointed at it
```

Configuration is by environment:

| Variable | Default | Meaning |
| --- | --- | --- |
| `CLAUDE_ROUTER_LISTEN` | `127.0.0.1:8787` | Loopback bind address |
| `ANTHROPIC_UPSTREAM_URL` | `https://api.anthropic.com` | Passthrough target |
| `GLM_API_KEY` | *(unset)* | Second-provider key (dev fallback; prefer `LoadCredential`) |
| `GLM_BASE_URL` | `https://api.z.ai/api/paas/v4` | OpenAI-compatible base URL |
| `GLM_MODELS` | `glm-4.7` | Comma-separated IDs, served as `anthropic/<id>` |

Without a GLM credential the router is pure passthrough. Token counting for
provider models is a coarse local estimate (`bytes / 4`) — the provider has no
count endpoint.

## NixOS

```nix
{
  imports = [ claude-code-transparent-router.nixosModules.default ];
  services.claude-router = {
    enable = true;
    glm.apiKeyFile = "/run/secrets/glm-api-key";  # via systemd LoadCredential
  };
}
```

The unit is socket-activated on `127.0.0.1:8787`, runs as `DynamicUser` under
a hardened sandbox, and installs the `claude-routed` wrapper system-wide
(`installClaudeRouted = false` to opt out).

## Development

`nix develop` provides the toolchain (nightly rust via rust-overlay,
rust-analyzer, cargo-nextest). `cargo test` runs the unit tests.

## License

MIT — see [LICENSE](LICENSE).
