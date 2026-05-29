FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y \
    curl \
    unzip \
    git \
    ca-certificates \
    expect \
    && rm -rf /var/lib/apt/lists/*

RUN curl -fsSL https://deb.nodesource.com/setup_22.x | bash - \
    && apt-get install -y nodejs \
    && rm -rf /var/lib/apt/lists/*

RUN npm install -g @anthropic-ai/claude-code

RUN useradd -m -s /bin/bash claude
RUN mkdir -p /home/claude/.claude && chown -R claude:claude /home/claude

COPY claude.json /opt/claude.json
COPY CLAUDE.md /opt/CLAUDE.md
COPY skills /opt/skills
COPY entrypoint.sh /entrypoint.sh
RUN chmod +x /entrypoint.sh

COPY auto-confirm-claude.exp /auto-confirm-claude.exp
RUN chmod +x /auto-confirm-claude.exp

COPY space-channel/space-channel-linux-x64 /home/claude/space-channel/space-channel
COPY settings.json /opt/settings.json
RUN chmod +x /home/claude/space-channel/space-channel \
    && chown -R claude:claude /home/claude/space-channel /home/claude/.claude

USER claude
WORKDIR /home/claude

VOLUME /home/claude/.claude

ENTRYPOINT ["/entrypoint.sh"]
