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
  statSync,
  unlinkSync,
  existsSync,
} from "fs";
import { hostname, homedir } from "os";
import { join, dirname, basename } from "path";
import { randomUUID } from "crypto";
import { spawn, spawnSync } from "child_process";
import { DbConnection } from "./generated";
import type { Message, MessageImage, PermissionRequest, QuestionRequest } from "./generated/types";

const BUILD_SHA = process.env.SPACE_CHANNEL_BUILD_SHA ?? "dev";

const subcommand = process.argv[2];
if (subcommand === "version") {
  process.stdout.write(`${BUILD_SHA}\n`);
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
const AGENT_ID = `${args.agent}@${HOST}`;
const FILES_URL = process.env.SPACE_CHANNEL_FILES_URL || filesUrlFor(args.stdbUri);
const HEARTBEAT_MS = 120_000;
const LOG_FILE = `/tmp/space-channel-${args.agent}.log`;
const INBOX_DIR = join(homedir(), ".claude", "channels", "space-channel", "inbox");
const INBOX_TTL_MS = 48 * 60 * 60 * 1000;
const IMAGE_PAIR_WAIT_MS = 500;
const VAULT_FILE_LINK = /spacenotes:\/\/file\/([0-9a-fA-F-]{36})\)/g;
const VAULT_RESOLVE_TIMEOUT_MS = 10_000;
const INBOX_IMAGE_EXTENSIONS = new Set(["jpg", "jpeg", "png", "gif", "webp"]);
const INBOX_SWEEP_MS = 60 * 60 * 1000;
const NEXT_MESSAGE_MAX_WAIT_MS = 25_000;

let conn: DbConnection | null = null;
let heartbeatTimer: ReturnType<typeof setInterval> | null = null;
let reconnectTimer: ReturnType<typeof setTimeout> | null = null;
let reconnectAttempt = 0;
let hasEverConnected = false;
let shuttingDown = false;
const RECONNECT_BASE_MS = 1_000;
const RECONNECT_MAX_MS = 60_000;

type Outcomes<T> = { done: Map<string, T>; waiting: Map<string, (v: T | undefined) => void> };
const permissionOutcomes: Outcomes<string> = { done: new Map(), waiting: new Map() };
const questionOutcomes: Outcomes<string | null> = { done: new Map(), waiting: new Map() };

function settleOutcome<T>(o: Outcomes<T>, id: string, value: T) {
  const waiter = o.waiting.get(id);
  if (waiter) {
    o.waiting.delete(id);
    waiter(value);
    return;
  }
  o.done.set(id, value);
}

function awaitOutcome<T>(o: Outcomes<T>, id: string, timeoutMs: number): Promise<T | undefined> {
  if (o.done.has(id)) {
    const value = o.done.get(id);
    o.done.delete(id);
    return Promise.resolve(value);
  }
  return new Promise((resolve) => {
    const timer = setTimeout(() => {
      o.waiting.delete(id);
      resolve(undefined);
    }, timeoutMs);
    o.waiting.set(id, (v) => {
      clearTimeout(timer);
      resolve(v);
    });
  });
}
const pendingImages = new Map<string, MessageImage>();
const pendingMessages = new Map<string, Message>();

type InboundMessage = {
  id: string;
  text: string;
  source: string;
  sender: string;
  imagePaths?: string[];
};
const inboundQueue: InboundMessage[] = [];
let inboundWaiter: ((m: InboundMessage | null) => void) | null = null;

const A2A_DEFAULT_MAX_HOPS = 4;
const A2A_HOP_DECAY_MS = 5 * 60_000;
const a2aSendTimes: number[] = [];
const a2aLastSendToTarget = new Map<string, number>();
let incomingAgentHop: number | null = null;
let incomingAgentHopAt = 0;

