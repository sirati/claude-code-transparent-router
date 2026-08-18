#!/usr/bin/env bash
# Replay the captured requests through a running router and report which ones
# it calls compaction — the real /compact next to the ordinary turns.
set -u
LOG=/home/sirati/.local/share/claude-router-capture/captured.jsonl
PORT=${1:-8787}

n=0
while IFS= read -r line; do
  n=$((n + 1))
  body=$(printf '%s' "$line" | jq -c '.body')
  msgs=$(printf '%s' "$body" | jq '.messages | length')
  # Route it at the capture model so nothing real is spent.
  body=$(printf '%s' "$body" | jq -c '.model = "test" | .stream = false | .max_tokens = 16')
  reply=$(curl -s --max-time 20 -X POST "http://127.0.0.1:$PORT/v1/messages" \
    -H 'content-type: application/json' -d "$body" | head -c 120)
  printf 'line %s  messages=%-6s -> %s\n' "$n" "$msgs" "${reply:0:80}"
done < "$LOG"
