//! LLM client：Chat Completions 协议封装（reqwest rustls）。
//! - ollama：POST {base}/v1/chat/completions（base 已含 /v1 时不重复拼）
//! - kimi：POST {base}/chat/completions（Bearer api_key）
//!
//! 单次提交策略（2026-09 收敛，替代原 Responses→chat 三档降级链）：
//! 每文件 1 次请求，response_format = json_schema(strict) 约束解码保证合法 JSON
//! ——这是 Kimi 官方唯一文档化并推荐的 Structured Output 路径（Ollama ≥0.5 的
//! OpenAI 兼容端点同样支持）。不做档位降级、不做网络重试：任何失败即该文件
//! 提取失败，由用户在前端决定是否重新提取。
//! 历史教训：Kimi 的 Responses API 会静默忽略 text.format 约束而输出自由文本
//! （生产实证 4 次成功响应中 3 次非 JSON）；降级链导致同一份全文提示词被重复
//! 提交 2~3 次，input token 成倍计费，且单文件屡屡撞超时预算被强制中止。
//! 唯一保留的流控：kimi 的 RPM 滑动窗口（发送前占位，非重试）。

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::time::sleep;

use crate::error::AppError;
use crate::settings::{self, ProviderSettings};

/// Kimi API 的限流：滚动窗口内最多 N 次请求（Kimi 免费账号 RPM=3）。
const RPM_LIMIT: usize = 3;
const RPM_WINDOW_SECS: u64 = 60;

/// LLM 交互日志回调（任务日志埋点；None 表示不记录）。
pub type LogFn<'a> = Option<&'a (dyn Fn(&str) + Send + Sync)>;

pub(crate) fn logf(log: LogFn<'_>, msg: String) {
    if let Some(f) = log {
        f(&msg);
    }
}

/// 结构化输出方式标识（日志与 llm.json 留档）。历史上为多档降级链
/// （responses/json_schema/json_object/plain），已收敛为单一 json_schema；
/// 保留枚举以维持 tier 字段在日志与留档文件中的语义不变。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResponseTier {
    JsonSchema,
}

impl ResponseTier {
    pub fn name(self) -> &'static str {
        match self {
            ResponseTier::JsonSchema => "json_schema",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        ChatMessage {
            role: "system".into(),
            content: content.into(),
        }
    }
    pub fn user(content: impl Into<String>) -> Self {
        ChatMessage {
            role: "user".into(),
            content: content.into(),
        }
    }
    pub fn assistant(content: impl Into<String>) -> Self {
        ChatMessage {
            role: "assistant".into(),
            content: content.into(),
        }
    }
}

#[derive(Debug)]
pub struct ChatOutcome {
    pub content: String,
    pub tier: ResponseTier,
}

/// chat 单次请求的失败：summary 为摘要（日志与提取失败行 error_msg 用）；
/// raw 为 LLM 原始输出/HTTP 错误响应体（2-提取结果集 llm.json 失败留档用）。
#[derive(Debug)]
pub struct ChatError {
    pub summary: String,
    pub raw: Option<String>,
}

impl ChatError {
    fn new(summary: String) -> Self {
        ChatError { summary, raw: None }
    }
    fn with_raw(summary: String, raw: String) -> Self {
        // raw 留档截断上限，防异常巨型响应撑爆留档文件
        const RAW_CAP: usize = 20_000;
        let raw = if raw.chars().count() > RAW_CAP {
            raw.chars().take(RAW_CAP).collect::<String>() + "…（留档截断）"
        } else {
            raw
        };
        ChatError {
            summary,
            raw: Some(raw),
        }
    }
}

pub struct LlmClient {
    http: reqwest::Client,
    url: String,
    api_key: Option<String>,
    model: String,
    /// RPM 限流窗口（仅 kimi；滑动窗口内已发请求的时间戳队列）
    rate_window: Option<(
        Duration,
        std::sync::Mutex<std::collections::VecDeque<std::time::Instant>>,
    )>,
}

