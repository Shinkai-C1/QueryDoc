/**
 * 关键词列的共享视图模型与工具函数。
 * 供 keyword-modal（编辑草稿）与 extract-table（结果表头）共用。
 * 列索引（columnIndex）仅作后端/数据逻辑用，不在前端展示为字母列号。
 */

/** 信息类型（选填）：空串=未设置。 */
export type InfoType = "" | "text" | "number" | "date";

/** 关键词定义长度上限（字符数，与后端 DEFINITION_MAX_CHARS 一致）。 */
export const DEFINITION_MAX_CHARS = 200;

export interface KeywordCol {
  columnIndex: number;
  keyword: string;
  /** 关键词定义（选填，注入 LLM 提示词提高准确度）。 */
  definition: string;
  /** 信息类型（选填）。 */
  infoType: InfoType;
}

/** 草稿/已保存一致性比较的规范化串（关键词/定义 trim 后参与比较）。 */
export function normalize(cols: KeywordCol[]): string {
  return JSON.stringify(
    cols
      .map((c) => ({
        columnIndex: c.columnIndex,
        keyword: c.keyword.trim(),
        definition: c.definition.trim(),
        infoType: c.infoType,
      }))
      .sort((a, b) => a.columnIndex - b.columnIndex),
  );
}
