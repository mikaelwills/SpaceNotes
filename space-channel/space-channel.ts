#!/usr/bin/env bun
import { Server } from "@modelcontextprotocol/sdk/server/index.js";
import { StdioServerTransport } from "@modelcontextprotocol/sdk/server/stdio.js";
import {
  ListToolsRequestSchema,
  CallToolRequestSchema,
} from "@modelcontextprotocol/sdk/types.js";

const args = parseArgs();

let ws: WebSocket | null = null;
let reconnectTimer: ReturnType<typeof setTimeout> | null = null;
const RECONNECT_INTERVAL = 3000;

const mcp = new Server(
  { name: "space-channel", version: "0.1.0" },
  {
    capabilities: {
      experimental: { "claude/channel": {}, "claude/channel/permission": {} },
      tools: {},
    },
    instructions: [
      "The user reads the SpaceNotes app, not this terminal session.",
      "Anything you want them to see MUST go through the reply tool — your transcript output never reaches the app.",
      'Messages from the user arrive as <channel source="space-channel" ...>.',
      "Reply using the reply tool. Use edit_message to update a previous reply by id.",
    ].join(" "),
  }
);

mcp.setRequestHandler(ListToolsRequestSchema, async () => ({
  tools: [
    {
      name: "reply",
      description: "Send a message back to the user via SpaceNotes",
      inputSchema: {
        type: "object" as const,
        properties: {
          text: { type: "string", description: "The message to send" },
        },
        required: ["text"],
      },
    },
    {
      name: "edit_message",
      description: "Edit a previously sent message by id",
      inputSchema: {
        type: "object" as const,
        properties: {
          id: { type: "string", description: "The message id to edit" },
          text: { type: "string", description: "The new message text" },
        },
        required: ["id", "text"],
      },
    },
  ],
}));

mcp.setRequestHandler(CallToolRequestSchema, async (req) => {
  if (req.params.name === "reply") {
    const { text } = req.params.arguments as { text: string };
    const id = `reply-${Date.now()}`;
    sendToServer({
      type: "reply",
      session: args.session,
      id,
      text,
    });
    return { content: [{ type: "text" as const, text: `sent (id: ${id})` }] };
  }

  if (req.params.name === "edit_message") {
    const { id, text } = req.params.arguments as { id: string; text: string };
    sendToServer({
      type: "edit",
      session: args.session,
      id,
      text,
    });
    return { content: [{ type: "text" as const, text: "edited" }] };
  }

  throw new Error(`unknown tool: ${req.params.name}`);
});

mcp.setNotificationHandler(
  { method: "notifications/claude/channel/permission_request" },
  async (notification: any) => {
    const params = notification.params || {};
    sendToServer({
      type: "permission_request",
      session: args.session,
      project: args.project,
      task: args.task,
      request_id: params.request_id,
      tool_name: params.tool_name,
      description: params.description,
      input_preview: params.input_preview,
    });
  }
);

await mcp.connect(new StdioServerTransport());

connectToServer();

await startHookServer();

function parseArgs() {
  const serverUrl =
    getArg("--server") || process.env.SPACE_CHANNEL_SERVER || "ws://127.0.0.1:5055/ws";
  const project = getArg("--project") || process.env.SPACE_CHANNEL_PROJECT || "Unknown";
  const task = getArg("--task") || process.env.SPACE_CHANNEL_TASK || "default";
  const session = getArg("--session") || process.env.SPACE_CHANNEL_SESSION || `session-${Date.now()}`;
  const hookPort = parseInt(getArg("--hook-port") || process.env.SPACE_CHANNEL_HOOK_PORT || "0");
  const isMaster = process.argv.includes("--master") || process.env.SPACE_CHANNEL_MASTER === "true";

  return { serverUrl, project, task, session, hookPort, isMaster };
}

function getArg(name: string): string | undefined {
  const idx = process.argv.indexOf(name);
  if (idx !== -1 && idx + 1 < process.argv.length) {
    return process.argv[idx + 1];
  }
  return undefined;
}

function connectToServer() {
  ws = new WebSocket(args.serverUrl);

  ws.addEventListener("open", () => {
    log(`Connected to SpaceChannelServer at ${args.serverUrl}`);
    sendToServer({
      type: "register",
      session: args.session,
      project: args.project,
      task: args.task,
      is_master: args.isMaster,
    });
    if (reconnectTimer) {
      clearTimeout(reconnectTimer);
      reconnectTimer = null;
    }
  });

  ws.addEventListener("message", async (event) => {
    const data = typeof event.data === "string" ? event.data : event.data.toString();
    let parsed: any;
    try {
      parsed = JSON.parse(data);
    } catch {
      return;
    }

    if (parsed.type === "chat") {
      await mcp.notification({
        method: "notifications/claude/channel",
        params: {
          content: parsed.text,
          meta: {
            chat_id: parsed.chat_id || "flutter",
            message_id: parsed.id || `msg-${Date.now()}`,
            user: "flutter",
            ts: new Date().toISOString(),
          },
        },
      });
    } else if (parsed.type === "webhook") {
      await mcp.notification({
        method: "notifications/claude/channel",
        params: {
          content: parsed.text,
          meta: {
            chat_id: "webhook",
            message_id: `webhook-${Date.now()}`,
            user: parsed.source || "webhook",
            ts: new Date().toISOString(),
            source: parsed.source || "unknown",
          },
        },
      });
    } else if (parsed.type === "permission_response") {
      await mcp.notification({
        method: "notifications/claude/channel/permission",
        params: {
          request_id: parsed.request_id,
          behavior: parsed.behavior,
        },
      });
    } else if (parsed.type === "worker_reply") {
      await mcp.notification({
        method: "notifications/claude/channel",
        params: {
          content: parsed.text,
          meta: {
            chat_id: "worker",
            message_id: `worker-${Date.now()}`,
            user: parsed.session || "worker",
            ts: new Date().toISOString(),
            project: parsed.project || "unknown",
            task: parsed.task || "unknown",
            session: parsed.session || "unknown",
          },
        },
      });
    }
  });

  ws.addEventListener("close", () => {
    log("Disconnected from SpaceChannelServer, reconnecting...");
    scheduleReconnect();
  });

  ws.addEventListener("error", () => {
    log("WebSocket error, will reconnect...");
  });
}

function scheduleReconnect() {
  if (reconnectTimer) return;
  reconnectTimer = setTimeout(() => {
    reconnectTimer = null;
    connectToServer();
  }, RECONNECT_INTERVAL);
}

function sendToServer(msg: Record<string, unknown>) {
  if (ws && ws.readyState === WebSocket.OPEN) {
    ws.send(JSON.stringify(msg));
  }
}

async function startHookServer() {
  const server = Bun.serve({
    port: 0,
    hostname: "127.0.0.1",
    async fetch(req) {
      if (req.method !== "POST") {
        return new Response("method not allowed", { status: 405 });
      }

      const body = await req.json().catch(() => null);
      if (!body) {
        return new Response("invalid json", { status: 400 });
      }

      const toolEvent = {
        type: "tool_event",
        session: args.session,
        project: args.project,
        task: args.task,
        tool: body.tool_name || "unknown",
        input: body.tool_input || {},
        ts: Date.now(),
      };

      sendToServer(toolEvent);

      return new Response("ok");
    },
  });
  log(`Hook HTTP server listening on http://127.0.0.1:${server.port}`);
}

function log(msg: string) {
  process.stderr.write(`[space-channel] ${msg}\n`);
}
