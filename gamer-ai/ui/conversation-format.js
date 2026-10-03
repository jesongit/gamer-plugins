import { TOOL_LABELS, eventDetails, safeImage } from './ai-format'

export const DELIVERY_LABELS = { queued: '待处理', received: '已接收', incorporated: '已纳入', withdrawn: '已撤回', cancelled: '已中断', unknown: '接收状态未知' }
export function toolReceipt(result) {
  if(result?.structuredContent && typeof result.structuredContent==='object') return result.structuredContent
  if(result?.content) {
    for(const content of result.content) if(content.type==='text') {try{return JSON.parse(content.text)}catch{/* Ordinary result text is displayed in the tool details. */}}
  }
  return result && typeof result==='object' ? result : {}
}
function toolImage(data) {
  const direct=safeImage(data.image_data_url)
  if(direct) return direct
  const image=data.result?.content?.find(content=>content.type==='image' && typeof content.data==='string')
  return image?safeImage(`data:${image.mimeType};base64,${image.data}`):''
}
export function mergeEvents(current, incoming) {
  const bySeq = new Map(current.map(event => [event.seq, event]))
  for (const event of incoming || []) if (Number.isSafeInteger(event.seq)) bySeq.set(event.seq, event)
  return [...bySeq.values()].sort((left, right) => left.seq - right.seq)
}
export function conversationTurns(events) {
  const turns = [], byTurn = new Map(), messages = new Map(), calls = new Map(), owners = new Map()
  let fallback = 'history'
  function turn(id) {
    if (!byTurn.has(id)) { const value = { id, users: [], process: [], answers: [], notices: [], completed: false }; byTurn.set(id, value); turns.push(value) }
    return byTurn.get(id)
  }
  for (const event of events) {
    const data = event.data || {}
    if (event.kind === 'user') fallback = data.turn_id || data.message_id || `user:${event.seq}`
    const group = turn(data.turn_id || fallback)
    if (event.kind === 'user') {
      const id = data.message_id || `user:${event.seq}`
      if (messages.has(id)) continue
      const value = { id, kind: 'user', text: event.message, status: data.status || 'received', at: event.at, seq: event.seq }
      messages.set(id, value); owners.set(id,group); group.users.push(value)
    } else if (event.kind === 'user_status') {
      const value = messages.get(data.message_id)
      if (value) {
        value.status = data.status || value.status
        const previous=owners.get(data.message_id)
        if(data.turn_id && previous && previous!==group) {previous.users=previous.users.filter(message=>message.id!==value.id);group.users.push(value);owners.set(data.message_id,group)}
      }
    } else if (event.kind === 'assistant_delta' || event.kind === 'assistant_final') {
      const channel = data.channel || 'text'
      if (!['text', 'summary', 'thinking'].includes(channel)) continue
      const id = `${data.message_id || `assistant:${event.seq}`}:${channel}`
      let value = messages.get(id)
      if (!value) {
        value = { id, kind: channel === 'text' ? 'answer' : 'thinking', label: channel === 'summary' ? '公开摘要' : '公开思考', text: '', status: 'streaming', at: event.at, seq: event.seq, stepId: data.step_id }
        messages.set(id, value); (channel === 'text' ? group.answers : group.process).push(value)
      }
      if (event.kind === 'assistant_final') {
        value.text = data.text ?? event.message ?? value.text; value.status = data.interrupted ? 'interrupted' : 'complete'
        if(Array.isArray(data.summary) && data.summary.length) {
          const summaryId=`${data.message_id}:summary`,summary=messages.get(summaryId)
          if(summary) {summary.text=data.summary.join('\n');summary.status='complete'}
          else {const publicSummary={id:summaryId,kind:'thinking',label:'公开摘要',text:data.summary.join('\n'),status:'complete',seq:event.seq,at:event.at};messages.set(summaryId,publicSummary);group.process.push(publicSummary)}
        }
      }
      else value.text += data.delta || ''
    } else if (event.kind === 'tool_start' || event.kind === 'tool_end') {
      const key = `${data.turn_id || fallback}:${data.step_id || ''}:${data.operation_id || data.call_id || event.seq}`
      let value = calls.get(key)
      if (!value) {
        value = { id: `tool:${key}`, kind: 'tool', name: data.name, label: TOOL_LABELS[data.name] || data.name || '工具', seq: event.seq, at: event.at, stepId: data.step_id, data: {}, status: 'running' }
        calls.set(key, value); group.process.push(value)
      }
      value.data = { ...value.data, ...data }; value.message = event.message
      if (event.kind === 'tool_end') value.status = data.ok === false || data.result?.isError ? 'failed' : 'complete'
      value.image = toolImage(data);value.receipt=toolReceipt(data.result)
    } else if (event.kind === 'state') {
      if (['idle', 'cancelled', 'error'].includes(data.state)) group.completed = true
      if (group.completed) for (const value of [...group.process, ...group.answers]) {
        if (value.status === 'streaming') value.status = data.state === 'idle' && value.kind === 'thinking' ? 'complete' : 'interrupted'
      }
      if (['cancelled', 'error', 'paused'].includes(data.state)) group.notices.push({ ...event, id: `state:${event.seq}`, text: data.detail || event.message })
    } else if (event.kind === 'compression') {
      group.notices.push({ ...event, id: `compression:${event.seq}`, text: event.message || '早期过程已压缩，用户指令及相关记忆保留。' })
    }
  }
  return turns.filter(group => group.users.length || group.process.length || group.answers.length || group.notices.length)
    .sort((left,right)=>Math.min(...[...left.users,...left.process,...left.answers,...left.notices].map(item=>item.seq)) - Math.min(...[...right.users,...right.process,...right.answers,...right.notices].map(item=>item.seq)))
}
export function diagnosticCategory(event) {
  const category = event.data?.category
  if (category) return category
  if (event.kind.startsWith('tool')) return 'tool'
  if (event.kind.startsWith('memory')) return 'memory'
  if (event.kind === 'state') return 'state'
  if (event.kind.includes('error') || event.data?.error) return 'error'
  return 'model'
}
export function safeDiagnostic(value) {
  return JSON.parse(eventDetails(value) || '{}')
}
export function lineDiff(before, after) {
  const left = String(before || '').split('\n'), right = String(after || '').split('\n')
  const rows = [], length = Math.max(left.length, right.length)
  for (let index = 0; index < length; index++) {
    if (left[index] === right[index]) rows.push({ type: 'same', text: left[index] })
    else { if (left[index] != null) rows.push({ type: 'removed', text: left[index] }); if (right[index] != null) rows.push({ type: 'added', text: right[index] }) }
  }
  return rows
}
