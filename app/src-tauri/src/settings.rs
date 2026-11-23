//! 设置业务逻辑：settings 表按 provider 分行存取（ollama / kimi
//! 各存一份完整配置），is_active 标记当前使用的 provider；模型清单获取。

use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::db::Db;
use crate::error::AppError;
use crate::paths;

pub const PROVIDER_OLLAMA: &str = "ollama";
pub const PROVIDER_KIMI: &str = "kimi";

/// 单个 provider 的完整连接配置。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProviderSettings {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    /// 提取请求超时（秒）。
    pub timeout: i64,
}

impl ProviderSettings {
    /// provider 的出厂默认（ollama 本地地址 / kimi 官方 API 地址预填）。
    fn default_for(provider: &str) -> Self {
        ProviderSettings {
            base_url: if provider == PROVIDER_OLLAMA {
                "http://localhost:11434".to_string()
            } else {
                "https://api.moonshot.cn/v1".to_string()
            },
            api_key: String::new(),
            model: String::new(),
            timeout: 300,
        }
    }
}

/// 前端设置视图：当前 provider + 两个 provider 各自的完整配置。
#[derive(Debug, Clone, Serialize)]
pub struct SettingsView {
    pub active_provider: String,
    pub ollama: ProviderSettings,
    pub kimi: ProviderSettings,
}

/// save_settings 的入参（updated_at 由服务端生成；保存即设为当前 provider）。
#[derive(Debug, Clone, Deserialize)]
pub struct SettingsInput {
    pub provider: String,
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub timeout: i64,
}

fn valid_provider(provider: &str) -> bool {
    matches!(provider, PROVIDER_OLLAMA | PROVIDER_KIMI)
}

fn row_to_provider_settings(row: &rusqlite::Row<'_>) -> rusqlite::Result<ProviderSettings> {
    Ok(ProviderSettings {
        base_url: row.get("base_url")?,
        api_key: row.get("api_key")?,
        model: row.get("model")?,
        timeout: row.get("timeout")?,
    })
}

/// 读取设置视图（两个 provider 各一份 + 当前 provider；缺失行用默认值补齐）。
pub fn get_settings(db: &Db) -> Result<SettingsView, AppError> {
    let conn = db.conn()?;
    let read_one = |provider: &str| -> Result<(ProviderSettings, bool), AppError> {
        let found = conn
            .query_row(
                "SELECT base_url, api_key, model, timeout, is_active
                 FROM settings WHERE provider = ?1",
                params![provider],
                |row| Ok((row_to_provider_settings(row)?, row.get::<_, i64>(4)? != 0)),
            )
            .optional()?;
        Ok(found.unwrap_or_else(|| (ProviderSettings::default_for(provider), false)))
    };
    let (ollama, _ollama_active) = read_one(PROVIDER_OLLAMA)?;
    let (kimi, kimi_active) = read_one(PROVIDER_KIMI)?;
    let active_provider = if kimi_active {
        PROVIDER_KIMI
    } else {
        PROVIDER_OLLAMA
    };
    Ok(SettingsView {
        active_provider: active_provider.to_string(),
        ollama,
        kimi,
    })
}

/// 当前使用中的 provider 及其配置（提取/模型清单/Token 预估统一入口）。
pub fn active_settings(db: &Db) -> Result<(String, ProviderSettings), AppError> {
    let view = get_settings(db)?;
    let cfg = if view.active_provider == PROVIDER_KIMI {
        view.kimi
    } else {
        view.ollama
    };
    Ok((view.active_provider, cfg))
}

fn validate_input(input: &SettingsInput) -> Result<(), AppError> {
    if !valid_provider(&input.provider) {
        return Err(AppError::InvalidInput(format!(
            "未知 LLM provider: {}（可选 {PROVIDER_OLLAMA}/{PROVIDER_KIMI}）",
            input.provider
        )));
    }
    if !input.base_url.trim().starts_with("http://")
        && !input.base_url.trim().starts_with("https://")
    {
        return Err(AppError::InvalidInput(
            "base_url 需以 http(s):// 开头".to_string(),
        ));
    }
    if input.provider == PROVIDER_KIMI && input.api_key.trim().is_empty() {
        return Err(AppError::InvalidInput(
            "KIMI-API 需要填写 API Key".to_string(),
        ));
    }
    if !(10..=3600).contains(&input.timeout) {
        return Err(AppError::InvalidInput(
            "timeout 需为 10–3600 的整数（秒）".to_string(),
        ));
    }
    Ok(())
}

