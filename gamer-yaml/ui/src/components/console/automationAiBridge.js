import { pluginMessageChannel } from '../../../../../../web/src/workspace/plugin-messages'
export const automationAiRequest = pluginMessageChannel('gamer-ai:automation-context')
export function requestAutomationContext(packageId, automation) {
  if (!packageId || !automation?.script_id) return false
  automationAiRequest.packageId = String(packageId)
  automationAiRequest.automation = { script_id: String(automation.script_id), ...(automation.run_id ? { run_id: String(automation.run_id) } : {}), ...(automation.candidate_id ? { candidate_id: String(automation.candidate_id) } : {}) }
  automationAiRequest.seq += 1
  return true
}
