//! 导出业务（Phase 6）：表格导出 xlsx、完整导出 zip。
//! - 表格导出：`2-提取结果集/extraction.xlsx`，A 列文件名 HYPERLINK，
//!   链接目标 = 源文件磁盘绝对路径（ensure_under_project 校验后取）；
//! - 完整导出：临时目录按唯一文件名复制 PDF（同名自动 _N 去重）+ 同结构 xlsx
//!   （A 列链接为相对路径 `./<文件名>`，正斜杠、禁止 file://），打包为
//!   `3-导出结果集/export-<YYYYMMDDHHMMSS>.zip`（xlsx 与 pdf 平铺同目录）。
//!   导出失败原因只进任务日志（export_tasks 不再落 error_msg 列）。

use std::io::Write;
use std::path::{Path, PathBuf};

use rusqlite::params;
use rust_xlsxwriter::{Format, Workbook};
use serde::Serialize;
use serde_json::Value;

use crate::db::Db;
use crate::error::AppError;
use crate::paths;
use crate::projects;
use crate::queue::ProgressPayload;

// ---------------------------------------------------------------------------
// 任务记录查询
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct ExportTaskInfo {
    pub id: i64,
    pub project_id: i64,
    pub kind: String,
    pub status: String,
    pub output_path: String,
    pub created_at: String,
}

