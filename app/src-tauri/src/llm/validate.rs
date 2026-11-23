//! 引用校验器（Instructor 方法论 ③ citation validator）：
//! quote 必须在文档 text_flow 拼接版上命中，返回原文 offset 区间供坐标回写。
//! 五级匹配（全部为连续子串语义，UIE 式 start/end，字词顺序保持）：
//! ①精确子串 → ②规范化（全角→半角、去空白与页/块分隔）→ ③金额规范化
//!（数字串内千分位逗号与货币符号忽略）→ ④近似匹配（Sellers 子串编辑距离，
//! 容忍 OCR/LLM 断行后的单字误读、多字、缺字）→ ⑤尽力近似（更大编辑距离容差，
//! 低置信度 + suspect 软标记）。结构校验：JSON 解析 + 每列字段齐全。
//! 召回优先：五级均未命中不再判负（编排层转 suspect 软标记，见 llm/mod.rs）。

use serde_json::Value;

use crate::llm::ColumnSpec;

/// 命中档位（置信系数见 [`MatchTier::factor`]）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchTier {
    /// 精确子串
    Exact,
    /// 规范化（全角→半角、去空白）
    Normalized,
    /// 金额规范化（去千分位逗号、忽略货币符号）
    Amount,
    /// 近似匹配（编辑距离 ≤ ~12%，容忍 OCR/LLM 单字误读）
    Fuzzy,
    /// 尽力近似（编辑距离 ≤ ~33%）：定位到原文最相似区间，供红框参考定位；
    /// 低置信度且对应列标 suspect 提示人工复核
    Approx,
}

impl MatchTier {
    pub fn factor(self) -> f64 {
        match self {
            MatchTier::Exact => 1.0,
            MatchTier::Normalized => 0.9,
            MatchTier::Amount => 0.85,
            // 近似命中需人工复核（系数压低）
            MatchTier::Fuzzy => 0.75,
            // 尽力近似：定位仅供参考，置信度压到明显低档
            MatchTier::Approx => 0.3,
        }
    }

    /// 档位名（日志/A-B 输出用）。
    #[allow(dead_code)]
    pub fn name(self) -> &'static str {
        match self {
            MatchTier::Exact => "exact",
            MatchTier::Normalized => "normalized",
            MatchTier::Amount => "amount",
            MatchTier::Fuzzy => "fuzzy",
            MatchTier::Approx => "approx",
        }
    }
}

/// 命中区间：原文（context.text）上的字符区间 [start, end)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MatchSpan {
    pub start: usize,
    pub end: usize,
    pub tier: MatchTier,
}

/// 规范化文本：比较用字符序列 + 回原文字符 offset 的映射。
struct NormText {
    chars: Vec<char>,
    map: Vec<usize>,
}

/// 全角 ASCII（！-～）→ 半角；全角空格 → 空格。
fn full_to_half(c: char) -> char {
    match c {
        '\u{3000}' => ' ',
        c if ('\u{FF01}'..='\u{FF5E}').contains(&c) => {
            char::from_u32(c as u32 - 0xFEE0).unwrap_or(c)
        }
        c => c,
    }
}

/// 二级规范化：全角→半角；丢弃空白与 keep=false 的字符（页分隔标记、块间换行）。
fn normalize(orig: &[char], keep: Option<&[bool]>) -> NormText {
    let mut chars = Vec::with_capacity(orig.len());
    let mut map = Vec::with_capacity(orig.len());
    for (i, &c) in orig.iter().enumerate() {
        if keep.is_some_and(|k| !k[i]) {
            continue;
        }
        let c = full_to_half(c);
        if c.is_whitespace() {
            continue;
        }
        chars.push(c);
        map.push(i);
    }
    NormText { chars, map }
}

