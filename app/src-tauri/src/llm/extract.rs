//! 提取业务（任务 4.3–4.5）：配置 CRUD、结果查询/编辑、坐标回写、项目级执行体。

use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::db::Db;
use crate::error::AppError;
use crate::llm::{
    self, client::LlmClient, CharLoc, ColumnSpec, DocContext, ExtractMode, ExtractionResult,
};
use crate::ocr::schema::{self, OcrJson};
use crate::parse;
use crate::paths;
use crate::projects;
use crate::queue::ProgressPayload;
use crate::settings;

// ---------------------------------------------------------------------------
// 提取配置（4.5）
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct ExtractConfigInfo {
    pub id: i64,
    pub project_id: i64,
    pub column_index: i64,
    pub keyword: String,
    /// 关键词定义（选填；空串=未设置）。
    pub definition: String,
    /// 信息类型（选填：""=未设置 | "text" | "number" | "date"）。
    pub info_type: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ExtractConfigInput {
    pub column_index: i64,
    pub keyword: String,
    #[serde(default)]
    pub definition: String,
    #[serde(default)]
    pub info_type: String,
}

/// 关键词定义长度上限（字符数；防提示词膨胀）。
pub const DEFINITION_MAX_CHARS: usize = 200;
/// 合法信息类型集合（""=未设置）。
const INFO_TYPES: [&str; 4] = ["", "text", "number", "date"];

pub fn get_extract_configs(db: &Db, project_id: i64) -> Result<Vec<ExtractConfigInfo>, AppError> {
    projects::get_project(db, project_id)?;
    let conn = db.conn()?;
    let mut stmt = conn.prepare(
        "SELECT id, project_id, column_index, keyword, definition, info_type, created_at
         FROM extract_configs WHERE project_id = ?1 ORDER BY column_index ASC",
    )?;
    let rows = stmt
        .query_map(params![project_id], |row| {
            Ok(ExtractConfigInfo {
                id: row.get("id")?,
                project_id: row.get("project_id")?,
                column_index: row.get("column_index")?,
                keyword: row.get("keyword")?,
                definition: row.get("definition")?,
                info_type: row.get("info_type")?,
                created_at: row.get("created_at")?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// 整批替换项目提取配置（事务）。column_index 仅作稳定标识与排序（≥2），
/// 不再对应导出表格的具体列位置（导出布局：A 列文件名，关键词列随配置顺序）。
/// 关键词定义/信息类型选填：缺省照常提取（仅提示词信息减少，准确度可能下降）。
pub fn save_extract_configs(
    db: &Db,
    project_id: i64,
    configs: &[ExtractConfigInput],
) -> Result<Vec<ExtractConfigInfo>, AppError> {
    projects::get_project(db, project_id)?;
    let mut seen = std::collections::HashSet::new();
    for c in configs {
        if c.column_index < 2 {
            return Err(AppError::InvalidInput(format!(
                "列索引需 ≥2（仅作稳定标识与排序）: {}",
                c.column_index
            )));
        }
        if c.keyword.trim().is_empty() {
            return Err(AppError::InvalidInput(format!(
                "第 {} 列关键词不能为空",
                c.column_index
            )));
        }
        if c.definition.chars().count() > DEFINITION_MAX_CHARS {
            return Err(AppError::InvalidInput(format!(
                "第 {} 列关键词定义超过 {} 字上限",
                c.column_index, DEFINITION_MAX_CHARS
            )));
        }
        if !INFO_TYPES.contains(&c.info_type.as_str()) {
            return Err(AppError::InvalidInput(format!(
                "第 {} 列信息类型非法: {}（可选 text/number/date）",
                c.column_index, c.info_type
            )));
        }
        if !seen.insert(c.column_index) {
            return Err(AppError::InvalidInput(format!(
                "列索引重复: {}",
                c.column_index
            )));
        }
    }
    let created_at = paths::now_display();
    let conn = db.conn()?;
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "DELETE FROM extract_configs WHERE project_id = ?1",
        params![project_id],
    )?;
    for c in configs {
        tx.execute(
            "INSERT INTO extract_configs(project_id, column_index, keyword, definition,
                                         info_type, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                project_id,
                c.column_index,
                c.keyword.trim(),
                c.definition.trim(),
                c.info_type.as_str(),
                created_at
            ],
        )?;
    }
    tx.commit()?;
    drop(conn);
    get_extract_configs(db, project_id)
}

// ---------------------------------------------------------------------------
// 提取结果查询 / 编辑（4.5）
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct ExtractResultInfo {
    pub id: i64,
    pub project_id: i64,
    pub source_file_id: i64,
    pub source_file_name: String,
    pub cell_data: Value,
    pub is_edited: bool,
    pub error_msg: String,
    pub created_at: String,
    pub updated_at: String,
}

fn row_to_extract_result(row: &rusqlite::Row<'_>) -> rusqlite::Result<ExtractResultInfo> {
    let cell_text: String = row.get("cell_data")?;
    Ok(ExtractResultInfo {
        id: row.get("id")?,
        project_id: row.get("project_id")?,
        source_file_id: row.get("source_file_id")?,
        source_file_name: row.get("source_file_name")?,
        cell_data: serde_json::from_str(&cell_text).unwrap_or_else(|_| json!({})),
        is_edited: row.get::<_, i64>("is_edited")? != 0,
        error_msg: row.get("error_msg")?,
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
    })
}

const EXTRACT_RESULT_SELECT: &str = "SELECT er.id, er.project_id, er.source_file_id,
            sf.original_name AS source_file_name, er.cell_data, er.is_edited,
            er.error_msg, er.created_at, er.updated_at
     FROM extract_results er
     JOIN source_files sf ON sf.id = er.source_file_id";

pub fn get_extract_results(db: &Db, project_id: i64) -> Result<Vec<ExtractResultInfo>, AppError> {
    projects::get_project(db, project_id)?;
    let conn = db.conn()?;
    let mut stmt = conn.prepare(&format!(
        "{EXTRACT_RESULT_SELECT} WHERE er.project_id = ?1 ORDER BY er.created_at ASC, er.id ASC"
    ))?;
    let rows = stmt
        .query_map(params![project_id], row_to_extract_result)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

fn get_extract_result_by_id(db: &Db, id: i64) -> Result<ExtractResultInfo, AppError> {
    let conn = db.conn()?;
    conn.query_row(
        &format!("{EXTRACT_RESULT_SELECT} WHERE er.id = ?1"),
        params![id],
        row_to_extract_result,
    )
    .optional()?
    .ok_or_else(|| AppError::NotFound(format!("提取结果不存在: {id}")))
}

/// 更新某行某列的 value（人工编辑）：is_edited=1、updated_at 刷新，suspect 清除
///（人工确认/修正后不再是存疑值，前端「疑」转「改」），其余字段不动。
/// 列不存在时创建骨架 cell（仅 value，其余空）——前端编辑任意格子都安全。
pub fn update_extract_cell(
    db: &Db,
    id: i64,
    col_index: i64,
    value: &str,
) -> Result<ExtractResultInfo, AppError> {
    let info = get_extract_result_by_id(db, id)?;
    let mut cell = info.cell_data.clone();
    let key = col_index.to_string();
    let entry = cell
        .as_object_mut()
        .ok_or_else(|| AppError::Internal("cell_data 不是 JSON 对象".into()))?;
    let col = entry.entry(key).or_insert_with(empty_cell_json);
    if let Some(obj) = col.as_object_mut() {
        obj.insert("value".to_string(), json!(value));
        obj.insert("suspect".to_string(), json!(false));
    }
    let now = paths::now_display();
    db.conn()?.execute(
        "UPDATE extract_results SET cell_data = ?2, is_edited = 1, updated_at = ?3 WHERE id = ?1",
        params![id, cell.to_string(), now],
    )?;
    get_extract_result_by_id(db, id)
}

/// 空 quote（未找到）列的 cell 骨架。
fn empty_cell_json() -> Value {
    json!({
        "value": "",
        "quote": "",
        "reasoning": "",
        "page_idx": null,
        "bbox": null,
        "fragments": [],
        "char_spans": [],
        "confidence": 0,
        "suspect": false
    })
}

// ---------------------------------------------------------------------------
// 坐标回写（4.4）：命中区间 → page_idx + char_spans / bbox / fragments / confidence
// ---------------------------------------------------------------------------

/// 由命中区间与文档上下文构建 cell JSON。
/// 几何信息（page_idx/bbox/fragments/char_spans）取首个命中页；
/// 跨页命中的后续页字符忽略几何（v1 简化，见汇报遗留）。
fn build_cell(col: &llm::ColumnOutcome, ocr: &OcrJson, ctx: &DocContext) -> Value {
    let Some(span) = col.span else {
        // 无命中区间：含「未找到」（value 空）与「存疑」（value 非空但 quote
        // 未命中/为空）两种。存疑列 value 照常返回单元格，仅 suspect 标记。
        let mut cell = empty_cell_json();
        cell["value"] = json!(col.value);
        cell["quote"] = json!(col.quote);
        cell["reasoning"] = json!(col.reasoning);
        cell["suspect"] = json!(col.suspect);
        return cell;
    };

    // 命中区间逐字符 → (page_idx, 行索引, 字符 bbox, 行 score)
    struct Hit {
        page_idx: usize,
        line: usize,
        bbox: [i32; 4],
        c: String,
        line_score: f64,
    }
    let mut hits: Vec<Hit> = Vec::new();
    for idx in span.start..span.end {
        let Some(CharLoc {
            page_idx,
            block,
            offset,
        }) = ctx.locs.get(idx).copied().flatten()
        else {
            continue; // 页标记/块间换行
        };
        let Some(page) = ocr.pages.iter().find(|p| p.page_idx == page_idx) else {
            continue;
        };
        let Some(block) = page.blocks.get(block) else {
            continue;
        };
        let total = block.text_flow.chars().count();
        let Some((line_idx, char_off)) = schema::locate(&block.flow_map, total, offset) else {
            continue;
        };
        let Some(line) = block.lines.get(line_idx) else {
            continue;
        };
        let Some(ch) = line.chars.get(char_off) else {
            continue;
        };
        hits.push(Hit {
            page_idx,
            line: line_idx,
            bbox: ch.bbox,
            c: ch.c.clone(),
            line_score: line.score.map(|s| s as f64).unwrap_or(1.0),
        });
    }
    if hits.is_empty() {
        // 理论上罕见（匹配层命中但字符映射缺失），降级为空 cell 但保留 value/quote
        let mut cell = empty_cell_json();
        cell["value"] = json!(col.value);
        cell["quote"] = json!(col.quote);
        cell["reasoning"] = json!(col.reasoning);
        cell["suspect"] = json!(col.suspect);
        return cell;
    }

    let first_page = hits[0].page_idx;
    let page_hits: Vec<&Hit> = hits.iter().filter(|h| h.page_idx == first_page).collect();

    // fragments：按物理行分组的合并矩形（保持行序；命中字符在行内连续）
    let mut fragments: Vec<[i32; 4]> = Vec::new();
    let mut cur_line = usize::MAX;
    for h in &page_hits {
        if h.line != cur_line {
            fragments.push(h.bbox);
            cur_line = h.line;
        } else if let Some(f) = fragments.last_mut() {
            f[0] = f[0].min(h.bbox[0]);
            f[1] = f[1].min(h.bbox[1]);
            f[2] = f[2].max(h.bbox[2]);
            f[3] = f[3].max(h.bbox[3]);
        }
    }

    // bbox：外包矩形
    let mut bbox = page_hits[0].bbox;
    for h in &page_hits[1..] {
        bbox[0] = bbox[0].min(h.bbox[0]);
        bbox[1] = bbox[1].min(h.bbox[1]);
        bbox[2] = bbox[2].max(h.bbox[2]);
        bbox[3] = bbox[3].max(h.bbox[3]);
    }

    let char_spans: Vec<Value> = page_hits
        .iter()
        .map(|h| json!({"c": h.c, "bbox": h.bbox}))
        .collect();
    let avg_score = page_hits.iter().map(|h| h.line_score).sum::<f64>() / page_hits.len() as f64;
    let confidence = span.tier.factor() * avg_score;

    json!({
        "value": col.value,
        "quote": col.quote,
        "reasoning": col.reasoning,
        "page_idx": first_page,
        "bbox": bbox,
        "fragments": fragments,
        "char_spans": char_spans,
        "confidence": (confidence * 1000.0).round() / 1000.0,
        "suspect": col.suspect
    })
}

fn build_cell_data(result: &ExtractionResult, ocr: &OcrJson, ctx: &DocContext) -> Value {
    let mut map = serde_json::Map::new();
    for col in &result.columns {
        map.insert(col.column_index.to_string(), build_cell(col, ocr, ctx));
    }
    Value::Object(map)
}

// ---------------------------------------------------------------------------
// 项目级执行体（4.4，队列 Extract 臂调用）
// ---------------------------------------------------------------------------

/// 单文件硬预算在 settings.timeout 上的余量（秒）。
const BUDGET_SLACK_SECS: u64 = 60;
/// 「全部停止」标记的轮询间隔（毫秒）。
const STOP_POLL_MS: u64 = 200;
/// Token 预估请求的 HTTP 超时（秒，与提取超时不联动——预估是轻快接口）。
const ESTIMATE_TIMEOUT_SECS: u64 = 60;

struct EligibleFile {
    id: i64,
    original_name: String,
}

/// 待提取文件：解析 done 且（无提取行 或 提取行为失败行）。
fn eligible_files(db: &Db, project_id: i64) -> Result<Vec<EligibleFile>, AppError> {
    let conn = db.conn()?;
    let mut stmt = conn.prepare(
        "SELECT sf.id, sf.original_name
         FROM source_files sf
         JOIN upload_batches b ON b.id = sf.batch_id
         JOIN parse_results pr ON pr.source_file_id = sf.id AND pr.status = 'done'
         LEFT JOIN extract_results er ON er.project_id = ?1 AND er.source_file_id = sf.id
         WHERE b.project_id = ?1 AND (er.id IS NULL OR er.error_msg != '')
         ORDER BY sf.created_at ASC, sf.id ASC",
    )?;
    let rows = stmt
        .query_map(params![project_id], |row| {
            Ok(EligibleFile {
                id: row.get(0)?,
                original_name: row.get(1)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

#[allow(clippy::too_many_arguments)]
async fn extract_one_file(
    db: &Db,
    project_id: i64,
    file: &EligibleFile,
    client: &LlmClient,
    cols: &[ColumnSpec],
    max_retries: i64,
    provider: &str,
    log: crate::task_log::LogSink<'_>,
) -> Result<(), AppError> {
    let t0 = std::time::Instant::now();
    let info = parse::get_parse_result(db, file.id)?;
    let json_str = std::fs::read_to_string(&info.json_path)?;
    let ocr: OcrJson = serde_json::from_str(&json_str)
        .map_err(|e| AppError::Internal(format!("ocr.json 解析失败: {e}")))?;
    let ctx = llm::build_context(&ocr);
    let client_log = |m: &str| log("提取", m);
    let result = match llm::extract_document(
        client,
        &ctx,
        cols,
        max_retries,
        ExtractMode::WithCitation,
        Some(&client_log),
    )
    .await
    {
        Ok(r) => r,
        Err(f) => {
            // 失败同样留档 LLM 原始输出（2-提取结果集/<主体>.llm.json，含错误摘要）
            write_llm_failure_json(db, project_id, file, provider, client.model(), &f)?;
            return Err(AppError::Llm(f.summary));
        }
    };
    let cell_data = build_cell_data(&result, &ocr, &ctx);
    // 提取过程中被删除的关键词：其返回内容直接抛弃（任务启动时的配置快照
    // 只管提交内容，落库以当前配置为准）
    let current = get_extract_configs(db, project_id)?;
    let keep: std::collections::HashSet<i64> = current.iter().map(|c| c.column_index).collect();
    let mut cell_data = cell_data;
    let mut dropped = 0usize;
    if let Some(map) = cell_data.as_object_mut() {
        let before = map.len();
        map.retain(|k, _| k.parse::<i64>().is_ok_and(|i| keep.contains(&i)));
        dropped = before - map.len();
    }
    if dropped > 0 {
        log(
            "提取",
            &format!(
                "{}：{} 列关键词已在提取中被删除，其内容已抛弃",
                file.original_name, dropped
            ),
        );
    }
    let suspect_count = cell_data
        .as_object()
        .map(|m| {
            m.values()
                .filter(|c| c["suspect"].as_bool().unwrap_or(false))
                .count()
        })
        .unwrap_or(0);
    let now = paths::now_display();
    db.conn()?.execute(
        "INSERT INTO extract_results(project_id, source_file_id, cell_data,
                                    is_edited, error_msg, created_at, updated_at)
         VALUES (?1, ?2, ?3, 0, '', ?4, ?4)
         ON CONFLICT(project_id, source_file_id) DO UPDATE SET
            cell_data = excluded.cell_data,
            is_edited = 0,
            error_msg = '',
            updated_at = excluded.updated_at",
        params![project_id, file.id, cell_data.to_string(), now],
    )?;
    // LLM 原始输出分文件落盘留档（2-提取结果集/<文件名主体>.llm.json）
    write_llm_json(
        db,
        project_id,
        file,
        provider,
        client.model(),
        &result,
        &cell_data,
    )?;
    log(
        "提取",
        &format!(
            "提取完成：{}（{} 档，存疑 {} 列，耗时 {:.1}s）",
            file.original_name,
            result.tier.name(),
            suspect_count,
            t0.elapsed().as_secs_f64()
        ),
    );
    Ok(())
}

/// llm.json 留档路径：`2-提取结果集/<文件名主体>.llm.json`（项目内前缀校验）。
fn llm_json_path(
    db: &Db,
    project_id: i64,
    file: &EligibleFile,
) -> Result<std::path::PathBuf, AppError> {
    let project = projects::get_project(db, project_id)?;
    let stem = paths::safe_filename(&paths::split_filename(&file.original_name).0, 100);
    paths::ensure_under_project(
        &project.full_path(),
        std::path::Path::new(paths::SUBDIR_EXTRACT)
            .join(format!("{stem}.llm.json"))
            .as_path(),
    )
}

fn write_json_file(path: &std::path::Path, doc: Value) -> Result<(), AppError> {
    let text = serde_json::to_string_pretty(&doc)
        .map_err(|e| AppError::Internal(format!("llm.json 序列化失败: {e}")))?;
    std::fs::write(path, text)?;
    Ok(())
}

/// 提取成功后把 LLM 原始输出与结构化结果分文件落盘留档：
/// `2-提取结果集/<文件名主体>.llm.json`（raw_response + cell_data）。
fn write_llm_json(
    db: &Db,
    project_id: i64,
    file: &EligibleFile,
    provider: &str,
    model: &str,
    result: &ExtractionResult,
    cell_data: &Value,
) -> Result<(), AppError> {
    let path = llm_json_path(db, project_id, file)?;
    let doc = json!({
        "file_name": file.original_name,
        "provider": provider,
        "model": model,
        "tier": result.tier.name(),
        "status": "done",
        "created_at": paths::now_display(),
        "raw_response": result.raw,
        "cell_data": cell_data,
    });
    write_json_file(&path, doc)
}

/// 提取失败同样落档 LLM 原始输出（无论成败均留档）：
/// error 为失败摘要，raw_response 为模型原始输出/HTTP 错误响应体（无则 null）。
fn write_llm_failure_json(
    db: &Db,
    project_id: i64,
    file: &EligibleFile,
    provider: &str,
    model: &str,
    failure: &llm::ExtractFailure,
) -> Result<(), AppError> {
    let path = llm_json_path(db, project_id, file)?;
    let doc = json!({
        "file_name": file.original_name,
        "provider": provider,
        "model": model,
        "status": "error",
        "created_at": paths::now_display(),
        "error": failure.summary,
        "raw_response": failure.raw,
    });
    write_json_file(&path, doc)?;
    Ok(())
}

/// 项目级 Extract 执行体（async，网络为主）。单文件失败落 error 行并继续，
/// 且无论成败 LLM 原始输出均留档 2-提取结果集 llm.json（失败含 error 摘要）。
/// 返回 (成功数, 失败数)。
/// 串行保证：逐文件 await，且每文件内部仅 1 次 LLM 请求（单次提交策略，
/// 不降级不重试）——同时最多 1 个 LLM 请求在飞，适配限制并发的 Kimi API 平台。
/// 每文件 1 次逻辑提交（max_retries=0，不再 reask）；单文件硬预算
/// = settings.timeout + 60s，超时 drop future 强制中止在飞请求。
pub async fn run_project_extract(
    db: &Db,
    project_id: i64,
    mut progress: impl FnMut(ProgressPayload),
    log: crate::task_log::LogSink<'_>,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> Result<(u32, u32), AppError> {
    projects::get_project(db, project_id)?;
    let configs = get_extract_configs(db, project_id)?;
    if configs.is_empty() {
        return Err(AppError::InvalidInput(
            "未配置提取关键词，请先保存提取列配置".to_string(),
        ));
    }
    let (provider, st) = settings::active_settings(db)?;
    if st.model.trim().is_empty() {
        return Err(AppError::InvalidInput(
            "未配置 LLM 模型，请先在设置页选择模型".to_string(),
        ));
    }
    let client = LlmClient::from_settings(&provider, &st)?;
    let cols: Vec<ColumnSpec> = configs
        .iter()
        .map(|c| ColumnSpec {
            column_index: c.column_index,
            keyword: c.keyword.clone(),
            definition: c.definition.clone(),
            info_type: c.info_type.clone(),
        })
        .collect();

    let files = eligible_files(db, project_id)?;
    if files.is_empty() {
        return Ok((0, 0));
    }
    projects::set_project_status(db, project_id, "extracting")?;
    log(
        "提取",
        &format!(
            "提取任务开始：{} 个待提取文件，{} 个关键词，模型 {}（{}）",
            files.len(),
            cols.len(),
            st.model,
            provider
        ),
    );

    // 单文件硬预算：唯一的 LLM 请求与坐标回写都在同一预算内
    let budget = std::time::Duration::from_secs(st.timeout.max(1) as u64 + BUDGET_SLACK_SECS);
    let total = files.len() as u32;
    let (mut ok, mut failed) = (0u32, 0u32);
    for (i, f) in files.iter().enumerate() {
        // 「全部停止」：文件边界中断（未提取文件保持待提取，不落失败行）
        if stop.load(std::sync::atomic::Ordering::SeqCst) {
            log("提取", "「全部停止」：提取任务中断，未提取文件保持待提取");
            break;
        }
        progress(ProgressPayload {
            project_id,
            kind: "extract".to_string(),
            file_id: Some(f.id),
            status: "running".to_string(),
            done: i as u32,
            total,
            current_file: Some(f.original_name.clone()),
        });
        log("提取", &format!("开始提取：{}", f.original_name));
        // 提取次数固定 1 次（max_retries=0，不再 reask）；超时强制中止；
        // 「全部停止」置位时强制中止在飞请求
        let stop_wait = stop.clone();
        let result = tokio::select! {
            r = tokio::time::timeout(
                budget,
                extract_one_file(db, project_id, f, &client, &cols, 0, &provider, log),
            ) => {
                match r {
                    Ok(r) => r,
                    Err(_) => Err(AppError::Llm(format!(
                        "提取超时（>{} 秒），已强制中止",
                        budget.as_secs()
                    ))),
                }
            }
            _ = async {
                while !stop_wait.load(std::sync::atomic::Ordering::SeqCst) {
                    tokio::time::sleep(std::time::Duration::from_millis(STOP_POLL_MS)).await;
                }
            } => {
                log("提取", &format!("「全部停止」：强制中止 {} 的在飞请求", f.original_name));
                break;
            }
        };
        let (file_status, error_msg) = match &result {
            Ok(()) => {
                ok += 1;
                ("done", None)
            }
            Err(e) => {
                failed += 1;
                log("提取", &format!("提取失败：{}：{e}", f.original_name));
                ("error", Some(e.to_string()))
            }
        };
        if let Some(msg) = &error_msg {
            // 失败行落库（error_msg 供 queue_status 展示与失败重提）
            let now = paths::now_display();
            db.conn()?.execute(
                "INSERT INTO extract_results(project_id, source_file_id, cell_data,
                                            is_edited, error_msg, created_at, updated_at)
                 VALUES (?1, ?2, '{}', 0, ?3, ?4, ?4)
                 ON CONFLICT(project_id, source_file_id) DO UPDATE SET
                    error_msg = excluded.error_msg,
                    updated_at = excluded.updated_at",
                params![project_id, f.id, msg, now],
            )?;
        }
        progress(ProgressPayload {
            project_id,
            kind: "extract".to_string(),
            file_id: Some(f.id),
            status: file_status.to_string(),
            done: (i + 1) as u32,
            total,
            current_file: Some(f.original_name.clone()),
        });
    }
    projects::set_project_status(db, project_id, if failed > 0 { "error" } else { "idle" })?;
    Ok((ok, failed))
}

// ---------------------------------------------------------------------------
// Token 预估（进度面板「预估Token」列，仅 KIMI-API）
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct TokenEstimate {
    pub file_id: i64,
    /// 估算的提取输入 Token 数；失败为 None（error 带原因）
    pub tokens: Option<u64>,
    pub error: Option<String>,
}

/// 逐文件估算提取输入 Token 数：与提取完全相同的 system+user 提示词（纯文本），
/// POST {base}/tokenizers/estimate-token-count（Kimi 兼容接口）。单文件失败不影响其余。
/// 每文件估算完成即落库 source_files.estimated_tokens（成功写数值，失败置 NULL
/// 清掉旧值）；该列随文件删除连带清除，重新点击按钮即重新请求并覆盖刷新。
pub async fn estimate_tokens(
    db: &Db,
    project_id: i64,
    log: crate::task_log::LogSink<'_>,
) -> Result<Vec<TokenEstimate>, AppError> {
    projects::get_project(db, project_id)?;
    let configs = get_extract_configs(db, project_id)?;
    if configs.is_empty() {
        return Err(AppError::InvalidInput(
            "未配置提取关键词，请先保存提取列配置".to_string(),
        ));
    }
    let (provider, st) = settings::active_settings(db)?;
    if provider != settings::PROVIDER_KIMI {
        return Err(AppError::InvalidInput(
            "Token 预估仅支持 KIMI-API（Ollama 无该接口）".to_string(),
        ));
    }
    if st.model.trim().is_empty() {
        return Err(AppError::InvalidInput(
            "未配置 LLM 模型，请先在设置页选择模型".to_string(),
        ));
    }
    let cols: Vec<ColumnSpec> = configs
        .iter()
        .map(|c| ColumnSpec {
            column_index: c.column_index,
            keyword: c.keyword.clone(),
            definition: c.definition.clone(),
            info_type: c.info_type.clone(),
        })
        .collect();

    // 全部已解析文件（无论是否已提取）
    let files: Vec<EligibleFile> = {
        let conn = db.conn()?;
        let mut stmt = conn.prepare(
            "SELECT sf.id, sf.original_name
             FROM source_files sf
             JOIN upload_batches b ON b.id = sf.batch_id
             JOIN parse_results pr ON pr.source_file_id = sf.id AND pr.status = 'done'
             WHERE b.project_id = ?1
             ORDER BY sf.created_at ASC, sf.id ASC",
        )?;
        let rows = stmt
            .query_map(params![project_id], |row| {
                Ok(EligibleFile {
                    id: row.get(0)?,
                    original_name: row.get(1)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        rows
    };

    let base = st.base_url.trim().trim_end_matches('/');
    let url = format!("{base}/tokenizers/estimate-token-count");
    let builder =
        reqwest::Client::builder().timeout(std::time::Duration::from_secs(ESTIMATE_TIMEOUT_SECS));
    let builder = if settings::is_loopback_base(base) {
        builder.no_proxy()
    } else {
        builder
    };
    let http = builder
        .build()
        .map_err(|e| AppError::Llm(format!("创建 HTTP 客户端失败: {e}")))?;

    let mut out = Vec::with_capacity(files.len());
    for f in &files {
        match estimate_one_file(&http, &url, &st, db, f, &cols).await {
            Ok(n) => {
                persist_estimate(db, f.id, Some(n))?;
                log("提取", &format!("预估 Token：{} → {}", f.original_name, n));
                out.push(TokenEstimate {
                    file_id: f.id,
                    tokens: Some(n),
                    error: None,
                });
            }
            Err(e) => {
                persist_estimate(db, f.id, None)?;
                let msg = e.to_string();
                log(
                    "提取",
                    &format!("预估 Token 失败：{}：{msg}", f.original_name),
                );
                out.push(TokenEstimate {
                    file_id: f.id,
                    tokens: None,
                    error: Some(msg),
                });
            }
        }
    }
    Ok(out)
}

/// 预估值落库（成功写数值，失败置 NULL 清掉旧值——列始终反映最近一次估算结果）。
/// 值随 source_files 行删除连带清除，无需级联代码。
fn persist_estimate(db: &Db, file_id: i64, tokens: Option<u64>) -> Result<(), AppError> {
    db.conn()?.execute(
        "UPDATE source_files SET estimated_tokens = ?2 WHERE id = ?1",
        params![file_id, tokens.map(|t| t as i64)],
    )?;
    Ok(())
}

/// 单文件估算：组装与提取一致的 system+user 提示词并调用 estimate-token-count。
async fn estimate_one_file(
    http: &reqwest::Client,
    url: &str,
    st: &settings::ProviderSettings,
    db: &Db,
    f: &EligibleFile,
    cols: &[ColumnSpec],
) -> Result<u64, AppError> {
    let info = parse::get_parse_result(db, f.id)?;
    let json_str = std::fs::read_to_string(&info.json_path)?;
    let ocr: OcrJson = serde_json::from_str(&json_str)
        .map_err(|e| AppError::Internal(format!("ocr.json 解析失败: {e}")))?;
    let ctx = llm::build_context(&ocr);
    let body = serde_json::json!({
        "model": st.model,
        "messages": [
            {"role": "system", "content": llm::prompt::system_prompt(llm::ExtractMode::WithCitation)},
            {"role": "user", "content": llm::prompt::user_prompt(&ctx.text, cols, llm::ExtractMode::WithCitation)},
        ],
    });
    let mut req = http.post(url).json(&body);
    if !st.api_key.is_empty() {
        req = req.bearer_auth(&st.api_key);
    }
    let resp = req
        .send()
        .await
        .map_err(|e| AppError::Llm(format!("预估请求失败: {e}")))?;
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        let snippet: String = text.chars().take(200).collect();
        return Err(AppError::Llm(format!("预估失败 HTTP {status}: {snippet}")));
    }
    let v: Value = resp
        .json()
        .await
        .map_err(|e| AppError::Llm(format!("预估响应不是合法 JSON: {e}")))?;
    v["data"]["total_tokens"]
        .as_u64()
        .ok_or_else(|| AppError::Llm("预估响应缺少 data.total_tokens".to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::files;
    use crate::ocr::schema::{BlockJson, CharJson, FlowMapEntry, LineJson, PageJson};

    fn fixture() -> (tempfile::TempDir, Db, projects::Project, i64) {
        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open_in_memory().unwrap();
        let p = projects::create_project(&db, tmp.path().to_str().unwrap(), "提取项目", "测试组")
            .unwrap();
        let src = tmp.path().join("a.pdf");
        std::fs::write(&src, b"%PDF-1.7\n").unwrap();
        let up = files::upload_files(&db, p.id, &[src.to_str().unwrap().to_string()]).unwrap();
        (tmp, db, p, up.files[0].id)
    }

    #[test]
    fn save_and_get_configs_replace_batch() {
        let (_t, db, p, _f) = fixture();
        let saved = save_extract_configs(
            &db,
            p.id,
            &[
                ExtractConfigInput {
                    column_index: 3,
                    keyword: "甲方".into(),
                    ..Default::default()
                },
                ExtractConfigInput {
                    column_index: 2,
                    keyword: "合同编号".into(),
                    ..Default::default()
                },
            ],
        )
        .unwrap();
        assert_eq!(saved.len(), 2);
        assert_eq!(saved[0].column_index, 2); // 按列升序
                                              // 整批替换
        let saved2 = save_extract_configs(
            &db,
            p.id,
            &[ExtractConfigInput {
                column_index: 5,
                keyword: "金额".into(),
                ..Default::default()
            }],
        )
        .unwrap();
        assert_eq!(saved2.len(), 1);
        assert_eq!(saved2[0].keyword, "金额");
        // 非法：列 <2、空关键词、重复列
        for bad in [
            vec![ExtractConfigInput {
                column_index: 1,
                keyword: "x".into(),
                ..Default::default()
            }],
            vec![ExtractConfigInput {
                column_index: 2,
                keyword: "  ".into(),
                ..Default::default()
            }],
            vec![
                ExtractConfigInput {
                    column_index: 2,
                    keyword: "a".into(),
                    ..Default::default()
                },
                ExtractConfigInput {
                    column_index: 2,
                    keyword: "b".into(),
                    ..Default::default()
                },
            ],
        ] {
            assert!(save_extract_configs(&db, p.id, &bad).is_err());
        }
    }

    #[test]
    fn save_and_get_configs_with_definition_and_info_type() {
        let (_t, db, p, _f) = fixture();
        let saved = save_extract_configs(
            &db,
            p.id,
            &[
                ExtractConfigInput {
                    column_index: 2,
                    keyword: "金额".into(),
                    definition: "  指甲乙双方约定的含税总金额  ".into(),
                    info_type: "number".into(),
                },
                // 选填项全缺省：与旧版配置等价
                ExtractConfigInput {
                    column_index: 3,
                    keyword: "甲方".into(),
                    ..Default::default()
                },
            ],
        )
        .unwrap();
        assert_eq!(saved[0].definition, "指甲乙双方约定的含税总金额"); // trim 生效
        assert_eq!(saved[0].info_type, "number");
        assert_eq!(saved[1].definition, "");
        assert_eq!(saved[1].info_type, "");
        // 读回一致
        let got = get_extract_configs(&db, p.id).unwrap();
        assert_eq!(got[0].definition, saved[0].definition);
        assert_eq!(got[0].info_type, "number");
    }

    #[test]
    fn configs_reject_overlong_definition_and_bad_info_type() {
        let (_t, db, p, _f) = fixture();
        for bad in [
            // 定义超长（200 字上限）
            vec![ExtractConfigInput {
                column_index: 2,
                keyword: "金额".into(),
                definition: "长".repeat(DEFINITION_MAX_CHARS + 1),
                ..Default::default()
            }],
            // 非法信息类型
            vec![ExtractConfigInput {
                column_index: 2,
                keyword: "金额".into(),
                info_type: "bool".into(),
                ..Default::default()
            }],
        ] {
            assert!(save_extract_configs(&db, p.id, &bad).is_err());
        }
        // 边界：恰好 200 字合法
        let ok = vec![ExtractConfigInput {
            column_index: 2,
            keyword: "金额".into(),
            definition: "长".repeat(DEFINITION_MAX_CHARS),
            info_type: "date".into(),
        }];
        assert!(save_extract_configs(&db, p.id, &ok).is_ok());
    }

    #[test]
    fn update_cell_sets_edited_and_keeps_others() {
        let (_t, db, p, f) = fixture();
        let cell = json!({
            "2": {"value": "旧值", "quote": "引用", "page_idx": 0, "bbox": [1,2,3,4],
                  "fragments": [[1,2,3,4]], "char_spans": [], "confidence": 0.9, "suspect": true}
        });
        db.conn()
            .unwrap()
            .execute(
                "INSERT INTO extract_results(project_id, source_file_id, cell_data,
                                            is_edited, error_msg, created_at, updated_at)
                 VALUES (?1, ?2, ?3, 0, '', '2026-01-01 00:00:00', '2026-01-01 00:00:00')",
                params![p.id, f, cell.to_string()],
            )
            .unwrap();
        let id: i64 = db
            .conn()
            .unwrap()
            .query_row("SELECT id FROM extract_results", [], |r| r.get(0))
            .unwrap();

        let updated = update_extract_cell(&db, id, 2, "新值").unwrap();
        assert!(updated.is_edited);
        assert_eq!(updated.cell_data["2"]["value"], "新值");
        assert_eq!(updated.cell_data["2"]["quote"], "引用"); // 其余字段不动
        assert_eq!(updated.cell_data["2"]["confidence"], 0.9);
        assert_eq!(updated.cell_data["2"]["suspect"], json!(false)); // 人工编辑清除存疑
        assert_ne!(updated.updated_at, "2026-01-01 00:00:00");

        // 新列创建骨架
        let updated2 = update_extract_cell(&db, id, 7, "手工补充").unwrap();
        assert_eq!(updated2.cell_data["7"]["value"], "手工补充");
        assert_eq!(updated2.cell_data["7"]["page_idx"], Value::Null);
    }

    /// 手工构造两页 ocr.json：第 1 页两行（跨行「合同」），行 score 0.9/0.8。
    fn sample_ocr() -> OcrJson {
        let line0 = LineJson {
            text: "……本合".to_string(),
            bbox: [10, 10, 90, 20],
            chars: vec![
                CharJson {
                    c: "…".into(),
                    bbox: [10, 10, 14, 20],
                },
                CharJson {
                    c: "…".into(),
                    bbox: [14, 10, 18, 20],
                },
                CharJson {
                    c: "本".into(),
                    bbox: [18, 10, 22, 20],
                },
                CharJson {
                    c: "合".into(),
                    bbox: [22, 10, 26, 20],
                },
            ],
            amount_suspect: None,
            score: Some(0.9),
        };
        let line1 = LineJson {
            text: "同金额：1,234,567.89 元".to_string(),
            bbox: [10, 30, 90, 40],
            chars: vec![
                CharJson {
                    c: "同".into(),
                    bbox: [10, 30, 14, 40],
                },
                CharJson {
                    c: "金".into(),
                    bbox: [14, 30, 18, 40],
                },
            ],
            amount_suspect: None,
            score: Some(0.8),
        };
        OcrJson {
            version: 1,
            engine: "pp-ocrv6".into(),
            lang: "ch".into(),
            page_count: 1,
            pages: vec![PageJson {
                page_idx: 0,
                width: 1000,
                height: 1400,
                rotation: 0,
                deskew_angle: 0.0,
                blocks: vec![BlockJson {
                    kind: "paragraph".into(),
                    bbox: [10, 10, 90, 40],
                    text_flow: "……本合同金额：1,234,567.89 元".to_string(),
                    flow_map: vec![FlowMapEntry {
                        offset: 4,
                        line: 1,
                        char_start: 0,
                        char_len: 13,
                    }],
                    lines: vec![line0, line1],
                }],
            }],
        }
    }

    #[test]
    fn cell_geometry_cross_line_quote() {
        let ocr = sample_ocr();
        let ctx = llm::build_context(&ocr);
        // 「合同」跨行：精确命中 text_flow 偏移 2..4
        let col = llm::ColumnOutcome {
            column_index: 2,
            reasoning: "r".into(),
            quote: "合同".into(),
            value: "合同".into(),
            span: crate::llm::validate::find_quote(&ctx.text, &ctx.keep, "合同"),
            suspect: false,
        };
        assert!(col.span.is_some());
        let cell = build_cell(&col, &ocr, &ctx);
        assert_eq!(cell["page_idx"], 0);
        // fragments 两个物理行各一个矩形
        let frags = cell["fragments"].as_array().unwrap();
        assert_eq!(frags.len(), 2);
        assert_eq!(frags[0], json!([22, 10, 26, 20])); // 行0「合」
        assert_eq!(frags[1], json!([10, 30, 14, 40])); // 行1「同」
                                                       // bbox 为外包矩形
        assert_eq!(cell["bbox"], json!([10, 10, 26, 40]));
        // char_spans 逐字
        let spans = cell["char_spans"].as_array().unwrap();
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0]["c"], "合");
        assert_eq!(spans[1]["c"], "同");
        // confidence = 1.0(exact) × mean(0.9, 0.8) = 0.85
        assert_eq!(cell["confidence"], json!(0.85));
    }

    #[test]
    fn cell_empty_quote_has_null_geometry() {
        let ocr = sample_ocr();
        let ctx = llm::build_context(&ocr);
        let col = llm::ColumnOutcome {
            column_index: 2,
            reasoning: "未找到".into(),
            quote: "".into(),
            value: "".into(),
            span: None,
            suspect: false,
        };
        let cell = build_cell(&col, &ocr, &ctx);
        assert_eq!(cell["page_idx"], Value::Null);
        assert_eq!(cell["bbox"], Value::Null);
        assert_eq!(cell["confidence"], json!(0));
        assert_eq!(cell["fragments"], json!([]));
        assert_eq!(cell["suspect"], json!(false));
    }

    #[test]
    fn cell_suspect_without_geometry_carries_no_value() {
        // 存疑二选一：无命中区间的存疑列不落 value（编排层已清空），仅 suspect 标记
        let ocr = sample_ocr();
        let ctx = llm::build_context(&ocr);
        let col = llm::ColumnOutcome {
            column_index: 2,
            reasoning: "r".into(),
            quote: "不存在的引用".into(),
            value: "".into(),
            span: None,
            suspect: true,
        };
        let cell = build_cell(&col, &ocr, &ctx);
        assert_eq!(cell["value"], json!(""));
        assert_eq!(cell["suspect"], json!(true));
        assert_eq!(cell["page_idx"], Value::Null);
        assert_eq!(cell["confidence"], json!(0));
    }

    /// 串行锁定：项目级提取对 stub 平台的 HTTP 请求不得并发（同时在飞 ≤1），
    /// 且每文件恰好 1 次请求（max_retries=0，无 reask）。
    #[tokio::test]
    async fn run_project_extract_is_serial_and_single_attempt() {
        use std::io::{Read, Write};
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::{Arc, Mutex};

        // ---- stub LLM 平台：统计同时在飞连接数 ----
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let in_flight = Arc::new(AtomicUsize::new(0));
        let max_in_flight = Arc::new(AtomicUsize::new(0));
        let bodies: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        {
            let in_flight = in_flight.clone();
            let max_in_flight = max_in_flight.clone();
            let bodies = bodies.clone();
            std::thread::spawn(move || {
                for _ in 0..2 {
                    let Ok((mut stream, _)) = listener.accept() else {
                        return;
                    };
                    let in_flight = in_flight.clone();
                    let max_in_flight = max_in_flight.clone();
                    let bodies = bodies.clone();
                    std::thread::spawn(move || {
                        let cur = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                        max_in_flight.fetch_max(cur, Ordering::SeqCst);
                        let mut buf = vec![0u8; 4 * 1024 * 1024];
                        let n = stream.read(&mut buf).unwrap_or(0);
                        let req = String::from_utf8_lossy(&buf[..n]).to_string();
                        bodies
                            .lock()
                            .unwrap()
                            .push(req.split("\r\n\r\n").nth(1).unwrap_or("").to_string());
                        // 拉长在飞窗口：若客户端并发提交，此处必然观察到 >1
                        std::thread::sleep(std::time::Duration::from_millis(30));
                        let resp_body = json!({
                            "choices": [{"message": {"role": "assistant",
                                "content": "{\"col_2\":{\"reasoning\":\"r\",\"quote\":\"合同\",\"value\":\"合同\"}}"}}]
                        })
                        .to_string();
                        let resp = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{resp_body}",
                            resp_body.len()
                        );
                        let _ = stream.write_all(resp.as_bytes());
                        in_flight.fetch_sub(1, Ordering::SeqCst);
                    });
                }
            });
        }

        // ---- 任务夹具：2 个已解析文件（ocr.json 落盘）+ 关键词 + stub 设置 ----
        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open_in_memory().unwrap();
        let p =
            projects::create_project(&db, tmp.path().to_str().unwrap(), "串行", "测试组").unwrap();
        let mut file_ids = Vec::new();
        let mut paths = Vec::new();
        for name in ["a.pdf", "b.pdf"] {
            let fp = tmp.path().join(name);
            std::fs::write(&fp, b"%PDF-1.7\n").unwrap();
            paths.push(fp.to_str().unwrap().to_string());
        }
        let up = files::upload_files(&db, p.id, &paths).unwrap();
        for f in &up.files {
            file_ids.push(f.id);
        }
        let ocr_text = serde_json::to_string(&sample_ocr()).unwrap();
        for fid in &file_ids {
            let json_path = tmp.path().join(format!("ocr_{fid}.json"));
            std::fs::write(&json_path, &ocr_text).unwrap();
            db.conn()
                .unwrap()
                .execute(
                    "INSERT INTO parse_results(source_file_id, status, output_dir, json_path,
                                              page_count)
                     VALUES (?1, 'done', '', ?2, 1)",
                    params![fid, json_path.to_string_lossy()],
                )
                .unwrap();
        }
        save_extract_configs(
            &db,
            p.id,
            &[ExtractConfigInput {
                column_index: 2,
                keyword: "关键词".into(),
                ..Default::default()
            }],
        )
        .unwrap();
        settings::save_settings(
            &db,
            &settings::SettingsInput {
                provider: settings::PROVIDER_OLLAMA.to_string(),
                base_url: format!("http://127.0.0.1:{port}"),
                api_key: String::new(),
                model: "m".to_string(),
                timeout: 30,
            },
        )
        .unwrap();

        let (ok, failed) = run_project_extract(
            &db,
            p.id,
            |_| {},
            &|_, _| {},
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        )
        .await
        .unwrap();
        assert_eq!((ok, failed), (2, 0));
        assert_eq!(
            max_in_flight.load(Ordering::SeqCst),
            1,
            "LLM 请求必须逐笔串行"
        );
        assert_eq!(bodies.lock().unwrap().len(), 2, "每文件恰好 1 次请求");
        // 两行成功结果落库，quote「合同」精确命中 → 非 suspect
        let results = get_extract_results(&db, p.id).unwrap();
        assert_eq!(results.len(), 2);
        for r in &results {
            assert_eq!(r.error_msg, "");
            assert_eq!(r.cell_data["2"]["value"], json!("合同"));
            assert_eq!(r.cell_data["2"]["suspect"], json!(false));
        }
        // 项目状态回到 idle
        assert_eq!(projects::get_project(&db, p.id).unwrap().status, "idle");

        // LLM 原始输出落盘：2-提取结果集/<主体>.llm.json 含 raw_response + cell_data
        for stem in ["a", "b"] {
            let path = p
                .full_path()
                .join(paths::SUBDIR_EXTRACT)
                .join(format!("{stem}.llm.json"));
            assert!(path.is_file(), "llm.json 未落盘: {}", path.display());
            let doc: Value =
                serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            assert!(doc["raw_response"].as_str().unwrap().contains("合同"));
            assert_eq!(doc["cell_data"]["2"]["value"], json!("合同"));
            assert_eq!(doc["tier"], json!("json_schema"));
        }
        // 重新提取：llm.json 随结果行一并清理
        projects::reextract_files(&db, p.id, &file_ids).unwrap();
        for stem in ["a", "b"] {
            assert!(
                !p.full_path()
                    .join(paths::SUBDIR_EXTRACT)
                    .join(format!("{stem}.llm.json"))
                    .exists(),
                "reextract 后 llm.json 应删除"
            );
        }
    }

    /// 「全部停止」：flag 置位时不提取任何文件、不落行、项目状态回 idle。
    #[tokio::test]
    async fn run_project_extract_stops_before_first_file() {
        use std::sync::atomic::AtomicBool;
        use std::sync::Arc;

        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open_in_memory().unwrap();
        let p =
            projects::create_project(&db, tmp.path().to_str().unwrap(), "停止", "测试组").unwrap();
        let fp = tmp.path().join("a.pdf");
        std::fs::write(&fp, b"%PDF-1.7\n").unwrap();
        let up = files::upload_files(&db, p.id, &[fp.to_str().unwrap().to_string()]).unwrap();
        let fid = up.files[0].id;
        let ocr_text = serde_json::to_string(&sample_ocr()).unwrap();
        let json_path = tmp.path().join("ocr.json");
        std::fs::write(&json_path, &ocr_text).unwrap();
        db.conn()
            .unwrap()
            .execute(
                "INSERT INTO parse_results(source_file_id, status, output_dir, json_path,
                                          page_count)
                 VALUES (?1, 'done', '', ?2, 1)",
                params![fid, json_path.to_string_lossy()],
            )
            .unwrap();
        save_extract_configs(
            &db,
            p.id,
            &[ExtractConfigInput {
                column_index: 2,
                keyword: "关键词".into(),
                ..Default::default()
            }],
        )
        .unwrap();
        settings::save_settings(
            &db,
            &settings::SettingsInput {
                provider: settings::PROVIDER_OLLAMA.to_string(),
                base_url: "http://127.0.0.1:1".to_string(),
                api_key: String::new(),
                model: "m".to_string(),
                timeout: 30,
            },
        )
        .unwrap();

        let stop = Arc::new(AtomicBool::new(true));
        let (ok, failed) = run_project_extract(&db, p.id, |_| {}, &|_, _| {}, stop)
            .await
            .unwrap();
        assert_eq!((ok, failed), (0, 0), "停止置位后不应提取任何文件");
        assert!(
            get_extract_results(&db, p.id).unwrap().is_empty(),
            "停止中断不落任何结果行"
        );
        assert_eq!(projects::get_project(&db, p.id).unwrap().status, "idle");
    }

    /// 预估 Token 落库：成功写数值、失败置 NULL 清旧值；list_files 读回；
    /// 删除文件随行连带删除。
    #[tokio::test]
    async fn estimate_tokens_persists_per_file() {
        use std::io::{Read, Write};

        // ---- stub tokenizer 端点：第 1 个文件成功，第 2 个文件 500 ----
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let payloads = [
                (200, r#"{"data":{"total_tokens":1234}}"#.to_string()),
                (500, r#"{"error":"boom"}"#.to_string()),
            ];
            for (status, payload) in payloads {
                let Ok((mut stream, _)) = listener.accept() else {
                    return;
                };
                let mut buf = vec![0u8; 1024 * 1024];
                let _ = stream.read(&mut buf);
                let resp = format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
                    payload.len()
                );
                let _ = stream.write_all(resp.as_bytes());
            }
        });

        // ---- 夹具：2 个已解析文件（ocr.json 落盘）+ 关键词 + kimi 设置指向 stub ----
        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open_in_memory().unwrap();
        let p =
            projects::create_project(&db, tmp.path().to_str().unwrap(), "预估", "测试组").unwrap();
        let mut up_paths = Vec::new();
        for name in ["a.pdf", "b.pdf"] {
            let fp = tmp.path().join(name);
            std::fs::write(&fp, b"%PDF-1.7\n").unwrap();
            up_paths.push(fp.to_str().unwrap().to_string());
        }
        let up = files::upload_files(&db, p.id, &up_paths).unwrap();
        let ocr_text = serde_json::to_string(&sample_ocr()).unwrap();
        for f in &up.files {
            let json_path = tmp.path().join(format!("ocr_{}.json", f.id));
            std::fs::write(&json_path, &ocr_text).unwrap();
            db.conn()
                .unwrap()
                .execute(
                    "INSERT INTO parse_results(source_file_id, status, output_dir, json_path,
                                              page_count)
                     VALUES (?1, 'done', '', ?2, 1)",
                    params![f.id, json_path.to_string_lossy()],
                )
                .unwrap();
        }
        // b 文件预置旧预估值：失败重估应被清为 NULL
        let fid_a = up.files[0].id;
        let fid_b = up.files[1].id;
        db.conn()
            .unwrap()
            .execute(
                "UPDATE source_files SET estimated_tokens = 999 WHERE id = ?1",
                params![fid_b],
            )
            .unwrap();
        save_extract_configs(
            &db,
            p.id,
            &[ExtractConfigInput {
                column_index: 2,
                keyword: "关键词".into(),
                ..Default::default()
            }],
        )
        .unwrap();
        settings::save_settings(
            &db,
            &settings::SettingsInput {
                provider: settings::PROVIDER_KIMI.to_string(),
                base_url: format!("http://127.0.0.1:{port}"),
                api_key: "sk-x".to_string(),
                model: "m".to_string(),
                timeout: 30,
            },
        )
        .unwrap();

        let out = estimate_tokens(&db, p.id, &|_, _| {}).await.unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].tokens, Some(1234));
        assert!(out[1].tokens.is_none() && out[1].error.is_some());

        // 落库读回：成功文件得数值，失败文件旧值被清
        let listed = files::list_files(&db, p.id).unwrap();
        let by_id: std::collections::HashMap<i64, Option<i64>> =
            listed.iter().map(|f| (f.id, f.estimated_tokens)).collect();
        assert_eq!(by_id[&fid_a], Some(1234), "成功文件落库数值");
        assert_eq!(by_id[&fid_b], None, "失败文件清掉旧值（NULL）");

        // 删除文件连带删除（source_files 行删即预估消失；回收站操作注入空操作，
        // 本测试只关心行级联，磁盘产物由 delete_files 专项测试覆盖）
        projects::delete_files_with(&db, p.id, &[fid_a], |_| Ok(())).unwrap();
        let listed = files::list_files(&db, p.id).unwrap();
        assert!(listed.iter().all(|f| f.id != fid_a));
    }

    /// 提取失败同样留档 llm.json：status=error + error 摘要 + raw_response 原始输出。
    #[tokio::test]
    async fn extract_failure_writes_llm_json_with_raw() {
        use std::io::{Read, Write};

        // ---- stub LLM：200 但输出非 JSON（单文件 1 次请求即失败）----
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = vec![0u8; 1024 * 1024];
                let _ = stream.read(&mut buf);
                let resp_body = json!({
                    "choices": [{"message": {"role": "assistant", "content": "这不是 JSON 输出"},
                                 "finish_reason": "stop"}]
                })
                .to_string();
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{resp_body}",
                    resp_body.len()
                );
                let _ = stream.write_all(resp.as_bytes());
            }
        });

        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open_in_memory().unwrap();
        let p = projects::create_project(&db, tmp.path().to_str().unwrap(), "失败留档", "测试组")
            .unwrap();
        let fp = tmp.path().join("bad.pdf");
        std::fs::write(&fp, b"%PDF-1.7\n").unwrap();
        let up = files::upload_files(&db, p.id, &[fp.to_str().unwrap().to_string()]).unwrap();
        let fid = up.files[0].id;
        let ocr_text = serde_json::to_string(&sample_ocr()).unwrap();
        let json_path = tmp.path().join("ocr.json");
        std::fs::write(&json_path, &ocr_text).unwrap();
        db.conn()
            .unwrap()
            .execute(
                "INSERT INTO parse_results(source_file_id, status, output_dir, json_path,
                                          page_count)
                 VALUES (?1, 'done', '', ?2, 1)",
                params![fid, json_path.to_string_lossy()],
            )
            .unwrap();
        save_extract_configs(
            &db,
            p.id,
            &[ExtractConfigInput {
                column_index: 2,
                keyword: "关键词".into(),
                ..Default::default()
            }],
        )
        .unwrap();
        settings::save_settings(
            &db,
            &settings::SettingsInput {
                provider: settings::PROVIDER_OLLAMA.to_string(),
                base_url: format!("http://127.0.0.1:{port}"),
                api_key: String::new(),
                model: "m".to_string(),
                timeout: 30,
            },
        )
        .unwrap();

        let (ok, failed) = run_project_extract(
            &db,
            p.id,
            |_| {},
            &|_, _| {},
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        )
        .await
        .unwrap();
        assert_eq!((ok, failed), (0, 1));
        // 失败行落库
        let results = get_extract_results(&db, p.id).unwrap();
        assert_eq!(results.len(), 1);
        assert!(results[0].error_msg.contains("非合法 JSON"));
        // llm.json 失败留档：error + raw_response 原始输出
        let path = p
            .full_path()
            .join(paths::SUBDIR_EXTRACT)
            .join("bad.llm.json");
        assert!(path.is_file(), "失败也应落盘 llm.json");
        let doc: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(doc["status"], json!("error"));
        assert!(doc["error"].as_str().unwrap().contains("非合法 JSON"));
        assert_eq!(doc["raw_response"], json!("这不是 JSON 输出"));
        assert!(doc["cell_data"].is_null() || doc.get("cell_data").is_none());
    }

    /// 重新提取不累积文件：reextract 清旧档后重跑，llm.json 同路径重建为最新输出，
    /// 目录内该文件始终只有 1 个留档。
    #[tokio::test]
    async fn reextract_rebuilds_llm_json_without_accumulating() {
        use std::io::{Read, Write};

        // ---- stub LLM：两轮提取各 1 次应答（value 不同）----
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for content in [
                r#"{"col_2":{"reasoning":"r","quote":"合同","value":"合同"}}"#,
                r#"{"col_2":{"reasoning":"r","quote":"金额","value":"金额"}}"#,
            ] {
                let Ok((mut stream, _)) = listener.accept() else {
                    return;
                };
                let mut buf = vec![0u8; 1024 * 1024];
                let _ = stream.read(&mut buf);
                let resp_body = json!({
                    "choices": [{"message": {"role": "assistant", "content": content},
                                 "finish_reason": "stop"}]
                })
                .to_string();
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{resp_body}",
                    resp_body.len()
                );
                let _ = stream.write_all(resp.as_bytes());
            }
        });

        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open_in_memory().unwrap();
        let p =
            projects::create_project(&db, tmp.path().to_str().unwrap(), "覆盖", "测试组").unwrap();
        let fp = tmp.path().join("a.pdf");
        std::fs::write(&fp, b"%PDF-1.7\n").unwrap();
        let up = files::upload_files(&db, p.id, &[fp.to_str().unwrap().to_string()]).unwrap();
        let fid = up.files[0].id;
        let ocr_text = serde_json::to_string(&sample_ocr()).unwrap();
        let json_path = tmp.path().join("ocr.json");
        std::fs::write(&json_path, &ocr_text).unwrap();
        db.conn()
            .unwrap()
            .execute(
                "INSERT INTO parse_results(source_file_id, status, output_dir, json_path,
                                          page_count)
                 VALUES (?1, 'done', '', ?2, 1)",
                params![fid, json_path.to_string_lossy()],
            )
            .unwrap();
        save_extract_configs(
            &db,
            p.id,
            &[ExtractConfigInput {
                column_index: 2,
                keyword: "关键词".into(),
                ..Default::default()
            }],
        )
        .unwrap();
        settings::save_settings(
            &db,
            &settings::SettingsInput {
                provider: settings::PROVIDER_OLLAMA.to_string(),
                base_url: format!("http://127.0.0.1:{port}"),
                api_key: String::new(),
                model: "m".to_string(),
                timeout: 30,
            },
        )
        .unwrap();

        let dir = p.full_path().join(paths::SUBDIR_EXTRACT);
        let path = dir.join("a.llm.json");
        let stop = || std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));

        // 第 1 轮提取：落盘 value=合同
        let (ok, failed) = run_project_extract(&db, p.id, |_| {}, &|_, _| {}, stop())
            .await
            .unwrap();
        assert_eq!((ok, failed), (1, 0));
        let v1: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(v1["cell_data"]["2"]["value"], json!("合同"));

        // 重新提取：旧档清除后重跑
        projects::reextract_files(&db, p.id, &[fid]).unwrap();
        assert!(!path.exists(), "reextract 应清除旧 llm.json");
        let (ok, failed) = run_project_extract(&db, p.id, |_| {}, &|_, _| {}, stop())
            .await
            .unwrap();
        assert_eq!((ok, failed), (1, 0));

        // 同路径重建为最新输出，目录内仍只有 1 个 llm.json（不累积）
        let v2: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(v2["cell_data"]["2"]["value"], json!("金额"));
        let llm_files = std::fs::read_dir(&dir)
            .unwrap()
            .filter(|e| {
                e.as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .ends_with(".llm.json")
            })
            .count();
        assert_eq!(llm_files, 1, "重新提取不得累积 llm.json 文件");
    }
}

