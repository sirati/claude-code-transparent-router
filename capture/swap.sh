#!/usr/bin/env bash
# Put the capture router on 8787 in place of the home-manager one, with the
# smallest gap possible: a live Claude Code session talks through that port.
#
# Undo with: capture/swap.sh --restore
set -u
DIR=/home/sirati/.local/share/claude-router-capture

if [ "${1:-}" = "--restore" ]; then
  pkill -f active-config.toml
  sleep 1
  systemctl --user start claude-router.socket
  sleep 1
  systemctl --user is-active claude-router.socket
  exit 0
fi

systemctl --user stop claude-router.socket claude-router.service
cd "$DIR" || exit 1
setsid ./claude-router --daemon --config active-config.toml > active-router.log 2>&1 < /dev/null &
sleep 2
curl -s --max-time 2 http://127.0.0.1:8787/__router/picker