/// 三级金额规范化：在二级基础上丢弃「数字之间的逗号」与货币符号（¥ ￥ $）。
fn amount_normalize(n: &NormText) -> NormText {
    let mut chars = Vec::with_capacity(n.chars.len());
    let mut map = Vec::with_capacity(n.chars.len());
    let len = n.chars.len();
    for (i, &c) in n.chars.iter().enumerate() {
        let drop = match c {
            ',' => {
                i > 0
                    && i + 1 < len
                    && n.chars[i - 1].is_ascii_digit()
                    && n.chars[i + 1].is_ascii_digit()
            }
            '¥' | '￥' | '$' => true,
            _ => false,
        };
        if !drop {
            chars.push(c);
            map.push(n.map[i]);
        }
    }
    NormText { chars, map }
}

fn find_subslice(haystack: &[char], needle: &[char]) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn span_from(norm: &NormText, pos: usize, len: usize) -> MatchSpan {
    let start = norm.map[pos];
    let end = norm.map[pos + len - 1] + 1;
    MatchSpan {
        start,
        end,
        tier: MatchTier::Normalized,
    }
}

/// 近似匹配（Sellers 子串编辑距离）：返回全文中与 needle 编辑距离最小的
/// 子串 (起点, 终点, 距离)，不做阈值过滤——阈值分档由 find_quote 决定
///（④ Fuzzy：≤ max(1, len/8, 6)；⑤ Approx：≤ max(2, len/3, 10)）。
/// 覆盖 OCR 断行（已规范化去空白）之上的单字误读/多字/缺字（如「拾」误作「叁」）。
fn fuzzy_find(hay: &NormText, needle: &NormText) -> Option<(usize, usize, usize)> {
    let h = &hay.chars;
    let n = &needle.chars;
    if n.is_empty() || h.is_empty() {
        return None;
    }
    let nh = n.len();
    // 内存保护：超大文本不做近似匹配（O(H*N) 矩阵）
    if h.len().saturating_mul(nh + 1) > 25_000_000 {
        return None;
    }

    // Sellers DP：prev[j] = hay 前一前缀下 needle 前 j 字符的最小编辑距离（起点任意）
    let mut prev: Vec<usize> = (0..=nh).collect();
    let mut start_pos: Vec<usize> = vec![0; nh + 1];
    let mut best: Option<(usize, usize, usize)> = None; // (dist, end, start)

    for i in 1..=h.len() {
        let mut cur = vec![0usize; nh + 1];
        let mut cur_start = vec![i; nh + 1]; // cur[0]=0：needle 空，新起点为 i
        for j in 1..=nh {
            let cost = usize::from(h[i - 1] != n[j - 1]);
            let (sub, s_start) = (prev[j - 1] + cost, start_pos[j - 1]);
            let (del, d_start) = (prev[j] + 1, start_pos[j]);
            let (ins, i_start) = (cur[j - 1] + 1, cur_start[j - 1]);
            if sub <= del && sub <= ins {
                cur[j] = sub;
                cur_start[j] = s_start;
            } else if del <= ins {
                cur[j] = del;
                cur_start[j] = d_start;
            } else {
                cur[j] = ins;
                cur_start[j] = i_start;
            }
        }
        let d = cur[nh];
        if best.is_none_or(|(bd, _, _)| d < bd) {
            best = Some((d, i, cur_start[nh]));
        }
        prev = cur;
        start_pos = cur_start;
    }
    best
}