// ---------------------------------------------------------------------------
// A/B 决策门（任务 4.6）：纯 value 直出 vs reasoning+quote+reask
// 手动运行：cargo test llm::extract::tests_ab -- --ignored --nocapture
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests_ab {
    use super::*;
    use crate::settings::ProviderSettings;
    use std::path::Path;

    const SAMPLE_ENV: &str = "QUERYDOC_AB_SAMPLE";

    #[tokio::test]
    #[ignore = "A/B 决策门（4.6）：需要本机 Ollama + gemma4:latest"]
    async fn ab_direct_vs_citation_on_sample() {
        // 样本路径：环境变量 QUERYDOC_AB_SAMPLE 指定（缺省 temp_dir 下 Phase 3 集成测试的留样）
        let sample = std::env::var(SAMPLE_ENV).unwrap_or_else(|_| {
            std::env::temp_dir()
                .join("querydoc_phase3_sample/测试1_ocr.json")
                .to_string_lossy()
                .to_string()
        });
        if !Path::new(&sample).exists() {
            eprintln!("[ab] 样本缺失（{sample}），先跑 Phase 3 集成测试重建；本次跳过");
            return;
        }
        let ocr: OcrJson =
            serde_json::from_str(&std::fs::read_to_string(&sample).unwrap()).unwrap();
        let ctx = llm::build_context(&ocr);
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
            ColumnSpec {
                column_index: 4,
                keyword: "监控账号".into(),
                ..Default::default()
            },
        ];
        let st = ProviderSettings {
            base_url: "http://localhost:11434".to_string(),
            api_key: String::new(),
            model: "gemma4:latest".to_string(),
            timeout: 300,
        };
        let client = LlmClient::from_settings(settings::PROVIDER_OLLAMA, &st).unwrap();

        eprintln!(
            "[ab] 文档字符数: {}，模型: {}",
            ctx.text.chars().count(),
            client.model()
        );
        for mode in [ExtractMode::DirectValue, ExtractMode::WithCitation] {
            let label = if mode == ExtractMode::DirectValue {
                "A 直出"
            } else {
                "B 引用+reask"
            };
            let t0 = std::time::Instant::now();
            // A/B 决策门手动测试保留 reask 能力（生产路径固定 0）
            let result = llm::extract_document(&client, &ctx, &cols, 2, mode, None).await;
            let secs = t0.elapsed().as_secs_f64();
            match result {
                Ok(r) => {
                    eprintln!(
                        "[ab] {label}: tier={} attempts={} 耗时={secs:.1}s",
                        r.tier.name(),
                        r.attempts
                    );
                    for c in &r.columns {
                        let span_info = match &c.span {
                            Some(s) => format!("命中({})", s.tier.name()),
                            None => "未命中/空".to_string(),
                        };
                        eprintln!(
                            "[ab] {label} col_{} {}:\n  value = {:?}\n  quote = {:?}\n  span  = {span_info}",
                            c.column_index,
                            cols.iter().find(|k| k.column_index == c.column_index).unwrap().keyword,
                            c.value,
                            c.quote
                        );
                    }
                }
                Err(e) => {
                    eprintln!("[ab] {label}: 失败 {}（耗时 {secs:.1}s）", e.summary);
                    panic!("{label} 提取失败: {}", e.summary);
                }
            }
        }
    }
}
