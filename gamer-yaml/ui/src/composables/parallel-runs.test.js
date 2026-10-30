import { expect, it, vi } from 'vitest'
import { submitParallelRuns } from './parallel-runs'

it('submits different accounts concurrently with the same frozen arguments and isolates rejection', async () => {
  let finish
  const gate = new Promise(resolve => { finish = resolve })
  const calls = [], results = []
  const config = { packageId: 'hsr', runnerId: 'gamer-yaml', entrypoint: 'hsr/redeem.yaml', args: { codes: ['甲', '乙', '丙'] } }
  const submit = vi.fn(async request => { calls.push(request); await gate; if (request.device_id === 'bad') throw new Error('conflict'); return { run_id: request.device_id } })
  const job = submitParallelRuns({ targetIds: ['a', 'bad', 'a', 'b', 'busy'], config, submit, active: id => id === 'busy', onResult: r => results.push(r) })
  expect(calls.map(r => r.device_id)).toEqual(['a', 'bad', 'b'])
  config.args.codes.push('changed')
  expect(calls.every(r => r.payload.args.codes.length === 3)).toBe(true)
  finish()
  const final = await job
  expect(final.map(r => r.status)).toEqual(['started', 'error', 'started', 'skipped'])
  expect(final[1].reason).toBe('conflict')
  expect(results.filter(r => r.status === 'started')).toHaveLength(2)
})

it('blocks a repeated submission while that account is being accepted', async () => {
  let finish
  const pending = new Set()
  const submit = vi.fn(() => new Promise(resolve => { finish = resolve }))
  const options = { targetIds: ['a'], config: { args: {} }, submit, pending, active: () => false }
  const first = submitParallelRuns(options)
  expect((await submitParallelRuns(options))[0].status).toBe('skipped')
  finish({ run_id: 'r1' }); await first
  expect(pending.size).toBe(0)
  expect(submit).toHaveBeenCalledTimes(1)
})
