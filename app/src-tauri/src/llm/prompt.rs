//! Prompt 构造：system（角色与规则）/ user（文档全文 + 列说明）/ reask（错误反馈）。

use crate::llm::{ColumnSpec, ExtractMode};

pub fn system_prompt(mode: ExtractMode) -> String {
    match mode {
        ExtractMode::WithCitation => "你是合同文档信息提取助手。请从用户提供的中文扫描件 OCR 文本中提取指定字段。
要求：
1. 只能依据原文提取，不得编造，不得使用原文之外的知识。
2. 每个字段按 reasoning → quote → value 输出：
   - reasoning：定位依据，简述在原文何处找到该信息；
   - quote：原文逐字引用，不得改写任何字符（包括标点符号；原文跨行处已直接拼接、无换行符）；
   - value：规整后的提取值；金额保留原文千分位格式；若原文中找不到该信息，value 与 quote 均为空字符串。
3. 只输出符合 JSON Schema 的 JSON 对象，不要输出任何其他内容。"
            .to_string(),
        ExtractMode::DirectValue => "你是合同文档信息提取助手。请从用户提供的中文扫描件 OCR 文本中提取指定字段。
要求：
1. 只能依据原文提取，不得编造，不得使用原文之外的知识。
2. 每个字段只输出 value：提取值；金额保留原文千分位格式；若原文中找不到该信息，输出空字符串。
3. 只输出符合 JSON Schema 的 JSON 对象，不要输出任何其他内容。"
            .to_string(),
    }
}

/// user 消息：文档全文（已含【第N页】分隔）+ 列说明。
/// TODO(后续优化)：文档超长（超出模型上下文）时目前不做截断，直接全量下发。
pub fn user_prompt(doc_text: &str, cols: &[ColumnSpec], mode: ExtractMode) -> String {
    let mut lines = String::from("以下是文档全文（OCR 识别结果，按页给出）：\n\n");
    lines.push_str(doc_text);
    lines.push_str("\n\n需要提取的字段（输出 JSON 的键 → 字段含义）：\n");
    for c in cols {
        lines.push_str(&format!("- col_{}：{}\n", c.column_index, c.describe()));
    }
    match mode {
        ExtractMode::WithCitation => {
            lines.push_str("\n请按 JSON Schema 输出，每列含 reasoning、quote、value 三个字段。");
        }
        ExtractMode::DirectValue => {
            lines.push_str("\n请按 JSON Schema 输出，每列只含 value 字段。");
        }
    }
    lines
}

/// reask 反馈消息：逐条列出校验失败原因（Instructor 方法论 ④ 错误反馈重试）。
pub fn reask_prompt(errors: &[String]) -> String {
    let mut s = String::from("上一次输出的 JSON 未通过校验：\n");
    for e in errors {
        s.push_str(&format!("- {e}\n"));
    }
    s.push_str(
        "\n请修正后重新输出完整的 JSON 对象（只输出 JSON，不要输出解释）。\
                注意：quote 必须是原文逐字引用，一个字符都不能改写。",
    );
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cols() -> Vec<ColumnSpec> {
        vec![
            ColumnSpec {
                column_index: 2,
                keyword: "合同编号".into(),
                ..Default::default()
            },
            ColumnSpec {
                column_index: 3,
                keyword: "甲方".into(),
                ..Default::default()
            },
        ]
    }

    #[test]
    fn user_prompt_contains_doc_keywords_and_rules() {
        let p = user_prompt(
            "【第1页】\n正文内容测试",
            &cols(),
            ExtractMode::WithCitation,
        );
        assert!(p.contains("【第1页】"));
        assert!(p.contains("正文内容测试"));
        assert!(p.contains("col_2：合同编号"));
        assert!(p.contains("col_3：甲方"));
        assert!(p.contains("reasoning、quote、value"));
    }

    #[test]
    fn system_prompt_states_verbatim_quote_rule() {
        let s = system_prompt(ExtractMode::WithCitation);
        assert!(s.contains("逐字引用"));
        assert!(s.contains("空字符串"));
        let direct = system_prompt(ExtractMode::DirectValue);
        assert!(direct.contains("只输出 value") || direct.contains("只含 value"));
    }

    #[test]
    fn reask_lists_errors() {
        let r = reask_prompt(&["col_3：quote 未命中".into(), "col_2：缺少字段 value".into()]);
        assert!(r.contains("col_3：quote 未命中"));
        assert!(r.contains("col_2：缺少字段 value"));
        assert!(r.contains("逐字引用"));
    }

    #[test]
    fn user_prompt_contains_definition_and_info_type() {
        let cols = vec![
            ColumnSpec {
                column_index: 2,
                keyword: "金额".into(),
                definition: "指甲乙双方约定的含税总金额".into(),
                info_type: "number".into(),
            },
            ColumnSpec {
                column_index: 3,
                keyword: "签订日期".into(),
                info_type: "date".into(),
                ..Default::default()
            },
            // 无选填项：提示词保持只有关键词（行为同旧版）
            ColumnSpec {
                column_index: 4,
                keyword: "甲方".into(),
                ..Default::default()
            },
        ];
        let p = user_prompt("【第1页】\n正文", &cols, ExtractMode::WithCitation);
        assert!(p.contains("col_2：金额（信息类型：数字；定义：指甲乙双方约定的含税总金额）"));
        assert!(p.contains("col_3：签订日期（信息类型：日期）"));
        assert!(p.contains("- col_4：甲方\n"));
    }
}
