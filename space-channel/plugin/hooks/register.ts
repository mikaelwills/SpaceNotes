import type { Register } from 'claude-code'

const POLL_TIMEOUT_MS = 20_000
const PERMISSION_TIMEOUT_MS = 9.5 * 60_000
const QUESTION_TIMEOUT_MS = 10 * 60_000
const RESPAWN_MAX_MS = 30_000
const RESPAWN_RESET_MS = 60_000
const STREAM_INTERVAL_MS = 400

const SEND_TOOL = 'send_to_agent'
const SEND_TOOL_DESCRIPTION =
  "Send a message to another agent's channel. It arrives in that agent's Claude session like a user message, attributed to this agent. `agent` takes a base name (e.g. 'workflow-agent') which resolves against the live agent registry, or a full id ('name@host') when the agent runs on several machines — an ambiguous name fails with the candidate ids, an unknown name fails with the registered list. The target must have a running session to receive it. Limits (per-target cooldown, hourly cap, reply-chain hop cap) come from live channel_config and may be off - when refused, summarise the exchange to the user instead of retrying."

const INSTRUCTIONS = [
  'Your user talks to you from the SpaceNotes app as well as this terminal; everything you write reaches both, so answer in plain text — there is no reply tool.',
  "Use send_to_agent to message another agent's channel directly; it arrives in their session like a user message, attributed to you.",
  'To point the user at a vault file, link it as markdown: [name](spacenotes://file/<id>) using the file id any spacenotes-mcp read returns; for a folder use [name](spacenotes://folder/<url-encoded path>). Tapping the link opens it in the app.',
  'Put each vault link alone on its own line, with no caption or other text sharing that line, so the app renders it as a card with a thumbnail; a link inside a sentence renders as plain text.',
  'SpaceChannel (this mod plus the Rust bridge, owned by the spacenotes agent) updates itself: publish-space-channel.sh announces a release, every bridge on every machine installs it, and every session reloads the mod, re-running tool registration. A SpaceChannel change never needs a session restart or a manual reload, so never ask for one, never tell another agent one is needed, and treat any claim that one is needed as wrong.',
].join(' ')

type LiveRow = { id?: string; text: string; sent: string; chain: Promise<void>; timer: boolean }

type Inbound ={ id: string; text: string; source: string; sender: string; imagePaths?: string[] }

let socketPath = ''
let bridgeReady = false
let sendToolName = ''
let instructionsPending = true
let turnRunning = false
let lastPromptFromPhone = false
let currentTurnId: string | undefined
let draining = false
let toolsRunning = 0
let answerRow: LiveRow | undefined
let herdrPane: string | undefined
let herdrBin: string | undefined
const deliveries: Inbound[] = []
const midTurn: Inbound[] = []

function toolArguments(e: Record<string, unknown>): Record<string, unknown> {
  const { tool: _tool, tool_use_id: _id, consent: _consent, agentId: _agent, ...rest } = e
  return rest
}

async function bridge($: any, tool: string, args: Record<string, unknown>): Promise<any> {
  if (!socketPath) throw new Error('bridge not running')
  const r = await $.http.fetch(`http://bridge/call/${tool}`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(args),
    socketPath,
  })
  let body: any
  try {
    body = JSON.parse(r.text)
  } catch {
    body = r.text
  }
  if (!r.ok) throw new Error(body?.error || r.text || `${tool} failed`)
  return body
}

const BRIDGE_TIMEOUT_MS = 3_000
const LONG_POLLS = new Set(['poll_question', 'poll_permission'])

function withTimeout<T>(call: Promise<T>, ms: number, what: string): Promise<T> {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error(`${what} timed out after ${ms}ms`)), ms)
    call.then(
      (v) => {
        clearTimeout(timer)
        resolve(v)
      },
      (e) => {
        clearTimeout(timer)
        reject(e)
      },
    )
  })
}

async function tryBridge($: any, tool: string, args: Record<string, unknown>): Promise<any> {
  try {
    const call = bridge($, tool, args)
    return await (LONG_POLLS.has(tool) ? call : withTimeout(call, BRIDGE_TIMEOUT_MS, tool))
  } catch (err) {
    $.ui.log(`${tool} failed: ${err instanceof Error ? err.message : String(err)}`, { to: 'debug' })
    return undefined
  }
}

function touchActivity(fromPhone: boolean) {
  lastPromptFromPhone = fromPhone
}

