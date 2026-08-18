# Claude Code pointed at the router. The credential flow stays Claude Code's
# own; only the base URL and model-picker plumbing change.
#
# `claude-code` defaults to the one from the nixpkgs this is instantiated
# with. Override it to use a different source, e.g. an input flake's overlay
# or package:
#
#   pkgs.callPackage ./nix/wrapper.nix { claude-code = inputs.claude-code-nix.packages.${system}.default; }
#
# `name` is the installed command; set it to "claude" to shadow the plain
# CLI rather than living beside it as "claude-routed".
{
  lib,
  writeShellScriptBin,
  curl,
  claude-code,
  name ? "claude-routed",
  routerUrl ? "http://127.0.0.1:8787",
}:
writeShellScriptBin name ''
  # Everything here yields to a value already in the environment, so a
  # variable set on the command line still means what it says. CLAUDE_ROUTER_URL
  # is this wrapper's own knob and so outranks a stale base URL.
  if [ -n "''${CLAUDE_ROUTER_URL:-}" ]; then
    ANTHROPIC_BASE_URL="$CLAUDE_ROUTER_URL"
  fi
  export ANTHROPIC_BASE_URL="''${ANTHROPIC_BASE_URL:-${routerUrl}}"

  # Gateway discovery (Claude Code >= 2.1.129) adds every routed model to the
  # picker under "From gateway", but it only runs when ANTHROPIC_AUTH_TOKEN or
  # ANTHROPIC_API_KEY is set. A claude.ai OAuth login sets neither, so the CLI
  # reads ~/.claude/cache/gateway-models.json instead -- which the wrapper
  # writes below. The custom option further down is the extra single entry.
  export CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY="''${CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY:-1}"

  # Pre-write the gateway model cache so /model lists every configured model
  # under a claude.ai login, where live discovery never runs. Best-effort: a
  # cold or absent daemon just skips it and the picker falls back to the single
  # custom entry. Only replace the file on success, so a good cache is never
  # clobbered while the daemon is down.
  cache_dir="''${CLAUDE_CONFIG_DIR:-$HOME/.claude}/cache"
  if mkdir -p "$cache_dir" 2>/dev/null; then
    tmp="$cache_dir/gateway-models.json.tmp"
    if ${curl}/bin/curl -sf --max-time 2 "$ANTHROPIC_BASE_URL/__router/gateway-models" > "$tmp" 2>/dev/null; then
      mv "$tmp" "$cache_dir/gateway-models.json"
    else
      rm -f "$tmp"
    fi
  fi

  if [ -z "''${ANTHROPIC_CUSTOM_MODEL_OPTION:-}" ]; then
    entry=$(${curl}/bin/curl -sf --max-time 2 "$ANTHROPIC_BASE_URL/__router/picker" | head -n1) || entry=""
    if [ -n "$entry" ]; then
      export ANTHROPIC_CUSTOM_MODEL_OPTION=$(printf '%s' "$entry" | cut -f1)
      model_name=$(printf '%s' "$entry" | cut -f2)
      if [ -n "$model_name" ] && [ -z "''${ANTHROPIC_CUSTOM_MODEL_OPTION_NAME:-}" ]; then
        export ANTHROPIC_CUSTOM_MODEL_OPTION_NAME="$model_name"
      fi
    fi
  fi
  exec ${lib.getExe claude-code} "$@"
''
