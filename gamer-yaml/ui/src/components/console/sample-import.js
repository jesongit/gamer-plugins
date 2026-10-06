/** Self-contained JSON envelope shared with the video sample export action. No server paths are accepted. */
export function parseSampleFile(text) {
  const value = JSON.parse(text)
  const sample = value.sample || value
  if (!sample.manifest || !Array.isArray(sample.files)) throw new Error('请选择包含 manifest 和 files 的自包含素材包 JSON')
  const seen = new Set()
  for (const file of sample.files) {
    if (typeof file.path !== 'string' || !file.path || file.path.startsWith('/') || file.path.includes('\\') || file.path.split('/').some(part => part === '..' || !part) || seen.has(file.path)) throw new Error('素材包含重复或不安全的资源路径')
    if (typeof file.base64 !== 'string' || !file.base64 || !/^[A-Za-z0-9+/]*={0,2}$/.test(file.base64)) throw new Error(`素材资源内容无效：${file.path}`)
    seen.add(file.path)
  }
  if (!sample.files.length) throw new Error('素材包缺少图片证据')
  return sample
}
export function sampleId(sample) { return sample.manifest?.sample_id || sample.manifest?.id || '' }
