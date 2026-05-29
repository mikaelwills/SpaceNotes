#!/usr/bin/env bun
import { Server } from "@modelcontextprotocol/sdk/server/index.js";
import { StdioServerTransport } from "@modelcontextprotocol/sdk/server/stdio.js";
import {
  ListToolsRequestSchema,
  CallToolRequestSchema,
} from "@modelcontextprotocol/sdk/types.js";
import * as z from "zod/v4";
import {
  appendFileSync,
  mkdirSync,
  writeFileSync,
  readdirSync,
  readFileSync,
  existsSync,
  statSync,
  unlinkSync,
} from "fs";
import { hostname, homedir } from "os";
import { join } from "path";
import { randomUUID } from "crypto";
import { spawn, spawnSync } from "child_process";
import { createServer } from "net";
import { DbConnection } from "./generated";
import type { Message, MessageImage, PermissionRequest, QuestionRequest } from "./generated/types";

const subcommand = process.argv[2];
if (subcommand === "hook-post") {
  await runHookPost();
  process.exit(0);
}
if (subcommand === "launch") {
  await runLaunch(process.argv.slice(3));
  process.exit(0);
}

const args = parseArgs();
const RAW_HOST = hostname().split(".")[0] ?? "unknown";
const HOST_ALIASES: Record<string, string> = {
  "mikael-NUC10i3FNK": "robert",
};
const HOST = HOST_ALIASES[RAW_HOST] ?? RAW_HOST;
const CLIENT_ID = randomUUID();
const SESSION_ID = `${args.session}@${HOST}`;
const HEARTBEAT_MS = 20_000;
const TOOL_USE_STUCK_MS = 30_000;
const LOG_FILE = `/tmp/space-channel-${args.session}.log`;
const INBOX_DIR = join(homedir(), ".claude", "channels", "space-channel", "inbox");
const INBOX_TTL_MS = 48 * 60 * 60 * 1000;

let conn: DbConnection | null = null;
let heartbeatTimer: ReturnType<typeof setInterval> | null = null;
let reconnectTimer: ReturnType<typeof setTimeout> | null = null;
let reconnectAttempt = 0;
let hasEverConnected = false;
let shuttingDown = false;
const RECONNECT_BASE_MS = 1_000;
const RECONNECT_MAX_MS = 60_000;
const pendingPermissions = new Map<string, (behavior: string) => void>();
const pendingQuestions = new Map<string, (response: string | null) => void>();
const QUESTION_TIMEOUT_MS = 10 * 60 * 1000;
const pendingImages = new Map<string, MessageImage>();
const pendingMessages = new Map<string, Message>();
const openToolCalls = new Map<string, { tool: string; startedAt: number }>();
let lastKnownState: string = "idle";

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
      lastKnownState = "idle";
      await conn.reducers.pushStatus({ sessionId: SESSION_ID, state: "idle" });
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
  if (shuttingDown) return;
  if (reconnectTimer) {
    clearTimeout(reconnectTimer);
    reconnectTimer = null;
  }

  DbConnection.builder()
    .withUri(args.stdbUri)
    .withDatabaseName(args.stdbDb)
    .withCompression("none")
    .onConnect(async (connection, identity, _token) => {
      conn = connection;
      hasEverConnected = true;
      reconnectAttempt = 0;
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
        return;
      }

      conn.subscriptionBuilder()
        .onApplied(() => log("Subscriptions applied"))
        .onError((_ctx) => log("Subscription error"))
        .subscribe([
          `SELECT * FROM permission_request WHERE session_id = '${SESSION_ID}'`,
          `SELECT * FROM question_request WHERE session_id = '${SESSION_ID}'`,
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

      conn.db.question_request.onUpdate((_ctx, _oldRow, newRow) => {
        handleQuestionUpdate(newRow);
      });

      conn.db.question_request.onInsert((ctx, row) => {
        if ((ctx as any).event?.tag === "SubscribeApplied") return;
        handleQuestionUpdate(row);
      });

      conn.db.message.onInsert((ctx, row) => {
        const tag = (ctx as any).event?.tag;
        log(`message.onInsert id=${row.id} session_id=${row.sessionId} role=${row.role} source=${row.source} event_tag=${tag}`);
        if (tag === "SubscribeApplied") return;
        handleIncomingMessage(row);
      });

      conn.db.message_image.onInsert((ctx, row) => {
        if ((ctx as any).event?.tag === "SubscribeApplied") return;
        handleIncomingImage(row);
      });

      let heartbeatCount = 0;
      heartbeatTimer = setInterval(async () => {
        try {
          await conn?.reducers.heartbeat({ sessionId: SESSION_ID });
          heartbeatCount++;
          if (heartbeatCount === 1 || heartbeatCount % 15 === 0) {
            log(`heartbeat ok (count=${heartbeatCount})`);
          }
        } catch (e) {
          log(`heartbeat failed: ${e}`);
        }
        await sweepStaleToolCalls();
      }, HEARTBEAT_MS);
    })
    .onConnectError((_ctx, err) => {
      log(`SpacetimeDB connect error: ${err.message}`);
      scheduleReconnect();
    })
    .onDisconnect((_ctx, err) => {
      log(`SpacetimeDB disconnected: ${err?.message || "clean"}`);
      conn = null;
      if (heartbeatTimer) {
        clearInterval(heartbeatTimer);
        heartbeatTimer = null;
      }
      scheduleReconnect();
    })
    .build();
}