const mcp = new Server(
  { name: "space-channel", version: "0.3.0" },
  {
    capabilities: { tools: {} },
    instructions: [
      "Your user talks to you from the SpaceNotes app as well as this terminal; everything you write reaches both, so answer in plain text — there is no reply tool.",
      "Use send_to_agent to message another agent's channel directly; it arrives in their session like a user message, attributed to you.",
      "The other space-channel tools are internal plumbing for the session; never call them yourself.",
      "To point the user at a vault file, link it as markdown: [name](spacenotes://file/<id>) using the file id any spacenotes-mcp read returns; for a folder use [name](spacenotes://folder/<url-encoded path>). Tapping the link opens it in the app.",
      "Put each vault link alone on its own line, with no caption or other text sharing that line, so the app renders it as a card with a thumbnail; a link inside a sentence renders as plain text.",
    ].join(" "),
  }
);

function describeZodError(error: z.ZodError): string {
  return error.issues
    .map((i) => {
      const path = i.path.join(".");
      return path ? `${path}: ${i.message}` : i.message;
    })
    .join("; ");
}

const SendToAgentArgs = z.object({ agent: z.string().min(1), text: z.string().min(1) }).strict();
const PushMessageArgs = z
  .object({
    role: z.enum(["user", "assistant"]),
    text: z.string().min(1),
    source: z.string().min(1).default("mod"),
    id: z.string().min(1).optional(),
  })
  .strict();
const EditMessageArgs = z.object({ id: z.string().min(1), text: z.string().min(1) }).strict();
const PushStatusArgs = z.object({ state: z.enum(["idle", "thinking", "tool_use"]) }).strict();
const PushToolEventArgs = z.object({ tool: z.string().min(1), detail: z.string() }).strict();
const PushContextUsageArgs = z
  .object({ used: z.number().int().nonnegative(), window: z.number().int().positive() })
  .strict();
const NextMessageArgs = z
  .object({ timeoutMs: z.number().int().min(0).max(NEXT_MESSAGE_MAX_WAIT_MS).default(20_000) })
  .strict();
const RequestPermissionArgs = z
  .object({ id: z.string().min(1), tool: z.string().min(1), input: z.string() })
  .strict();
const PollArgs = z
  .object({ id: z.string().min(1), timeoutMs: z.number().int().min(0).max(NEXT_MESSAGE_MAX_WAIT_MS).default(20_000) })
  .strict();
const RequestQuestionArgs = z
  .object({
    questions: z.array(
      z.object({
        question: z.string().min(1),
        header: z.string().default(""),
        options: z.array(z.string()).default([]),
        multiSelect: z.boolean().default(false),
      })
    ),
  })
  .strict();

function schemaOf(shape: Record<string, unknown>, required: string[]) {
  return { type: "object" as const, properties: shape, required, additionalProperties: false };
}

const INTERNAL = "Internal to the session bridge. Never call this yourself.";

