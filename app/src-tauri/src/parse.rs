//! 解析业务（任务 3.3/3.4）：项目级 Parse 执行体（状态机 + parse_results 回写 +
//! 每页进度回调）与查询接口（parse_results 读取、ocr.json 读取、PDF 路径、受控文件字节）。

use std::path::{Path, PathBuf};

use pdfium_render::prelude::*;
use rusqlite::{params, OptionalExtension};
use serde::Serialize;

use crate::db::Db;
use crate::error::AppError;
use crate::ocr::{executor, models::OrtEngine};
use crate::paths;
use crate::projects;
use crate::queue::ProgressPayload;

#[derive(Debug, Clone, Serialize)]
pub struct ParseResultInfo {
    pub id: i64,
    pub source_file_id: i64,
    pub output_dir: String,
    pub json_path: String,
    pub page_count: i64,
    pub status: String,
    pub error_msg: String,
}

fn row_to_parse_result(row: &rusqlite::Row<'_>) -> rusqlite::Result<ParseResultInfo> {
    Ok(ParseResultInfo {
        id: row.get("id")?,
        source_file_id: row.get("source_file_id")?,
        output_dir: row.get("output_dir")?,
        json_path: row.get("json_path")?,
        page_count: row.get("page_count")?,
        status: row.get("status")?,
        error_msg: row.get("error_msg")?,
    })
}

/// 查询某源文件的解析结果记录（3.4）。
pub fn get_parse_result(db: &Db, source_file_id: i64) -> Result<ParseResultInfo, AppError> {
    let conn = db.conn()?;
    conn.query_row(
        "SELECT id, source_file_id, output_dir, json_path, page_count, status, error_msg
         FROM parse_results WHERE source_file_id = ?1",
        params![source_file_id],
        row_to_parse_result,
    )
    .optional()?
    .ok_or_else(|| AppError::NotFound(format!("解析结果不存在: source_file_id={source_file_id}")))
}

