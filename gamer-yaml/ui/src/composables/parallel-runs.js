/** Submit one frozen script/argument set independently on each selected target. */
export async function submitParallelRuns({ targetIds, config, submit, active, pending = new Set(), onResult = () => {} }) {
  const snapshot = JSON.parse(JSON.stringify(config))
  const ids = [...new Set(targetIds)]
  return Promise.all(ids.map(async targetId => {
    let result
    if (pending.has(targetId) || active(targetId)) result = { targetId, status: 'skipped', reason: '账号已有运行' }
    else {
      pending.add(targetId)
      onResult({ targetId, status: 'starting' })
      try {
        const rep = await submit({ device_id: targetId, content_package: snapshot.packageId, runner_id: snapshot.runnerId, entrypoint: snapshot.entrypoint, payload: { args: snapshot.args } })
        result = { targetId, status: 'started', runId: rep.run_id, record: rep }
      } catch (e) { result = { targetId, status: 'error', reason: e.message || String(e) } }
      finally { pending.delete(targetId) }
    }
    onResult(result)
    return result
  }))
}
