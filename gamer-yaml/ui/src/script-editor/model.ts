/** Parameter and value models; YAML source is owned by the server parser. */
export const PARAM_TYPES = ['any','boolean','integer','number','string','list','object','duration','point','template','key'] as const
export type ParamType = (typeof PARAM_TYPES)[number]
export interface ParamDecl { items?: {type: ParamType}; name:string; type:ParamType; required:boolean; default:unknown|null; hasDefault?:boolean; desc:string }
export type Cell = {lit:unknown;ref?:undefined;missing?:boolean}|{ref:string;lit?:undefined;missing?:boolean}
export function isRefCell(cell:Cell|null|undefined):cell is {ref:string}{return cell!=null && typeof cell.ref==='string'}