impl LlmClient {
    pub fn from_settings(provider: &str, s: &ProviderSettings) -> Result<Self, AppError> {
        let base = s.base_url.trim().trim_end_matches('/');
        if !base.starts_with("http://") && !base.starts_with("https://") {
            return Err(AppError::InvalidInput(format!(
                "base_url 需以 http(s):// 开头: {base}"
            )));
        }
        let (url, api_key) = match provider {
            settings::PROVIDER_OLLAMA => {
                let u = if base.ends_with("/v1") {
                    format!("{base}/chat/completions")
                } else {
                    format!("{base}/v1/chat/completions")
                };
                (u, None)
            }
            settings::PROVIDER_KIMI => {
                let u = format!("{base}/chat/completions");
                let k = if s.api_key.is_empty() {
                    None
                } else {
                    Some(s.api_key.clone())
                };
                (u, k)
            }
            other => {
                return Err(AppError::InvalidInput(format!(
                    "未知 LLM provider: {other}"
                )));
            }
        };
        // timeout：单次 HTTP 请求总时限（connect+read）；connect_timeout 单独收紧，
        // 避免对端不可达时长时间挂起。单文件整体硬预算在 extract_one_file 调用侧。
        let builder = reqwest::Client::builder()
            .timeout(Duration::from_secs(s.timeout.max(1) as u64))
            .connect_timeout(Duration::from_secs(10));
        let builder = if settings::is_loopback_base(base) {
            builder.no_proxy()
        } else {
            builder
        };
        let http = builder
            .build()
            .map_err(|e| AppError::Llm(format!("创建 HTTP 客户端失败: {e}")))?;
        // 限流窗口仅 kimi：RPM 3（Kimi 免费账号按分钟限流 3 次）；
        // ollama 本地部署不受限
        let rate_window = (provider == settings::PROVIDER_KIMI).then(|| {
            (
                Duration::from_secs(RPM_WINDOW_SECS),
                std::sync::Mutex::new(std::collections::VecDeque::new()),
            )
        });
        Ok(LlmClient {
            http,
            url,
            api_key,
            model: s.model.clone(),
            rate_window,
        })
    }

    /// 测试/A-B 用：自定义限流窗口参数。
    #[allow(dead_code)]
    pub fn with_rate_window(mut self, window: Duration) -> Self {
        if self.rate_window.is_some() {
            self.rate_window = Some((
                window,
                std::sync::Mutex::new(std::collections::VecDeque::new()),
            ));
        }
        self
    }

    /// RPM 限流：滑动窗口内已发 RPM_LIMIT 次则睡到窗口腾出位置（仅 kimi）。
    /// 发送前占位（非重试）；串行调用下实现即安全。
    async fn throttle(&self, log: LogFn<'_>) {
        let Some((window, queue)) = &self.rate_window else {
            return;
        };
        loop {
            let wait = {
                let mut q = queue.lock().unwrap();
                let now = std::time::Instant::now();
                while q.front().is_some_and(|t| now.duration_since(*t) >= *window) {
                    q.pop_front();
                }
                if q.len() < RPM_LIMIT {
                    q.push_back(now);
                    None
                } else {
                    Some(
                        q.front()
                            .unwrap()
                            .checked_add(*window)
                            .unwrap()
                            .saturating_duration_since(now),
                    )
                }
            };
            match wait {
                None => return,
                Some(w) => {
                    logf(log, format!("限流控制（RPM {RPM_LIMIT}）：{w:.1?} 后发送"));
                    sleep(w).await;
                }
            }
        }
    }

    /// 测试/A-B 用：读取当前模型名。
    #[allow(dead_code)]
    pub fn model(&self) -> &str {
        &self.model
    }

