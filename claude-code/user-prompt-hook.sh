#!/bin/bash
INPUT=$(cat)

SESSION="${SPACE_CHANNEL_SESSION:-note-assistant}"

curl -s -o /dev/null -X POST "http://127.0.0.1:5056/webhook" \
  -H "Content-Type: application/json" \
  -d "{\"type\":\"status\",\"session\":\"$SESSION\",\"state\":\"thinking\"}" \
  --connect-timeout 1 \
  --max-time 2 2>/dev/null

exit 0
