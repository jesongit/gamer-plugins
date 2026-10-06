import { onScopeDispose, ref, watch } from 'vue'
import { api } from '../../../../../../web/src/api'

export function safeTraceImageUrl(runId, image) {
  if (!runId || !image?.image_id) return ''
  // Never trust a server-supplied URL as a cross-origin image upload or credential sink.
  return `/api/runs/${encodeURIComponent(runId)}/trace/images/${encodeURIComponent(image.image_id)}`
}
export function traceImageFilename(runId, image) {
  const safePart = value => String(value || '').replace(/[^a-zA-Z0-9._-]/g, '_').slice(0, 100)
  return `trace-${safePart(runId)}-${safePart(image?.image_id)}.png`
}
export function traceImagesForNode(images, node) {
  const events = [node, ...(node?.matches || []), ...(node?.details || [])]
  const frameIds = new Set(events.flatMap(event => [event?.trace?.frame_id, event?.frame_id]).filter(value => value !== undefined && value !== null))
  const path = node?.path || node?.trace?.path
  return images.filter(image => path
    ? image.metadata?.path === path && (!frameIds.size || frameIds.has(image.metadata?.frame_id))
    : frameIds.has(image.metadata?.frame_id))
}
export function traceBoxStyle(box, width, height) {
  if (!Array.isArray(box) || box.length !== 4 || !box.every(Number.isFinite) || !(width > 0 && height > 0) || box[2] <= 0 || box[3] <= 0) return null
  const [x, y, w, h] = box
  if (x >= width || y >= height || x + w <= 0 || y + h <= 0) return null
  return { left: `${100 * Math.max(0, x) / width}%`, top: `${100 * Math.max(0, y) / height}%`, width: `${100 * Math.min(w, width - Math.max(0, x)) / width}%`, height: `${100 * Math.min(h, height - Math.max(0, y)) / height}%` }
}
export function useRunTrace(runId, visible, client = api) {
  const images = ref([]), gaps = ref([]), status = ref('unavailable'), enabled = ref(false), loading = ref(false), error = ref(''), hasMore = ref(false), snapshot = ref(null)
  let generation = 0, cursor = 0, inFlight = null
  async function refresh() {
    if (!runId.value || !visible.value || inFlight === generation) return
    const token = generation, id = runId.value
    inFlight = token; loading.value = true
    try {
      const result = await client.getRunTrace(id, cursor, 100)
      if (token !== generation) return
      status.value = result.status; enabled.value = result.enabled; gaps.value = result.gaps || []; snapshot.value = result.snapshot
      images.value = result.status === 'expired' ? [] : [...new Map([...images.value, ...(result.images || [])].map(image => [image.image_id, image])).values()]
      cursor = result.next; hasMore.value = !!result.has_more; error.value = ''
    } catch (e) { if (token === generation) { error.value = e.message; if (e.status === 410) { status.value = 'expired'; images.value = [] } } }
    finally { if (token === generation) { loading.value = false; inFlight = null } }
  }
  watch(runId, () => { generation++; cursor = 0; images.value = []; gaps.value = []; status.value = 'unavailable'; enabled.value = false; loading.value = false; error.value = ''; hasMore.value = false; snapshot.value = null; void refresh() }, { immediate: true, flush: 'sync' })
  watch(visible, value => { if (value) void refresh() })
  const timer = setInterval(refresh, 1500)
  onScopeDispose(() => { generation++; clearInterval(timer) })
  return { images, gaps, status, enabled, loading, error, hasMore, snapshot, refresh }
}