    /// 单次请求（chat/completions，response_format=json_schema strict）。
    /// 成功条件：HTTP 2xx 且 choices[0].message.content 为合法 JSON。
    /// Kimi k2.6 在 json_schema 下偶发 Markdown 围栏输出（官方已知行为，
    /// 官方建议业务层 strip 后再解析）——先剥围栏再校验。
    /// 其余一律 Err 且不降级不重试：非 2xx、网络/超时错误、响应缺 content、
    /// finish_reason=length（输出被 max tokens 截断）、输出非合法 JSON。
    pub async fn chat(
        &self,
        messages: &[ChatMessage],
        schema: &Value,
        log: LogFn<'_>,
    ) -> Result<ChatOutcome, ChatError> {
        let t0 = std::time::Instant::now();
        logf(
            log,
            format!(
                "LLM 请求：POST {}（{}）",
                self.url,
                ResponseTier::JsonSchema.name()
            ),
        );
        let body = self.build_body(messages, schema);
        self.throttle(log).await;
        let mut req = self.http.post(&self.url).json(&body);
        if let Some(k) = &self.api_key {
            req = req.bearer_auth(k);
        }
        let fail = |m: String| {
            logf(log, format!("LLM 请求失败：{m}"));
            ChatError::new(m)
        };
        let resp = req.send().await.map_err(|e| {
            if e.is_timeout() {
                fail("请求超时（连接或读取超过设定时限）".to_string())
            } else {
                fail(format!("网络错误：{e}"))
            }
        })?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            let summary = format!("HTTP {status}: {}", snippet(&text));
            logf(log, format!("LLM 请求失败：{summary}"));
            return Err(ChatError::with_raw(summary, text));
        }
        let v: Value = resp
            .json()
            .await
            .map_err(|e| fail(format!("响应体解析失败: {e}")))?;
        let choice = &v["choices"][0];
        if choice["finish_reason"].as_str() == Some("length") {
            return Err(fail(
                "输出达到模型 max_tokens 上限被截断（finish_reason=length）".to_string(),
            ));
        }
        let Some(content) = choice["message"]["content"].as_str() else {
            return Err(fail("响应缺少 choices[0].message.content".to_string()));
        };
        let cleaned = strip_code_fence(content);
        if serde_json::from_str::<Value>(cleaned).is_err() {
            let summary = format!("模型输出非合法 JSON：{}", snippet(content));
            logf(log, format!("LLM 请求失败：{summary}"));
            return Err(ChatError::with_raw(summary, content.to_string()));
        }
        if cleaned.len() != content.len() {
            logf(log, "输出含 Markdown 围栏，已剥离后解析".to_string());
        }
        logf(
            log,
            format!(
                "LLM 响应成功（{}，耗时 {:.1}s）",
                ResponseTier::JsonSchema.name(),
                t0.elapsed().as_secs_f64()
            ),
        );
        Ok(ChatOutcome {
            content: cleaned.to_string(),
            tier: ResponseTier::JsonSchema,
        })
    }

    /// 请求体：messages + response_format=json_schema(strict)。
    /// 温度固定 1.0（Kimi 思考模型等仅允许 1；不再提供设置项，settings 表亦无此列）。
    fn build_body(&self, messages: &[ChatMessage], schema: &Value) -> Value {
        json!({
            "model": self.model,
            "messages": messages,
            "temperature": 1.0,
            "stream": false,
            "response_format": {
                "type": "json_schema",
                "json_schema": {
                    "name": "extraction",
                    "strict": true,
                    "schema": schema,
                }
            }
        })
    }
}

fn snippet(body: &str) -> String {
    body.chars().take(200).collect()
}

