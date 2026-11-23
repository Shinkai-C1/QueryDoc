//! LLM 信息提取（Instructor 方法论的 Rust 实现，任务 4.1–4.4）。
//! 编排：动态 JSON Schema（schema.rs）→ prompt（prompt.rs）→ client 单次请求
//! （client.rs，json_schema strict，不降级不重试）→ 结构/引用校验（validate.rs）
//! → 失败 reask（错误反馈重试，max_retries 参数控制，生产路径固定 0 即单次提交）。
//! 显示语义（先内容、后坐标）：value 非空才回写红框坐标（quote/六级链定位），
//! 弱命中（Approx 档）标 suspect（前端红色「疑」）；value 空白或无法定位来源
//! 则不落内容、无坐标、不标疑（纯"未提取到"空白态）。

pub mod client;
pub mod extract;
pub mod prompt;
pub mod schema;
pub mod validate;

use crate::ocr::schema::OcrJson;

/// 一列提取配置（按 column_index 升序传入）。
#[derive(Debug, Clone, Default)]
pub struct ColumnSpec {
    pub column_index: i64,
    pub keyword: String,
    /// 关键词定义（选填，≤200 字；Instructor 字段描述理念，注入 prompt/schema）。
    pub definition: String,
    /// 信息类型（选填：""=未设置 | "text" | "number" | "date"）。
    pub info_type: String,
}

impl ColumnSpec {
    /// 类型中文名（未设置/未知类型返回 None）。
    fn info_type_label(&self) -> Option<&'static str> {
        match self.info_type.as_str() {
            "text" => Some("文本"),
            "number" => Some("数字"),
            "date" => Some("日期"),
            _ => None,
        }
    }

    /// prompt/schema 用的字段描述：关键词 + 可选类型与定义
    /// （Instructor 方法论 ① Schema-first：字段描述明确每个键要取什么）。
    pub fn describe(&self) -> String {
        let mut s = self.keyword.clone();
        let mut extras: Vec<String> = Vec::new();
        if let Some(label) = self.info_type_label() {
            extras.push(format!("信息类型：{label}"));
        }
        if !self.definition.is_empty() {
            extras.push(format!("定义：{}", self.definition));
        }
        if !extras.is_empty() {
            s.push_str(&format!("（{}）", extras.join("；")));
        }
        s
    }

    /// value 字段描述中的类型约定（数字/日期给格式要求；其余沿用原金额规则）。
    pub fn value_hint(&self) -> &'static str {
        match self.info_type.as_str() {
            "number" => "规整后的提取值（数字，保留原文千分位格式）；找不到则为空字符串",
            "date" => "规整后的提取值（日期，统一输出 YYYY-MM-DD）；找不到则为空字符串",
            _ => "规整后的提取值；金额保留原文千分位格式；找不到则为空字符串",
        }
    }
}

/// 提取模式：B = reasoning+quote+reask 完整版；A = 纯 value 直出（A/B 对比用）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtractMode {
    WithCitation,
    DirectValue,
}

/// 文档上下文：含页标记的拼接全文 + 每字符归属（匹配与坐标回写共用）。
pub struct DocContext {
    /// 拼接全文（页间【第N页】标记、块间换行）
    pub text: String,
    /// 每字符是否参与匹配（页标记/块间换行为 false）
    pub keep: Vec<bool>,
    /// 每字符归属（页标记/换行为 None）
    pub locs: Vec<Option<CharLoc>>,
}

#[derive(Debug, Clone, Copy)]
pub struct CharLoc {
    pub page_idx: usize,
    /// 页内块索引
    pub block: usize,
    /// 块 text_flow 内字符 offset
    pub offset: usize,
}

