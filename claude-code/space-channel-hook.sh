#!/bin/bash
INPUT=$(cat)

SESSION_ID=$(echo "$INPUT" | jq -r '.session_id // empty')
[ -z "$SESSION_ID" ] && exit 0

HOOK_EVENT=$(echo "$INPUT" | jq -r '.hook_event_name // "PreToolUse"')
PAYLOAD=$(echo "$INPUT" | jq -c --arg hook "$HOOK_EVENT" '. + {hook_event: $hook}')

if [ -n "$SPACE_CHANNEL_SESSION" ]; then
  PORT_FILE="/tmp/space-channel-${SPACE_CHANNEL_SESSION}.port"
  if [ -f "$PORT_FILE" ]; then
    PORT=$(cat "$PORT_FILE")
    RESULT=$(curl -s -o /dev/null -w "%{http_code}" -X POST "http://127.0.0.1:$PORT" \
      -H "Content-Type: application/json" \
      -d "$PAYLOAD" \
      --connect-timeout 1 \
      --max-time 2 2>/dev/null)
    if [ "$RESULT" = "000" ]; then
      rm -f "$PORT_FILE"
    fi
  fi
else
  for f in /tmp/space-channel-*.port; do
    [ -f "$f" ] || continue
    PORT=$(cat "$f")
    RESULT=$(curl -s -o /dev/null -w "%{http_code}" -X POST "http://127.0.0.1:$PORT" \
      -H "Content-Type: application/json" \
      -d "$PAYLOAD" \
      --connect-timeout 1 \
      --max-time 2 2>/dev/null)
    if [ "$RESULT" = "000" ]; then
      rm -f "$f"
    fi &
  done
  wait
fi
exit 0