/// 三级匹配主入口。`keep[i]=false` 的字符（页/块分隔）不参与匹配但可落在命中区间内。
pub fn find_quote(text: &str, keep: &[bool], quote: &str) -> Option<MatchSpan> {
    let orig: Vec<char> = text.chars().collect();
    debug_assert_eq!(orig.len(), keep.len());
    let q_orig: Vec<char> = quote.chars().collect();
    if q_orig.is_empty() {
        return None;
    }

    // ① 精确子串
    if let Some(pos) = find_subslice(&orig, &q_orig) {
        return Some(MatchSpan {
            start: pos,
            end: pos + q_orig.len(),
            tier: MatchTier::Exact,
        });
    }

    // ② 规范化（全角→半角、去空白与分隔）
    let hay = normalize(&orig, Some(keep));
    let needle = normalize(&q_orig, None);
    if needle.chars.is_empty() {
        return None;
    }
    if let Some(pos) = find_subslice(&hay.chars, &needle.chars) {
        return Some(span_from(&hay, pos, needle.chars.len()));
    }

    // ③ 金额规范化（去千分位逗号、忽略货币符号）
    let hay_a = amount_normalize(&hay);
    let needle_a = amount_normalize(&needle);
    if needle_a.chars.is_empty() {
        return None;
    }
    if let Some(pos) = find_subslice(&hay_a.chars, &needle_a.chars) {
        let mut span = span_from(&hay_a, pos, needle_a.chars.len());
        span.tier = MatchTier::Amount;
        return Some(span);
    }

    // ④ 近似匹配（编辑距离容忍单字误读/多字/缺字）
    if let Some((dist, end, start)) = fuzzy_find(&hay, &needle) {
        let nh = needle.chars.len();
        let mut span = span_from(&hay, start, end - start);
        if dist <= (nh / 8).clamp(1, 6) {
            span.tier = MatchTier::Fuzzy;
            return Some(span);
        }
        // ⑤ 尽力近似：更大容差给出原文最相似区间（低置信红框定位，编排侧标 suspect）
        if dist <= (nh / 3).clamp(2, 10) {
            span.tier = MatchTier::Approx;
            return Some(span);
        }
    }
    None
}

/// 单列解析结果（结构校验后）。
#[derive(Debug, Clone, Default)]
pub struct ParsedColumn {
    pub reasoning: String,
    pub quote: String,
    pub value: String,
}

