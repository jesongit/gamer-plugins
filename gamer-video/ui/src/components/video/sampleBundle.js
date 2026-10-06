import { Unzip, UnzipInflate, strFromU8 } from 'fflate'

export const SAMPLE_ARCHIVE_LIMIT = 128 * 1024 * 1024
const safePath = path => path.length > 0 && path.length < 200 && path.split('/').every(p => p && p !== '.' && p !== '..' && /^[a-zA-Z0-9._-]+$/.test(p))
export const canonicalSampleJson = value => JSON.stringify(sortKeys(value))
function sortKeys(value) {
  if (Array.isArray(value)) return value.map(sortKeys)
  if (value && typeof value === 'object') return Object.fromEntries(Object.keys(value).sort().map(k => [k, sortKeys(value[k])]))
  return value
}
async function sha256(bytes) {
  if (!globalThis.crypto?.subtle) throw new Error('需要安全上下文（HTTPS 或 localhost）校验素材 SHA256')
  return [...new Uint8Array(await crypto.subtle.digest('SHA-256', bytes))].map(n => n.toString(16).padStart(2, '0')).join('')
}
function base64(bytes) {
  let text = ''
  for (let i = 0; i < bytes.length; i += 32768) text += String.fromCharCode(...bytes.subarray(i, i + 32768))
  return btoa(text)
}
/** Validate ZIP central directory before decompression, including inflated sizes. */
export function inspectZipDirectory(bytes) {
  if (bytes.length > SAMPLE_ARCHIVE_LIMIT || bytes.length < 22) throw new Error('素材包为空或超过 128 MiB')
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength)
  let end = bytes.length - 22
  const min = Math.max(0, end - 65535)
  while (end >= min && view.getUint32(end, true) !== 0x06054b50) end--
  if (end < min || view.getUint16(end + 4, true) || view.getUint16(end + 6, true)) throw new Error('无效 ZIP 或不支持分卷')
  if (end + 22 + view.getUint16(end + 20, true) !== bytes.length || (end >= 20 && view.getUint32(end - 20, true) === 0x07064b50)) throw new Error('ZIP 尾部或 ZIP64 格式无效')
  const count = view.getUint16(end + 10, true)
  if (count !== view.getUint16(end + 8, true) || count === 65535) throw new Error('ZIP 条目计数不一致')
  const centralSize = view.getUint32(end + 12, true)
  let pos = view.getUint32(end + 16, true)
  if (!count || count > 260 || pos + centralSize !== end) throw new Error('ZIP 条目数量或目录范围无效')
  const entries = new Map()
  let total = 0
  for (let i = 0; i < count; i++) {
    if (pos + 46 > end || view.getUint32(pos, true) !== 0x02014b50) throw new Error('ZIP 目录损坏')
    const flags = view.getUint16(pos + 8, true)
    const method = view.getUint16(pos + 10, true)
    const size = view.getUint32(pos + 24, true)
    const nameLength = view.getUint16(pos + 28, true)
    const extraLength = view.getUint16(pos + 30, true)
    const commentLength = view.getUint16(pos + 32, true)
    const mode = view.getUint32(pos + 38, true) >>> 16
    if (view.getUint16(pos + 34, true) || view.getUint32(pos + 42, true) >= view.getUint32(end + 16, true)) throw new Error('ZIP 条目位置无效')
    const next = pos + 46 + nameLength + extraLength + commentLength
    if (next > end || flags & 1 || ![0, 8].includes(method) || (mode & 0xf000) === 0xa000) throw new Error('ZIP 使用不支持或不安全的条目')
    for (let extra = pos + 46 + nameLength; extra < pos + 46 + nameLength + extraLength;) {
      if (extra + 4 > next || view.getUint16(extra, true) === 1) throw new Error('不支持 ZIP64')
      extra += 4 + view.getUint16(extra + 2, true)
      if (extra > pos + 46 + nameLength + extraLength) throw new Error('ZIP 扩展字段损坏')
    }
    const path = strFromU8(bytes.subarray(pos + 46, pos + 46 + nameLength))
    total += size
    if (!safePath(path) || entries.has(path) || total > SAMPLE_ARCHIVE_LIMIT || (path === 'manifest.json' && size > 2 * 1024 * 1024)) throw new Error('ZIP 路径、重复条目或解压大小无效')
    entries.set(path, size)
    pos = next
  }
  if (pos !== end) throw new Error('ZIP 目录长度不一致')
  if (!entries.has('manifest.json')) throw new Error('缺少 manifest.json')
  return entries
}
/** Bounded streaming decompression: never trust a claimed inflated size.
 * Synchronous unzipSync allocates the advertised output buffer and can silently
 * truncate a larger actual stream. Feed small compressed chunks, inspect every
 * actual output chunk, and yield between batches so valid imports stay usable.
 */