/// 源文件 PDF 的绝对路径（3.4，前端 pdf.js 定位用）。
pub fn read_source_pdf_path(db: &Db, source_file_id: i64) -> Result<String, AppError> {
    let conn = db.conn()?;
    let (relative_path, base_path, project_dir): (String, String, String) = conn
        .query_row(
            "SELECT sf.relative_path, p.base_path, p.project_dir
             FROM source_files sf
             JOIN upload_batches b ON b.id = sf.batch_id
             JOIN projects p ON p.id = b.project_id
             WHERE sf.id = ?1",
            params![source_file_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?
        .ok_or_else(|| AppError::NotFound(format!("源文件不存在: {source_file_id}")))?;
    let abs = Path::new(&base_path)
        .join(&project_dir)
        .join(&relative_path);
    if !abs.is_file() {
        return Err(AppError::NotFound(format!(
            "PDF 文件不存在: {}",
            abs.display()
        )));
    }
    Ok(abs.to_string_lossy().to_string())
}

/// 校验路径位于任一项目目录内（防目录遍历；3.4 的 read_file_bytes 用）。
fn ensure_under_any_project(db: &Db, path: &Path) -> Result<PathBuf, AppError> {
    let resolved = path
        .canonicalize()
        .map_err(|e| AppError::NotFound(format!("文件不存在或不可读: {} ({e})", path.display())))?;
    if !resolved.is_file() {
        return Err(AppError::InvalidInput(format!(
            "不是普通文件: {}",
            resolved.display()
        )));
    }
    for p in projects::list_projects(db)? {
        let root = p.full_path();
        if let Ok(root) = root.canonicalize() {
            if resolved.starts_with(&root) {
                return Ok(resolved);
            }
        }
    }
    Err(AppError::PathTraversal(path.display().to_string()))
}

/// 受控读取文件字节（3.4；前端转 Blob 给 pdf.js）。
pub fn read_file_bytes(db: &Db, path: &str) -> Result<Vec<u8>, AppError> {
    let resolved = ensure_under_any_project(db, Path::new(path))?;
    Ok(std::fs::read(&resolved)?)
}

// ---------------------------------------------------------------------------
// Parse 执行体（3.3）
// ---------------------------------------------------------------------------

struct PendingFile {
    id: i64,
    original_name: String,
    relative_path: String,
}

fn pending_source_files(db: &Db, project_id: i64) -> Result<Vec<PendingFile>, AppError> {
    let conn = db.conn()?;
    let mut stmt = conn.prepare(
        "SELECT sf.id, sf.original_name, sf.relative_path
         FROM source_files sf
         JOIN upload_batches b ON b.id = sf.batch_id
         WHERE b.project_id = ?1 AND sf.status = 'pending'
         ORDER BY sf.created_at ASC, sf.id ASC",
    )?;
    let rows = stmt
        .query_map(params![project_id], |row| {
            Ok(PendingFile {
                id: row.get(0)?,
                original_name: row.get(1)?,
                relative_path: row.get(2)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// 项目级 Parse 执行体（阻塞式，供队列 worker 在 spawn_blocking 中调用）。
/// 逐文件执行 3.2 管线；单文件失败记 error 继续；每页完成回调一次进度。
/// 全程写任务日志（任务目录 `任务日志.log` + `queue://log` 事件）。
/// stop 置位时在文件边界中断（当前文件完成后停止，已完成文件结果保留）。
/// 返回 (成功数, 失败数)。
pub fn run_project_parse_blocking(
    db: &Db,
    project_id: i64,
    engine: &mut OrtEngine,
    pdfium: &Pdfium,
    mut progress: impl FnMut(ProgressPayload),
    log: crate::task_log::LogSink<'_>,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> Result<(u32, u32), AppError> {
    let project = projects::get_project(db, project_id)?;
    let files = pending_source_files(db, project_id)?;
    if files.is_empty() {
        return Ok((0, 0));
    }
    log(
        "解析",
        &format!("解析任务开始：{} 个待解析文件", files.len()),
    );

    // 位次查询依赖：先为每个文件插入 parse_results(pending) 行
    {
        let conn = db.conn()?;
        for f in &files {
            conn.execute(
                "INSERT OR IGNORE INTO parse_results(source_file_id, status) VALUES (?1, 'pending')",
                params![f.id],
            )?;
        }
    }
    projects::set_project_status(db, project_id, "parsing")?;

    let project_root = project.full_path();
    let (mut ok, mut failed) = (0u32, 0u32);
    for f in &files {
        // 「全部停止」：文件边界中断（已完成文件结果保留，未开始文件保持待解析）
        if stop.load(std::sync::atomic::Ordering::SeqCst) {
            log("解析", "「全部停止」：解析任务中断，未解析文件保持待解析");
            break;
        }
        {
            let conn = db.conn()?;
            conn.execute(
                "UPDATE source_files SET status = 'parsing' WHERE id = ?1",
                params![f.id],
            )?;
            conn.execute(
                "UPDATE parse_results SET status = 'running', error_msg = ''
                 WHERE source_file_id = ?1",
                params![f.id],
            )?;
        }
        progress(ProgressPayload {
            project_id,
            kind: "parse".to_string(),
            file_id: Some(f.id),
            status: "running".to_string(),
            done: 0,
            total: 0,
            current_file: Some(f.original_name.clone()),
        });
        log("解析", &format!("开始解析：{}", f.original_name));

        let pdf_path = project_root.join(&f.relative_path);
        let stem = paths::safe_filename(&paths::split_filename(&f.original_name).0, 100);
        let out_dir = paths::ensure_under_project(
            &project_root,
            Path::new(paths::SUBDIR_PARSE).join(&stem).as_path(),
        )?;

        let file_id = f.id;
        let file_name = f.original_name.clone();
        let log_name = f.original_name.clone();
        let result =
            executor::parse_pdf_file(engine, pdfium, &pdf_path, &out_dir, &stem, |done, total| {
                progress(ProgressPayload {
                    project_id,
                    kind: "parse".to_string(),
                    file_id: Some(file_id),
                    status: "running".to_string(),
                    done,
                    total,
                    current_file: Some(file_name.clone()),
                });
                log("解析", &format!("{log_name}：第 {done}/{total} 页"));
            });

        // 日志必须在 conn guard 释放后调用：task_log 会再次取 DB 锁，
        // 同线程持锁重入 std Mutex 会自死锁（曾导致解析卡死、整窗未响应）
        let outcome_msg = {
            let conn = db.conn()?;
            match &result {
                Ok(out) => {
                    conn.execute(
                        "UPDATE parse_results SET status = 'done',
                         output_dir = ?2, json_path = ?3, page_count = ?4
                         WHERE source_file_id = ?1",
                        params![
                            file_id,
                            out_dir.to_string_lossy(),
                            out.json_path.to_string_lossy(),
                            out.page_count as i64
                        ],
                    )?;
                    conn.execute(
                        "UPDATE source_files SET status = 'done' WHERE id = ?1",
                        params![file_id],
                    )?;
                    ok += 1;
                    format!("解析完成：{}（{} 页）", f.original_name, out.page_count)
                }
                Err(e) => {
                    conn.execute(
                        "UPDATE parse_results SET status = 'error', error_msg = ?2
                         WHERE source_file_id = ?1",
                        params![file_id, e.to_string()],
                    )?;
                    conn.execute(
                        "UPDATE source_files SET status = 'error' WHERE id = ?1",
                        params![file_id],
                    )?;
                    failed += 1;
                    format!("解析失败：{}：{e}", f.original_name)
                }
            }
        };
        log("解析", &outcome_msg);
    }
    projects::set_project_status(db, project_id, if failed > 0 { "error" } else { "idle" })?;
    Ok((ok, failed))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::files;
    use crate::ocr::models;

    /// 真实 PDF 样本路径：环境变量 QUERYDOC_TEST_PDF 指定（未设置则跳过）。
    fn test_pdf() -> Option<String> {
        std::env::var("QUERYDOC_TEST_PDF")
            .ok()
            .filter(|p| std::path::Path::new(p).is_file())
    }

    /// 集成验证（任务 3.2/3.3）：真实模型 + 真实 PDF 完整跑一遍解析。
    /// 手动运行：`QUERYDOC_TEST_PDF=<样本路径> cargo test -- --ignored`
    ///（需要 resources 下的模型与运行时库）。
    #[test]
    #[ignore = "需要模型与运行时库，手动 cargo test -- --ignored 运行"]
    fn parse_test_pdf_end_to_end() {
        let Some(test_pdf) = test_pdf() else {
            eprintln!("[integration] 未设置 QUERYDOC_TEST_PDF，跳过");
            return;
        };
        let res = models::resolve_resources_dir().unwrap();
        let mut engine = models::OrtEngine::load_from(&res).unwrap();
        let pdfium = Pdfium::new(
            Pdfium::bind_to_library(models::pdfium_dylib(&res)).expect("绑定 pdfium 失败"),
        );

        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open_in_memory().unwrap();
        let project =
            projects::create_project(&db, tmp.path().to_str().unwrap(), "集成测试", "测试组")
                .unwrap();
        let up = files::upload_files(&db, project.id, &[test_pdf]).unwrap();
        let file_id = up.files[0].id;

        let mut events: Vec<(u32, u32)> = Vec::new();
        let t0 = std::time::Instant::now();
        // 与生产 task_log 一致取 DB 锁的 sink：覆盖"日志调用不得持锁重入"路径
        let log = |_: &str, _: &str| {
            let _ = projects::get_project(&db, project.id).unwrap();
        };
        let (ok, failed) = run_project_parse_blocking(
            &db,
            project.id,
            &mut engine,
            &pdfium,
            |p| {
                events.push((p.done, p.total));
            },
            &log,
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        )
        .unwrap();
        let parse_ms = t0.elapsed().as_secs_f64() * 1e3;
        eprintln!("[integration] 完整管线单文件耗时: {parse_ms:.0}ms");
        assert_eq!((ok, failed), (1, 0));
        assert!(events.contains(&(1, 1)), "应有每页进度回调: {events:?}");

        // parse_results 回写
        let info = get_parse_result(&db, file_id).unwrap();
        assert_eq!(info.status, "done");
        assert_eq!(info.page_count, 1);
        let project_after = projects::get_project(&db, project.id).unwrap();
        assert_eq!(project_after.status, "idle");

        // ocr.json schema 与内容
        let json_str = std::fs::read_to_string(&info.json_path).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json_str).unwrap();
        assert_eq!(v["version"], 1);
        assert_eq!(v["engine"], "pp-ocrv6");
        assert_eq!(v["lang"], "ch");
        assert_eq!(v["page_count"], 1);
        let page = &v["pages"][0];
        assert_eq!(page["page_idx"], 0);
        assert!(page["width"].as_u64().unwrap() > 1000);
        assert!(page["height"].as_u64().unwrap() > 1000);
        assert_eq!(page["rotation"], 0);
        let deskew = page["deskew_angle"].as_f64().unwrap();
        eprintln!("[integration] deskew_angle = {deskew:.3}°");
        assert!(
            (deskew + 2.5).abs() < 1.0,
            "deskew_angle 应 ≈ -2.5°（±1°），实际 {deskew}"
        );

        let blocks = page["blocks"].as_array().unwrap();
        assert!(!blocks.is_empty());
        eprintln!(
            "[integration] blocks = {}，首块 type = {}，text_flow 样例 = {}…",
            blocks.len(),
            blocks[0]["type"].as_str().unwrap_or(""),
            blocks[0]["text_flow"]
                .as_str()
                .unwrap_or("")
                .chars()
                .take(40)
                .collect::<String>()
        );
        // text_flow 含关键文本；首行 chars 非空
        let all_flow: String = blocks
            .iter()
            .map(|b| b["text_flow"].as_str().unwrap_or(""))
            .collect();
        assert!(
            all_flow.contains("佛山市禅城区住房城乡建设和水利局"),
            "text_flow 缺少关键文本"
        );
        let first_line_chars = blocks[0]["lines"][0]["chars"].as_array().unwrap();
        assert!(!first_line_chars.is_empty(), "首行 chars 应非空");
        // 坐标归一化范围
        let bbox = blocks[0]["bbox"].as_array().unwrap();
        for c in bbox {
            let v = c.as_i64().unwrap();
            assert!((0..=1000).contains(&v), "bbox 越界: {v}");
        }
        // flow_map 一致性：能还原各行文本
        let b1 = blocks
            .iter()
            .find(|b| b["flow_map"].as_array().is_some_and(|m| !m.is_empty()));
        if let Some(b) = b1 {
            let flow: Vec<char> = b["text_flow"].as_str().unwrap().chars().collect();
            let lines = b["lines"].as_array().unwrap();
            let map = b["flow_map"].as_array().unwrap();
            let line0_len = map[0]["offset"].as_u64().unwrap() as usize;
            let line0: String = flow[..line0_len].iter().collect();
            assert_eq!(line0, lines[0]["text"].as_str().unwrap());
        }

        // .md 同步生成
        let md_path = Path::new(&info.output_dir).join("测试1.md");
        assert!(md_path.is_file(), "md 不存在: {}", md_path.display());
        assert!(std::fs::read_to_string(&md_path)
            .unwrap()
            .contains("佛山市禅城区"));

        // 留样供人工检查（仅 --ignored 手动运行时产生）
        let sample_dir = std::env::temp_dir().join("querydoc_phase3_sample");
        std::fs::create_dir_all(&sample_dir).unwrap();
        std::fs::copy(&info.json_path, sample_dir.join("测试1_ocr.json")).unwrap();
        std::fs::copy(&md_path, sample_dir.join("测试1.md")).unwrap();
        eprintln!("[integration] 留样: {}", sample_dir.display());

        // read_file_bytes 受控读取
        let pdf_abs = read_source_pdf_path(&db, file_id).unwrap();
        let bytes = read_file_bytes(&db, &pdf_abs).unwrap();
        assert!(bytes.starts_with(b"%PDF-"));
        assert!(matches!(
            read_file_bytes(&db, "/etc/hosts"),
            Err(AppError::PathTraversal(_))
        ));
    }
}