mcp.setRequestHandler(ListToolsRequestSchema, async () => ({
  tools: [
    {
      name: "send_to_agent",
      description:
        "Send a message to another agent's channel. It arrives in that agent's Claude session like a user message, attributed to this agent. `agent` takes a base name (e.g. 'workflow-agent') which resolves against the live agent registry, or a full id ('name@host') when the agent runs on several machines — an ambiguous name fails with the candidate ids, an unknown name fails with the registered list. The target must have a running session to receive it. Limits (per-target cooldown, hourly cap, reply-chain hop cap) come from live channel_config and may be off - when refused, summarise the exchange to the user instead of retrying.",
      inputSchema: schemaOf(
        {
          agent: { type: "string", description: "Target agent base name or full id ('name@host')" },
          text: { type: "string", description: "The message to send" },
        },
        ["agent", "text"]
      ),
    },
    {
      name: "push_message",
      description: INTERNAL,
      inputSchema: schemaOf(
        {
          role: { type: "string", enum: ["user", "assistant"] },
          text: { type: "string" },
          source: { type: "string" },
          id: { type: "string" },
        },
        ["role", "text"]
      ),
    },
    {
      name: "edit_message",
      description: INTERNAL,
      inputSchema: schemaOf({ id: { type: "string" }, text: { type: "string" } }, ["id", "text"]),
    },
    {
      name: "push_status",
      description: INTERNAL,
      inputSchema: schemaOf({ state: { type: "string", enum: ["idle", "thinking", "tool_use"] } }, ["state"]),
    },
    {
      name: "push_tool_event",
      description: INTERNAL,
      inputSchema: schemaOf({ tool: { type: "string" }, detail: { type: "string" } }, ["tool", "detail"]),
    },
    {
      name: "push_context_usage",
      description: INTERNAL,
      inputSchema: schemaOf({ used: { type: "integer" }, window: { type: "integer" } }, ["used", "window"]),
    },
    {
      name: "next_message",
      description: INTERNAL,
      inputSchema: schemaOf({ timeoutMs: { type: "integer" } }, []),
    },
    {
      name: "request_permission",
      description: INTERNAL,
      inputSchema: schemaOf({ id: { type: "string" }, tool: { type: "string" }, input: { type: "string" } }, ["id", "tool", "input"]),
    },
    {
      name: "poll_permission",
      description: INTERNAL,
      inputSchema: schemaOf({ id: { type: "string" }, timeoutMs: { type: "integer" } }, ["id"]),
    },
    {
      name: "request_question",
      description: INTERNAL,
      inputSchema: schemaOf(
        {
          questions: {
            type: "array",
            items: {
              type: "object",
              properties: {
                question: { type: "string" },
                header: { type: "string" },
                options: { type: "array", items: { type: "string" } },
                multiSelect: { type: "boolean" },
              },
              required: ["question"],
            },
          },
        },
        ["questions"]
      ),
    },
    {
      name: "poll_question",
      description: INTERNAL,
      inputSchema: schemaOf({ id: { type: "string" }, timeoutMs: { type: "integer" } }, ["id"]),
    },
  ],
}));

function ok(value: unknown) {
  return { content: [{ type: "text" as const, text: JSON.stringify(value) }] };
}

function fail(text: string) {
  return { content: [{ type: "text" as const, text: `FAILED: ${text}` }], isError: true };
}

function parseOr<T>(schema: z.ZodType<T>, raw: unknown, tool: string): T | ReturnType<typeof fail> {
  const parsed = schema.safeParse(raw);
  if (parsed.success) return parsed.data;
  return fail(`invalid arguments for ${tool} — ${describeZodError(parsed.error)}`);
}

function isFail(v: unknown): v is ReturnType<typeof fail> {
  return typeof v === "object" && v !== null && "isError" in v;
}

