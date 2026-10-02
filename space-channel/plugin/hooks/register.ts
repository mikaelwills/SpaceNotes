import type { Register } from 'claude-code'

const BRIDGE = 'space-channel'
const POLL_TIMEOUT_MS = 20_000
const IDLE_WRAPUP_MS = 30 * 60_000
const IDLE_CHECK_MS = 60_000
const AUTOWRAP_DEFAULT_TOKENS = 300_000
const PERMISSION_TIMEOUT_MS = 9.5 * 60_000
const QUESTION_TIMEOUT_MS = 10 * 60_000
const WRAPUP_TEXT =
  'Auto wrap up. Follow this session\'s wrap-up procedure — if the workflow defines its own, use that; otherwise read the SpaceNotes vault note Workflows/workflow-agent/execution-flow.md (section "On Session End") via the spacenotes-mcp get_file tool'

const INTERNAL_TOOLS = new Set([
  'push_message',
  'edit_message',
  'push_status',
  'push_tool_event',
  'push_context_usage',
  'next_message',
  'request_permission',
  'poll_permission',
  'request_question',
  'poll_question',
])

type Inbound = { id: string; text: string; source: string; sender: string; imagePaths?: string[] }

let server = ''
let turnRunning = false
let lastPromptAt = Date.now()
let userEngaged = false
let wrapUpFired = false
let lastPromptFromPhone = false
let currentTurnId: string | undefined
let autoWrapTokens = AUTOWRAP_DEFAULT_TOKENS
let autoWrap: 'armed' | 'queued' | 'running' | 'compacting' | 'failed' = 'armed'
let draining = false
const deliveries: Inbound[] = []

function internalToolName(tool: string): string | undefined {
  const m = /^mcp__.+__([a-z_]+)$/.exec(tool)
  if (!m) return undefined
  return INTERNAL_TOOLS.has(m[1]!) ? m[1] : undefined
}

function toolArguments(e: Record<string, unknown>): Record<string, unknown> {
  const { tool: _tool, tool_use_id: _id, consent: _consent, agentId: _agent, ...rest } = e
  return rest
}

async function bridge($: any, tool: string, args: Record<string, unknown>): Promise<any> {
  if (!server) throw new Error('bridge not connected')
  const r = await $.mcp.call(server, tool, args)
  const text = r.content.find((b: any) => b.type === 'text')?.text ?? ''
  if (r.isError) throw new Error(text || `${tool} failed`)
  try {
    return JSON.parse(text)
  } catch {
    return text
  }
}

async function tryBridge($: any, tool: string, args: Record<string, unknown>): Promise<any> {
  try {
    return await bridge($, tool, args)
  } catch (err) {
    $.ui.log(`${tool} failed: ${err instanceof Error ? err.message : String(err)}`, { to: 'debug' })
    return undefined
  }
}

function touchActivity(fromPhone: boolean) {
  lastPromptAt = Date.now()
  userEngaged = true
  wrapUpFired = false
  lastPromptFromPhone = fromPhone
}

async function deliver($: any, m: Inbound) {
  touchActivity(true)
  const fromAgent = m.source.startsWith('agent:')
  let text = m.text.trim()
  const images = m.imagePaths ?? []
  if (!text && images.length > 0) text = images.length === 1 ? '(image)' : '(images)'
  if (images.length === 1) text += `\n\n(image attached at ${images[0]} — read it with the Read tool)`
  if (images.length > 1) {
    text += `\n\n(${images.length} images attached — read each with the Read tool:\n${images.map(p => `- ${p}`).join('\n')})`
  }
  if (fromAgent) {
    text += `\n\n(a2a message from agent '${m.sender}' — to answer THEM use send_to_agent('${m.sender}'); plain text goes to your own human's chat, not to them)`
    await $.prompt.submit({ text })
    return
  }
  if (images.length === 0 && (await runSlashCommand($, text))) return
  await $.prompt.submit({ text, asUser: true })
}

