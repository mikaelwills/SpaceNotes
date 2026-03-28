#!/bin/bash
INPUT=$(cat)

SESSION="${SPACE_CHANNEL_SESSION:-note-assistant}"

MESSAGE=$(echo "$INPUT" | jq -r '.last_assistant_message // empty')

if [ -n "$MESSAGE" ]; then
  ID="hook-$(date +%s%3N)"
  PAYLOAD=$(jq -n --arg session "$SESSION" --arg text "$MESSAGE" --arg id "$ID" \
    '{type:"msg",session:$session,text:$text,id:$id,from:"assistant"}')

  curl -s -o /dev/null -X POST "http://127.0.0.1:5056/webhook" \
    -H "Content-Type: application/json" \
    -d "$PAYLOAD" \
    --connect-timeout 1 \
    --max-time 2 2>/dev/null
fi

curl -s -o /dev/null -X POST "http://127.0.0.1:5056/webhook" \
  -H "Content-Type: application/json" \
  -d "{\"type\":\"status\",\"session\":\"$SESSION\",\"state\":\"idle\"}" \
  --connect-timeout 1 \
  --max-time 2 2>/dev/null

exit 0