mcp.setRequestHandler(CallToolRequestSchema, async (req) => {
  const name = req.params.name;
  const raw = req.params.arguments;

  if (name === "next_message") {
    const a = parseOr(NextMessageArgs, raw, name);
    if (isFail(a)) return a;
    const message = await waitForInbound(a.timeoutMs);
    return ok(message ? { message } : {});
  }

  if (!conn) return fail("not connected to SpacetimeDB");
  const db = conn;

  try {
    switch (name) {
      case "send_to_agent": {
        const a = parseOr(SendToAgentArgs, raw, name);
        if (isFail(a)) return a;
        return await sendToAgent(db, a.agent, a.text);
      }
      case "push_message": {
        const a = parseOr(PushMessageArgs, raw, name);
        if (isFail(a)) return a;
        const id = a.id ?? `${a.role === "assistant" ? "reply" : "prompt"}-${Date.now()}-${randomUUID().slice(0, 6)}`;
        await db.reducers.pushMessage({ id, agentId: AGENT_ID, role: a.role, text: a.text, source: a.source });
        return ok({ id });
      }
      case "edit_message": {
        const a = parseOr(EditMessageArgs, raw, name);
        if (isFail(a)) return a;
        await db.reducers.editMessage({ id: a.id, text: a.text });
        return ok({ id: a.id });
      }
      case "push_status": {
        const a = parseOr(PushStatusArgs, raw, name);
        if (isFail(a)) return a;
        await db.reducers.pushStatus({ agentId: AGENT_ID, state: a.state });
        return ok({ state: a.state });
      }
      case "push_tool_event": {
        const a = parseOr(PushToolEventArgs, raw, name);
        if (isFail(a)) return a;
        const id = `tool-${Date.now()}-${randomUUID().slice(0, 8)}`;
        await db.reducers.pushToolEvent({ id, agentId: AGENT_ID, tool: a.tool, detail: a.detail });
        return ok({ id });
      }
      case "push_context_usage": {
        const a = parseOr(PushContextUsageArgs, raw, name);
        if (isFail(a)) return a;
        await db.reducers.pushContextUsage({ agentId: AGENT_ID, used: BigInt(a.used), window: BigInt(a.window) });
        return ok({});
      }
      case "request_permission": {
        const a = parseOr(RequestPermissionArgs, raw, name);
        if (isFail(a)) return a;
        permissionOutcomes.done.delete(a.id);
        await db.reducers.requestPermission({ id: a.id, agentId: AGENT_ID, tool: a.tool, input: a.input });
        return ok({ id: a.id });
      }
      case "poll_permission": {
        const a = parseOr(PollArgs, raw, name);
        if (isFail(a)) return a;
        const status = await awaitOutcome(permissionOutcomes, a.id, a.timeoutMs);
        if (status === undefined) return ok({ status: "pending" });
        return ok({ status: status === "allow" || status === "approved" ? "allow" : "deny" });
      }
      case "request_question": {
        const a = parseOr(RequestQuestionArgs, raw, name);
        if (isFail(a)) return a;
        const ids: string[] = [];
        for (const q of a.questions) {
          const id = `question-${Date.now()}-${randomUUID().slice(0, 8)}`;
          questionOutcomes.done.delete(id);
          await db.reducers.requestQuestion({
            id,
            agentId: AGENT_ID,
            question: q.question,
            header: q.header,
            options: JSON.stringify(q.options),
            multiSelect: q.multiSelect,
          });
          ids.push(id);
        }
        return ok({ ids });
      }
      case "poll_question": {
        const a = parseOr(PollArgs, raw, name);
        if (isFail(a)) return a;
        const response = await awaitOutcome(questionOutcomes, a.id, a.timeoutMs);
        if (response === undefined) return ok({ status: "pending" });
        return ok({ status: "answered", response });
      }
      default:
        return fail(`unknown tool: ${name}`);
    }
  } catch (e) {
    return fail(String(e));
  }
});

async function sendToAgent(db: DbConnection, agent: string, text: string) {
  const now = Date.now();
  const cfg = Array.from(db.db.channel_config.iter())[0] as
    | { a2AEnabled: boolean; a2ACooldownSecs: number; a2AHourlyLimit: number; a2AMaxHops: number }
    | undefined;
  if (cfg && !cfg.a2AEnabled) {
    return fail("REFUSED: agent-to-agent messaging is disabled by the kill switch - summarise to the user instead");
  }

  const cooldownMs = (cfg?.a2ACooldownSecs ?? 0) * 1000;
  const hourlyLimit = cfg?.a2AHourlyLimit ?? 0;
  const maxHops = cfg?.a2AMaxHops ?? A2A_DEFAULT_MAX_HOPS;

  const inheritedHop =
    incomingAgentHop !== null && now - incomingAgentHopAt < A2A_HOP_DECAY_MS ? incomingAgentHop : null;
  const chainHops = inheritedHop === null ? 0 : inheritedHop + 1;
  if (maxHops > 0 && chainHops >= maxHops) {
    return fail(`REFUSED: agent reply chain reached ${maxHops} hops - stop messaging agents and summarise the exchange to the user instead`);
  }

  while (a2aSendTimes.length > 0 && now - a2aSendTimes[0]! > 3_600_000) a2aSendTimes.shift();
  if (hourlyLimit > 0 && a2aSendTimes.length >= hourlyLimit) {
    return fail(`REFUSED: rate limited - ${hourlyLimit} agent messages per hour already sent; summarise to the user instead`);
  }

  const registry = Array.from(db.db.agent.iter()) as Array<{ id: string; baseName: string }>;
  let target = agent;
  if (!registry.some((a) => a.id === agent)) {
    const matches = registry.filter((a) => a.baseName === agent);
    if (matches.length === 1) {
      target = matches[0]!.id;
    } else if (matches.length > 1) {
      return fail(`'${agent}' runs on multiple hosts — specify one of: ${matches.map((a) => a.id).join(", ")}`);
    } else {
      const known = registry.map((a) => a.id).join(", ") || "none registered";
      return fail(`unknown agent '${agent}'. Registered agents: ${known}`);
    }
  }
  const lastToTarget = a2aLastSendToTarget.get(target);
  if (cooldownMs > 0 && lastToTarget !== undefined && now - lastToTarget < cooldownMs) {
    const wait = Math.ceil((cooldownMs - (now - lastToTarget)) / 1000);
    return fail(`REFUSED: rate limited - already messaged ${target} in the last ${cooldownMs / 1000}s, retry in ${wait}s or batch your points into one message`);
  }

  const id = `a2a-${chainHops}-${now}`;
  await db.reducers.pushMessage({ id, agentId: target, role: "user", text, source: `agent:${AGENT_ID}` });
  a2aSendTimes.push(now);
  a2aLastSendToTarget.set(target, now);
  return { content: [{ type: "text" as const, text: `sent to ${target} (id: ${id}, chain hop ${chainHops})` }] };
}