/// 保存某 provider 的配置（保存即设为当前使用 provider），返回完整视图。
pub fn save_settings(db: &Db, input: &SettingsInput) -> Result<SettingsView, AppError> {
    validate_input(input)?;
    let updated_at = paths::now_display();
    let conn = db.conn()?;
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "INSERT INTO settings(provider, base_url, api_key, model, timeout,
                             is_active, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, 1, ?6)
         ON CONFLICT(provider) DO UPDATE SET
            base_url = excluded.base_url,
            api_key = excluded.api_key,
            model = excluded.model,
            timeout = excluded.timeout,
            is_active = 1,
            updated_at = excluded.updated_at",
        params![
            input.provider,
            input.base_url.trim(),
            input.api_key,
            input.model,
            input.timeout,
            updated_at
        ],
    )?;
    // is_active 互斥：其余 provider 一律置 0
    tx.execute(
        "UPDATE settings SET is_active = 0 WHERE provider != ?1",
        params![input.provider],
    )?;
    tx.commit()?;
    drop(conn);
    get_settings(db)
}

// ---------------------------------------------------------------------------
// 模型清单获取（设置弹窗「刷新」按钮；按传入配置而非库中配置）
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct LlmTestResult {
    pub ok: bool,
    pub message: String,
    pub models: Vec<String>,
}

impl LlmTestResult {
    fn fail(message: String) -> Self {
        LlmTestResult {
            ok: false,
            message,
            models: Vec::new(),
        }
    }
}

/// 本机地址（Ollama 常见部署）必须直连，绕过环境变量里的系统代理；
/// 远端 KIMI-API 保留 reqwest 默认的 env proxy 行为。
/// 模型清单获取的 HTTP 超时（秒）。
const FETCH_MODELS_TIMEOUT_SECS: u64 = 15;

pub(crate) fn is_loopback_base(base: &str) -> bool {
    let after_scheme = base
        .trim_start_matches("http://")
        .trim_start_matches("https://");
    after_scheme.starts_with("localhost")
        || after_scheme.starts_with("127.")
        || after_scheme.starts_with("[::1]")
}

