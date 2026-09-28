import { computed, ref } from 'vue'
import { parseFunctionLibrary, serialize } from '../script-editor/codec'

/**
 * 原文 YAML 编辑会话：脚本保留服务端原文；指定函数时只展示该定义，
 * 保存时合并回加载时的函数库，并用资源版本保护其他函数。
 */
export function useRawYamlEditor({ api } = {}) {
  const kind = ref(null) // 'script' | 'function'
  const resourceId = ref(null)
  const content = ref('')
  const savedContent = ref('')
  const version = ref(null)
  const loading = ref(false)
  const saving = ref(false)
  const functionName = ref('')
  let functionLibrary = null

  const dirty = computed(() => content.value !== savedContent.value)

  async function load(nextKind, id, options = {}) {
    if (nextKind !== 'script' && nextKind !== 'function') {
      throw new Error('不支持的原文资源类型')
    }
    if (!id) throw new Error('原文资源不能为空')
    loading.value = true
    try {
      const data = nextKind === 'script'
        ? await api.getScript(id)
        : await api.getFunction(id)
      let nextContent = data.content ?? ''
      let library = null
      if (nextKind === 'function' && options.functionName) {
        const parsed = parseFunctionLibrary(nextContent)
        if (parsed.diagnostics.length) throw new Error(parsed.diagnostics[0].message)
        const fn = parsed.model.functions.find(fn => fn.name === options.functionName)
        if (!fn) throw new Error(`函数 ${options.functionName} 已不存在，请刷新列表`)
        library = parsed.model
        nextContent = serialize({ ...library, functions: [fn] })
      }
      kind.value = nextKind
      resourceId.value = data.id || id
      functionName.value = options.functionName || ''
      functionLibrary = library
      content.value = nextContent
      savedContent.value = content.value
      version.value = data.version ?? null
      return data
    } finally {
      loading.value = false
    }
  }

  async function save() {
    if (!resourceId.value || !kind.value) return { ok: false, reason: 'empty' }
    saving.value = true
    try {
      const submittedContent = content.value
      let wholeContent = submittedContent, nextLibrary = functionLibrary, nextName = functionName.value
      if (functionLibrary) {
        const parsed = parseFunctionLibrary(submittedContent)
        if (parsed.diagnostics.length) return { ok: false, reason: 'invalid', diagnostics: parsed.diagnostics }
        if (parsed.model.functions.length !== 1) throw new Error('这里只能编辑当前一个函数，请保留一个函数定义')
        const fn = parsed.model.functions[0]
        if (fn.name !== functionName.value && functionLibrary.functions.some(item => item.name === fn.name)) {
          throw new Error(`已存在同名函数：${fn.name}`)
        }
        nextLibrary = { ...functionLibrary, functions: functionLibrary.functions.map(item => item.name === functionName.value ? fn : item) }
        nextName = fn.name
        wholeContent = serialize(nextLibrary)
      }
      const payload = version.value
        ? { content: wholeContent, expected_version: version.value }
        : { content: wholeContent, force: true }
      const result = kind.value === 'script'
        ? await api.updateScript(resourceId.value, payload)
        : await api.updateFunction(resourceId.value, payload)
      savedContent.value = submittedContent
      functionLibrary = nextLibrary
      functionName.value = nextName
      version.value = result.version ?? version.value
      return { ok: true, result, functionName: nextName }
    } catch (error) {
      if (error?.status === 409 && error?.data?.code === 'version_conflict') {
        return { ok: false, reason: 'conflict', error }
      }
      if (Array.isArray(error?.data?.diagnostics)) {
        return { ok: false, reason: 'invalid', diagnostics: error.data.diagnostics, error }
      }
      return { ok: false, reason: 'error', error }
    } finally {
      saving.value = false
    }
  }

  function reset() {
    kind.value = null
    resourceId.value = null
    content.value = ''
    savedContent.value = ''
    version.value = null
    loading.value = false
    saving.value = false
    functionName.value = ''
    functionLibrary = null
  }

  return {
    kind,
    resourceId,
    content,
    version,
    loading,
    saving,
    dirty,
    functionName,
    load,
    save,
    reset,
  }
}