function waitForInbound(timeoutMs: number): Promise<InboundMessage | null> {
  const queued = inboundQueue.shift();
  if (queued) return Promise.resolve(queued);
  if (inboundWaiter) {
    const previous = inboundWaiter;
    inboundWaiter = null;
    previous(null);
  }
  return new Promise((resolve) => {
    const timer = setTimeout(() => {
      if (inboundWaiter === settle) inboundWaiter = null;
      resolve(null);
    }, timeoutMs);
    const settle = (m: InboundMessage | null) => {
      clearTimeout(timer);
      resolve(m);
    };
    inboundWaiter = settle;
  });
}

function enqueueInbound(message: InboundMessage) {
  if (inboundWaiter) {
    const waiter = inboundWaiter;
    inboundWaiter = null;
    waiter(message);
    return;
  }
  inboundQueue.push(message);
}

await mcp.connect(new StdioServerTransport());

sweepInbox();
setInterval(sweepInbox, INBOX_SWEEP_MS);
connectToStdb();

function filesUrlFor(stdbUri: string): string {
  const url = new URL(stdbUri.replace(/^ws/, "http"));
  url.port = "5051";
  return url.origin;
}

function parseArgs() {
  const stdbUri = getArg("--stdb-uri") || process.env.SPACE_CHANNEL_STDB_URI || "ws://127.0.0.1:5050";
  const stdbDb = getArg("--stdb-db") || process.env.SPACE_CHANNEL_STDB_DB || "spacenotes";
  const agent =
    getArg("--agent") ||
    process.env.SPACE_CHANNEL_AGENT ||
    `agent-${Date.now()}`;
  return { stdbUri, stdbDb, agent };
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
        await conn.reducers.registerAgent({
          id: AGENT_ID,
          baseName: args.agent,
          host: HOST,
          clientId: CLIENT_ID,
        });
        log(`Registered agent ${AGENT_ID}`);
      } catch (e) {
        log(`registerAgent failed: ${e}`);
        return;
      }

      conn.subscriptionBuilder()
        .onApplied(() => log("Subscriptions applied"))
        .onError((_ctx) => log("Subscription error"))
        .subscribe([
          `SELECT * FROM agent`,
          `SELECT * FROM channel_config`,
          `SELECT * FROM permission_request WHERE agent_id = '${AGENT_ID}'`,
          `SELECT * FROM question_request WHERE agent_id = '${AGENT_ID}'`,
          `SELECT * FROM message WHERE agent_id = '${AGENT_ID}' AND role = 'user'`,
          `SELECT message_image.* FROM message_image JOIN message ON message.id = message_image.message_id WHERE message.agent_id = '${AGENT_ID}' AND message.role = 'user'`,
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
        log(`message.onInsert id=${row.id} role=${row.role} source=${row.source} event_tag=${tag}`);
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
          await conn?.reducers.heartbeat({ agentId: AGENT_ID });
          heartbeatCount++;
          if (heartbeatCount === 1 || heartbeatCount % 15 === 0) {
            log(`heartbeat ok (count=${heartbeatCount})`);
          }
        } catch (e) {
          log(`heartbeat failed: ${e}`);
        }
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
  if (row.role === "user" && row.source === "control") {
    log(`control received id=${row.id} text=${row.text}`);
    enqueueInbound({ id: row.id, text: row.text, source: row.source, sender: "flutter" });
    return;
  }
  const fromAgent = row.source.startsWith("agent:");
  if (row.role !== "user" || (row.source !== "flutter" && !fromAgent)) {
    log(`inbound skipped id=${row.id} (role/source filter)`);
    return;
  }

  if (fromAgent) {
    const hopMatch = row.id.match(/^a2a-(\d+)-/);
    incomingAgentHop = hopMatch ? parseInt(hopMatch[1]!, 10) : 0;
    incomingAgentHopAt = Date.now();
  } else {
    incomingAgentHop = null;
  }

  const image = pendingImages.get(row.id);
  if (image) {
    pendingImages.delete(row.id);
    deliverInbound(row, image);
    return;
  }

  pendingMessages.set(row.id, row);
  setTimeout(() => {
    const buffered = pendingMessages.get(row.id);
    if (!buffered) return;
    pendingMessages.delete(row.id);
    deliverInbound(buffered);
  }, IMAGE_PAIR_WAIT_MS);
}