function composeText(m: Inbound): string {
  let text = m.text.trim()
  const images = m.imagePaths ?? []
  if (!text && images.length > 0) text = images.length === 1 ? '(image)' : '(images)'
  if (images.length === 1) text += `\n\n(image attached at ${images[0]} — read it with the Read tool)`
  if (images.length > 1) {
    text += `\n\n(${images.length} images attached — read each with the Read tool:\n${images.map(p => `- ${p}`).join('\n')})`
  }
  if (m.source.startsWith('agent:')) {
    text += `\n\n(a2a message from agent '${m.sender}' — to answer THEM use send_to_agent('${m.sender}'); plain text goes to your own human's chat, not to them)`
  }
  return text
}

function midTurnContext(m: Inbound): string {
  const from = m.source.startsWith('agent:') ? `agent '${m.sender}'` : 'the user, from the SpaceNotes app,'
  return `A message from ${from} arrived while you were working. Read it now and take it into account in what you do next:\n\n${composeText(m)}`
}

function takesMidTurn(m: Inbound): boolean {
  return !/^\/[\w:.-]+/.test(m.text.trim())
}

async function deliver($: any, m: Inbound) {
  touchActivity(true)
  const text = composeText(m)
  if (m.source.startsWith('agent:')) {
    await $.prompt.submit({ text })
    return
  }
  if ((m.imagePaths ?? []).length === 0 && (await runSlashCommand($, text))) return
  await $.prompt.submit({ text, asUser: true })
}

async function runSlashCommand($: any, text: string): Promise<boolean> {
  const match = /^\/([\w:.-]+)(?:\s+([\s\S]*))?$/.exec(text)
  if (!match) return false
  while (turnRunning) await $.clock.sleep(500)
  const args = (match[2] ?? '').trim()
  if (match[1] === 'model' && !args) {
    await tryBridge($, 'push_message', { role: 'assistant', text: await modelChoices($), source: 'notice' })
    return true
  }
  if (match[1] === 'model') {
    await switchModel($, args)
    return true
  }
  let result: any
  try {
    result = await $.command.run({ command: match[1], args })
  } catch (err) {
    $.ui.log(`/${match[1]} not run as a command: ${err instanceof Error ? err.message : String(err)}`, { to: 'debug' })
    return false
  }
  const output = typeof result?.text === 'string' ? result.text.trim() : ''
  if (output) await tryBridge($, 'push_message', { role: 'assistant', text: output, source: 'notice' })
  return true
}

async function modelChoices($: any): Promise<string> {
  const current = await $.session.model()
  const rows = await $.config.list()
  const options: string[] = rows.find((r: any) => r.key === 'model')?.options ?? []
  return `Model: ${current}\nOptions: ${options.join(', ')}\nSend /model <name> to switch.`
}

async function switchModel($: any, requested: string) {
  const rows = await $.config.list()
  const options: string[] = rows.find((r: any) => r.key === 'model')?.options ?? []
  if (!options.includes(requested)) {
    await tryBridge($, 'push_message', {
      role: 'assistant',
      text: `Unknown model '${requested}'.\nOptions: ${options.join(', ')}`,
      source: 'notice',
    })
    return
  }
  let text: string
  try {
    await $.config.set({ key: 'model', value: requested })
    text = `Model → ${await $.session.model()}`
  } catch (err) {
    text = `⚠️ model switch failed: ${err instanceof Error ? err.message : String(err)}`
  }
  await tryBridge($, 'push_message', { role: 'assistant', text, source: 'notice' })
}

function newLiveRow(): LiveRow {
  return { text: '', sent: '', chain: Promise.resolve(), timer: false }
}

async function syncLiveRow($: any, row: LiveRow) {
  const text = row.text.trim()
  if (!text || text === row.sent) return
  if (!row.id) {
    const pushed = await tryBridge($, 'push_message', { role: 'assistant', text, source: 'progress' })
    row.id = pushed?.id
  } else {
    await tryBridge($, 'edit_message', { id: row.id, text })
  }
  row.sent = text
}

function scheduleLiveRow($: any, row: LiveRow) {
  if (row.timer) return
  row.timer = true
  $.clock.after(STREAM_INTERVAL_MS, () => {
    row.timer = false
    row.chain = row.chain.then(() => syncLiveRow($, row))
  })
}

