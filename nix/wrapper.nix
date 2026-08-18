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

  # Gateway discovery adds every routed model to the picker, but only runs
  # under API-key auth. Under a claude.ai login it is a no-op, so the first
  # configured model is also exposed as the single supported custom entry.
  export CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY="''${CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY:-1}"

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
