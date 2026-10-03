import {expect,it} from 'vitest'
import {DEFAULT_LIMITS,LIMIT_FIELDS,validLimits} from './budget-format'
it('聊天与导入共用五项0规则，非零范围、空值和负数不会提交',()=>{
 const zero=Object.fromEntries(LIMIT_FIELDS.map(field=>[field.key,0]))
 expect(validLimits(zero)).toBe(true)
 expect(validLimits(DEFAULT_LIMITS)).toBe(true)
 for(const field of LIMIT_FIELDS){expect(validLimits({...zero,[field.key]:field.max+1})).toBe(false);expect(validLimits({...zero,[field.key]:-1})).toBe(false);expect(validLimits({...zero,[field.key]:''})).toBe(false)}
 expect(validLimits({...zero,max_seconds:9})).toBe(false)
})