function finishLiveRow($: any, row: LiveRow, text: string, source: string): Promise<void> {
  row.text = text
  row.chain = row.chain.then(async () => {
    const final = text.trim()
    if (!final) return
    if (!row.id) {
      const pushed = await tryBridge($, 'push_message', { role: 'assistant', text: final, source })
      row.id = pushed?.id
    } else {
      await tryBridge($, 'update_message', { id: row.id, text: final, source })
    }
    row.sent = final
  })
  return row.chain
}

async function drainDeliveries($: any) {
  if (draining) return
  draining = true
  try {
    while (deliveries.length > 0) {
      const m = deliveries.shift()!
      await deliver($, m)
    }
  } finally {
    draining = false
  }
}

async function reportTurnEnd($: any, reason: string, answer: string, row: LiveRow | undefined) {
  if (reason === 'aborted') {
    await tryBridge($, 'push_message', { role: 'assistant', text: 'Stopped', source: 'notice' })
  } else if (reason === 'answer' && answer) {
    if (row) await finishLiveRow($, row, answer, 'mcp')
    else await tryBridge($, 'push_message', { role: 'assistant', text: answer, source: 'mcp' })
  } else if (reason === 'error' || reason === 'refusal') {
    await tryBridge($, 'push_message', {
      role: 'assistant',
      text: `⚠️ ${answer || `turn ended: ${reason}`}`,
      source: 'error',
    })
  }
  await tryBridge($, 'push_status', { state: 'idle' })
}

async function stopTurn($: any) {
  if (!turnRunning || !currentTurnId) {
    $.ui.log('stop requested from SpaceNotes with no turn running', { to: 'debug' })
    await tryBridge($, 'push_status', { state: 'idle' })
    return
  }
  try {
    await $.turn.abort({ turnId: currentTurnId })
  } catch (err) {
    $.ui.log(`stop failed: ${err instanceof Error ? err.message : String(err)}`, { to: 'debug' })
  }
}

async function backgroundTool($: any) {
  if (toolsRunning === 0) {
    $.ui.log('background requested from SpaceNotes with no tool running', { to: 'debug' })
    return
  }
  if (!herdrPane || !herdrBin) {
    await tryBridge($, 'push_message', { role: 'assistant', text: '⚠️ background needs the session to run in a herdr pane', source: 'error' })
    return
  }
  try {
    await $.process.run({ argv: [herdrBin, 'pane', 'send-keys', herdrPane, 'ctrl+b'] })
  } catch (err) {
    $.ui.log(`background failed: ${err instanceof Error ? err.message : String(err)}`, { to: 'debug' })
  }
}

async function handleInbound($: any, m: Inbound) {
  if (m.source === 'control') {
    if (m.text === 'stop') await stopTurn($)
    if (m.text === 'background') await backgroundTool($)
    return
  }
  if (turnRunning && takesMidTurn(m)) {
    midTurn.push(m)
    return
  }
  deliveries.push(m)
  void drainDeliveries($)
}

function releaseMidTurn($: any) {
  if (midTurn.length === 0) return
  deliveries.push(...midTurn.splice(0))
  void drainDeliveries($)
}

function handleBridgeLine($: any, line: string) {
  let event: any
  try {
    event = JSON.parse(line)
  } catch {
    $.ui.log(`bridge sent a line that is not JSON: ${line.slice(0, 200)}`, { to: 'debug' })
    return
  }
  if (event.type === 'ready') {
    bridgeReady = true
    return
  }
  if (event.type === 'inbound' && event.message) void handleInbound($, event.message as Inbound)
}

async function runBridge($: any, agent: string) {
  let attempt = 0
  while (true) {
    const sock = `/tmp/space-channel-${agent}-${Date.now().toString(36)}.sock`
    const started = Date.now()
    socketPath = sock
    try {
      const child = $.process.spawn({
        argv: [`${$.plugin.root}/bin/space-channel`, 'bridge', '--agent', agent, '--socket', sock],
      })
      let pending = ''
      for await (const { stream, text } of child) {
        if (stream !== 'stdout') continue
        pending += text
        let newline = pending.indexOf('\n')
        while (newline >= 0) {
          const line = pending.slice(0, newline).trim()
          pending = pending.slice(newline + 1)
          if (line) handleBridgeLine($, line)
          newline = pending.indexOf('\n')
        }
      }
    } catch (err) {
      $.ui.log(`bridge failed: ${err instanceof Error ? err.message : String(err)}`, { to: 'debug' })
    }
    bridgeReady = false
    if (socketPath === sock) socketPath = ''
    if (Date.now() - started > RESPAWN_RESET_MS) attempt = 0
    const delay = Math.min(1000 * 2 ** attempt, RESPAWN_MAX_MS)
    attempt++
    try {
      $.ui.log(`bridge exited; restarting in ${delay}ms`, { to: 'debug' })
      await $.clock.sleep(delay)
    } catch {
      return
    }
  }
}