/// 剥离 Markdown 代码围栏（```json … ``` / ``` … ```）。
/// Kimi k2.6 在 json_schema strict 下仍偶发围栏包裹输出（官方文档已知行为，
/// 建议业务层 strip 后再做 schema 校验）；且生产实证其会「只开不合」——
/// 开 ```json 后输出完整 JSON 直接结束、无闭合围栏，此时同样接受主体。
/// 无围栏时原样返回；剥离结果是否可用由后续 JSON 校验兜底。
fn strip_code_fence(s: &str) -> &str {
    let t = s.trim();
    let Some(rest) = t.strip_prefix("```") else {
        return t;
    };
    // 跳过围栏首行（json / JSON / 空语言标记）
    let Some(nl) = rest.find('\n') else {
        return t;
    };
    let body = rest[nl + 1..].trim_end();
    // 闭合围栏剥尾部 ```；未闭合（k2.6 只开不合）则直接取主体
    match body.strip_suffix("```") {
        Some(inner) => inner.trim(),
        None => body,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::sync::{Arc, Mutex};

    /// 脚本化 HTTP 桩：按请求顺序依次返回脚本响应，记录全部请求体。
    struct StubServer {
        url: String,
        bodies: Arc<Mutex<Vec<String>>>,
    }

    fn stub_server(responses: Vec<(u16, String)>) -> StubServer {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let bodies = Arc::new(Mutex::new(Vec::new()));
        let bodies2 = bodies.clone();
        std::thread::spawn(move || {
            for (status, payload) in responses {
                let Ok((mut stream, _)) = listener.accept() else {
                    return;
                };
                let mut buf = vec![0u8; 1024 * 1024];
                let Ok(n) = stream.read(&mut buf) else {
                    continue;
                };
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                let body = req.to_string();
                bodies2.lock().unwrap().push(body);
                let resp = format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
                    payload.len()
                );
                let _ = stream.write_all(resp.as_bytes());
            }
        });
        StubServer {
            url: format!("http://127.0.0.1:{port}"),
            bodies,
        }
    }

    fn chat_response(content: &str) -> String {
        json!({
            "choices": [{"message": {"role": "assistant", "content": content},
                         "finish_reason": "stop"}]
        })
        .to_string()
    }

    fn test_settings(url: &str) -> ProviderSettings {
        ProviderSettings {
            base_url: url.to_string(),
            api_key: String::new(),
            model: "test-model".to_string(),
            timeout: 60,
        }
    }

    fn test_client(url: &str) -> LlmClient {
        LlmClient::from_settings(settings::PROVIDER_OLLAMA, &test_settings(url)).unwrap()
    }

    fn kimi_client(url: &str) -> LlmClient {
        LlmClient::from_settings(
            settings::PROVIDER_KIMI,
            &ProviderSettings {
                base_url: url.to_string(),
                api_key: "sk-x".to_string(),
                ..test_settings("")
            },
        )
        .unwrap()
    }

    #[tokio::test]
    async fn json_schema_success_single_request() {
        let stub = stub_server(vec![(200, chat_response(r#"{"col_2":{"value":"v"}}"#))]);
        let client = test_client(&stub.url);
        let out = client
            .chat(&[ChatMessage::user("hi")], &json!({"type": "object"}), None)
            .await
            .unwrap();
        assert_eq!(out.tier, ResponseTier::JsonSchema);
        assert!(out.content.contains("col_2"));
        let bodies = stub.bodies.lock().unwrap();
        assert_eq!(bodies.len(), 1, "单次提交：只发 1 次请求");
        assert!(bodies[0].contains("json_schema"));
        assert!(bodies[0].contains("\"strict\":true"));
        assert!(bodies[0].contains("test-model"));
        assert!(bodies[0].contains("/chat/completions"));
    }

    #[tokio::test]
    async fn fenced_json_output_is_stripped_and_accepted() {
        // Kimi k2.6 已知行为：json_schema 下偶发 ```json 围栏包裹 → 剥离后正常解析
        let stub = stub_server(vec![(
            200,
            chat_response("```json\n{\"col_2\":{\"value\":\"v\"}}\n```"),
        )]);
        let client = test_client(&stub.url);
        let out = client
            .chat(&[ChatMessage::user("hi")], &json!({"type": "object"}), None)
            .await
            .unwrap();
        assert_eq!(out.tier, ResponseTier::JsonSchema);
        assert_eq!(out.content, r#"{"col_2":{"value":"v"}}"#);
        assert_eq!(
            stub.bodies.lock().unwrap().len(),
            1,
            "围栏剥离不消耗额外请求"
        );
    }

    #[tokio::test]
    async fn non_json_output_fails_without_retry() {
        // 200 但输出非 JSON（剥围栏后仍非 JSON）：直接失败，不降级不重试；raw 留档全文
        let stub = stub_server(vec![(200, chat_response("我无法输出 JSON"))]);
        let client = test_client(&stub.url);
        let err = client
            .chat(&[ChatMessage::user("hi")], &json!({}), None)
            .await
            .unwrap_err();
        assert!(err.summary.contains("非合法 JSON"), "{}", err.summary);
        assert_eq!(
            err.raw.as_deref(),
            Some("我无法输出 JSON"),
            "raw 留档原始输出"
        );
        assert_eq!(stub.bodies.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn http_400_fails_without_retry() {
        let stub = stub_server(vec![(
            400,
            r#"{"error":"does not support response_format"}"#.to_string(),
        )]);
        let client = test_client(&stub.url);
        let err = client
            .chat(&[ChatMessage::user("hi")], &json!({}), None)
            .await
            .unwrap_err();
        assert!(err.summary.contains("400"), "{}", err.summary);
        assert!(err.raw.is_some(), "HTTP 错误响应体留档");
        assert_eq!(stub.bodies.lock().unwrap().len(), 1, "400 不重试");
    }

    #[tokio::test]
    async fn http_429_fails_without_retry() {
        let stub = stub_server(vec![(429, r#"{"error":"rate limited"}"#.to_string())]);
        let client = kimi_client(&stub.url);
        let err = client
            .chat(&[ChatMessage::user("hi")], &json!({}), None)
            .await
            .unwrap_err();
        assert!(err.summary.contains("429"), "{}", err.summary);
        assert_eq!(stub.bodies.lock().unwrap().len(), 1, "429 不重试");
    }

    #[tokio::test]
    async fn http_500_fails_without_retry() {
        let stub = stub_server(vec![(500, "{}".to_string())]);
        let client = test_client(&stub.url);
        let err = client
            .chat(&[ChatMessage::user("hi")], &json!({}), None)
            .await
            .unwrap_err();
        assert!(err.summary.contains("500"), "{}", err.summary);
        assert_eq!(stub.bodies.lock().unwrap().len(), 1, "5xx 不退避重试");
    }

    #[tokio::test]
    async fn finish_reason_length_is_error() {
        // 输出被 max_tokens 截断：JSON 必不完整，报明确截断原因
        let stub = stub_server(vec![(
            200,
            json!({
                "choices": [{"message": {"role": "assistant", "content": "{\"col_2\":"},
                             "finish_reason": "length"}]
            })
            .to_string(),
        )]);
        let client = test_client(&stub.url);
        let err = client
            .chat(&[ChatMessage::user("hi")], &json!({}), None)
            .await
            .unwrap_err();
        assert!(err.summary.contains("截断"), "{}", err.summary);
        assert_eq!(stub.bodies.lock().unwrap().len(), 1);
    }

    #[test]
    fn strip_code_fence_variants() {
        assert_eq!(strip_code_fence(r#"{"a":1}"#), r#"{"a":1}"#, "无围栏原样");
        assert_eq!(strip_code_fence("```json\n{\"a\":1}\n```"), r#"{"a":1}"#);
        assert_eq!(strip_code_fence("```\n{\"a\":1}\n```"), r#"{"a":1}"#);
        assert_eq!(
            strip_code_fence("  ```JSON\r\n{\"a\":1}\r\n```  "),
            r#"{"a":1}"#
        );
        // 只开不合（k2.6 生产实证）：接受主体
        assert_eq!(strip_code_fence("```json\n{\"a\":1}"), r#"{"a":1}"#);
        assert_eq!(
            strip_code_fence("```json\n{\n  \"a\": 1\n}\n"),
            "{\n  \"a\": 1\n}"
        );
    }

    #[tokio::test]
    async fn unclosed_fence_json_output_is_accepted() {
        // 生产回归（测试文件2-印章字符识别.llm.json）：kimi-k2.6 开 ```json 后
        // 输出完整 JSON 直接结束、无闭合围栏 → 剥主体后正常解析
        let stub = stub_server(vec![(
            200,
            chat_response("```json\n{\n  \"col_2\": {\"reasoning\": \"r\", \"quote\": \"\", \"value\": \"v\"}\n}\n"),
        )]);
        let client = test_client(&stub.url);
        let out = client
            .chat(&[ChatMessage::user("hi")], &json!({"type": "object"}), None)
            .await
            .unwrap();
        assert_eq!(out.tier, ResponseTier::JsonSchema);
        let v: Value = serde_json::from_str(&out.content).unwrap();
        assert_eq!(v["col_2"]["value"], json!("v"));
        assert_eq!(
            stub.bodies.lock().unwrap().len(),
            1,
            "未闭合围栏剥离不消耗额外请求"
        );
    }

    #[test]
    fn url_rules_by_provider() {
        // ollama：自动拼 /v1
        let c = LlmClient::from_settings(
            settings::PROVIDER_OLLAMA,
            &ProviderSettings {
                base_url: "http://localhost:11434/".to_string(),
                ..test_settings("")
            },
        )
        .unwrap();
        assert_eq!(c.url, "http://localhost:11434/v1/chat/completions");
        assert!(c.api_key.is_none());
        // ollama：base 已含 /v1 不重复
        let c2 = LlmClient::from_settings(
            settings::PROVIDER_OLLAMA,
            &ProviderSettings {
                base_url: "http://localhost:11434/v1".to_string(),
                ..test_settings("")
            },
        )
        .unwrap();
        assert_eq!(c2.url, "http://localhost:11434/v1/chat/completions");
        assert!(c2.rate_window.is_none(), "ollama 不限流");
        // kimi：chat/completions 端点且带 key
        let c3 = LlmClient::from_settings(
            settings::PROVIDER_KIMI,
            &ProviderSettings {
                base_url: "https://api.example.com/v1".to_string(),
                api_key: "sk-x".to_string(),
                ..test_settings("")
            },
        )
        .unwrap();
        assert_eq!(c3.url, "https://api.example.com/v1/chat/completions");
        assert_eq!(c3.api_key.as_deref(), Some("sk-x"));
        assert!(c3.rate_window.is_some(), "kimi 走 RPM 限流");
    }

    #[tokio::test]
    async fn kimi_rate_limited_by_rpm_window() {
        // RPM 3 / 窗口 120ms（测试缩小窗口）：第 4 次请求须等窗口腾出位置
        let stub = stub_server(vec![
            (200, chat_response(r#"{"a":1}"#)),
            (200, chat_response(r#"{"a":1}"#)),
            (200, chat_response(r#"{"a":1}"#)),
            (200, chat_response(r#"{"a":1}"#)),
        ]);
        let client = kimi_client(&stub.url).with_rate_window(Duration::from_millis(120));
        let t0 = std::time::Instant::now();
        for _ in 0..4 {
            client
                .chat(&[ChatMessage::user("hi")], &json!({}), None)
                .await
                .unwrap();
        }
        assert!(
            t0.elapsed() >= Duration::from_millis(120),
            "窗口内第 4 次请求应被限流等待: {:?}",
            t0.elapsed()
        );
        assert_eq!(stub.bodies.lock().unwrap().len(), 4);
    }

    #[tokio::test]
    async fn ollama_not_rate_limited() {
        // ollama 本地部署不限流：4 次请求即刻完成
        let stub = stub_server(vec![
            (200, chat_response(r#"{"a":1}"#)),
            (200, chat_response(r#"{"a":1}"#)),
            (200, chat_response(r#"{"a":1}"#)),
            (200, chat_response(r#"{"a":1}"#)),
        ]);
        let client = test_client(&stub.url);
        let t0 = std::time::Instant::now();
        for _ in 0..4 {
            client
                .chat(&[ChatMessage::user("hi")], &json!({}), None)
                .await
                .unwrap();
        }
        assert!(
            t0.elapsed() < Duration::from_secs(1),
            "ollama 不应被限流: {:?}",
            t0.elapsed()
        );
        assert_eq!(stub.bodies.lock().unwrap().len(), 4);
    }
}