async function unzipBounded(bytes, index) {
  const files = {}, started = new Set(), finished = new Set()
  let failure = null, total = 0
  const unzip = new Unzip(file => {
    const expected = index.get(file.name)
    if (expected === undefined || started.has(file.name)) { failure = new Error('ZIP 实际条目与已校验目录不符'); return }
    started.add(file.name)
    const chunks = []; let received = 0
    file.ondata = (error, data, final) => {
      if (failure) return
      if (error) { failure = error; return }
      received += data.length; total += data.length
      if (received > expected || total > SAMPLE_ARCHIVE_LIMIT) {
        failure = new Error('ZIP 实际解压大小超过声明或安全上限')
        file.terminate?.()
        return
      }
      if (data.length) chunks.push(data)
      if (final) {
        if (received !== expected) { failure = new Error('ZIP 实际解压大小不匹配'); return }
        const output = new Uint8Array(received); let offset = 0
        for (const chunk of chunks) { output.set(chunk, offset); offset += chunk.length }
        files[file.name] = output; finished.add(file.name)
      }
    }
    file.start()
  })
  unzip.register(UnzipInflate)
  for (let offset = 0; offset < bytes.length; offset += 4096) {
    const end = Math.min(bytes.length, offset + 4096)
    unzip.push(bytes.subarray(offset, end), end === bytes.length)
    if (failure) throw failure
    if (offset && offset % (4096 * 32) === 0) await new Promise(resolve => setTimeout(resolve, 0))
  }
  if (failure) throw failure
  if (started.size !== index.size || finished.size !== index.size) throw new Error('ZIP 有缺失或未完成的条目')
  return files
}

/** Portable import works without a running video plugin; server validates again. */
export async function inspectSampleArchive(input) {
  if (typeof input?.size === 'number' && input.size > SAMPLE_ARCHIVE_LIMIT) throw new Error('素材包超过 128 MiB，请缩短录制或分段制作')
  const bytes = input instanceof Uint8Array ? input : new Uint8Array(await input.arrayBuffer())
  const index = inspectZipDirectory(bytes)
  const files = await unzipBounded(bytes, index)
  const manifest = JSON.parse(strFromU8(files['manifest.json']))
  if (manifest.schema_version !== 1 || !/^[a-z0-9-]{1,80}$/.test(manifest.id || '')) throw new Error('素材格式或 ID 无效')
  if (!Array.isArray(manifest.files) || !Array.isArray(manifest.frames) || manifest.frames.length > 240) throw new Error('素材证据目录无效')
  if (Object.keys(files).length !== index.size || manifest.files.length !== index.size - 1) throw new Error('素材文件集合不匹配')
  const seen = new Set()
  for (const file of manifest.files) {
    const body = files[file.path]
    if (seen.has(file.path) || !body || body.length !== file.size || index.get(file.path) !== body.length || await sha256(body) !== file.sha256) throw new Error(`素材完整性校验失败：${file.path}`)
    seen.add(file.path)
  }
  const expected = await sha256(new TextEncoder().encode(canonicalSampleJson({ ...manifest, content_sha256: '' })))
  if (expected !== manifest.content_sha256) throw new Error('素材 manifest SHA256 不匹配')
  return {
    manifest,
    files: manifest.frames.map(frame => {
      const body = files[frame.path]
      if (!body) throw new Error(`缺少画面：${frame.path}`)
      return { path: frame.path, base64: base64(body) }
    }),
    bytes,
  }
}
