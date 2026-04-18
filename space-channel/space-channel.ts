#!/usr/bin/env bun
import { Server } from "@modelcontextprotocol/sdk/server/index.js";
import { StdioServerTransport } from "@modelcontextprotocol/sdk/server/stdio.js";
import {
  ListToolsRequestSchema,
  CallToolRequestSchema,
} from "@modelcontextprotocol/sdk/types.js";
import * as z from "zod/v4";
import { appendFileSync, mkdirSync, writeFileSync, readdirSync, statSync, unlinkSync } from "fs";
import { hostname, homedir } from "os";
import { join } from "path";
import { randomUUID } from "crypto";
import { DbConnection } from "./generated";
import type { Message, MessageImage, PermissionRequest } from "./generated/types";

const args = parseArgs();
const HOST = hostname().split(".")[0] ?? "unknown";
const CLIENT_ID = randomUUID();
const SESSION_ID = `${args.session}@${HOST}`;
const HEARTBEAT_MS = 20_000;
const LOG_FILE = `/tmp/space-channel-${args.session}.log`;
const INBOX_DIR = join(homedir(), ".claude", "channels", "space-channel", "inbox");
const INBOX_TTL_MS = 48 * 60 * 60 * 1000;

let conn: DbConnection | null = null;
let heartbeatTimer: ReturnType<typeof setInterval> | null = null;
const pendingPermissions = new Map<string, (behavior: string) => void>();
const pendingImages = new Map<string, MessageImage>();
const pendingMessages = new Map<string, Message>();

