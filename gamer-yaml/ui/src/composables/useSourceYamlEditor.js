import { computed, ref } from 'vue'

/** Source is the only editable representation. Never deserialize/re-serialize a document. */
export function useSourceYamlEditor({ api, call }) {
  const content = ref(''), savedContent = ref(''), id = ref(''), name = ref(''), version = ref(null)
  const loading = ref(false), saving = ref(false), validating = ref(false), diagnostics = ref([]), error = ref('')
  const valid = ref(false), conflict = ref(false)
  let generation = 0, validationRequest = 0, kind = 'script', packageId = ''
  const dirty = computed(() => content.value !== savedContent.value || !!name.value && !id.value)
  function reset(nextPackage = '', nextKind = 'script') {
    generation++; validationRequest++; kind = nextKind; packageId = nextPackage
    content.value = ''; savedContent.value = ''; id.value = ''; name.value = ''; version.value = null
    loading.value = false; saving.value = false; validating.value = false; diagnostics.value = []; error.value = ''; valid.value = false; conflict.value = false
  }
  function edited() { validationRequest++; validating.value = false; valid.value = false; diagnostics.value = []; error.value = ''; conflict.value = false }
  function create(filename, source) { reset(packageId, kind); name.value = filename; content.value = source }
  async function load(resourceId) {
    const token = ++generation; validationRequest++; loading.value = true; error.value = ''; valid.value = false
    try {
      const result = await (kind === 'script' ? api.getScript(resourceId) : api.getFunction(resourceId))
      if (token !== generation) return false
      id.value = result.id || resourceId; name.value = id.value.slice(id.value.indexOf('/') + 1)
      content.value = result.content ?? ''; savedContent.value = content.value; version.value = result.version ?? null
      diagnostics.value = []; conflict.value = false; return true
    } catch (e) { if (token === generation) error.value = e.message; return false }
    finally { if (token === generation) loading.value = false }
  }
  async function validate() {
    if (!packageId || loading.value || saving.value) return false
    const token = generation, request = ++validationRequest, source = content.value
    validating.value = true; error.value = ''
    try {
      const result = await call('automation.validate_source', { package_id: packageId, yaml: source, kind })
      if (token !== generation || request !== validationRequest || source !== content.value) return false
      diagnostics.value = result.diagnostics || []; valid.value = result.valid === true && diagnostics.value.length === 0
      return valid.value
    } catch (e) { if (token === generation && request === validationRequest) { error.value = e.message; valid.value = false } return false }
    finally { if (token === generation && request === validationRequest) validating.value = false }
  }
  async function save() {
    if (!packageId || !name.value.trim() || saving.value || loading.value) return false
    const token = generation, source = content.value, target = id.value, filename = name.value.trim()
    saving.value = true; error.value = ''; conflict.value = false
    try {
      if (target && !version.value) throw new Error('资源版本缺失，请重新加载后再保存')
      const payload = { content: source, ...(target ? { expected_version: version.value } : { pkg: packageId, name: filename }) }
      const result = target
        ? await (kind === 'script' ? api.updateScript(target, payload) : api.updateFunction(target, payload))
        : await (kind === 'script' ? api.createScript(payload) : api.createFunction(payload))
      if (token !== generation) return false
      id.value = result.id || target || `${packageId}/${filename}`; version.value = result.version ?? null; savedContent.value = source
      diagnostics.value = []; valid.value = content.value === source; return true
    } catch (e) {
      if (token === generation) { error.value = e.message; diagnostics.value = e.data?.diagnostics || e.details || []; conflict.value = e.status === 409; valid.value = false }
      return false
    } finally { if (token === generation) saving.value = false }
  }
  return { content, savedContent, id, name, version, dirty, loading, saving, validating, diagnostics, valid, conflict, error, reset, create, load, edited, validate, save }
}
