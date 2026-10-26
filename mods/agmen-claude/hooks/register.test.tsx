import { expect, mock, test } from 'claude-code/testing'
import type { FsStat, On, PromptSubmitInput } from 'claude-code'

// Nothing sits beneath the plugin: each hook registered here stands for the
// engine. A call on `$` is answered `{ value }`; an event by its own result.

const START = { cwd: '/work', surface: 'terminal', isInteractive: true } as const

const dm = (seq: number, isToMe: boolean): string =>
  `${JSON.stringify({ seq, verb: 'DM', body: `tick ${seq}`, to_me: isToMe })}\n`

/** Stands for the session's engine: starts, takes prompts, records them. */
function engine(on: On): PromptSubmitInput[] {
  const submitted: PromptSubmitInput[] = []
  on('session.start', (_, e) => ({ cwd: e.cwd }))
  on('turn.start', (_, e) => ({ turnId: e.turnId }))
  on('turn.complete', (_, e) => ({ text: e.answer }))
  on('prompt.submit', (_, e) => {
    submitted.push(e)

    return { text: e.text }
  })

  return submitted
}

/** Stands for fake-watch: writes `pieces` once `gate` opens, then exits 0. */
function watch(on: On, pieces: readonly string[], gate: Promise<void> = Promise.resolve()): void {
  on('process.spawn', async function* () {
    await gate
    for (const text of pieces) {
      yield { stream: 'stdout', text }
    }

    return { value: { code: 0, signal: null } }
  })
}

const STAT: FsStat = { kind: 'file', size: 1, mtimeMs: 0, isLink: false }

/** Stands for the file system in /work: `real` maps a spelling to where it lands. */
function files(on: On, real: Readonly<Record<string, string>>): void {
  on('session.cwd', () => ({ value: '/work' }))
  on('fs.stat', (_, e) => {
    const realPath = real[e.path]
    if (realPath === undefined) {
      throw new Error(`ENOENT: ${e.path}`)
    }

    return { value: { ...STAT, realPath } }
  })
}

/** Stands for the tools: records each call that got past the plugin. */
function tools(on: On): string[] {
  const ran: string[] = []
  on('tool.call', (_, e) => {
    ran.push(e.tool)

    return { result: {} }
  })

  return ran
}

test('wakeup_on_dm_when_idle', async ($, on) => {
  const submitted = engine(on)
  const session = mock.session(on)
  const clock = mock.clock(on)
  // A line may arrive split across pieces of the child's output.
  watch(on, ['{"seq":1,"verb":"DM",', '"body":"tick 1","to_me":true}\n'])

  await $.session.start(START)
  await clock.settle()

  expect(submitted.length).toBe(1)
  // It names what arrived; the body reaches the model only in the data row.
  expect(submitted[0]?.text).toContain('DM #1')
  expect(submitted[0]?.text).not.toContain('tick 1')
  // Framed: the model reads "The agmen plugin sent a message: ...".
  expect(submitted[0]?.origin).toEqual({ kind: 'plugin', name: 'agmen' })

  const rows = session.appended()
  expect(rows.length).toBe(1)
  expect(rows[0]?.door).toBe('note')
  expect(rows[0]?.message.type).toBe('user')
  expect(JSON.stringify(rows[0]?.message.content)).toContain('tick 1')
})

test('no_wakeup_for_own_message', async ($, on) => {
  const submitted = engine(on)
  const session = mock.session(on)
  const clock = mock.clock(on)
  // The session's own message echoed on the feed, then one addressed to it:
  // only the second wakes the session or reaches it as a row.
  watch(on, [dm(1, false), dm(2, true)])

  await $.session.start(START)
  await clock.settle()

  expect(submitted.map(e => e.text)).toEqual([expect.stringContaining('DM #2')])
  expect(submitted[0]?.text).not.toContain('#1')
  expect(JSON.stringify(session.appended())).not.toContain('tick 1')
  expect(session.appended().length).toBe(1)
})

test('wakeup_waits_for_turn_end', async ($, on) => {
  const submitted = engine(on)
  const clock = mock.clock(on)
  let open = (): void => {}
  const gate = new Promise<void>(resolve => {
    open = resolve
  })
  watch(on, [dm(1, true), dm(2, true)], gate)

  await $.session.start(START)
  await $.turn.start({ text: 'refactor', turnId: 't1' })
  open()
  await clock.settle()

  // Busy: the rows go in, the running turn is not cut into.
  expect(submitted.length).toBe(0)

  await $.turn.complete({
    answer: 'done',
    durationMs: 1,
    isAborted: false,
    turnId: 't1',
    reason: 'answer',
  })
  await clock.settle()

  // One wake-up for both lines, queued once the turn has ended.
  expect(submitted.length).toBe(1)
  expect(submitted[0]?.text).toContain('DM #1, DM #2')
})

test('guard_denies_forbidden_path', async ($, on) => {
  const ran = tools(on)
  files(on, {
    '/work': '/work',
    '/work/forbidden/a.ts': '/work/forbidden/a.ts',
    '/work/src/link.ts': '/work/forbidden/b.ts',
    '/work/src': '/work/src',
    '/work/src/ok.ts': '/work/src/ok.ts',
  })

  const edit = await $.tool.call({
    tool: 'Edit',
    file_path: '/work/forbidden/a.ts',
    old_string: 'a',
    new_string: 'b',
  })
  expect(edit.deny).toContain('forbidden/')

  // A link that lands under forbidden/ is judged by where it lands.
  const write = await $.tool.call({ tool: 'Write', file_path: '/work/src/link.ts', content: 'x' })
  expect(write.deny).toContain('forbidden/')

  const notebook = await $.tool.call({
    tool: 'NotebookEdit',
    notebook_path: '/work/forbidden/a.ts',
    new_source: 'x',
  })
  expect(notebook.deny).toContain('forbidden/')

  // A path outside forbidden/ goes on, one that exists and one not there yet.
  const allowed = await $.tool.call({ tool: 'Write', file_path: '/work/src/ok.ts', content: 'x' })
  expect(allowed.deny).toBeUndefined()
  const created = await $.tool.call({ tool: 'Write', file_path: '/work/src/new.ts', content: 'x' })
  expect(created.deny).toBeUndefined()
  expect(ran).toEqual(['Write', 'Write'])
})

test('guard_denies_on_error', async ($, on) => {
  const ran = tools(on)
  on('fs.stat', () => {
    throw new Error('the file system is gone')
  })

  const edit = await $.tool.call({
    tool: 'Edit',
    file_path: '/work/src/ok.ts',
    old_string: 'a',
    new_string: 'b',
  })

  expect(edit.deny).toContain('guard failed')
  expect(ran).toEqual([])
})
