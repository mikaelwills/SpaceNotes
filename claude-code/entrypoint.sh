#!/bin/bash
set -e

export SPACE_CHANNEL_SESSION="${SPACE_CHANNEL_SESSION:-note-assistant}"

mkdir -p $HOME/.claude

CLAUDE_JSON="$HOME/.claude.json"

if [ -f "$HOME/.claude/.claude.json.bak" ] && [ ! -s "$CLAUDE_JSON" ]; then
    cp "$HOME/.claude/.claude.json.bak" "$CLAUDE_JSON"
    echo "Restored claude.json from backup"
fi

if [ -f "$CLAUDE_JSON" ]; then
    node -e "
      const fs = require('fs');
      const existing = JSON.parse(fs.readFileSync('$CLAUDE_JSON', 'utf8'));
      const mcp = JSON.parse(fs.readFileSync('/opt/claude.json', 'utf8'));
      existing.mcpServers = { ...existing.mcpServers, ...mcp.mcpServers };
      if (!existing.projects) existing.projects = {};
      if (!existing.projects[process.env.HOME]) existing.projects[process.env.HOME] = {};
      existing.projects[process.env.HOME].hasTrustDialogAccepted = true;
      existing.hasCompletedOnboarding = true;
      existing.theme = existing.theme || 'dark';
      fs.writeFileSync('$CLAUDE_JSON', JSON.stringify(existing, null, 2));
    "
    echo "Merged MCP config into existing claude.json"
else
    cp /opt/claude.json "$CLAUDE_JSON"
    echo "Created new claude.json with MCP config"
fi

cp "$CLAUDE_JSON" "$HOME/.claude/.claude.json.bak"

cp /opt/CLAUDE.md $HOME/.claude/CLAUDE.md
cp /opt/settings.json $HOME/.claude/settings.json
echo "System prompt and settings configured"

if [ -d /opt/skills ]; then
    rm -rf $HOME/.claude/skills
    cp -r /opt/skills $HOME/.claude/skills
    echo "Skills synced from /opt/skills"
fi

if [ ! -d "$HOME/.git" ]; then
    git init "$HOME" > /dev/null 2>&1
    echo "Initialized git repo to skip trust prompt"
fi

echo "Starting Claude Code with SpaceChannel..."
exec /auto-confirm-claude.exp