async function sendToAgent($: any, input: Record<string, unknown>): Promise<any> {
  try {
    const sent = await withTimeout(bridge($, 'send_to_agent', { agent: input.agent, text: input.text }), BRIDGE_TIMEOUT_MS * 2, SEND_TOOL)
    return { result: String(sent?.text ?? 'sent') }
  } catch (err) {
    return { deny: err instanceof Error ? err.message : String(err) }
  }
}

async function relayQuestion($: any, questions: unknown): Promise<Record<string, string> | undefined> {
  if (!Array.isArray(questions) || questions.length === 0) return undefined
  const shaped = questions.map((q: any) => ({
    question: String(q?.question ?? ''),
    header: String(q?.header ?? ''),
    options: Array.isArray(q?.options)
      ? q.options.map((o: any) => String(o?.label ?? '')).filter((l: string) => l.length > 0)
      : [],
    multiSelect: q?.multiSelect === true,
  }))
  let requested: any
  try {
    requested = await withTimeout(bridge($, 'request_question', { questions: shaped }), BRIDGE_TIMEOUT_MS, 'request_question')
  } catch (err) {
    void tryBridge($, 'push_message', {
      role: 'assistant',
      text: `⚠️ request_question failed: ${err instanceof Error ? err.message : String(err)}`,
      source: 'error',
    })
    return undefined
  }
  const ids: string[] = requested?.ids ?? []
  if (ids.length !== shaped.length) return undefined

  const answers: Record<string, string> = {}
  const deadline = Date.now() + QUESTION_TIMEOUT_MS
  for (let i = 0; i < ids.length; i++) {
    let response: string | null | undefined
    while (Date.now() < deadline) {
      const polled = await tryBridge($, 'poll_question', { id: ids[i], timeoutMs: POLL_TIMEOUT_MS })
      if (polled === undefined) return undefined
      if (polled.status === 'answered') {
        response = polled.response
        break
      }
    }
    if (response === undefined || response === null) return undefined
    let selected: string
    try {
      const parsed = JSON.parse(response)
      selected = Array.isArray(parsed) ? parsed.join(', ') : String(parsed)
    } catch {
      selected = response
    }
    answers[shaped[i]!.question] = selected
  }
  return answers
}

