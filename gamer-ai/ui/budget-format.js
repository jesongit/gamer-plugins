export const DEFAULT_LIMITS = {max_turns:40,max_actions:120,max_seconds:600,max_tokens:100000,max_failures:3}
export const LIMIT_FIELDS = [
  {key:'max_turns',label:'模型轮数',min:1,max:500},
  {key:'max_actions',label:'工具次数',min:1,max:2000},
  {key:'max_seconds',label:'活动秒数',min:10,max:7200},
  {key:'max_tokens',label:'累计 Token',min:2048,max:2000000},
  {key:'max_failures',label:'连续失败',min:1,max:20},
]
export const validLimits = limits => LIMIT_FIELDS.every(field => Number.isInteger(limits[field.key]) && limits[field.key]<=field.max && (limits[field.key]===0 || limits[field.key]>=field.min))
