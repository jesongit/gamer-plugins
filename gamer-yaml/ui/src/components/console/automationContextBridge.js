import { pluginMessageChannel } from '../../../../../../web/src/workspace/plugin-messages'

/** Open the exact failed source version for user review. Never starts a model,
 * a private run, a publication or a source edit. */
export function requestRunSourceReview(packageId, scriptId, runId) {
  if (!packageId || !scriptId || !runId) return false
  const request = pluginMessageChannel('gamer-yaml:open-generation')
  request.packageId = String(packageId)
  request.candidateId = null
  request.scriptId = String(scriptId)
  request.repairRun = { scriptId: String(scriptId), runId: String(runId) }
  request.seq += 1
  return true
}