/// 由 ocr.json 构建上下文：页标记【第N页】分隔、块间换行（均不参与匹配）。
pub fn build_context(ocr: &OcrJson) -> DocContext {
    let mut text = String::new();
    let mut locs: Vec<Option<CharLoc>> = Vec::new();
    for page in &ocr.pages {
        for ch in format!("【第{}页】\n", page.page_idx + 1).chars() {
            text.push(ch);
            locs.push(None);
        }
        for (bi, block) in page.blocks.iter().enumerate() {
            for (oi, ch) in block.text_flow.chars().enumerate() {
                text.push(ch);
                locs.push(Some(CharLoc {
                    page_idx: page.page_idx,
                    block: bi,
                    offset: oi,
                }));
            }
            text.push('\n');
            locs.push(None);
        }
    }
    let keep = locs.iter().map(|l| l.is_some()).collect();
    DocContext { text, keep, locs }
}

/// 单列提取产出（校验通过版）。
#[derive(Debug, Clone)]
pub struct ColumnOutcome {
    pub column_index: i64,
    pub reasoning: String,
    pub quote: String,
    /// 模型输出的规整值；坐标回写时保持模型 value 不动
    /// （quote 仅用于定位与校验，不回写覆盖 value——见任务 4.4 说明）。
    pub value: String,
    /// quote 在 ctx.text 上的命中区间与档位；空 quote（未找到）为 None
    pub span: Option<validate::MatchSpan>,
    /// 存疑标记：quote 未命中原文 / value 非空但 quote 为空。召回优先——
    /// value 照常落库返回单元格，仅以前端红色「疑」提示人工复核。
    pub suspect: bool,
}

#[derive(Debug)]
pub struct ExtractionResult {
    pub columns: Vec<ColumnOutcome>,
    /// 实际命中的结构化输出档位
    pub tier: client::ResponseTier,
    /// 实际 LLM 调用次数（含 reask）
    pub attempts: u32,
    /// 最后一次 LLM 调用的原始返回文本（供 llm.json 落盘留档）
    pub raw: String,
}

/// 提取失败：summary 供日志与失败行 error_msg；raw 为 LLM 原始输出
/// （无论成功失败 LLM 输出均落 2-提取结果集 llm.json；网络层无输出时为 None）。
#[derive(Debug)]
pub struct ExtractFailure {
    pub summary: String,
    pub raw: Option<String>,
}

/// 单文档提取：schema → prompt → chat → 校验 → 失败 reask（≤ max_retries 次）。
/// log 为任务日志回调（LLM 请求/响应/限流逐条记录）。
pub async fn extract_document(
    client: &client::LlmClient,
    ctx: &DocContext,
    cols: &[ColumnSpec],
    max_retries: i64,
    mode: ExtractMode,
    log: client::LogFn<'_>,
) -> Result<ExtractionResult, ExtractFailure> {
    let schema = match mode {
        ExtractMode::WithCitation => schema::build_extraction_schema(cols),
        ExtractMode::DirectValue => schema::build_direct_schema(cols),
    };
    let mut messages = vec![
        client::ChatMessage::system(prompt::system_prompt(mode)),
        client::ChatMessage::user(prompt::user_prompt(&ctx.text, cols, mode)),
    ];
    let max_attempts = max_retries.max(0) as u32 + 1;
    let mut last_errors: Vec<String> = Vec::new();
    let mut last_raw = String::new();
    for attempt in 0..max_attempts {
        let outcome = client
            .chat(&messages, &schema, log)
            .await
            .map_err(|e| ExtractFailure {
                summary: e.summary,
                raw: e.raw,
            })?;
        let content = outcome.content;
        match validate_response(&content, ctx, cols, mode) {
            Ok(columns) => {
                return Ok(ExtractionResult {
                    columns,
                    tier: outcome.tier,
                    attempts: attempt + 1,
                    raw: content.clone(),
                });
            }
            Err(errors) => {
                last_errors = errors;
                last_raw = content.clone();
                if attempt + 1 < max_attempts {
                    // reask：原样回填模型输出 + 具体错误反馈（Instructor 方法论 ④）
                    client::logf(
                        log,
                        format!("结构校验失败，reask 重新请求：{}", last_errors.join("；")),
                    );
                    messages.push(client::ChatMessage::assistant(&content));
                    messages.push(client::ChatMessage::user(prompt::reask_prompt(
                        &last_errors,
                    )));
                }
            }
        }
    }
    Err(ExtractFailure {
        summary: format!(
            "验证重试 {} 次后仍失败: {}",
            max_attempts - 1,
            last_errors.join("；")
        ),
        raw: Some(last_raw),
    })
}

