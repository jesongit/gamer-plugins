import AiWorkspace from './AiWorkspace.vue'
import AiGoalEditor from './AiGoalEditor.vue'
import { registerRunnerEditor } from '../../../web/src/components/task/runner-editors'
import { api } from '../../../web/src/api'
export const sdkVersion = 1
export const panels = { AiWorkspace: { component: AiWorkspace } }
registerRunnerEditor({
  runnerId: 'gamer-ai', title: 'AI 自然语言目标', payloadEditor: AiGoalEditor,
  entrypoints: async ctx => {
    if (!ctx.packageId) return []
    const result = await api.callExtension('gamer-ai', 'plans.read', { package_id: ctx.packageId })
    return [{ value: `${ctx.packageId}#goal`, label: '临时目标' }, ...(result.resources || []).map(p => ({ value: `${ctx.packageId}/${p.path.slice(6)}`, label: p.path.slice(6) }))]
  },
  resolveAppPackages: (_entrypoint, _payload, ctx) => ({ android_package: ctx.androidPackageName || '', content_package: ctx.packageId || null }),
})