function scheduleReconnect() {
  if (shuttingDown) return;
  if (reconnectTimer) return;
  const delay = Math.min(RECONNECT_BASE_MS * 2 ** reconnectAttempt, RECONNECT_MAX_MS);
  reconnectAttempt++;
  log(`Reconnecting in ${delay}ms (attempt ${reconnectAttempt}${hasEverConnected ? "" : ", initial"})`);
  reconnectTimer = setTimeout(() => {
    reconnectTimer = null;
    connectToStdb();
  }, delay);
}

function handleIncomingMessage(row: Message) {
  log(`handleIncomingMessage entered id=${row.id} role=${row.role} source=${row.source}`);
  if (row.role !== "user" || row.source !== "flutter") {
    log(`handleIncomingMessage skipping id=${row.id} (role/source filter)`);
    return;
  }

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

  log(`emitMessage sending mcp notification message_id=${message.id} content_len=${content.length}`);
  mcp.notification({
    method: "notifications/claude/channel",
    params: { content, meta },
  })
    .then(() => log(`mcp notification sent ok message_id=${message.id}`))
    .catch((e) => log(`channel notification failed: ${e}`));
}

async function sweepStaleToolCalls() {
  if (!conn) return;
  if (openToolCalls.size === 0) return;
  const cutoff = Date.now() - TOOL_USE_STUCK_MS;
  let sweptAny = false;
  for (const [id, entry] of openToolCalls) {
    if (entry.startedAt < cutoff) {
      log(`Stale PreToolUse swept: ${entry.tool} (age ${Date.now() - entry.startedAt}ms, id ${id})`);
      openToolCalls.delete(id);
      sweptAny = true;
    }
  }
  if (sweptAny && openToolCalls.size === 0 && lastKnownState === "tool_use") {
    try {
      lastKnownState = "thinking";
      await conn.reducers.pushStatus({ sessionId: SESSION_ID, state: "thinking" });
      log("Watchdog cleared stuck tool_use → thinking");
    } catch (e) {
      log(`Watchdog state reset failed: ${e}`);
    }
  }
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

function handleQuestionUpdate(row: QuestionRequest) {
  if (row.status === "pending") return;
  const pending = pendingQuestions.get(row.id);
  if (!pending) return;
  pendingQuestions.delete(row.id);
  pending(row.response ?? null);
}

function readContextUsage(
  transcriptPath: unknown,
  model: unknown,
): { used: number; window: number } | null {
  if (typeof transcriptPath !== "string" || !transcriptPath) return null;
  let modelId = "";
  if (model && typeof model === "object" && "id" in (model as any)) {
    modelId = String((model as any).id ?? "");
  } else if (typeof model === "string") {
    modelId = model;
  }
  const window = modelId.endsWith("[1m]") ? 1_000_000 : 200_000;
  let raw: string;
  try {
    raw = readFileSync(transcriptPath, "utf8");
  } catch (e) {
    log(`readContextUsage: open failed (${transcriptPath}): ${e}`);
    return null;
  }
  let used = 0;
  for (const line of raw.split("\n")) {
    if (!line) continue;
    let parsed: any;
    try {
      parsed = JSON.parse(line);
    } catch {
      continue;
    }
    const u = parsed?.message?.usage;
    if (u) {
      used =
        (u.input_tokens ?? 0) +
        (u.cache_read_input_tokens ?? 0) +
        (u.cache_creation_input_tokens ?? 0);
    }
  }
  if (used <= 0) return null;
  return { used, window };
}

type AskQuestion = {
  question?: string;
  header?: string;
  options?: Array<{ label?: string; description?: string }>;
  multiSelect?: boolean;
};

// Surfaces each question as a question_request row, waits for the Flutter answer
// (or QUESTION_TIMEOUT_MS), and returns the hook stdout payload that injects the
// answers. On timeout/no-answer, returns {} so Claude Code falls back to its own
// terminal prompt (no worse than today).
async function handleAskUserQuestion(
  toolInput: unknown,
): Promise<Record<string, unknown>> {
  const input = (toolInput ?? {}) as { questions?: AskQuestion[] };
  const questions = Array.isArray(input.questions) ? input.questions : [];
  if (questions.length === 0 || !conn) return {};

  const answers: Record<string, string> = {};
  let answeredAny = false;

  for (const q of questions) {
    const questionText = q.question ?? "";
    if (!questionText) continue;
    const labels = (q.options ?? [])
      .map((o) => o.label)
      .filter((l): l is string => typeof l === "string" && l.length > 0);

    const id = `question-${Date.now()}-${randomUUID().slice(0, 8)}`;
    try {
      await conn.reducers.requestQuestion({
        id,
        sessionId: SESSION_ID,
        question: questionText,
        header: q.header ?? "",
        options: JSON.stringify(labels),
        multiSelect: q.multiSelect === true,
      });
    } catch (e) {
      log(`requestQuestion failed: ${e}`);
      continue;
    }

    const response = await new Promise<string | null>((resolve) => {
      const timer = setTimeout(() => {
        pendingQuestions.delete(id);
        log(`Question ${id} timed out after ${QUESTION_TIMEOUT_MS}ms`);
        resolve(null);
      }, QUESTION_TIMEOUT_MS);
      pendingQuestions.set(id, (r) => {
        clearTimeout(timer);
        resolve(r);
      });
    });

    if (response === null) continue;

    // response is JSON: a string label, or an array of labels for multiSelect.
    let selected: string;
    try {
      const parsed = JSON.parse(response);
      selected = Array.isArray(parsed) ? parsed.join(", ") : String(parsed);
    } catch {
      selected = response;
    }
    answers[questionText] = selected;
    answeredAny = true;
  }

  if (!answeredAny) return {};

  return {
    hookSpecificOutput: {
      hookEventName: "PreToolUse",
      permissionDecision: "allow",
      updatedToolInput: { questions, answers },
    },
  };
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

      // AskUserQuestion: surface the question in Flutter and BLOCK until answered.
      // The answer is returned to the hook process, which injects it via stdout.
      if (hookEvent === "PreToolUse" && body.tool_name === "AskUserQuestion") {
        const out = await handleAskUserQuestion(body.tool_input);
        return Response.json(out);
      }

      try {
        if (hookEvent === "UserPromptSubmit") {
          lastKnownState = "thinking";
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
          const usage = readContextUsage(body.transcript_path, body.model);
          if (usage) {
            try {
              await conn.reducers.pushContextUsage({
                sessionId: SESSION_ID,
                used: BigInt(usage.used),
                window: BigInt(usage.window),
              });
            } catch (e) {
              log(`pushContextUsage failed: ${e}`);
            }
          }
          openToolCalls.clear();
          lastKnownState = "idle";
          await conn.reducers.pushStatus({ sessionId: SESSION_ID, state: "idle" });
        } else if (hookEvent === "PreToolUse") {
          const toolName: string = body.tool_name ?? "unknown";
          const toolEventId = `tool-${Date.now()}-${randomUUID().slice(0, 8)}`;
          const detail = JSON.stringify({
            tool: toolName,
            input: body.tool_input ?? {},
          });
          await conn.reducers.pushToolEvent({
            id: toolEventId,
            sessionId: SESSION_ID,
            tool: toolName,
            detail,
          });
          const isOwnReplyTool =
            toolName === "mcp__space-channel__reply" ||
            toolName === "mcp__space-channel__edit_message";
          if (!isOwnReplyTool) {
            openToolCalls.set(toolEventId, { tool: toolName, startedAt: Date.now() });
            lastKnownState = "tool_use";
            await conn.reducers.pushStatus({ sessionId: SESSION_ID, state: "tool_use" });
          }
        } else if (hookEvent === "PostToolUse" || hookEvent === "PostToolUseFailure") {
          const toolName: string = body.tool_name ?? "";
          const isOwnReplyTool =
            toolName === "mcp__space-channel__reply" ||
            toolName === "mcp__space-channel__edit_message";
          if (!isOwnReplyTool) {
            for (const [id, entry] of openToolCalls) {
              if (entry.tool === toolName) {
                openToolCalls.delete(id);
                break;
              }
            }
          }
          const nextState = isOwnReplyTool ? "idle" : "thinking";
          lastKnownState = nextState;
          await conn.reducers.pushStatus({
            sessionId: SESSION_ID,
            state: nextState,
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

async function shutdown(code = 0) {
  if (shuttingDown) return;
  shuttingDown = true;
  if (heartbeatTimer) clearInterval(heartbeatTimer);
  if (reconnectTimer) clearTimeout(reconnectTimer);
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

async function runHookPost() {
  const argv = process.argv.slice(3);
  let port: string | undefined;
  for (let i = 0; i < argv.length; i++) {
    if (argv[i] === "--port" && i + 1 < argv.length) {
      port = argv[i + 1];
      break;
    }
  }
  port ??= process.env.SPACE_CHANNEL_HOOK_PORT;
  if (!port) return;

  const chunks: Buffer[] = [];
  for await (const chunk of process.stdin) {
    chunks.push(chunk as Buffer);
  }
  const body = Buffer.concat(chunks);
  const url = `http://127.0.0.1:${port}/hook`;

  // AskUserQuestion blocks server-side until the Flutter user answers (or the
  // 10-min question timeout). Wait for that response and forward its body to
  // stdout so Claude Code injects the answer. Empty/error → print nothing, which
  // lets Claude fall back to its own terminal prompt.
  let parsed: any = null;
  try {
    parsed = JSON.parse(body.toString("utf8"));
  } catch {}
  const isAskUserQuestion =
    parsed?.hook_event_name === "PreToolUse" &&
    parsed?.tool_name === "AskUserQuestion";

  if (isAskUserQuestion) {
    try {
      const res = await fetch(url, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body,
        signal: AbortSignal.timeout(11 * 60 * 1000),
      });
      if (res.ok) {
        const text = (await res.text()).trim();
        if (text && text !== "{}") process.stdout.write(text);
      }
    } catch {}
    return;
  }

  const deadline = Date.now() + 3000;
  let attempt = 0;
  while (Date.now() < deadline && attempt < 10) {
    try {
      const res = await fetch(url, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body,
        signal: AbortSignal.timeout(1000),
      });
      if (res.ok) return;
    } catch {}
    attempt++;
  }
}

async function runLaunch(rawArgs: string[]) {
  if (rawArgs.length < 2) {
    process.stderr.write("usage: space-channel launch <session> <skill> [args...]\n");
    process.exit(2);
  }
  const session = rawArgs[0]!;
  const skill = rawArgs[1]!;
  const rest = rawArgs.slice(2);

  if (!commandExists("claude")) {
    process.stderr.write("claude CLI not found in PATH\n");
    process.exit(1);
  }

  const selfPath: string = process.execPath.endsWith("/bun")
    ? (process.argv[1] ?? process.execPath)
    : process.execPath;

  const lockDir = ".claude/.spacechannel-sessions";
  mkdirSync(lockDir, { recursive: true });
  const lockFile = join(lockDir, String(process.pid));
  writeFileSync(lockFile, "");

  const hookPort = await findFreePort();
  const hookCmd = [selfPath, "hook-post", "--port", String(hookPort)];
  const hookEntry = [{ hooks: [{ type: "command", command: hookCmd.join(" ") }] }];
  const hooksObj = {
    SessionStart: hookEntry,
    PreToolUse: hookEntry,
    PostToolUse: hookEntry,
    PostToolUseFailure: hookEntry,
    UserPromptSubmit: hookEntry,
    Stop: hookEntry,
    SessionEnd: hookEntry,
    Notification: hookEntry,
  };

  const settingsFile = ".claude/settings.local.json";
  mkdirSync(".claude", { recursive: true });
  let settings: any = {};
  if (existsSync(settingsFile)) {
    try {
      settings = JSON.parse(readFileSync(settingsFile, "utf8"));
    } catch {
      settings = {};
    }
  }
  settings.hooks = hooksObj;
  writeFileSync(settingsFile + ".tmp", JSON.stringify(settings, null, 2));
  spawnSync("mv", [settingsFile + ".tmp", settingsFile], { stdio: "inherit" });

  const cleanup = () => {
    try { unlinkSync(lockFile); } catch {}
    if (countActiveSessions(lockDir) === 0) {
      if (existsSync(settingsFile)) {
        try {
          const s = JSON.parse(readFileSync(settingsFile, "utf8"));
          delete s.hooks;
          writeFileSync(settingsFile, JSON.stringify(s, null, 2));
        } catch {}
      }
      spawnSync("claude", ["mcp", "remove", "space-channel", "--scope", "project"], {
        stdio: "ignore",
      });
    }
  };
  process.on("exit", cleanup);
  process.on("SIGTERM", () => { cleanup(); process.exit(143); });
  process.on("SIGINT", () => { cleanup(); process.exit(130); });

  spawnSync("claude", ["mcp", "remove", "space-channel", "--scope", "project"], {
    stdio: "ignore",
  });

  const stdbUri = process.env.SPACE_CHANNEL_STDB_URI || "ws://100.84.184.121:5050";
  const stdbDb = process.env.SPACE_CHANNEL_STDB_DB || "spacenotes";
  const mcpAdd = spawnSync(
    "claude",
    [
      "mcp", "add", "space-channel", "--scope", "project",
      "-e", `SPACE_CHANNEL_SESSION=${session}`,
      "-e", `SPACE_CHANNEL_PROJECT=${session}`,
      "-e", `SPACE_CHANNEL_STDB_URI=${stdbUri}`,
      "-e", `SPACE_CHANNEL_STDB_DB=${stdbDb}`,
      "-e", `SPACE_CHANNEL_HOOK_PORT=${hookPort}`,
      "--", selfPath,
    ],
    { stdio: "ignore" }
  );
  if (mcpAdd.status !== 0) {
    process.stderr.write("space-channel setup FAILED\n");
    process.exit(1);
  }
  process.stderr.write(`space-channel ready (session: ${session}, hook: ${hookPort})\n`);

  const claude = spawn(
    "claude",
    [
      "--dangerously-load-development-channels", "server:space-channel",
      "--dangerously-skip-permissions",
      `/${skill}`,
      ...rest,
    ],
    { stdio: "inherit" }
  );
  const code: number = await new Promise((resolve) => {
    claude.on("exit", (c) => resolve(c ?? 0));
  });
  process.exit(code);
}

function countActiveSessions(lockDir: string): number {
  let count = 0;
  try {
    const entries = readdirSync(lockDir);
    for (const name of entries) {
      const file = join(lockDir, name);
      try {
        const pid = parseInt(name, 10);
        if (Number.isFinite(pid)) {
          try {
            process.kill(pid, 0);
            count++;
          } catch {
            unlinkSync(file);
          }
        }
      } catch {}
    }
  } catch {}
  return count;
}

function commandExists(cmd: string): boolean {
  const r = spawnSync("command", ["-v", cmd], { stdio: "ignore", shell: true });
  return r.status === 0;
}

async function findFreePort(): Promise<number> {
  return new Promise((resolve, reject) => {
    const srv = createServer();
    srv.unref();
    srv.on("error", reject);
    srv.listen(0, "127.0.0.1", () => {
      const addr = srv.address();
      if (addr && typeof addr === "object") {
        const port = addr.port;
        srv.close(() => resolve(port));
      } else {
        srv.close(() => reject(new Error("no address")));
      }
    });
  });
}