const mcp = new Server(
  { name: "space-channel", version: "0.2.0" },
  {
    capabilities: {
      experimental: { "claude/channel": {}, "claude/channel/permission": {} },
      tools: {},
    },
    instructions: [
      "The user reads the SpaceNotes app, not this terminal session.",
      "Anything you want them to see MUST go through the reply tool — your transcript output never reaches the app.",
      'Messages from the user arrive as <channel source="space-channel" ...>.',
      "If the tag has a file_path attribute, Read that file — it is an image from the user.",
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
  if (!conn) {
    return { content: [{ type: "text" as const, text: "FAILED: not connected to SpacetimeDB" }], isError: true };
  }

  if (req.params.name === "reply") {
    const { text } = req.params.arguments as { text: string };
    const id = `reply-${Date.now()}`;
    try {
      await conn.reducers.pushMessage({ id, sessionId: SESSION_ID, role: "assistant", text, source: "mcp" });
      conn.reducers.pushStatus({ sessionId: SESSION_ID, state: "idle" });
      return { content: [{ type: "text" as const, text: `sent (id: ${id})` }] };
    } catch (e) {
      return { content: [{ type: "text" as const, text: `FAILED: ${e}` }], isError: true };
    }
  }

  if (req.params.name === "edit_message") {
    const { id, text } = req.params.arguments as { id: string; text: string };
    try {
      await conn.reducers.editMessage({ id, text });
      return { content: [{ type: "text" as const, text: "edited" }] };
    } catch (e) {
      return { content: [{ type: "text" as const, text: `FAILED: ${e}` }], isError: true };
    }
  }

  throw new Error(`unknown tool: ${req.params.name}`);
});

const PermissionRequestSchema = z.object({
  method: z.literal("notifications/claude/channel/permission_request"),
  params: z.object({
    request_id: z.string().optional(),
    tool_name: z.string().optional(),
    description: z.string().optional(),
    input_preview: z.string().optional(),
  }).optional(),
});

mcp.setNotificationHandler(PermissionRequestSchema, async (notification) => {
  if (!conn) return;
  const params = notification.params || {};
  const requestId = params.request_id || randomUUID();
  const input = JSON.stringify({
    description: params.description,
    input_preview: params.input_preview,
  });

  try {
    await conn.reducers.requestPermission({
      id: requestId,
      sessionId: SESSION_ID,
      tool: params.tool_name || "unknown",
      input,
    });
  } catch (e) {
    log(`requestPermission failed: ${e}`);
  }
});

await mcp.connect(new StdioServerTransport());

sweepInbox();
connectToStdb();
startHookServer();

function parseArgs() {
  const stdbUri = getArg("--stdb-uri") || process.env.SPACE_CHANNEL_STDB_URI || "ws://127.0.0.1:5050";
  const stdbDb = getArg("--stdb-db") || process.env.SPACE_CHANNEL_STDB_DB || "spacenotes";
  const session = getArg("--session") || process.env.SPACE_CHANNEL_SESSION || `session-${Date.now()}`;
  const hookPort = parseInt(getArg("--hook-port") ?? process.env.SPACE_CHANNEL_HOOK_PORT ?? "0", 10);
  return { stdbUri, stdbDb, session, hookPort };
}

function getArg(name: string): string | undefined {
  const idx = process.argv.indexOf(name);
  if (idx !== -1 && idx + 1 < process.argv.length) {
    return process.argv[idx + 1];
  }
  return undefined;
}

function connectToStdb() {
  DbConnection.builder()
    .withUri(args.stdbUri)
    .withDatabaseName(args.stdbDb)
    .withCompression("none")
    .onConnect(async (connection, identity, _token) => {
      conn = connection;
      log(`Connected to SpacetimeDB ${args.stdbUri}/${args.stdbDb} as ${identity.toHexString().slice(0, 12)}…`);

      try {
        await conn.reducers.registerSession({
          id: SESSION_ID,
          baseName: args.session,
          host: HOST,
          clientId: CLIENT_ID,
        });
        log(`Registered session ${SESSION_ID}`);
      } catch (e) {
        log(`registerSession failed: ${e}`);
        process.exit(1);
      }

      conn.subscriptionBuilder()
        .onApplied(() => log("Subscriptions applied"))
        .onError((_ctx) => log("Subscription error"))
        .subscribe([
          `SELECT * FROM permission_request WHERE session_id = '${SESSION_ID}'`,
          `SELECT * FROM message WHERE session_id = '${SESSION_ID}' AND role = 'user' AND source = 'flutter'`,
          `SELECT message_image.* FROM message_image JOIN message ON message.id = message_image.message_id WHERE message.session_id = '${SESSION_ID}' AND message.role = 'user' AND message.source = 'flutter'`,
        ]);

      conn.db.permission_request.onUpdate((_ctx, _oldRow, newRow) => {
        handlePermissionUpdate(newRow);
      });

      conn.db.permission_request.onInsert((ctx, row) => {
        if ((ctx as any).event?.tag === "SubscribeApplied") return;
        handlePermissionUpdate(row);
      });

      conn.db.message.onInsert((ctx, row) => {
        if ((ctx as any).event?.tag === "SubscribeApplied") return;
        handleIncomingMessage(row);
      });

      conn.db.message_image.onInsert((ctx, row) => {
        if ((ctx as any).event?.tag === "SubscribeApplied") return;
        handleIncomingImage(row);
      });

      heartbeatTimer = setInterval(async () => {
        try {
          await conn?.reducers.heartbeat({ sessionId: SESSION_ID });
        } catch (e) {
          log(`heartbeat failed: ${e}`);
        }
      }, HEARTBEAT_MS);
    })
    .onConnectError((_ctx, err) => {
      log(`SpacetimeDB connect error: ${err.message}`);
      process.exit(1);
    })
    .onDisconnect((_ctx, err) => {
      log(`SpacetimeDB disconnected: ${err?.message || "clean"}`);
      if (heartbeatTimer) clearInterval(heartbeatTimer);
      process.exit(1);
    })
    .build();
}

function handleIncomingMessage(row: Message) {
  if (row.role !== "user" || row.source !== "flutter") return;

  const image = pendingImages.get(row.id);
  if (image) {
    pendingImages.delete(row.id);
    emitMessage(row, image);
    return;
  }

  pendingMessages.set(row.id, row);
  setTimeout(() => {
    const buffered = pendingMessages.get(row.id);
    if (!buffered) return;
    pendingMessages.delete(row.id);
    emitMessage(buffered);
  }, 500);
}

function handleIncomingImage(row: MessageImage) {
  const message = pendingMessages.get(row.messageId);
  if (message) {
    pendingMessages.delete(row.messageId);
    emitMessage(message, row);
    return;
  }
  pendingImages.set(row.messageId, row);
  setTimeout(() => {
    pendingImages.delete(row.messageId);
  }, 5000);
}

function emitMessage(message: Message, image?: MessageImage) {
  const meta: Record<string, string> = {
    chat_id: "flutter",
    message_id: message.id,
    user: "flutter",
    ts: new Date().toISOString(),
  };

  if (image) {
    try {
      mkdirSync(INBOX_DIR, { recursive: true });
      const filePath = join(INBOX_DIR, `${message.id}.png`);
      writeFileSync(filePath, Buffer.from(image.bytes));
      meta.file_path = filePath;
    } catch (e) {
      log(`inbox write failed for ${message.id}: ${e}`);
    }
  }

  const content = message.text.trim().length > 0
    ? message.text
    : image
      ? "(image)"
      : message.text;

  mcp.notification({
    method: "notifications/claude/channel",
    params: { content, meta },
  }).catch((e) => log(`channel notification failed: ${e}`));
}

function sweepInbox() {
  try {
    const entries = readdirSync(INBOX_DIR);
    const cutoff = Date.now() - INBOX_TTL_MS;
    for (const name of entries) {
      const path = join(INBOX_DIR, name);
      try {
        const st = statSync(path);
        if (st.mtimeMs < cutoff) unlinkSync(path);
      } catch {}
    }
  } catch {}
}

function handlePermissionUpdate(row: PermissionRequest) {
  if (row.status === "pending") return;
  const pending = pendingPermissions.get(row.id);
  if (pending) {
    pending(row.status);
    pendingPermissions.delete(row.id);
  }

  mcp.notification({
    method: "notifications/claude/channel/permission",
    params: {
      request_id: row.id,
      behavior: row.status === "approved" ? "allow" : "deny",
    },
  }).catch((e) => log(`permission notification failed: ${e}`));
}

function startHookServer() {
  const server = Bun.serve({
    port: args.hookPort || 0,
    hostname: "127.0.0.1",
    async fetch(req) {
      if (req.method !== "POST") {
        return new Response("method not allowed", { status: 405 });
      }

      const body = (await req.json().catch(() => null)) as Record<string, any> | null;
      if (!body) {
        return new Response("invalid json", { status: 400 });
      }
      if (!conn) {
        return new Response("not connected", { status: 503 });
      }

      const hookEvent: string = body.hook_event_name || body.hook_event || "unknown";
      log(`Hook: ${hookEvent}`);

      try {
        if (hookEvent === "UserPromptSubmit") {
          await conn.reducers.pushStatus({ sessionId: SESSION_ID, state: "thinking" });
        } else if (hookEvent === "Stop") {
          const message: string | undefined = body.last_assistant_message;
          if (message) {
            await conn.reducers.pushMessage({
              id: `hook-${Date.now()}`,
              sessionId: SESSION_ID,
              role: "assistant",
              text: message,
              source: "hook",
            });
          }
          await conn.reducers.pushStatus({ sessionId: SESSION_ID, state: "idle" });
        } else if (hookEvent === "PreToolUse") {
          const toolName: string = body.tool_name ?? "unknown";
          const detail = JSON.stringify({
            tool: toolName,
            input: body.tool_input ?? {},
          });
          await conn.reducers.pushToolEvent({
            id: `tool-${Date.now()}-${randomUUID().slice(0, 8)}`,
            sessionId: SESSION_ID,
            tool: toolName,
            detail,
          });
          const isOwnReplyTool =
            toolName === "mcp__space-channel__reply" ||
            toolName === "mcp__space-channel__edit_message";
          if (!isOwnReplyTool) {
            await conn.reducers.pushStatus({ sessionId: SESSION_ID, state: "tool_use" });
          }
        } else if (hookEvent === "PostToolUse" || hookEvent === "PostToolUseFailure") {
          const toolName: string = body.tool_name ?? "";
          const isOwnReplyTool =
            toolName === "mcp__space-channel__reply" ||
            toolName === "mcp__space-channel__edit_message";
          await conn.reducers.pushStatus({
            sessionId: SESSION_ID,
            state: isOwnReplyTool ? "idle" : "thinking",
          });
        } else if (hookEvent === "SessionEnd") {
          await conn.reducers.endSession({ sessionId: SESSION_ID });
        }
      } catch (e) {
        log(`Hook reducer failed (${hookEvent}): ${e}`);
      }

      return new Response(null, { status: 200 });
    },
  });
  log(`Hook HTTP server listening on http://127.0.0.1:${server.port}`);
}

process.on("SIGTERM", () => { log("SIGTERM"); shutdown(); });
process.on("SIGINT", () => { log("SIGINT"); shutdown(); });
process.on("uncaughtException", (err) => { log(`Uncaught: ${err.message}\n${err.stack}`); shutdown(1); });

let shuttingDown = false;
async function shutdown(code = 0) {
  if (shuttingDown) return;
  shuttingDown = true;
  if (heartbeatTimer) clearInterval(heartbeatTimer);
  if (conn) {
    try {
      await conn.reducers.endSession({ sessionId: SESSION_ID });
      log(`Session ended: ${SESSION_ID}`);
    } catch (e) {
      log(`endSession on shutdown failed: ${e}`);
    }
    try { conn.disconnect(); } catch {}
  }
  process.exit(code);
}

function log(msg: string) {
  const line = `[${new Date().toISOString()}] ${msg}`;
  process.stderr.write(`[space-channel] ${msg}\n`);
  try { appendFileSync(LOG_FILE, line + "\n"); } catch {}
}