/// 结构校验：JSON 可解析 + 每列对象与字段齐全。
/// 返回 Ok（每列一值，与 cols 同序）或 Err（逐条错误描述）。
pub fn parse_extraction_json(
    content: &str,
    cols: &[ColumnSpec],
    value_only: bool,
) -> Result<Vec<ParsedColumn>, Vec<String>> {
    let v: Value = match serde_json::from_str(content) {
        Ok(v) => v,
        Err(e) => {
            return Err(vec![format!(
                "输出不是合法 JSON（{e}）。请只输出一个 JSON 对象。"
            )]);
        }
    };
    let required: [&str; 3] = ["reasoning", "quote", "value"];
    let mut errors = Vec::new();
    let mut out = Vec::with_capacity(cols.len());
    for c in cols {
        let key = format!("col_{}", c.column_index);
        let label = format!("{key}（{}）", c.keyword);
        match v.get(&key).and_then(|x| x.as_object()) {
            None => {
                errors.push(format!("{label}：缺少该列对象"));
                out.push(ParsedColumn::default());
            }
            Some(obj) => {
                let mut col = ParsedColumn::default();
                let fields: &[&str] = if value_only { &["value"] } else { &required };
                for f in fields {
                    match obj.get(*f).and_then(|x| x.as_str()) {
                        Some(s) => match *f {
                            "reasoning" => col.reasoning = s.to_string(),
                            "quote" => col.quote = s.to_string(),
                            _ => col.value = s.to_string(),
                        },
                        None => errors.push(format!("{label}：缺少必需字段 {f}")),
                    }
                }
                out.push(col);
            }
        }
    }
    if errors.is_empty() {
        Ok(out)
    } else {
        Err(errors)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keep_all(text: &str) -> Vec<bool> {
        vec![true; text.chars().count()]
    }

    fn span_text(text: &str, span: &MatchSpan) -> String {
        text.chars()
            .skip(span.start)
            .take(span.end - span.start)
            .collect()
    }

    #[test]
    fn exact_substring_hit() {
        let text = "甲方：佛山市禅城区住房城乡建设和水利局乙方：";
        let m = find_quote(text, &keep_all(text), "佛山市禅城区住房城乡建设和水利局").unwrap();
        assert_eq!(m.tier, MatchTier::Exact);
        assert_eq!(span_text(text, &m), "佛山市禅城区住房城乡建设和水利局");
    }

    #[test]
    fn normalized_fullwidth_and_whitespace() {
        // 全角数字/标点 → 半角；quote 中多余空白被忽略
        let text = "金额：１２３４５元整";
        let m = find_quote(text, &keep_all(text), "12345元").unwrap();
        assert_eq!(m.tier, MatchTier::Normalized);
        assert_eq!(span_text(text, &m), "１２３４５元");

        let text2 = "合同金额： 1,234,567.89  元";
        let m2 = find_quote(text2, &keep_all(text2), "1,234,567.89 元").unwrap();
        assert_eq!(m2.tier, MatchTier::Normalized);
        assert_eq!(span_text(text2, &m2), "1,234,567.89  元");
    }

    #[test]
    fn normalized_skips_page_markers() {
        // 页分隔标记（keep=false）不计入匹配：跨页 quote 可命中
        let text = "本合【第2页】\n同金额";
        let mut keep = keep_all(text);
        for (i, c) in text.chars().enumerate() {
            if "【第2页】\n".contains(c) {
                keep[i] = false;
            }
        }
        let m = find_quote(text, &keep, "本合同金额").unwrap();
        assert_eq!(m.tier, MatchTier::Normalized);
        // 命中区间覆盖原文（含标记），首尾是真实字符
        assert_eq!(span_text(text, &m), "本合【第2页】\n同金额");
    }

    #[test]
    fn amount_tier_comma_and_currency() {
        // 千分位差异：quote 无逗号，原文有逗号
        let text = "监控账号446899991010003029262的商品房屋";
        let m = find_quote(text, &keep_all(text), "446899991010003029262").unwrap();
        assert_eq!(m.tier, MatchTier::Exact); // 无差异时仍走精确档

        let text2 = "金额为 1,234,567.89 元整";
        let m2 = find_quote(text2, &keep_all(text2), "1234567.89").unwrap();
        assert_eq!(m2.tier, MatchTier::Amount);
        assert_eq!(span_text(text2, &m2), "1,234,567.89");

        // 货币符号忽略（符号本身被丢弃，命中区间从数字起）
        let text3 = "合计￥9,999.00元";
        let m3 = find_quote(text3, &keep_all(text3), "9999.00元").unwrap();
        assert_eq!(m3.tier, MatchTier::Amount);
        assert_eq!(span_text(text3, &m3), "9,999.00元");
    }

    #[test]
    fn no_match_returns_none() {
        let text = "完全不相关的文本";
        assert!(find_quote(text, &keep_all(text), "不存在的引用").is_none());
        assert!(find_quote(text, &keep_all(text), "").is_none());
    }

    #[test]
    fn fuzzy_tolerates_single_char_misread() {
        // OCR「参拾式万」误作「参叁式万」（1 字差异 → Fuzzy 档）
        let text = "本合同金额为参拾式万元整";
        let m = find_quote(text, &keep_all(text), "本合同金额为参叁式万元整").unwrap();
        assert_eq!(m.tier, MatchTier::Fuzzy);
        assert_eq!(span_text(text, &m), "本合同金额为参拾式万元整");
    }

    #[test]
    fn rewritten_quote_uses_fuzzy_or_approx_contiguous_span() {
        // 模型改写了连接词：连续子串链定位（Fuzzy/Approx），区间保持连续，
        // 不再产出跳脱并集（防红框内容与提取内容不一致/巨框）
        let text = "甲方：佛山市禅城区住房城乡建设和水利局，乙方：交通银行股份有限公司佛山分行";
        let quote = "佛山市禅城区住房城乡建设和水利局与交通银行股份有限公司佛山分行";
        let m = find_quote(text, &keep_all(text), quote).unwrap();
        assert!(matches!(m.tier, MatchTier::Fuzzy | MatchTier::Approx));
        // 区间连续且覆盖真实来源（UIE 式 start/end）
        let hit = span_text(text, &m);
        assert!(hit.contains("佛山市"), "{hit}");
    }

    #[test]
    fn distant_parts_quote_yields_no_giant_span() {
        // 回归锁（红框过大）：quote 由相距很远的两段拼接而成时，连续链不得
        // 返回跨段大区间（不出巨框）；要么 Approx 短区间，要么不命中
        let text = "开头处有甲方名称甲公司。".to_string()
            + &"无关内容段落。".repeat(60)
            + "结尾处有乙方名称乙公司。";
        let quote = "甲公司和乙公司";
        if let Some(m) = find_quote(&text, &keep_all(&text), quote) {
            let span_len = m.end - m.start;
            assert!(
                span_len <= quote.chars().count() + 20,
                "拼接型 quote 不得产出跨段巨框: span_len={span_len}"
            );
        }
    }

    #[test]
    fn fuzzy_rejects_beyond_threshold() {
        // 差异过大（连续多字不同）不得命中
        let text = "本合同金额为人民币叁拾万元整";
        let m = find_quote(text, &keep_all(text), "本合同金额壹佰贰拾万元整啊");
        assert!(m.is_none());
    }

    #[test]
    fn approx_tier_locates_best_effort_span() {
        // ⑥ 尽力近似：超出 Fuzzy 容差但在 len/3 内 → Approx 档定位（供低置信红框）
        let text = "甲方：佛山市禅城区住房城乡建设和水利局乙方";
        // 3 字差异：Fuzzy（≤16/8=2）不收，Approx（≤16/3=5）收
        let m = find_quote(text, &keep_all(text), "佛山市A城区住房B乡建设和C利局").unwrap();
        assert_eq!(m.tier, MatchTier::Approx);
        let hit = span_text(text, &m);
        assert!(hit.starts_with("佛山市"), "{hit}");
        assert!(hit.ends_with("利局"), "{hit}");
    }

    #[test]
    fn approx_rejects_unrelated_text() {
        // 连 len/3 容差都超出（基本不相关）：不定位，交由编排层标 suspect（无坐标）
        let text = "甲方：佛山市禅城区住房城乡建设和水利局";
        assert!(find_quote(text, &keep_all(text), "广州市天河区人民政府办公室").is_none());
    }

    #[test]
    fn parse_json_structure_validation() {
        let cols = vec![
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
        ];
        // 完整
        let ok = r#"{"col_2":{"reasoning":"r","quote":"q","value":"v"},
                    "col_3":{"reasoning":"r2","quote":"q2","value":"v2"}}"#;
        let parsed = parse_extraction_json(ok, &cols, false).unwrap();
        assert_eq!(parsed[0].value, "v");
        assert_eq!(parsed[1].quote, "q2");
        // 缺列
        let missing = r#"{"col_2":{"reasoning":"r","quote":"q","value":"v"}}"#;
        let errs = parse_extraction_json(missing, &cols, false).unwrap_err();
        assert!(errs
            .iter()
            .any(|e| e.contains("col_3") && e.contains("缺少该列对象")));
        // 缺字段
        let miss_field = r#"{"col_2":{"reasoning":"r","value":"v"},
                            "col_3":{"reasoning":"r","quote":"q","value":"v"}}"#;
        let errs2 = parse_extraction_json(miss_field, &cols, false).unwrap_err();
        assert!(errs2
            .iter()
            .any(|e| e.contains("col_2") && e.contains("quote")));
        // 非 JSON
        let errs3 = parse_extraction_json("not json at all", &cols, false).unwrap_err();
        assert!(errs3[0].contains("不是合法 JSON"));
        // value_only 模式不要求 quote
        let direct = r#"{"col_2":{"value":"v"},"col_3":{"value":""}}"#;
        assert!(parse_extraction_json(direct, &cols, true).is_ok());
    }
}