function handleIncomingImage(row: MessageImage) {
  const message = pendingMessages.get(row.messageId);
  if (message) {
    pendingMessages.delete(row.messageId);
    deliverInbound(message, row);
    return;
  }
  pendingImages.set(row.messageId, row);
  setTimeout(() => {
    pendingImages.delete(row.messageId);
  }, 5000);
}

function deliverInbound(message: Message, image?: MessageImage) {
  const sender = message.source.startsWith("agent:")
    ? message.source.slice("agent:".length)
    : "flutter";
  const inbound: InboundMessage = {
    id: message.id,
    text: message.text,
    source: message.source,
    sender,
  };

  if (image) {
    try {
      mkdirSync(INBOX_DIR, { recursive: true });
      const filePath = join(INBOX_DIR, `${message.id}.png`);
      writeFileSync(filePath, Buffer.from(image.bytes));
      inbound.imagePaths = [filePath];
    } catch (e) {
      log(`inbox write failed for ${message.id}: ${e}`);
    }
  }

  deliveryChain = deliveryChain
    .then(() => attachVaultImages(inbound))
    .catch((e) => log(`vault image fetch failed for ${message.id}: ${e}`))
    .then(() => {
      log(`inbound queued id=${message.id} sender=${sender} text_len=${message.text.length} images=${inbound.imagePaths?.length ?? 0}`);
      enqueueInbound(inbound);
    });
}

let deliveryChain: Promise<void> = Promise.resolve();

async function attachVaultImages(inbound: InboundMessage) {
  if (inbound.source !== "flutter") return;
  const ids = [...inbound.text.matchAll(VAULT_FILE_LINK)].map((m) => m[1]!);
  if (ids.length === 0) return;
  const db = conn;
  if (!db) return;
  mkdirSync(INBOX_DIR, { recursive: true });
  const paths: string[] = [];
  for (const [n, id] of ids.entries()) {
    const remote = await resolveFilePath(db, id);
    if (!remote) {
      log(`vault link ${id} did not resolve to a file`);
      continue;
    }
    const ext = remote.split(".").pop()?.toLowerCase() ?? "";
    if (!INBOX_IMAGE_EXTENSIONS.has(ext)) continue;
    const url = `${FILES_URL}/files/${remote.split("/").map(encodeURIComponent).join("/")}`;
    const res = await fetch(url);
    if (!res.ok) {
      log(`vault image download ${res.status} for ${remote}`);
      continue;
    }
    const local = join(INBOX_DIR, `${inbound.id}-${n + 1}.${ext}`);
    writeFileSync(local, Buffer.from(await res.arrayBuffer()));
    paths.push(local);
  }
  if (paths.length > 0) inbound.imagePaths = [...(inbound.imagePaths ?? []), ...paths];
}