/// 项目导出任务列表（创建时间倒序）。
pub fn get_export_tasks(db: &Db, project_id: i64) -> Result<Vec<ExportTaskInfo>, AppError> {
    projects::get_project(db, project_id)?;
    let conn = db.conn()?;
    let mut stmt = conn.prepare(
        "SELECT id, project_id, kind, status, output_path, created_at
         FROM export_tasks WHERE project_id = ?1
         ORDER BY created_at DESC, id DESC",
    )?;
    let rows = stmt
        .query_map(params![project_id], |row| {
            Ok(ExportTaskInfo {
                id: row.get("id")?,
                project_id: row.get("project_id")?,
                kind: row.get("kind")?,
                status: row.get("status")?,
                output_path: row.get("output_path")?,
                created_at: row.get("created_at")?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

// ---------------------------------------------------------------------------
// 数据加载
// ---------------------------------------------------------------------------

struct ExportRow {
    original_name: String,
    relative_path: String,
    cell_data: Value,
}

struct ExportData {
    /// (column_index, keyword)，按 column_index 升序
    columns: Vec<(i64, String)>,
    rows: Vec<ExportRow>,
}

fn load_export_data(db: &Db, project_id: i64) -> Result<ExportData, AppError> {
    let conn = db.conn()?;
    let mut stmt = conn.prepare(
        "SELECT column_index, keyword FROM extract_configs
         WHERE project_id = ?1 ORDER BY column_index ASC",
    )?;
    let columns: Vec<(i64, String)> = stmt
        .query_map(params![project_id], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    drop(stmt);

    let mut stmt = conn.prepare(
        "SELECT sf.original_name, sf.relative_path, er.cell_data
         FROM extract_results er
         JOIN source_files sf ON sf.id = er.source_file_id
         WHERE er.project_id = ?1
         ORDER BY er.created_at ASC, er.id ASC",
    )?;
    let rows: Vec<ExportRow> = stmt
        .query_map(params![project_id], |row| {
            let cell_text: String = row.get(2)?;
            Ok(ExportRow {
                original_name: row.get(0)?,
                relative_path: row.get(1)?,
                cell_data: serde_json::from_str(&cell_text).unwrap_or_else(|_| Value::Null),
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(ExportData { columns, rows })
}

// ---------------------------------------------------------------------------
// xlsx 生成
// ---------------------------------------------------------------------------

/// A 列链接目标模式。
enum LinkMode<'a> {
    /// 表格导出：源文件磁盘绝对路径（经项目目录校验）
    Absolute { project_root: &'a Path },
    /// 完整导出：相对路径 `./<zip 内唯一文件名>`（正斜杠、禁止 file://）
    Relative,
}

fn formula_escape(s: &str) -> String {
    s.replace('"', "\"\"")
}

/// 行在导出产物中的唯一文件名（`stem.pdf` / `stem_N.pdf`，按写入顺序去重）。
fn unique_export_name(taken: &mut std::collections::HashSet<String>, original: &str) -> String {
    let (stem, ext) = paths::split_filename(original);
    let stem = if stem.is_empty() { "unnamed" } else { &stem };
    let ext = if ext.is_empty() { ".pdf" } else { ext.as_str() };
    let mut candidate = format!("{stem}{ext}");
    let mut n = 1;
    while !taken.insert(candidate.clone()) {
        candidate = format!("{stem}_{n}{ext}");
        n += 1;
    }
    candidate
}

/// 生成提取表 xlsx（表头 A1=文件名、B1 起=关键词；A 列为指向 PDF 的链接；空数据也产出表头行）。
fn write_extraction_xlsx(
    path: &Path,
    data: &ExportData,
    link_mode: &LinkMode<'_>,
    row_names: &[String],
) -> Result<(), AppError> {
    let mut workbook = Workbook::new();
    let worksheet = workbook.add_worksheet();
    let bold = Format::new().set_bold();

    worksheet.write_string_with_format(0, 0, "文件名", &bold)?;
    for (i, (_, keyword)) in data.columns.iter().enumerate() {
        worksheet.write_string_with_format(0, (i + 1) as u16, keyword, &bold)?;
    }

    for (r, row) in data.rows.iter().enumerate() {
        let r32 = (r + 1) as u32;
        let display = &row.original_name;
        let target = match link_mode {
            LinkMode::Absolute { project_root } => {
                // 路径安全：必须落在项目目录内
                let abs = paths::ensure_under_project(project_root, Path::new(&row.relative_path))?;
                abs.to_string_lossy().replace('\\', "/")
            }
            LinkMode::Relative => format!("./{}", row_names[r]),
        };
        // HYPERLINK 公式（目标不含 file://，统一正斜杠）
        let formula = format!(
            "=HYPERLINK(\"{}\",\"{}\")",
            formula_escape(&target),
            formula_escape(display)
        );
        worksheet.write_formula(r32, 0, formula.as_str())?;
        for (i, (col_index, _)) in data.columns.iter().enumerate() {
            let value = row
                .cell_data
                .get(col_index.to_string())
                .and_then(|c| c.get("value"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            worksheet.write_string(r32, (i + 1) as u16, value)?;
        }
    }

    workbook.save(path)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// 导出执行体
// ---------------------------------------------------------------------------

fn export_table(db: &Db, project: &projects::Project) -> Result<PathBuf, AppError> {
    let data = load_export_data(db, project.id)?;
    let dir = paths::ensure_under_project(&project.full_path(), Path::new(paths::SUBDIR_EXTRACT))?;
    std::fs::create_dir_all(&dir)?;
    let xlsx_path = dir.join("extraction.xlsx");
    let row_names: Vec<String> = {
        let mut taken = std::collections::HashSet::new();
        data.rows
            .iter()
            .map(|r| unique_export_name(&mut taken, &r.original_name))
            .collect()
    };
    write_extraction_xlsx(
        &xlsx_path,
        &data,
        &LinkMode::Absolute {
            project_root: &project.full_path(),
        },
        &row_names,
    )?;
    Ok(xlsx_path)
}

fn export_full(db: &Db, project: &projects::Project) -> Result<PathBuf, AppError> {
    let data = load_export_data(db, project.id)?;
    let project_root = project.full_path();

    // 临时目录：按唯一文件名复制 PDF（同名文件自动 _N 去重）+ 生成 xlsx（相对链接），
    // 退出作用域自动清理
    let tmp = tempfile::tempdir()?;
    let mut zip_entries: Vec<(String, PathBuf)> = Vec::new();
    let mut taken: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut row_names: Vec<String> = Vec::with_capacity(data.rows.len());
    for row in &data.rows {
        let src = paths::ensure_under_project(&project_root, Path::new(&row.relative_path))?;
        let name = unique_export_name(&mut taken, &row.original_name);
        let dest = tmp.path().join(&name);
        std::fs::copy(&src, &dest)?;
        row_names.push(name.clone());
        zip_entries.push((name, dest));
    }

    let xlsx_path = tmp.path().join("extraction.xlsx");
    write_extraction_xlsx(&xlsx_path, &data, &LinkMode::Relative, &row_names)?;
    zip_entries.push(("extraction.xlsx".to_string(), xlsx_path));

    // 打包 zip（条目平铺同目录，相对链接才能生效）
    let export_dir = paths::ensure_under_project(&project_root, Path::new(paths::SUBDIR_EXPORT))?;
    std::fs::create_dir_all(&export_dir)?;
    let zip_name = paths::unique_file_name(
        &export_dir,
        &format!("export-{}", paths::now_compact()),
        ".zip",
    );
    let zip_path = export_dir.join(&zip_name);
    write_zip(&zip_path, &zip_entries)?;
    Ok(zip_path)
}

fn write_zip(zip_path: &Path, entries: &[(String, PathBuf)]) -> Result<(), AppError> {
    let file = std::fs::File::create(zip_path)?;
    let mut zip = zip::ZipWriter::new(file);
    // 条目时间 = 导出时点（不设则 zip 默认 1980-01-01）
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Stored)
        .last_modified_time(now_zip_datetime());
    for (name, path) in entries {
        zip.start_file(name, options)
            .map_err(|e| AppError::Internal(format!("zip 写入失败: {e}")))?;
        let bytes = std::fs::read(path)?;
        zip.write_all(&bytes)?;
    }
    zip.finish()
        .map_err(|e| AppError::Internal(format!("zip 完成失败: {e}")))?;
    Ok(())
}

/// 当前本地时间 → zip DateTime（chrono 特性未启用，手工构造；范围异常时回退默认）。
fn now_zip_datetime() -> zip::DateTime {
    use chrono::{Datelike, Timelike};
    let now = chrono::Local::now();
    zip::DateTime::from_date_and_time(
        now.year() as u16,
        now.month() as u8,
        now.day() as u8,
        now.hour() as u8,
        now.minute() as u8,
        now.second() as u8,
    )
    .unwrap_or_default()
}

/// Export 执行体（阻塞式，供队列 worker 在 spawn_blocking 中调用）。
/// 写 export_tasks 状态机（running→done/error）；project.status 不变。
pub fn run_export(
    db: &Db,
    project_id: i64,
    kind: &str,
    mut progress: impl FnMut(ProgressPayload),
    log: crate::task_log::LogSink<'_>,
) -> Result<(u32, u32), AppError> {
    let project = projects::get_project(db, project_id)?;
    if kind != "table" && kind != "full" {
        return Err(AppError::InvalidInput(format!(
            "未知导出类型: {kind}（可选 table/full）"
        )));
    }
    log("导出", &format!("导出任务开始（{kind}）"));

    // 任务行：running
    let now = paths::now_display();
    let task_id = {
        let conn = db.conn()?;
        conn.execute(
            "INSERT INTO export_tasks(project_id, kind, status, created_at)
             VALUES (?1, ?2, 'running', ?3)",
            params![project_id, kind, now],
        )?;
        conn.last_insert_rowid()
    };

    let stage_text = if kind == "table" {
        "生成表格"
    } else {
        "打包导出"
    };
    progress(ProgressPayload {
        project_id,
        kind: "export".to_string(),
        file_id: None,
        status: "running".to_string(),
        done: 0,
        total: 1,
        current_file: Some(stage_text.to_string()),
    });

    let result = if kind == "table" {
        export_table(db, &project)
    } else {
        export_full(db, &project)
    };

    // 日志必须在 conn guard 释放后调用：task_log 会再次取 DB 锁，
    // 同线程持锁重入 std Mutex 会自死锁
    match result {
        Ok(output) => {
            {
                let conn = db.conn()?;
                conn.execute(
                    "UPDATE export_tasks SET status = 'done', output_path = ?2
                     WHERE id = ?1",
                    params![task_id, output.to_string_lossy()],
                )?;
            }
            log("导出", &format!("导出完成：{}", output.to_string_lossy()));
            progress(ProgressPayload {
                project_id,
                kind: "export".to_string(),
                file_id: None,
                status: "done".to_string(),
                done: 1,
                total: 1,
                current_file: Some(stage_text.to_string()),
            });
            Ok((1, 0))
        }
        Err(e) => {
            {
                let conn = db.conn()?;
                conn.execute(
                    "UPDATE export_tasks SET status = 'error' WHERE id = ?1",
                    params![task_id],
                )?;
            }
            log("导出", &format!("导出失败：{e}"));
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::files;
    use crate::llm::extract::{save_extract_configs, ExtractConfigInput};

    struct Fixture {
        _tmp: tempfile::TempDir,
        db: Db,
        project: projects::Project,
    }

    /// 项目 + 2 个已上传假 PDF + 2 列关键词 + 2 行提取结果。
    fn fixture() -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open_in_memory().unwrap();
        let project =
            projects::create_project(&db, tmp.path().to_str().unwrap(), "导出项目", "测试组")
                .unwrap();
        let src_dir = tmp.path().join("src");
        std::fs::create_dir_all(&src_dir).unwrap();
        let mut paths_ = Vec::new();
        for name in ["合同A.pdf", "合同B.pdf"] {
            let p = src_dir.join(name);
            std::fs::write(&p, b"%PDF-1.7\nfake").unwrap();
            paths_.push(p.to_str().unwrap().to_string());
        }
        let up = files::upload_files(&db, project.id, &paths_).unwrap();

        save_extract_configs(
            &db,
            project.id,
            &[
                ExtractConfigInput {
                    column_index: 2,
                    keyword: "甲方".into(),
                    ..Default::default()
                },
                ExtractConfigInput {
                    column_index: 3,
                    keyword: "监控账号".into(),
                    ..Default::default()
                },
            ],
        )
        .unwrap();

        for (i, f) in up.files.iter().enumerate() {
            let cell = serde_json::json!({
                "2": {"value": format!("甲方{i}"), "quote": "", "page_idx": null,
                      "bbox": null, "fragments": [], "char_spans": [], "confidence": 0},
                "3": {"value": format!("账号{i}"), "quote": "", "page_idx": null,
                      "bbox": null, "fragments": [], "char_spans": [], "confidence": 0}
            });
            db.conn()
                .unwrap()
                .execute(
                    "INSERT INTO extract_results(project_id, source_file_id, cell_data,
                                                is_edited, error_msg, created_at, updated_at)
                     VALUES (?1, ?2, ?3, 0, '', '2026-01-01 00:00:00', '2026-01-01 00:00:00')",
                    params![project.id, f.id, cell.to_string()],
                )
                .unwrap();
        }
        Fixture {
            _tmp: tmp,
            db,
            project,
        }
    }

    /// 读取 xlsx（zip 包）内容：(sheet1.xml, sharedStrings.xml)。
    /// 文本单元格在共享字符串表，公式（HYPERLINK）内联在 sheet XML。
    fn read_xlsx_parts(xlsx_path: &Path) -> (String, String) {
        let file = std::fs::File::open(xlsx_path).unwrap();
        let mut archive = zip::ZipArchive::new(file).unwrap();
        let mut sheet = String::new();
        let mut shared = String::new();
        for i in 0..archive.len() {
            let mut entry = archive.by_index(i).unwrap();
            match entry.name() {
                "xl/worksheets/sheet1.xml" => {
                    std::io::Read::read_to_string(&mut entry, &mut sheet).unwrap();
                }
                "xl/sharedStrings.xml" => {
                    std::io::Read::read_to_string(&mut entry, &mut shared).unwrap();
                }
                _ => {}
            }
        }
        assert!(!sheet.is_empty(), "xlsx 中未找到 sheet1.xml");
        (sheet, shared)
    }

    #[test]
    fn export_table_generates_xlsx_with_absolute_hyperlinks() {
        let fx = fixture();
        let out = export_table(&fx.db, &fx.project).unwrap();
        assert!(out.ends_with("extraction.xlsx"));
        assert!(out.starts_with(
            fx.project
                .full_path()
                .join(paths::SUBDIR_EXTRACT)
                .canonicalize()
                .unwrap()
        ));
        let (sheet, shared) = read_xlsx_parts(&out);
        // 表头关键词在共享字符串表；display 与目标在 sheet 公式内
        assert!(shared.contains("甲方"), "{shared}");
        assert!(shared.contains("监控账号"), "{shared}");
        assert!(sheet.contains("合同A.pdf"), "{sheet}");
        assert!(sheet.contains("HYPERLINK"), "{sheet}");
        // 绝对路径目标（正斜杠、无 file://）
        let pdf_abs = fx
            .project
            .full_path()
            .join(paths::SUBDIR_RAW)
            .canonicalize()
            .unwrap();
        assert!(
            sheet.contains(&pdf_abs.to_string_lossy().replace('\\', "/")),
            "{sheet}"
        );
        assert!(!sheet.contains("file://"), "{sheet}");
        assert!(!sheet.contains("\\\\"), "{sheet}");
        // 提取值
        assert!(shared.contains("甲方1"), "{shared}");
        assert!(shared.contains("账号0"), "{shared}");
    }

    #[test]
    fn export_full_generates_zip_with_relative_links_and_pdfs() {
        let fx = fixture();
        let out = export_full(&fx.db, &fx.project).unwrap();
        let name = out.file_name().unwrap().to_string_lossy().to_string();
        assert!(name.starts_with("export-"), "{name}");
        assert!(name.ends_with(".zip"), "{name}");
        assert!(out.starts_with(
            fx.project
                .full_path()
                .join(paths::SUBDIR_EXPORT)
                .canonicalize()
                .unwrap()
        ));

        // zip 内容清单：extraction.xlsx + 2 个按文件名命名的 PDF（平铺同目录）
        let file = std::fs::File::open(&out).unwrap();
        let mut archive = zip::ZipArchive::new(file).unwrap();
        let mut names: Vec<String> = (0..archive.len())
            .map(|i| archive.by_index(i).unwrap().name().to_string())
            .collect();
        names.sort();
        assert_eq!(
            names,
            vec![
                "extraction.xlsx".to_string(),
                "合同A.pdf".to_string(),
                "合同B.pdf".to_string()
            ]
        );
        // PDF 内容与源文件一致
        let mut buf = Vec::new();
        {
            let mut entry = archive.by_name("合同A.pdf").unwrap();
            std::io::Read::read_to_end(&mut entry, &mut buf).unwrap();
        }
        assert_eq!(buf, b"%PDF-1.7\nfake");

        // zip 条目时间应为导出时点（而非 zip 默认 1980-01-01）
        {
            let entry = archive.by_name("extraction.xlsx").unwrap();
            let mt = entry.last_modified();
            assert!(mt.is_some(), "zip 条目应带修改时间");
            assert_ne!(
                mt.unwrap(),
                zip::DateTime::default(),
                "zip 条目时间不应为 1980 默认值"
            );
        }

        // xlsx 内为相对链接 ./<文件名>，无 file://、无绝对项目路径
        // （zip 内的 extraction.xlsx 条目先取出再读 xlsx 部件）
        let mut xlsx_bytes = Vec::new();
        {
            let mut xlsx_entry = archive.by_name("extraction.xlsx").unwrap();
            std::io::Read::read_to_end(&mut xlsx_entry, &mut xlsx_bytes).unwrap();
        }
        let tmp_xlsx = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp_xlsx.path(), &xlsx_bytes).unwrap();
        let (sheet, _shared) = read_xlsx_parts(tmp_xlsx.path());
        assert!(sheet.contains("./合同A.pdf"), "{sheet}");
        assert!(!sheet.contains("file://"), "{sheet}");
        assert!(
            !sheet.contains(&fx.project.full_path().to_string_lossy().to_string()),
            "{sheet}"
        );
    }

    #[test]
    fn export_empty_project_still_writes_header() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open_in_memory().unwrap();
        let project =
            projects::create_project(&db, tmp.path().to_str().unwrap(), "空项目", "测试组")
                .unwrap();
        save_extract_configs(
            &db,
            project.id,
            &[ExtractConfigInput {
                column_index: 2,
                keyword: "甲方".into(),
                ..Default::default()
            }],
        )
        .unwrap();
        let out = export_table(&db, &project).unwrap();
        let (sheet, shared) = read_xlsx_parts(&out);
        assert!(shared.contains("文件名"), "{shared}");
        assert!(shared.contains("甲方"), "{shared}");
        assert!(!sheet.contains("HYPERLINK"), "{sheet}");

        // 空项目完整导出：zip 内只有 xlsx
        let zip_out = export_full(&db, &project).unwrap();
        let file = std::fs::File::open(&zip_out).unwrap();
        let mut archive = zip::ZipArchive::new(file).unwrap();
        assert_eq!(archive.len(), 1);
        assert_eq!(archive.by_index(0).unwrap().name(), "extraction.xlsx");
    }

    #[test]
    fn run_export_writes_task_rows() {
        let fx = fixture();
        let (ok, failed) = run_export(&fx.db, fx.project.id, "table", |_| {}, &|_, _| {}).unwrap();
        assert_eq!((ok, failed), (1, 0));
        let tasks = get_export_tasks(&fx.db, fx.project.id).unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].kind, "table");
        assert_eq!(tasks[0].status, "done");
        assert!(tasks[0].output_path.ends_with("extraction.xlsx"));

        // 未知类型直接报错且不落任务行
        assert!(run_export(&fx.db, fx.project.id, "bogus", |_| {}, &|_, _| {}).is_err());
        assert_eq!(get_export_tasks(&fx.db, fx.project.id).unwrap().len(), 1);
    }

    /// 回归：生产日志 sink（task_log）会再次查询 projects 表取 DB 锁；
    /// 日志调用不得发生在持有 conn guard 的作用域内（同线程重入 std Mutex 自死锁）。
    #[test]
    fn run_export_logs_without_db_deadlock() {
        let fx = fixture();
        let log = |_: &str, _: &str| {
            // 与 task_log 一致：sink 内取 DB 锁
            let _ = projects::get_project(&fx.db, fx.project.id).unwrap();
        };
        let (ok, failed) = run_export(&fx.db, fx.project.id, "table", |_| {}, &log).unwrap();
        assert_eq!((ok, failed), (1, 0));
    }
}