/// 结构校验 + 引用校验（WithCitation 模式）。Err 为逐条错误描述（供 reask）。
/// 结构错误（非 JSON、缺列、缺字段）由 parse_extraction_json 提前返回 Err。
/// 显示语义（先内容、后坐标）：合法态只有两种——
///   A 有内容（value 非空）：必有红框坐标；仅弱命中（Approx 档）时标 suspect；
///   B 无内容（value 空白/不可靠）：无坐标、无 suspect（quote 完全忽略）。
fn validate_response(
    content: &str,
    ctx: &DocContext,
    cols: &[ColumnSpec],
    mode: ExtractMode,
) -> Result<Vec<ColumnOutcome>, Vec<String>> {
    let value_only = mode == ExtractMode::DirectValue;
    let parsed = validate::parse_extraction_json(content, cols, value_only)?;
    let mut out = Vec::with_capacity(cols.len());
    for (spec, p) in cols.iter().zip(parsed) {
        let mut span = None;
        let mut suspect = false;
        let mut value = p.value;
        if !value_only && !value.is_empty() {
            // 有内容才谈定位：quote 非空先按 quote 走六级匹配链，
            // quote 为空或未命中则按 value 走同一匹配链
            let hit = (!p.quote.is_empty())
                .then(|| validate::find_quote(&ctx.text, &ctx.keep, &p.quote))
                .flatten()
                .or_else(|| validate::find_quote(&ctx.text, &ctx.keep, &value));
            match hit {
                Some(s) => {
                    suspect = s.tier == validate::MatchTier::Approx;
                    span = Some(s);
                }
                None => {
                    // 内容无法定位来源：视为不可靠产出，不落内容（回空白态 B）
                    value = String::new();
                }
            }
        }
        out.push(ColumnOutcome {
            column_index: spec.column_index,
            reasoning: p.reasoning,
            quote: p.quote,
            value,
            span,
            suspect,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::{self, ProviderSettings};
    use serde_json::json;
    use std::io::{Read, Write};
    use std::sync::{Arc, Mutex};

    /// 脚本化 HTTP 桩（顺序应答 + 请求体记录），供 reask 循环测试。
    fn stub_server(responses: Vec<String>) -> (String, Arc<Mutex<Vec<String>>>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let bodies = Arc::new(Mutex::new(Vec::new()));
        let bodies2 = bodies.clone();
        std::thread::spawn(move || {
            for payload in responses {
                let Ok((mut stream, _)) = listener.accept() else {
                    return;
                };
                let mut buf = vec![0u8; 4 * 1024 * 1024];
                let Ok(n) = stream.read(&mut buf) else {
                    continue;
                };
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                let body = req.to_string();
                bodies2.lock().unwrap().push(body);
                let resp_body = json!({
                    "choices": [{"message": {"role": "assistant", "content": payload}}]
                })
                .to_string();
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{resp_body}",
                    resp_body.len()
                );
                let _ = stream.write_all(resp.as_bytes());
            }
        });
        (format!("http://127.0.0.1:{port}"), bodies)
    }

    fn stub_ctx() -> DocContext {
        let text =
            "【第1页】\n甲方：佛山市禅城区住房城乡建设和水利局\n监控账号446899991010003029262";
        let keep: Vec<bool> = text.chars().map(|c| !"【第1页】\n".contains(c)).collect();
        DocContext {
            text: text.to_string(),
            locs: vec![None; text.chars().count()],
            keep,
        }
    }

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

    fn client_for(url: &str) -> client::LlmClient {
        client::LlmClient::from_settings(
            settings::PROVIDER_OLLAMA,
            &ProviderSettings {
                base_url: url.to_string(),
                api_key: String::new(),
                model: "m".to_string(),
                timeout: 60,
            },
        )
        .unwrap()
    }

    #[tokio::test]
    async fn reask_loop_recovers_and_sends_feedback() {
        let ctx = stub_ctx();
        let good = json!({
            "col_2": {"reasoning": "未找到", "quote": "", "value": ""},
            "col_3": {"reasoning": "开头", "quote": "佛山市禅城区住房城乡建设和水利局", "value": "佛山市禅城区住房城乡建设和水利局"}
        })
        .to_string();
        // 结构错误（缺 quote 字段）才会触发 reask；引用未命中已转为 suspect 软标记
        let bad_struct = json!({
            "col_2": {"reasoning": "r", "quote": "", "value": ""},
            "col_3": {"reasoning": "r", "value": "x"}
        })
        .to_string();
        // 第 1 次：结构校验失败 → 编排层 reask（第 2 次应答，正确）
        let (url, bodies) = stub_server(vec![bad_struct, good]);
        let client = client_for(&url);
        let r = extract_document(&client, &ctx, &cols(), 2, ExtractMode::WithCitation, None)
            .await
            .unwrap();
        assert_eq!(r.attempts, 2);
        assert_eq!(r.tier, client::ResponseTier::JsonSchema);
        assert_eq!(r.columns[1].value, "佛山市禅城区住房城乡建设和水利局");
        assert!(r.columns[1].span.is_some());
        assert!(r.columns[0].span.is_none(), "空 quote 不应有 span");

        let bodies = bodies.lock().unwrap();
        assert_eq!(bodies.len(), 2, "单次提交策略：无档降级，仅 reask 一次");
        assert!(bodies[0].contains("json_schema"));
        assert!(bodies[1].contains("json_schema"));
        // reask 请求含具体错误反馈，且把模型上一次输出回填进对话
        assert!(bodies[1].contains("缺少必需字段 quote"), "{}", bodies[1]);
        assert!(
            bodies[1].contains("\\\"value\\\":\\\"x\\\""),
            "{}",
            bodies[1]
        );
    }

    #[tokio::test]
    async fn approx_hit_is_suspect_but_located() {
        let ctx = stub_ctx();
        // 尽力近似（3 字差异）：标 suspect 但仍回写命中区间（低置信红框定位）
        let resp = json!({
            "col_2": {"reasoning": "r", "quote": "", "value": ""},
            "col_3": {"reasoning": "r", "quote": "佛山市A城区住房B乡建设和C利局", "value": "佛山市禅城区住房城乡建设和水利局"}
        })
        .to_string();
        let (url, _bodies) = stub_server(vec![resp]);
        let client = client_for(&url);
        let r = extract_document(&client, &ctx, &cols(), 0, ExtractMode::WithCitation, None)
            .await
            .unwrap();
        assert!(r.columns[1].suspect, "尽力近似应标 suspect");
        assert!(r.columns[1].span.is_some(), "尽力近似应带定位区间");
        assert_eq!(
            r.columns[1].span.unwrap().tier,
            crate::llm::validate::MatchTier::Approx
        );
    }

    #[tokio::test]
    async fn quote_miss_and_value_miss_drops_content_quietly() {
        let ctx = stub_ctx();
        // quote 与 value 均未命中：不 reask/判负，内容不落库，且不标疑（空白态 B）
        let miss = json!({
            "col_2": {"reasoning": "r", "quote": "", "value": ""},
            "col_3": {"reasoning": "r", "quote": "广州市天河区人民政府", "value": "广州市天河区人民政府"}
        })
        .to_string();
        let (url, bodies) = stub_server(vec![miss]);
        let client = client_for(&url);
        let r = extract_document(&client, &ctx, &cols(), 2, ExtractMode::WithCitation, None)
            .await
            .unwrap();
        assert_eq!(r.attempts, 1, "未命中不应触发 reask");
        assert_eq!(r.columns[1].value, "", "无法定位来源的内容不落库");
        assert!(r.columns[1].span.is_none());
        assert!(!r.columns[1].suspect, "空白态不标疑（消除异常 3）");
        assert!(!r.columns[0].suspect, "空 quote 空 value 属正常未找到");
        assert_eq!(bodies.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn quote_hits_but_value_empty_is_blank_no_coords() {
        let ctx = stub_ctx();
        // 回归锁（异常 1/2）：quote 精确命中但 value 为空 → 空白态，
        // 不回写坐标、不标疑（先确保内容，再谈坐标）
        let resp = json!({
            "col_2": {"reasoning": "r", "quote": "", "value": ""},
            "col_3": {"reasoning": "r", "quote": "佛山市禅城区住房城乡建设和水利局", "value": ""}
        })
        .to_string();
        let (url, _bodies) = stub_server(vec![resp]);
        let client = client_for(&url);
        let r = extract_document(&client, &ctx, &cols(), 0, ExtractMode::WithCitation, None)
            .await
            .unwrap();
        assert_eq!(r.columns[1].value, "");
        assert!(r.columns[1].span.is_none(), "空白态不回写坐标");
        assert!(!r.columns[1].suspect, "空白态不标疑");
    }

    #[tokio::test]
    async fn non_json_output_fails_immediately() {
        let ctx = stub_ctx();
        // 单次提交策略：输出非 JSON 直接失败（客户端层 Err），不降级、不进入 reask
        let (url, bodies) = stub_server(vec!["前言不是 JSON".to_string()]);
        let client = client_for(&url);
        let err = extract_document(&client, &ctx, &cols(), 0, ExtractMode::WithCitation, None)
            .await
            .unwrap_err();
        assert!(err.summary.contains("非合法 JSON"), "{}", err.summary);
        assert!(err.raw.is_some(), "失败也应携带 LLM 原始输出留档");
        assert_eq!(bodies.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn request_body_carries_definition_and_info_type() {
        let ctx = stub_ctx();
        // Instructor 字段描述理念端到端验证：选填的定义/类型须随 user 提示词与
        // JSON Schema 一并下发（stub 记录原始请求体，直接断言文本存在）
        let cols = vec![ColumnSpec {
            column_index: 2,
            keyword: "金额".into(),
            definition: "指甲乙双方约定的含税总金额".into(),
            info_type: "number".into(),
        }];
        let resp = json!({
            "col_2": {"reasoning": "r", "quote": "", "value": ""}
        })
        .to_string();
        let (url, bodies) = stub_server(vec![resp]);
        let client = client_for(&url);
        extract_document(&client, &ctx, &cols, 0, ExtractMode::WithCitation, None)
            .await
            .unwrap();
        let bodies = bodies.lock().unwrap();
        assert_eq!(bodies.len(), 1);
        let body = &bodies[0];
        assert!(body.contains("指甲乙双方约定的含税总金额"), "{body}");
        assert!(body.contains("信息类型：数字"), "{body}");
        assert!(body.contains("保留原文千分位"), "{body}");
    }

    #[tokio::test]
    async fn retries_exhausted_returns_error() {
        let ctx = stub_ctx();
        // 结构错误（缺 col_3 列对象）才会耗尽重试；引用未命中已是 suspect 软标记
        let always_bad = json!({
            "col_2": {"reasoning": "r", "quote": "", "value": ""}
        })
        .to_string();
        let (url, bodies) = stub_server(vec![always_bad.clone(), always_bad.clone(), always_bad]);
        let client = client_for(&url);
        let err = extract_document(&client, &ctx, &cols(), 2, ExtractMode::WithCitation, None)
            .await
            .unwrap_err();
        assert!(err.summary.contains("验证重试 2 次"), "{}", err.summary);
        assert!(err.summary.contains("缺少该列对象"), "{}", err.summary);
        assert!(err.raw.is_some(), "结构校验失败同样留档原始输出");
        assert_eq!(bodies.lock().unwrap().len(), 3);
    }

    #[tokio::test]
    async fn direct_value_mode_skips_quote_validation() {
        let ctx = stub_ctx();
        let direct = json!({
            "col_2": {"value": ""},
            "col_3": {"value": "任意编造的甲方"}
        })
        .to_string();
        let (url, _bodies) = stub_server(vec![direct]);
        let client = client_for(&url);
        let r = extract_document(&client, &ctx, &cols(), 2, ExtractMode::DirectValue, None)
            .await
            .unwrap();
        assert_eq!(r.columns[1].value, "任意编造的甲方");
        assert!(r.columns.iter().all(|c| c.span.is_none()));
    }

    #[tokio::test]
    async fn value_without_quote_located_exact_is_not_suspect() {
        let ctx = stub_ctx();
        // value 非空但 quote 为空：value 在原文精确命中 → 内容保留 + 坐标回写，
        // 非弱命中不标疑（先内容、后坐标）
        let bad = json!({
            "col_2": {"reasoning": "r", "quote": "", "value": ""},
            "col_3": {"reasoning": "r", "quote": "", "value": "佛山市禅城区住房城乡建设和水利局"}
        })
        .to_string();
        let (url, bodies) = stub_server(vec![bad]);
        let client = client_for(&url);
        let r = extract_document(&client, &ctx, &cols(), 2, ExtractMode::WithCitation, None)
            .await
            .unwrap();
        assert_eq!(r.attempts, 1);
        assert!(!r.columns[1].suspect, "精确命中不标疑");
        assert_eq!(r.columns[1].value, "佛山市禅城区住房城乡建设和水利局");
        let span = r.columns[1].span.expect("value 可定位应回写区间");
        assert_eq!(span.tier, crate::llm::validate::MatchTier::Exact);
        assert_eq!(bodies.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn assembled_quote_from_distant_parts_drops_content() {
        // 拼接型 quote（相距很远的两段拼成）：连续链未命中 → 不落内容、无坐标，
        // 回归锁：不再产出跨段巨框（红框过大/内容不一致的源头）
        let text = format!(
            "【第1页】\n甲方：甲公司。\n{}乙方：乙公司。",
            "无关段落内容。".repeat(60)
        );
        let keep = vec![true; text.chars().count()];
        let ctx = DocContext {
            text,
            locs: vec![],
            keep,
        };
        let cols = vec![ColumnSpec {
            column_index: 2,
            keyword: "双方".into(),
            ..Default::default()
        }];
        let resp = json!({
            "col_2": {"reasoning": "r", "quote": "甲公司与乙公司", "value": "甲公司与乙公司"}
        })
        .to_string();
        let (url, _bodies) = stub_server(vec![resp]);
        let client = client_for(&url);
        let r = extract_document(&client, &ctx, &cols, 0, ExtractMode::WithCitation, None)
            .await
            .unwrap();
        assert_eq!(r.columns[0].value, "", "拼接型内容不落库");
        assert!(r.columns[0].span.is_none(), "拼接型 quote 不产出坐标");
        assert!(!r.columns[0].suspect);
    }

    #[tokio::test]
    async fn value_unlocatable_within_tolerance_is_dropped() {
        let ctx = stub_ctx();
        // value 与原文基本无关（超出 Approx 容差）：内容不落库、无坐标、不标疑（空白态 B）
        let bad = json!({
            "col_2": {"reasoning": "r", "quote": "", "value": ""},
            "col_3": {"reasoning": "r", "quote": "", "value": "完全编造的甲方名称XYZ"}
        })
        .to_string();
        let (url, _bodies) = stub_server(vec![bad]);
        let client = client_for(&url);
        let r = extract_document(&client, &ctx, &cols(), 0, ExtractMode::WithCitation, None)
            .await
            .unwrap();
        assert!(!r.columns[1].suspect, "空白态不标疑（消除异常 3）");
        assert!(r.columns[1].span.is_none());
        assert_eq!(r.columns[1].value, "", "无法定位来源的内容不落库");
    }
}