export const register: Register = on => {
  on('session.start', async ($, e, next) => {
    const started = await next(e)
    const agent = await $.env.get('SPACE_CHANNEL_AGENT')
    if (!agent) {
      $.ui.log('SPACE_CHANNEL_AGENT is not set; the space-channel bridge is not started')
      return started
    }
    herdrPane = (await $.env.get('HERDR_PANE_ID')) || undefined
    const home = await $.env.get('HOME')
    if (home) herdrBin = `${home}/.local/bin/herdr`
    try {
      const registered = await $.tool.register({
        name: SEND_TOOL,
        description: SEND_TOOL_DESCRIPTION,
        inputSchema: {
          type: 'object',
          properties: {
            agent: { type: 'string', description: "Target agent base name or full id ('name@host')" },
            text: { type: 'string', description: 'The message to send' },
          },
          required: ['agent', 'text'],
          additionalProperties: false,
        },
        isDeferred: false,
      })
      sendToolName = registered.tool
    } catch (err) {
      $.ui.log(`${SEND_TOOL} not registered: ${err instanceof Error ? err.message : String(err)}`)
    }
    void runBridge($, agent)
    return started
  })

  on('session.compact', async ($, e, next) => {
    const result = await next(e)
    instructionsPending = true
    return result
  })

  on('prompt.submit', async ($, e, next) => {
    const kind = e.origin?.kind
    if (kind === 'composer' || kind === 'bridge') {
      touchActivity(false)
      void tryBridge($, 'push_message', { role: 'user', text: e.text, source: 'terminal' })
    }
    void tryBridge($, 'push_status', { state: 'thinking' })
    if (!instructionsPending) return next(e)
    instructionsPending = false
    return next({ ...e, context: [...(e.context ?? []), INSTRUCTIONS] })
  })

  on('turn.start', async ($, e, next) => {
    turnRunning = true
    answerRow = undefined
    currentTurnId = e.turnId
    void tryBridge($, 'push_status', { state: 'thinking' })
    return next(e)
  })

  on('turn.complete', async ($, e, next) => {
    if (e.agentId) return next(e)
    turnRunning = false
    currentTurnId = undefined
    releaseMidTurn($)
    const answer = e.answer.trim()
    const reason = e.reason
    const row = answerRow
    answerRow = undefined
    $.clock.after(0, () => reportTurnEnd($, reason, answer, row))
    return next(e)
  })

  on('turn.step', async function* ($, e, next) {
    if (e.agentId) return yield* next(e)
    if (answerRow) void finishLiveRow($, answerRow, answerRow.text, 'progress')
    const row = newLiveRow()
    answerRow = row
    const blocks = new Map<number, string>()
    for await (const chunk of next(e) as AsyncIterable<any>) {
      yield chunk
      if (chunk.kind === 'tool' && answerRow === row) {
        answerRow = undefined
        scheduleLiveRow($, row)
      }
      if (chunk.kind === 'text') {
        blocks.set(chunk.index, (blocks.get(chunk.index) ?? '') + chunk.text)
        row.text = [...blocks.values()].map(b => b.trim()).filter(Boolean).join('\n\n')
        scheduleLiveRow($, row)
      }
    }
  })

  on('tool.call', async ($, e, next) => {
    const fromModel = next.origin.plugin === 'engine'
    if (!fromModel) return next(e)
    const input = toolArguments(e as Record<string, unknown>)
    if (e.tool === 'AskUserQuestion') {
      const skip = !bridgeReady ? 'bridge not connected' : !lastPromptFromPhone ? 'last prompt was not from the phone' : ''
      const answers = skip ? undefined : await relayQuestion($, (input as any).questions)
      if (answers) return { result: { questions: (input as any).questions, answers } } as any
      void tryBridge($, 'push_message', {
        role: 'assistant',
        text: `⚠️ question not relayed to the app: ${skip || 'relay failed'}; it is waiting in the terminal`,
        source: 'error',
      })
    }
    void tryBridge($, 'push_tool_event', { tool: e.tool, detail: JSON.stringify({ tool: e.tool, input }) })
    void tryBridge($, 'push_status', { state: 'tool_use' })
    toolsRunning++
    try {
      const result = sendToolName && e.tool === sendToolName ? await sendToAgent($, input) : await next(e)
      if (e.agentId || midTurn.length === 0 || !result || (result as any).deny !== undefined) return result
      const arrived = midTurn.splice(0)
      touchActivity(true)
      return { ...result, context: [...(result.context ?? []), ...arrived.map(midTurnContext)] }
    } finally {
      toolsRunning--
      void tryBridge($, 'push_status', { state: 'thinking' })
    }
  })

  on('tool.check', async ($, e, next) => {
    if (sendToolName && e.tool === sendToolName) return { decision: 'allow', reason: 'space-channel agent message' }
    if (e.tool === 'AskUserQuestion' && lastPromptFromPhone) return { decision: 'allow', reason: 'asked through SpaceNotes' }
    const verdict = await next(e)
    if (verdict.decision !== 'ask' || !lastPromptFromPhone || !bridgeReady || !e.tool_use_id) return verdict
    const id = e.tool_use_id
    const requested = await tryBridge($, 'request_permission', {
      id,
      tool: e.tool,
      input: JSON.stringify(e.input ?? {}),
    })
    if (requested === undefined) return verdict
    const deadline = Date.now() + PERMISSION_TIMEOUT_MS
    while (Date.now() < deadline) {
      const polled = await tryBridge($, 'poll_permission', { id, timeoutMs: POLL_TIMEOUT_MS })
      if (polled === undefined) return verdict
      if (polled.status === 'allow') return { decision: 'allow', reason: 'approved from SpaceNotes' }
      if (polled.status === 'deny') return { decision: 'deny', reason: 'denied from SpaceNotes' }
    }
    return verdict
  })

  on('session.measure', async ($, e, next) => {
    const tokens = e.context?.tokens
    const window = e.context?.window
    if (e.changed.includes('context') && typeof tokens === 'number' && typeof window === 'number' && window > 0) {
      void tryBridge($, 'push_context_usage', { used: Math.round(tokens), window: Math.round(window) })
    }
    return next(e)
  })
}