function resolveFilePath(db: DbConnection, id: string): Promise<string | null> {
  const known = db.db.space_file.id.find(id);
  if (known) return Promise.resolve(known.path);
  return new Promise((resolve) => {
    let settled = false;
    const handle = db
      .subscriptionBuilder()
      .onApplied(() => {
        if (settled) return;
        settled = true;
        clearTimeout(timer);
        resolve(db.db.space_file.id.find(id)?.path ?? null);
        handle.unsubscribe();
      })
      .subscribe([`SELECT * FROM space_file WHERE id = '${id}'`]);
    const timer = setTimeout(() => {
      if (settled) return;
      settled = true;
      resolve(null);
      handle.unsubscribe();
    }, VAULT_RESOLVE_TIMEOUT_MS);
  });
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
  settleOutcome(permissionOutcomes, row.id, row.status);
}

function handleQuestionUpdate(row: QuestionRequest) {
  if (row.status === "pending") return;
  settleOutcome(questionOutcomes, row.id, row.response ?? null);
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
      await conn.reducers.endAgent({ agentId: AGENT_ID });
      log(`Agent ended: ${AGENT_ID}`);
    } catch (e) {
      log(`endAgent on shutdown failed: ${e}`);
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

function resolvePluginDir(selfPath: string): string {
  const fromEnv = process.env.SPACE_CHANNEL_PLUGIN_DIR;
  if (fromEnv) return fromEnv;
  if (basename(dirname(selfPath)) === "bin") return dirname(dirname(selfPath));
  return join(dirname(selfPath), "plugin");
}

async function runLaunch(rawArgs: string[]) {
  if (rawArgs.length < 1) {
    process.stderr.write("usage: space-channel launch <workflow> [args...]\n");
    process.exit(2);
  }
  const agent = rawArgs[0]!;
  const skill = agent;
  const rest = rawArgs.slice(1);

  if (!commandExists("claude")) {
    process.stderr.write("claude CLI not found in PATH\n");
    process.exit(1);
  }

  const selfPath: string = process.execPath.endsWith("/bun")
    ? (process.argv[1] ?? process.execPath)
    : process.execPath;
  const pluginDir = resolvePluginDir(selfPath);
  if (!existsSync(join(pluginDir, ".claude-plugin", "plugin.json"))) {
    process.stderr.write(`space-channel plugin not found at ${pluginDir} (set SPACE_CHANNEL_PLUGIN_DIR)\n`);
    process.exit(1);
  }

  const stdbUri = process.env.SPACE_CHANNEL_STDB_URI || "ws://100.84.184.121:5050";
  const stdbDb = process.env.SPACE_CHANNEL_STDB_DB || "spacenotes";

  process.stderr.write(`space-channel ready (agent: ${agent}, plugin: ${pluginDir}, build: ${BUILD_SHA})\n`);

  const claude = spawn(
    "claude",
    [
      "--plugin-dir", pluginDir,
      "--dangerously-skip-permissions",
      `/${skill}`,
      ...rest,
    ],
    {
      stdio: "inherit",
      env: {
        ...process.env,
        WORKFLOW_NAME: agent,
        SPACE_CHANNEL_AGENT: agent,
        SPACE_CHANNEL_STDB_URI: stdbUri,
        SPACE_CHANNEL_STDB_DB: stdbDb,
      },
    }
  );
  const code: number = await new Promise((resolve) => {
    claude.on("exit", (c) => resolve(c ?? 0));
  });
  process.exit(code);
}

function commandExists(cmd: string): boolean {
  const r = spawnSync("command", ["-v", cmd], { stdio: "ignore", shell: true });
  return r.status === 0;
}