/// 按给定配置获取模型清单；永不返回 Err，失败体现在 `ok=false + message`。
/// - ollama：GET `{base_url}/api/tags`，模型取 `.models[].name`
/// - kimi：GET `{base_url}/models`（Bearer api_key），模型取 `.data[].id`
pub async fn fetch_models(provider: &str, base_url: &str, api_key: &str) -> LlmTestResult {
    if !valid_provider(provider) {
        return LlmTestResult::fail(format!("未知 LLM provider: {provider}"));
    }
    let base = base_url.trim().trim_end_matches('/');
    if !base.starts_with("http://") && !base.starts_with("https://") {
        return LlmTestResult::fail(format!("base_url 需以 http(s):// 开头: {base}"));
    }

    let builder = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(FETCH_MODELS_TIMEOUT_SECS));
    let builder = if is_loopback_base(base) {
        builder.no_proxy()
    } else {
        builder
    };
    let client = match builder.build() {
        Ok(c) => c,
        Err(e) => return LlmTestResult::fail(format!("创建 HTTP 客户端失败: {e}")),
    };

    let (url, with_auth) = match provider {
        PROVIDER_OLLAMA => (format!("{base}/api/tags"), false),
        PROVIDER_KIMI => (format!("{base}/models"), true),
        _ => unreachable!(),
    };

    let mut req = client.get(&url);
    if with_auth && !api_key.is_empty() {
        req = req.bearer_auth(api_key);
    }

    let resp = match req.send().await {
        Ok(r) => r,
        Err(e) => return LlmTestResult::fail(format!("连接失败: {e}")),
    };
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        let snippet: String = body.chars().take(200).collect();
        return LlmTestResult::fail(format!("HTTP {status}: {snippet}"));
    }
    let body: serde_json::Value = match resp.json().await {
        Ok(v) => v,
        Err(e) => return LlmTestResult::fail(format!("响应不是合法 JSON: {e}")),
    };

    let models: Vec<String> = if provider == PROVIDER_OLLAMA {
        body.get("models")
            .and_then(|m| m.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|m| m.get("name").and_then(|n| n.as_str()).map(String::from))
                    .collect()
            })
            .unwrap_or_default()
    } else {
        body.get("data")
            .and_then(|d| d.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|m| m.get("id").and_then(|n| n.as_str()).map(String::from))
                    .collect()
            })
            .unwrap_or_default()
    };

    LlmTestResult {
        ok: true,
        message: format!("连接成功，已获取 {} 个模型", models.len()),
        models,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    fn input(provider: &str) -> SettingsInput {
        SettingsInput {
            provider: provider.to_string(),
            base_url: "http://localhost:11434".to_string(),
            api_key: String::new(),
            model: "qwen2.5:14b".to_string(),
            timeout: 300,
        }
    }

    #[test]
    fn get_settings_defaults_ollama_active() {
        let db = Db::open_in_memory().unwrap();
        let v = get_settings(&db).unwrap();
        assert_eq!(v.active_provider, PROVIDER_OLLAMA);
        assert_eq!(v.ollama.base_url, "http://localhost:11434");
        assert_eq!(v.kimi.base_url, "https://api.moonshot.cn/v1");
        assert_eq!(v.ollama.timeout, 300);
        // active_settings 一致
        let (p, cfg) = active_settings(&db).unwrap();
        assert_eq!(p, PROVIDER_OLLAMA);
        assert_eq!(cfg, v.ollama);
    }

    #[test]
    fn save_switches_active_and_keeps_both() {
        let db = Db::open_in_memory().unwrap();
        // 保存 ollama 配置
        let v1 = save_settings(&db, &input(PROVIDER_OLLAMA)).unwrap();
        assert_eq!(v1.active_provider, PROVIDER_OLLAMA);
        assert_eq!(v1.ollama.model, "qwen2.5:14b");
        // 保存 kimi 配置 → 激活切换，ollama 配置保留
        let mut i2 = input(PROVIDER_KIMI);
        i2.base_url = "https://api.moonshot.cn/v1".to_string();
        i2.api_key = "sk-x".to_string();
        i2.model = "kimi-k2.6".to_string();

        let v2 = save_settings(&db, &i2).unwrap();
        assert_eq!(v2.active_provider, PROVIDER_KIMI);
        assert_eq!(v2.kimi.api_key, "sk-x");
        assert_eq!(v2.ollama.model, "qwen2.5:14b", "另一 provider 配置应保留");
        // 再保存 ollama → 激活切回，kimi 配置仍在
        let v3 = save_settings(&db, &input(PROVIDER_OLLAMA)).unwrap();
        assert_eq!(v3.active_provider, PROVIDER_OLLAMA);
        assert_eq!(v3.kimi.model, "kimi-k2.6");
        // 再次保存 kimi（冲突更新路径）→ 激活必须跟随（回归锁：
        // ON CONFLICT 分支须一并置 is_active=1，否则激活位滞留旧值）
        let v4 = save_settings(&db, &i2).unwrap();
        assert_eq!(v4.active_provider, PROVIDER_KIMI);
        // active_settings 跟随切换
        let (p, _) = active_settings(&db).unwrap();
        assert_eq!(p, PROVIDER_KIMI);
    }

    #[test]
    fn save_settings_rejects_invalid_input() {
        let db = Db::open_in_memory().unwrap();
        let mut i = input("bogus");
        assert!(matches!(
            save_settings(&db, &i),
            Err(AppError::InvalidInput(_))
        ));
        i = input(PROVIDER_OLLAMA);
        i.base_url = "ftp://x".to_string();
        assert!(matches!(
            save_settings(&db, &i),
            Err(AppError::InvalidInput(_))
        ));
        i = input(PROVIDER_KIMI);
        i.api_key = "  ".to_string();
        assert!(matches!(
            save_settings(&db, &i),
            Err(AppError::InvalidInput(_))
        ));
        i = input(PROVIDER_OLLAMA);
        i.timeout = 0;
        assert!(matches!(
            save_settings(&db, &i),
            Err(AppError::InvalidInput(_))
        ));
    }

    /// 启动一个只回复固定 JSON 的一次性 HTTP 桩服务器，返回其 base_url。
    fn stub_http_server(body: &'static str, status: &'static str) -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf);
                let resp = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(resp.as_bytes());
            }
        });
        format!("http://127.0.0.1:{port}")
    }

    #[tokio::test]
    async fn fetch_models_ollama_parses_names() {
        let base = stub_http_server(
            r#"{"models":[{"name":"qwen2.5:14b"},{"name":"llama3.1:8b"}]}"#,
            "200 OK",
        );
        let r = fetch_models(PROVIDER_OLLAMA, &base, "").await;
        assert!(r.ok, "{}", r.message);
        assert_eq!(r.models, vec!["qwen2.5:14b", "llama3.1:8b"]);
        assert!(r.message.contains("2 个模型"), "{}", r.message);
    }

    #[tokio::test]
    async fn fetch_models_kimi_parses_data_ids() {
        let base = stub_http_server(
            r#"{"data":[{"id":"kimi-k2.6"},{"id":"kimi-k3"}]}"#,
            "200 OK",
        );
        let r = fetch_models(PROVIDER_KIMI, &base, "sk-x").await;
        assert!(r.ok, "{}", r.message);
        assert_eq!(r.models, vec!["kimi-k2.6", "kimi-k3"]);
    }

    #[tokio::test]
    async fn fetch_models_reports_http_error_and_refused() {
        let base = stub_http_server(r#"{"error":"unauthorized"}"#, "401 Unauthorized");
        let r = fetch_models(PROVIDER_KIMI, &base, "sk-x").await;
        assert!(!r.ok);
        assert!(r.message.contains("401"), "{}", r.message);

        let r = fetch_models(PROVIDER_OLLAMA, "http://127.0.0.1:1", "").await;
        assert!(!r.ok);
        assert!(r.message.contains("连接失败"), "{}", r.message);
    }
}