async function runSlashCommand($: any, text: string): Promise<boolean> {
  const match = /^\/([\w:.-]+)(?:\s+([\s\S]*))?$/.exec(text)
  if (!match) return false
  while (turnRunning) await $.clock.sleep(500)
  let result: any
  try {
    result = await $.command.run({ command: match[1], args: match[2] ?? '' })
  } catch (err) {
    $.ui.log(`/${match[1]} not run as a command: ${err instanceof Error ? err.message : String(err)}`, { to: 'debug' })
    return false
  }
  const output = typeof result?.text === 'string' ? result.text.trim() : ''
  if (output) await tryBridge($, 'push_message', { role: 'assistant', text: output, source: 'notice' })
  return true
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

async function stopTurn($: any) {
  if (!turnRunning || !currentTurnId) {
    $.ui.log('stop requested from SpaceNotes with no turn running', { to: 'debug' })
    return
  }
  try {
    await $.turn.abort({ turnId: currentTurnId })
  } catch (err) {
    $.ui.log(`stop failed: ${err instanceof Error ? err.message : String(err)}`, { to: 'debug' })
  }
}

async function inboundLoop($: any) {
  while (true) {
    const res = await tryBridge($, 'next_message', { timeoutMs: POLL_TIMEOUT_MS })
    if (res === undefined) {
      await $.clock.sleep(2000)
      continue
    }
    const m: Inbound | undefined = res.message
    if (!m) continue
    if (m.source === 'control') {
      if (m.text === 'stop') await stopTurn($)
      continue
    }
    deliveries.push(m)
    void drainDeliveries($)
  }
}

function checkAutoWrap($: any, tokens: number) {
  if (autoWrap === 'compacting' && tokens < autoWrapTokens) autoWrap = 'armed'
  if (autoWrap !== 'armed' || tokens < autoWrapTokens) return
  autoWrap = 'queued'
  const k = Math.round(autoWrapTokens / 1000)
  $.ui.log(`context passed ${k}k, running the wrap-up then /compact`, { to: 'debug' })
  void tryBridge($, 'push_message', { role: 'assistant', text: `🧹 context passed ${k}k: wrapping up, then /compact`, source: 'notice' })
  void $.prompt.submit({ text: `Context passed ${k}k tokens. ${WRAPUP_TEXT}. /compact runs automatically when you finish.` })
}

async function compactAfterWrap($: any) {
  try {
    await $.command.run({ command: 'compact', args: '' })
    await tryBridge($, 'push_message', { role: 'assistant', text: '🧹 compacted', source: 'notice' })
  } catch (err) {
    autoWrap = 'failed'
    const reason = err instanceof Error ? err.message : String(err)
    $.ui.log(`auto /compact failed: ${reason}`)
    await tryBridge($, 'push_message', { role: 'assistant', text: `⚠️ auto /compact failed: ${reason}`, source: 'error' })
  }
}

function checkIdleWrapUp($: any) {
  if (!userEngaged || wrapUpFired || turnRunning) return
  if (Date.now() - lastPromptAt < IDLE_WRAPUP_MS) return
  wrapUpFired = true
  $.ui.log('idle for 30 min, running the session wrap-up', { to: 'debug' })
  void $.prompt.submit({ text: WRAPUP_TEXT })
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
  const requested = await tryBridge($, 'request_question', { questions: shaped })
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
    const connected = await $.mcp.connect(BRIDGE)
    if (!connected.isConnected) {
      $.ui.log(`bridge not connected: ${connected.reason ?? ''} ${connected.message ?? ''}`.trim())
      return started
    }
    server = connected.server
    const configured = Number(await $.env.get('SPACE_CHANNEL_AUTOWRAP_TOKENS'))
    if (Number.isFinite(configured) && configured > 0) autoWrapTokens = configured
    void inboundLoop($)
    $.clock.every(IDLE_CHECK_MS, () => checkIdleWrapUp($))
    return started
  })

  on('prompt.submit', async ($, e, next) => {
    const kind = e.origin?.kind
    if (kind === 'composer' || kind === 'bridge') {
      touchActivity(false)
      void tryBridge($, 'push_message', { role: 'user', text: e.text, source: 'terminal' })
    }
    void tryBridge($, 'push_status', { state: 'thinking' })
    return next(e)
  })

  on('turn.start', async ($, e, next) => {
    turnRunning = true
    currentTurnId = e.turnId
    if (autoWrap === 'queued') autoWrap = 'running'
    void tryBridge($, 'push_status', { state: 'thinking' })
    return next(e)
  })

  on('turn.complete', async ($, e, next) => {
    if (e.agentId) return next(e)
    turnRunning = false
    currentTurnId = undefined
    const answer = e.answer.trim()
    if (e.reason === 'aborted') {
      await tryBridge($, 'push_message', { role: 'assistant', text: '⏹ stopped', source: 'notice' })
    } else if (e.reason === 'answer' && answer) {
      await tryBridge($, 'push_message', { role: 'assistant', text: answer, source: 'mcp' })
    } else if (e.reason === 'error' || e.reason === 'refusal') {
      await tryBridge($, 'push_message', {
        role: 'assistant',
        text: `⚠️ ${answer || `turn ended: ${e.reason}`}`,
        source: 'error',
      })
    }
    await tryBridge($, 'push_status', { state: 'idle' })
    if (autoWrap === 'running') {
      autoWrap = 'compacting'
      $.clock.after(0, () => compactAfterWrap($))
    }
    return next(e)
  })

  on('tool.call', async ($, e, next) => {
    const fromModel = next.origin.plugin === 'engine'
    if (!fromModel) return next(e)
    const internal = internalToolName(e.tool)
    if (internal) {
      return { deny: `${internal} is internal to the space-channel bridge; the session calls it, not you.` }
    }
    const input = toolArguments(e as Record<string, unknown>)
    if (e.tool === 'AskUserQuestion' && lastPromptFromPhone && server) {
      const answers = await relayQuestion($, (input as any).questions)
      if (answers) return next({ ...e, answers } as any)
    }
    void tryBridge($, 'push_tool_event', { tool: e.tool, detail: JSON.stringify({ tool: e.tool, input }) })
    void tryBridge($, 'push_status', { state: 'tool_use' })
    const ran = await next(e)
    void tryBridge($, 'push_status', { state: 'thinking' })
    return ran
  })

  on('tool.check', async ($, e, next) => {
    if (internalToolName(e.tool)) return { decision: 'allow', reason: 'space-channel bridge call' }
    const verdict = await next(e)
    if (verdict.decision !== 'ask' || !lastPromptFromPhone || !server || !e.tool_use_id) return verdict
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
      checkAutoWrap($, tokens)
    }
    return next(e)
  })
}
