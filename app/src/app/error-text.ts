/** 后端 invoke 错误（string）/ JS Error / 其他 → 展示用文本。 */
export function errText(e: unknown): string {
  return typeof e === "string" ? e : e instanceof Error ? e.message : String(e);
}
