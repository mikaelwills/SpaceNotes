#!/bin/bash
INPUT=$(cat)

SESSION_ID=$(echo "$INPUT" | jq -r '.session_id // empty')
[ -z "$SESSION_ID" ] && exit 0

HOOK_EVENT=$(echo "$INPUT" | jq -r '.hook_event_name // "PreToolUse"')
PAYLOAD=$(echo "$INPUT" | jq -c --arg hook "$HOOK_EVENT" '. + {hook_event: $hook}')

send_to_local() {
  local port_file="$1"
  [ -f "$port_file" ] || return
  local port=$(cat "$port_file")
  local result=$(curl -s -o /dev/null -w "%{http_code}" -X POST "http://127.0.0.1:$port" \
    -H "Content-Type: application/json" \
    -d "$PAYLOAD" \
    --connect-timeout 1 \
    --max-time 2 2>/dev/null)
  if [ "$result" = "000" ]; then
    rm -f "$port_file"
  fi
}

if [ -n "$SPACE_CHANNEL_SESSION" ]; then
  send_to_local "/tmp/space-channel-${SPACE_CHANNEL_SESSION}.port"
else
  for f in /tmp/space-channel-*.port; do
    send_to_local "$f" &
  done
  wait
fi

WEBHOOK_URL="${SPACE_CHANNEL_WEBHOOK:-}"
SESSION="${SPACE_CHANNEL_SESSION:-}"
if [ -n "$WEBHOOK_URL" ] && [ -n "$SESSION" ]; then
  if [ "$HOOK_EVENT" = "UserPromptSubmit" ]; then
    curl -s -o /dev/null -X POST "$WEBHOOK_URL" \
      -H "Content-Type: application/json" \
      -d "{\"type\":\"status\",\"session\":\"$SESSION\",\"state\":\"thinking\"}" \
      --connect-timeout 1 --max-time 2 2>/dev/null &
  elif [ "$HOOK_EVENT" = "Stop" ]; then
    MESSAGE=$(echo "$INPUT" | jq -r '.last_assistant_message // empty')
    if [ -n "$MESSAGE" ]; then
      ID="hook-$(date +%s000)"
      MSG_PAYLOAD=$(jq -n --arg session "$SESSION" --arg text "$MESSAGE" --arg id "$ID" \
        '{type:"msg",session:$session,text:$text,id:$id,from:"assistant"}')
      curl -s -o /dev/null -X POST "$WEBHOOK_URL" \
        -H "Content-Type: application/json" \
        -d "$MSG_PAYLOAD" \
        --connect-timeout 1 --max-time 2 2>/dev/null &
    fi
    curl -s -o /dev/null -X POST "$WEBHOOK_URL" \
      -H "Content-Type: application/json" \
      -d "{\"type\":\"status\",\"session\":\"$SESSION\",\"state\":\"idle\"}" \
      --connect-timeout 1 --max-time 2 2>/dev/null &
  fi
fi

wait
exit 0
