//! 项目业务逻辑：创建（含标准子目录）、列表、详情、删除（移入回收站 + 级联删库）。

use std::path::{Path, PathBuf};

use rusqlite::{params, OptionalExtension};
use serde::Serialize;

use crate::db::Db;
use crate::error::AppError;
use crate::paths;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Project {
    pub id: i64,
    pub name: String,
    /// 所属项目组（自由文本，新建任务时可选取已有组名或输入新组名）。
    pub group_name: String,
    /// 任务类型：'parse' 解析提取 | 'audit' 内控测试（迁移 v10 起，旧库默认 'parse'）。
    pub task_type: String,
    pub created_at: String,
    /// 最近修改时间：查询时跨子表现算（上传/关键词配置/结果编辑/导出的最新时间），
    /// 不落列，写路径无需维护。
    pub updated_at: String,
    pub base_path: String,
    pub project_dir: String,
    pub status: String,
}

impl Project {
    /// 项目目录的绝对路径 = base_path / project_dir。
    /// 内控测试任务（task_type='audit'）不落盘，两者皆空串（full_path 为空路径，
    /// exists() 为 false，任何基于项目目录的操作对 audit 任务均为安全无操作）。
    pub fn full_path(&self) -> PathBuf {
        Path::new(&self.base_path).join(&self.project_dir)
    }
}

fn row_to_project(row: &rusqlite::Row<'_>) -> rusqlite::Result<Project> {
    Ok(Project {
        id: row.get("id")?,
        name: row.get("name")?,
        group_name: row.get("group_name")?,
        task_type: row.get("task_type")?,
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
        base_path: row.get("base_path")?,
        project_dir: row.get("project_dir")?,
        status: row.get("status")?,
    })
}

/// 项目查询公共段：updated_at 用标量 MAX 现算（时间戳为 'YYYY-MM-DD HH:MM:SS'，
/// 字典序即时间序；多参 MAX 遇 NULL 返 NULL，故子查询一律 COALESCE 兜底空串）。
const PROJECT_SELECT: &str = r"
SELECT p.id, p.name, p.group_name, p.task_type, p.created_at, p.base_path, p.project_dir, p.status,
    MAX(
        p.created_at,
        COALESCE((SELECT MAX(ub.uploaded_at) FROM upload_batches ub
                  WHERE ub.project_id = p.id), ''),
        COALESCE((SELECT MAX(ec.created_at) FROM extract_configs ec
                  WHERE ec.project_id = p.id), ''),
        COALESCE((SELECT MAX(er.updated_at) FROM extract_results er
                  WHERE er.project_id = p.id), ''),
        COALESCE((SELECT MAX(et.created_at) FROM export_tasks et
                  WHERE et.project_id = p.id), '')
    ) AS updated_at
FROM projects p
";

/// 创建项目：校验 base_path 为已存在目录，生成 `<名称>-<YYYYMMDDHHMMSS>/`
/// 目录（冲突追加 `_N`）与 4 个标准子目录，写入 projects 表（status=idle，
/// task_type='parse'）。带任务类型的版本见 [`create_project_typed`]。
pub fn create_project(
    db: &Db,
    base_path: &str,
    name: &str,
    group_name: &str,
) -> Result<Project, AppError> {
    create_project_typed(db, base_path, name, group_name, "parse")
}

/// 同 [`create_project`]，但指定任务类型（'parse' 解析提取 | 'audit' 内控测试）。
/// 内控测试任务不生成本地文件夹：无需 base_path（忽略入参），
/// base_path/project_dir 落库为空串，制度文件等内容全部入库（ic_documents.data）。
pub fn create_project_typed(
    db: &Db,
    base_path: &str,
    name: &str,
    group_name: &str,
    task_type: &str,
) -> Result<Project, AppError> {
    if !matches!(task_type, "parse" | "audit") {
        return Err(AppError::InvalidInput(format!("未知任务类型: {task_type}")));
    }
    let name = name.trim();
    if name.is_empty() {
        return Err(AppError::InvalidInput("任务名称不能为空".to_string()));
    }
    let group_name = group_name.trim();
    if group_name.is_empty() {
        return Err(AppError::InvalidInput("所属项目不能为空".to_string()));
    }

    // 内控测试：不落盘，直接建行
    if task_type == "audit" {
        let created_at = paths::now_display();
        let conn = db.conn()?;
        conn.execute(
            "INSERT INTO projects(name, group_name, task_type, created_at, base_path, project_dir, status)
             VALUES (?1, ?2, 'audit', ?3, '', '', 'idle')",
            params![name, group_name, created_at],
        )?;
        let id = conn.last_insert_rowid();
        return Ok(Project {
            id,
            name: name.to_string(),
            group_name: group_name.to_string(),
            task_type: "audit".to_string(),
            created_at: created_at.clone(),
            updated_at: created_at,
            base_path: String::new(),
            project_dir: String::new(),
            status: "idle".to_string(),
        });
    }

    let base = Path::new(base_path);
    if !base.is_dir() {
        return Err(AppError::InvalidInput(format!(
            "任务存放目录不存在或不是目录: {base_path}"
        )));
    }
    // 统一存 canonical 绝对路径，保证后续 ensure_under_project 前缀判断一致
    let base = base.canonicalize()?;

    let safe_name = paths::safe_filename(name, 80);
    let desired = format!("{safe_name}-{ts}", ts = paths::now_compact());
    let dir_name = paths::unique_child_name(&base, &desired);
    let project_path = base.join(&dir_name);
    std::fs::create_dir_all(&project_path)?;
    for sub in paths::PROJECT_SUBDIRS {
        std::fs::create_dir_all(project_path.join(sub))?;
    }

    let created_at = paths::now_display();
    let conn = db.conn()?;
    conn.execute(
        "INSERT INTO projects(name, group_name, task_type, created_at, base_path, project_dir, status)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'idle')",
        params![
            name,
            group_name,
            task_type,
            created_at,
            base.to_string_lossy(),
            dir_name
        ],
    )?;
    let id = conn.last_insert_rowid();
    Ok(Project {
        id,
        name: name.to_string(),
        group_name: group_name.to_string(),
        task_type: task_type.to_string(),
        created_at: created_at.clone(),
        updated_at: created_at,
        base_path: base.to_string_lossy().to_string(),
        project_dir: dir_name,
        status: "idle".to_string(),
    })
}

/// 全部项目，按创建时间倒序（同刻按 id 倒序）。
pub fn list_projects(db: &Db) -> Result<Vec<Project>, AppError> {
    let conn = db.conn()?;
    let mut stmt = conn.prepare(&format!(
        "{PROJECT_SELECT} ORDER BY p.created_at DESC, p.id DESC"
    ))?;
    let rows = stmt
        .query_map([], row_to_project)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// 按 id 取项目；不存在返回 NotFound。
pub fn get_project(db: &Db, id: i64) -> Result<Project, AppError> {
    let conn = db.conn()?;
    conn.query_row(
        &format!("{PROJECT_SELECT} WHERE p.id = ?1"),
        params![id],
        row_to_project,
    )
    .optional()?
    .ok_or_else(|| AppError::NotFound(format!("任务不存在: {id}")))
}

// ---------------------------------------------------------------------------
// 列表页状态列：每项目的解析/提取进度计数
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ProjectProgress {
    pub project_id: i64,
    /// 项目状态（idle/parsing/extracting/error）。
    pub status: String,
    pub files_total: i64,
    pub parse_done: i64,
    pub parse_error: i64,
    /// 提取成功行数（error_msg 为空）。
    pub extract_done: i64,
    /// 提取失败行数（error_msg 非空）。
    pub extract_error: i64,
}

/// 全部项目的解析/提取进度计数（列表页状态列用，一次查询聚合）。
pub fn list_project_progress(db: &Db) -> Result<Vec<ProjectProgress>, AppError> {
    let conn = db.conn()?;
    let mut stmt = conn.prepare(
        "SELECT p.id, p.status,
            (SELECT COUNT(*) FROM source_files sf
             JOIN upload_batches b ON b.id = sf.batch_id
             WHERE b.project_id = p.id),
            (SELECT COUNT(*) FROM parse_results pr
             JOIN source_files sf ON sf.id = pr.source_file_id
             JOIN upload_batches b ON b.id = sf.batch_id
             WHERE b.project_id = p.id AND pr.status = 'done'),
            (SELECT COUNT(*) FROM parse_results pr
             JOIN source_files sf ON sf.id = pr.source_file_id
             JOIN upload_batches b ON b.id = sf.batch_id
             WHERE b.project_id = p.id AND pr.status = 'error'),
            (SELECT COUNT(*) FROM extract_results
             WHERE project_id = p.id AND error_msg = ''),
            (SELECT COUNT(*) FROM extract_results
             WHERE project_id = p.id AND error_msg != '')
         FROM projects p
         ORDER BY p.created_at DESC, p.id DESC",
    )?;
    let rows = stmt
        .query_map([], |row| {
            Ok(ProjectProgress {
                project_id: row.get(0)?,
                status: row.get(1)?,
                files_total: row.get(2)?,
                parse_done: row.get(3)?,
                parse_error: row.get(4)?,
                extract_done: row.get(5)?,
                extract_error: row.get(6)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// 删除项目：目录移入系统回收站（trash crate），再级联删除全部子表记录。
pub fn delete_project(db: &Db, id: i64) -> Result<(), AppError> {
    delete_project_with(db, id, |p| Ok(trash::delete(p)?))
}

/// 与 [`delete_project`] 相同，但回收站操作可注入（测试用内存替身）。
pub fn delete_project_with<F>(db: &Db, id: i64, trash_fn: F) -> Result<(), AppError>
where
    F: FnOnce(&Path) -> Result<(), AppError>,
{
    let project = get_project(db, id)?;
    let full_path = project.full_path();
    if full_path.exists() {
        trash_fn(&full_path)?;
    }

    let conn = db.conn()?;
    let tx = conn.unchecked_transaction()?;
    // 内控测试子表（v10 追加；解析提取项目这些表本无行，删除语义不变）
    for t in [
        "ic_documents",
        "ic_nodes",
        "ic_edges",
        "ic_groups",
        "ic_doc_links",
        "ic_roster",
        "ic_org",
    ] {
        tx.execute(
            &format!("DELETE FROM {t} WHERE project_id = ?1"),
            params![id],
        )?;
    }
    tx.execute("DELETE FROM ic_counters WHERE project_id = ?1", params![id])?;
    tx.execute(
        "DELETE FROM export_tasks WHERE project_id = ?1",
        params![id],
    )?;
    tx.execute(
        "DELETE FROM extract_configs WHERE project_id = ?1",
        params![id],
    )?;
    tx.execute(
        "DELETE FROM extract_results WHERE project_id = ?1",
        params![id],
    )?;
    tx.execute(
        "DELETE FROM parse_results WHERE source_file_id IN (
            SELECT sf.id FROM source_files sf
            JOIN upload_batches b ON b.id = sf.batch_id
            WHERE b.project_id = ?1
        )",
        params![id],
    )?;
    tx.execute(
        "DELETE FROM source_files WHERE batch_id IN (
            SELECT id FROM upload_batches WHERE project_id = ?1
        )",
        params![id],
    )?;
    tx.execute(
        "DELETE FROM upload_batches WHERE project_id = ?1",
        params![id],
    )?;
    tx.execute("DELETE FROM projects WHERE id = ?1", params![id])?;
    tx.commit()?;
    Ok(())
}

// ---------------------------------------------------------------------------
// 进度重置与选中重跑（任务工作区「全部重置」「重新解析/重新提取」）
// ---------------------------------------------------------------------------

/// 项目处理中（parsing/extracting）时拒绝写操作。
fn ensure_project_idle(project: &Project) -> Result<(), AppError> {
    if project.status == "parsing" || project.status == "extracting" {
        return Err(AppError::InvalidInput(
            "任务正在处理中，请完成后再试".to_string(),
        ));
    }
    Ok(())
}

/// 更新项目状态（parse/extract 执行体共用）。
pub fn set_project_status(db: &Db, project_id: i64, status: &str) -> Result<(), AppError> {
    db.conn()?.execute(
        "UPDATE projects SET status = ?2 WHERE id = ?1",
        params![project_id, status],
    )?;
    Ok(())
}

/// 全部重置：删除全部上传文件、解析/提取/导出记录与关键词配置，
/// 任务恢复到刚创建的状态（4 个标准子目录移入系统回收站后重建，项目行保留）。
pub fn reset_project(db: &Db, id: i64) -> Result<(), AppError> {
    reset_project_with(db, id, |p| Ok(trash::delete(p)?))
}

/// 与 [`reset_project`] 相同，但回收站操作可注入（测试用内存替身）。
pub fn reset_project_with<F>(db: &Db, id: i64, trash_fn: F) -> Result<(), AppError>
where
    F: Fn(&Path) -> Result<(), AppError>,
{
    let project = get_project(db, id)?;
    ensure_project_idle(&project)?;
    // 内控测试任务无本地目录（project_dir 为空）：磁盘段安全跳过
    let has_dir = !project.project_dir.is_empty();
    let conn = db.conn()?;
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "DELETE FROM export_tasks WHERE project_id = ?1",
        params![id],
    )?;
    tx.execute(
        "DELETE FROM extract_configs WHERE project_id = ?1",
        params![id],
    )?;
    tx.execute(
        "DELETE FROM extract_results WHERE project_id = ?1",
        params![id],
    )?;
    tx.execute(
        "DELETE FROM parse_results WHERE source_file_id IN (
            SELECT sf.id FROM source_files sf
            JOIN upload_batches b ON b.id = sf.batch_id
            WHERE b.project_id = ?1
        )",
        params![id],
    )?;
    tx.execute(
        "DELETE FROM source_files WHERE batch_id IN (
            SELECT id FROM upload_batches WHERE project_id = ?1
        )",
        params![id],
    )?;
    tx.execute(
        "DELETE FROM upload_batches WHERE project_id = ?1",
        params![id],
    )?;
    tx.execute(
        "UPDATE projects SET status = 'idle' WHERE id = ?1",
        params![id],
    )?;
    tx.commit()?;
    drop(conn);

    // 磁盘：4 个标准子目录移入系统回收站后重建（上传文件、解析产物、
    // 提取 llm.json、导出 zip/xlsx 一并入回收站，可从系统回收站恢复）
    if has_dir {
        for sub in paths::PROJECT_SUBDIRS {
            let dir = project.full_path().join(sub);
            if dir.exists() {
                trash_fn(&dir)?;
            }
            std::fs::create_dir_all(&dir)?;
        }
    }
    Ok(())
}

/// 校验 file_ids 均属于该项目；返回去重后的 id 列表。
fn checked_file_ids(db: &Db, project_id: i64, file_ids: &[i64]) -> Result<Vec<i64>, AppError> {
    let mut ids: Vec<i64> = file_ids.to_vec();
    ids.sort_unstable();
    ids.dedup();
    if ids.is_empty() {
        return Err(AppError::InvalidInput("未选择文件".to_string()));
    }
    let conn = db.conn()?;
    for fid in &ids {
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM source_files sf
             JOIN upload_batches b ON b.id = sf.batch_id
             WHERE sf.id = ?1 AND b.project_id = ?2",
            params![fid, project_id],
            |r| r.get(0),
        )?;
        if n == 0 {
            return Err(AppError::NotFound(format!(
                "文件不属于该任务或不存在: {fid}"
            )));
        }
    }
    Ok(ids)
}

/// 删除文件对应的 llm.json 留档（解析/提取结果作废时同步清理；失败不致命）。
fn remove_llm_jsons(project: &Project, conn: &rusqlite::Connection, ids: &[i64]) {
    for fid in ids {
        let name: Option<String> = conn
            .query_row(
                "SELECT original_name FROM source_files WHERE id = ?1",
                params![fid],
                |r| r.get(0),
            )
            .optional()
            .unwrap_or(None);
        if let Some(name) = name {
            let stem = paths::safe_filename(&paths::split_filename(&name).0, 100);
            let _ = std::fs::remove_file(
                project
                    .full_path()
                    .join(paths::SUBDIR_EXTRACT)
                    .join(format!("{stem}.llm.json")),
            );
        }
    }
}

/// 重新解析选中文件：删其 parse_results 行并级联删 extract_results 行
///（解析输出作废则提取结果失效），文件回到待解析；调用方负责再入队 Parse。
pub fn reparse_files(db: &Db, project_id: i64, file_ids: &[i64]) -> Result<(), AppError> {
    let project = get_project(db, project_id)?;
    ensure_project_idle(&project)?;
    let ids = checked_file_ids(db, project_id, file_ids)?;
    let conn = db.conn()?;
    let tx = conn.unchecked_transaction()?;
    for fid in &ids {
        tx.execute(
            "DELETE FROM parse_results WHERE source_file_id = ?1",
            params![fid],
        )?;
        tx.execute(
            "DELETE FROM extract_results WHERE project_id = ?1 AND source_file_id = ?2",
            params![project_id, fid],
        )?;
        tx.execute(
            "UPDATE source_files SET status = 'pending' WHERE id = ?1",
            params![fid],
        )?;
    }
    tx.commit()?;
    remove_llm_jsons(&project, &conn, &ids);
    Ok(())
}

/// 重新提取选中文件：只删其 extract_results 行（eligible_files 会重新选中）；
/// 调用方负责再入队 Extract。
pub fn reextract_files(db: &Db, project_id: i64, file_ids: &[i64]) -> Result<(), AppError> {
    let project = get_project(db, project_id)?;
    ensure_project_idle(&project)?;
    let ids = checked_file_ids(db, project_id, file_ids)?;
    let conn = db.conn()?;
    let tx = conn.unchecked_transaction()?;
    for fid in &ids {
        tx.execute(
            "DELETE FROM extract_results WHERE project_id = ?1 AND source_file_id = ?2",
            params![project_id, fid],
        )?;
    }
    tx.commit()?;
    remove_llm_jsons(&project, &conn, &ids);
    Ok(())
}

/// 删除选中的上传文件：DB 行（extract_results / parse_results / source_files）、
/// 磁盘原始 PDF、解析产物目录与 llm.json 留档一并移入系统回收站（可恢复）；
/// 空批次行顺带清理。
pub fn delete_files(db: &Db, project_id: i64, file_ids: &[i64]) -> Result<(), AppError> {
    delete_files_with(db, project_id, file_ids, |p| Ok(trash::delete(p)?))
}

/// 与 [`delete_files`] 相同，但回收站操作可注入（测试用内存替身）。
pub fn delete_files_with<F>(
    db: &Db,
    project_id: i64,
    file_ids: &[i64],
    trash_fn: F,
) -> Result<(), AppError>
where
    F: Fn(&Path) -> Result<(), AppError>,
{
    let project = get_project(db, project_id)?;
    ensure_project_idle(&project)?;
    let ids = checked_file_ids(db, project_id, file_ids)?;

    // 先取出磁盘清理所需路径（relative_path / parse output_dir）与 llm.json 留档
    let mut disk_files: Vec<std::path::PathBuf> = Vec::new();
    let mut disk_dirs: Vec<std::path::PathBuf> = Vec::new();
    {
        let conn = db.conn()?;
        for fid in &ids {
            let rel: Option<String> = conn
                .query_row(
                    "SELECT relative_path FROM source_files WHERE id = ?1",
                    params![fid],
                    |r| r.get(0),
                )
                .optional()?;
            if let Some(rel) = rel {
                disk_files.push(project.full_path().join(rel));
            }
            // llm.json 留档：按原始文件名主体推导（删除前收集，删行后查不到）
            let orig: Option<String> = conn
                .query_row(
                    "SELECT original_name FROM source_files WHERE id = ?1",
                    params![fid],
                    |r| r.get(0),
                )
                .optional()?;
            if let Some(name) = orig {
                let stem = paths::safe_filename(&paths::split_filename(&name).0, 100);
                disk_files.push(
                    project
                        .full_path()
                        .join(paths::SUBDIR_EXTRACT)
                        .join(format!("{stem}.llm.json")),
                );
            }
            let out_dir: Option<String> = conn
                .query_row(
                    "SELECT output_dir FROM parse_results WHERE source_file_id = ?1",
                    params![fid],
                    |r| r.get(0),
                )
                .optional()?;
            if let Some(d) = out_dir.filter(|d| !d.is_empty()) {
                disk_dirs.push(std::path::PathBuf::from(d));
            }
        }
    }

    let conn = db.conn()?;
    let tx = conn.unchecked_transaction()?;
    for fid in &ids {
        tx.execute(
            "DELETE FROM extract_results WHERE project_id = ?1 AND source_file_id = ?2",
            params![project_id, fid],
        )?;
        tx.execute(
            "DELETE FROM parse_results WHERE source_file_id = ?1",
            params![fid],
        )?;
        tx.execute("DELETE FROM source_files WHERE id = ?1", params![fid])?;
    }
    // 空批次行清理
    tx.execute(
        "DELETE FROM upload_batches WHERE project_id = ?1 AND id NOT IN (
            SELECT DISTINCT batch_id FROM source_files
        )",
        params![project_id],
    )?;
    tx.commit()?;
    drop(conn);

    // 磁盘清理：移入系统回收站（失败不致命：DB 已删，残留文件不影响功能）
    for f in disk_files.into_iter().chain(disk_dirs) {
        if f.exists() {
            let _ = trash_fn(&f);
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// 启动恢复（应用退出时中断的解析/提取回退）
// ---------------------------------------------------------------------------

/// 启动恢复：上次退出时若有进行中的解析/提取（worker 随应用退出而终止），
/// 相关状态回退到未开始，并清理解析半成品目录。
/// - projects.status：parsing/extracting → idle
/// - source_files.status：parsing → pending
/// - parse_results：status=running 的行删除（重跑时重新插入），对应
///   `1-解析结果集/<文件名主体>/` 半成品目录一并删除
/// - 提取无半成品行（完成才落行），仅需项目状态回退
pub fn recover_interrupted(db: &Db) -> Result<(), AppError> {
    // 收集 running 解析行对应的（项目目录, 文件名主体），用于清理半成品目录
    let interrupted: Vec<(String, String, String)> = {
        let conn = db.conn()?;
        let mut stmt = conn.prepare(
            "SELECT p.base_path, p.project_dir, sf.original_name
             FROM parse_results pr
             JOIN source_files sf ON sf.id = pr.source_file_id
             JOIN upload_batches b ON b.id = sf.batch_id
             JOIN projects p ON p.id = b.project_id
             WHERE pr.status = 'running'",
        )?;
        let rows = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        rows
    };

    let conn = db.conn()?;
    let tx = conn.unchecked_transaction()?;
    tx.execute("DELETE FROM parse_results WHERE status = 'running'", [])?;
    tx.execute(
        "UPDATE source_files SET status = 'pending' WHERE status = 'parsing'",
        [],
    )?;
    tx.execute(
        "UPDATE projects SET status = 'idle' WHERE status IN ('parsing', 'extracting')",
        [],
    )?;
    tx.commit()?;
    drop(conn);

    // 半成品解析目录清理（失败不致命）
    for (base_path, project_dir, original_name) in interrupted {
        let stem = paths::safe_filename(&paths::split_filename(&original_name).0, 100);
        let dir = Path::new(&base_path)
            .join(&project_dir)
            .join(paths::SUBDIR_PARSE)
            .join(&stem);
        let _ = std::fs::remove_dir_all(dir);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (tempfile::TempDir, Db) {
        (tempfile::tempdir().unwrap(), Db::open_in_memory().unwrap())
    }

    #[test]
    fn create_project_makes_dirs_and_row() {
        let (tmp, db) = setup();
        let p = create_project(&db, tmp.path().to_str().unwrap(), "测试任务", "组A").unwrap();
        assert_eq!(p.status, "idle");
        assert_eq!(p.group_name, "组A");
        assert_eq!(p.updated_at, p.created_at);
        assert!(p.project_dir.starts_with("测试任务-"));
        let dir = p.full_path();
        assert!(dir.is_dir());
        for sub in paths::PROJECT_SUBDIRS {
            assert!(dir.join(sub).is_dir(), "缺子目录 {sub}");
        }
        // base_path 已 canonicalize
        assert_eq!(Path::new(&p.base_path), tmp.path().canonicalize().unwrap());
        // 库中可读回
        assert_eq!(get_project(&db, p.id).unwrap(), p);
    }

    #[test]
    fn create_project_typed_audit_roundtrip_and_validation() {
        let (tmp, db) = setup();
        let base = tmp.path().to_str().unwrap();
        // 默认包装函数落 'parse'
        let p1 = create_project(&db, base, "解析任务", "组A").unwrap();
        assert_eq!(p1.task_type, "parse");
        // audit 类型写入并可读回；无需 base_path、不生成本地文件夹
        let p2 = create_project_typed(&db, "", "内控任务", "组A", "audit").unwrap();
        assert_eq!(p2.task_type, "audit");
        assert_eq!(get_project(&db, p2.id).unwrap().task_type, "audit");
        assert!(p2.base_path.is_empty() && p2.project_dir.is_empty());
        assert!(!p2.full_path().exists(), "内控任务不应生成本地目录");
        // 非法类型拒绝
        assert!(matches!(
            create_project_typed(&db, base, "x", "组A", "other"),
            Err(AppError::InvalidInput(_))
        ));
    }

    #[test]
    fn create_project_appends_counter_on_conflict() {
        let (tmp, db) = setup();
        let base = tmp.path().to_str().unwrap();
        let p1 = create_project(&db, base, "同名", "组A").unwrap();
        let p2 = create_project(&db, base, "同名", "组A").unwrap();
        assert_ne!(p1.project_dir, p2.project_dir);
        assert!(p2.project_dir.ends_with("_1"));
    }

    #[test]
    fn create_project_rejects_bad_input() {
        let (tmp, db) = setup();
        let base = tmp.path().to_str().unwrap();
        assert!(matches!(
            create_project(&db, base, "   ", "组A"),
            Err(AppError::InvalidInput(_))
        ));
        assert!(matches!(
            create_project(&db, base, "任务", "  "),
            Err(AppError::InvalidInput(_))
        ));
        assert!(matches!(
            create_project(&db, "/nonexistent/xyz", "a", "组A"),
            Err(AppError::InvalidInput(_))
        ));
    }

    #[test]
    fn list_and_get_project() {
        let (tmp, db) = setup();
        let base = tmp.path().to_str().unwrap();
        assert!(list_projects(&db).unwrap().is_empty());
        let p1 = create_project(&db, base, "甲", "组A").unwrap();
        let p2 = create_project(&db, base, "乙", "组B").unwrap();
        let list = list_projects(&db).unwrap();
        assert_eq!(list.len(), 2);
        // 倒序：后建的在前
        assert_eq!(list[0].id, p2.id);
        assert_eq!(list[1].id, p1.id);
        assert!(matches!(get_project(&db, 9999), Err(AppError::NotFound(_))));
    }

    #[test]
    fn list_projects_computes_updated_at_from_children() {
        let (tmp, db) = setup();
        let base = tmp.path().to_str().unwrap();
        let p = create_project(&db, base, "任务", "组A").unwrap();
        // 初始：updated_at = created_at
        assert_eq!(get_project(&db, p.id).unwrap().updated_at, p.created_at);
        // 插入一条更晚的上传批次后，updated_at 现算为上传时间
        {
            let conn = db.conn().unwrap();
            conn.execute(
                "INSERT INTO upload_batches(project_id, folder_name, uploaded_at)
                 VALUES (?1, 'b1', '2099-01-01 00:00:00')",
                params![p.id],
            )
            .unwrap();
        }
        assert_eq!(
            get_project(&db, p.id).unwrap().updated_at,
            "2099-01-01 00:00:00"
        );
    }

    #[test]
    fn delete_project_trashes_dir_and_cascades() {
        let (tmp, db) = setup();
        let base = tmp.path().to_str().unwrap();
        let p = create_project(&db, base, "将删除", "组A").unwrap();
        let dir = p.full_path();

        // 手工插入一批子表记录（外键层级齐全）
        {
            let conn = db.conn().unwrap();
            conn.execute(
                "INSERT INTO upload_batches(project_id, folder_name, uploaded_at)
                 VALUES (?1, 'b1', '2026-01-01 00:00:00')",
                params![p.id],
            )
            .unwrap();
            let batch_id = conn.last_insert_rowid();
            conn.execute(
                "INSERT INTO source_files(batch_id, original_name, stored_name, relative_path,
                                         file_size, status, created_at)
                 VALUES (?1, 'a.pdf', 'a.pdf', '0-原始文件集/b1/a.pdf', 1, 'pending',
                         '2026-01-01 00:00:00')",
                params![batch_id],
            )
            .unwrap();
            let sf_id = conn.last_insert_rowid();
            conn.execute(
                "INSERT INTO parse_results(source_file_id, status) VALUES (?1, 'done')",
                params![sf_id],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO extract_configs(project_id, column_index, keyword, created_at)
                 VALUES (?1, 2, '金额', '2026-01-01 00:00:00')",
                params![p.id],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO extract_results(project_id, source_file_id, cell_data,
                                            is_edited, created_at, updated_at)
                 VALUES (?1, ?2, '{}', 0,
                         '2026-01-01 00:00:00', '2026-01-01 00:00:00')",
                params![p.id, sf_id],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO export_tasks(project_id, kind, status, created_at)
                 VALUES (?1, 'table', 'pending', '2026-01-01 00:00:00')",
                params![p.id],
            )
            .unwrap();
            // 内控测试子表行（v10）：删除项目时应一并级联清空
            conn.execute(
                "INSERT INTO ic_counters(project_id, prefix, next_val) VALUES (?1, 'ACT', 3)",
                params![p.id],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO ic_roster(project_id, name) VALUES (?1, '张三')",
                params![p.id],
            )
            .unwrap();
        }

        // 注入假回收站：把目录改名到 trashed/
        let trashed = tmp.path().join("trashed");
        delete_project_with(&db, p.id, |path| {
            std::fs::rename(path, &trashed)?;
            Ok(())
        })
        .unwrap();

        assert!(!dir.exists(), "原目录应已被移走");
        assert!(trashed.is_dir());
        let conn = db.conn().unwrap();
        for table in [
            "projects",
            "upload_batches",
            "source_files",
            "parse_results",
            "extract_configs",
            "extract_results",
            "export_tasks",
            "ic_roster",
            "ic_counters",
        ] {
            let count: i64 = conn
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
                .unwrap();
            assert_eq!(count, 0, "{table} 应已级联清空");
        }
    }

    #[test]
    fn delete_project_missing_dir_still_deletes_row() {
        let (tmp, db) = setup();
        let base = tmp.path().to_str().unwrap();
        let p = create_project(&db, base, "无目录", "组A").unwrap();
        std::fs::remove_dir_all(p.full_path()).unwrap();
        delete_project_with(&db, p.id, |_| panic!("目录不存在时不应调用回收站")).unwrap();
        assert!(matches!(get_project(&db, p.id), Err(AppError::NotFound(_))));
    }

    // ---- 进度重置与选中重跑 ----

    /// 造一个含 1 文件且各阶段均有记录的项目，返回文件 id。
    fn fixture_with_progress(db: &Db, project_id: i64) -> i64 {
        let conn = db.conn().unwrap();
        conn.execute(
            "INSERT INTO upload_batches(project_id, folder_name, uploaded_at)
             VALUES (?1, 'b1', '2026-01-01 00:00:00')",
            params![project_id],
        )
        .unwrap();
        let batch_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO source_files(batch_id, original_name, stored_name, relative_path,
                                     file_size, status, created_at)
             VALUES (?1, 'a.pdf', 'a.pdf', '0-原始文件集/b1/a.pdf', 1, 'done',
                     '2026-01-01 00:00:00')",
            params![batch_id],
        )
        .unwrap();
        let fid = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO parse_results(source_file_id, status) VALUES (?1, 'done')",
            params![fid],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO extract_configs(project_id, column_index, keyword, created_at)
             VALUES (?1, 2, '金额', '2026-01-01 00:00:00')",
            params![project_id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO extract_results(project_id, source_file_id, cell_data,
                                        is_edited, created_at, updated_at)
             VALUES (?1, ?2, '{}', 0,
                     '2026-01-01 00:00:00', '2026-01-01 00:00:00')",
            params![project_id, fid],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO export_tasks(project_id, kind, status, created_at)
             VALUES (?1, 'table', 'done', '2026-01-01 00:00:00')",
            params![project_id],
        )
        .unwrap();
        fid
    }

    fn count_where(db: &Db, sql: &str, id: i64) -> i64 {
        db.conn()
            .unwrap()
            .query_row(sql, params![id], |r| r.get(0))
            .unwrap()
    }

    #[test]
    fn reset_project_clears_everything_back_to_fresh() {
        let (tmp, db) = setup();
        let p = create_project(&db, tmp.path().to_str().unwrap(), "重置", "组A").unwrap();
        let fid = fixture_with_progress(&db, p.id);
        // 磁盘放一个原始文件，验证一并清除
        let raw_dir = p.full_path().join(paths::SUBDIR_RAW).join("b1");
        std::fs::create_dir_all(&raw_dir).unwrap();
        std::fs::write(raw_dir.join("a.pdf"), b"%PDF-1.7\n").unwrap();

        // 注入假回收站：把子目录改名到 trashed/
        let trashed = tmp.path().join("trashed");
        std::fs::create_dir_all(&trashed).unwrap();
        reset_project_with(&db, p.id, |path| {
            std::fs::rename(path, trashed.join(path.file_name().unwrap()))?;
            Ok(())
        })
        .unwrap();

        for (sql, what) in [
            (
                "SELECT COUNT(*) FROM parse_results WHERE source_file_id = ?1",
                "解析",
            ),
            (
                "SELECT COUNT(*) FROM extract_results WHERE project_id = ?1",
                "提取",
            ),
            (
                "SELECT COUNT(*) FROM extract_configs WHERE project_id = ?1",
                "关键词",
            ),
            (
                "SELECT COUNT(*) FROM export_tasks WHERE project_id = ?1",
                "导出",
            ),
            ("SELECT COUNT(*) FROM source_files WHERE id = ?1", "文件"),
            (
                "SELECT COUNT(*) FROM upload_batches WHERE project_id = ?1",
                "批次",
            ),
        ] {
            let id = if matches!(what, "解析" | "文件") {
                fid
            } else {
                p.id
            };
            assert_eq!(count_where(&db, sql, id), 0, "{what}记录应已清空");
        }
        assert_eq!(get_project(&db, p.id).unwrap().status, "idle");
        // 4 个标准子目录重建为空；旧内容整体移入回收站（可从系统回收站恢复）
        for sub in paths::PROJECT_SUBDIRS {
            let dir = p.full_path().join(sub);
            assert!(dir.is_dir(), "{sub} 应已重建");
            assert!(
                std::fs::read_dir(&dir).unwrap().next().is_none(),
                "{sub} 应为空"
            );
        }
        assert!(
            trashed
                .join(paths::SUBDIR_RAW)
                .join("b1")
                .join("a.pdf")
                .is_file(),
            "上传文件应已在回收站中"
        );
    }

    #[test]
    fn reset_project_rejects_when_busy() {
        let (tmp, db) = setup();
        let p = create_project(&db, tmp.path().to_str().unwrap(), "忙", "组A").unwrap();
        db.conn()
            .unwrap()
            .execute(
                "UPDATE projects SET status = 'parsing' WHERE id = ?1",
                params![p.id],
            )
            .unwrap();
        assert!(matches!(
            reset_project(&db, p.id),
            Err(AppError::InvalidInput(_))
        ));
    }

    #[test]
    fn delete_files_removes_rows_and_disk() {
        let (tmp, db) = setup();
        let p = create_project(&db, tmp.path().to_str().unwrap(), "删文件", "组A").unwrap();
        let fid = fixture_with_progress(&db, p.id);
        // 磁盘：原始 PDF + 解析产物目录 + llm.json 留档
        let raw_dir = p.full_path().join(paths::SUBDIR_RAW).join("b1");
        std::fs::create_dir_all(&raw_dir).unwrap();
        let raw_pdf = raw_dir.join("a.pdf");
        std::fs::write(&raw_pdf, b"%PDF-1.7\n").unwrap();
        let parse_dir = p.full_path().join(paths::SUBDIR_PARSE).join("a");
        std::fs::create_dir_all(&parse_dir).unwrap();
        let llm_json = p.full_path().join(paths::SUBDIR_EXTRACT).join("a.llm.json");
        std::fs::write(&llm_json, b"{}").unwrap();
        // parse_results.output_dir 指向解析产物目录
        db.conn()
            .unwrap()
            .execute(
                "UPDATE parse_results SET output_dir = ?2 WHERE source_file_id = ?1",
                params![fid, parse_dir.to_string_lossy()],
            )
            .unwrap();

        // 注入假回收站：改名到 trashed/（basename 保持）
        let trashed = tmp.path().join("trashed");
        std::fs::create_dir_all(&trashed).unwrap();
        delete_files_with(&db, p.id, &[fid], |path| {
            std::fs::rename(path, trashed.join(path.file_name().unwrap()))?;
            Ok(())
        })
        .unwrap();

        // DB：文件、解析、提取行全删；空批次清理
        assert_eq!(
            count_where(&db, "SELECT COUNT(*) FROM source_files WHERE id = ?1", fid),
            0
        );
        assert_eq!(
            count_where(
                &db,
                "SELECT COUNT(*) FROM parse_results WHERE source_file_id = ?1",
                fid
            ),
            0
        );
        assert_eq!(
            count_where(
                &db,
                "SELECT COUNT(*) FROM extract_results WHERE project_id = ?1",
                p.id
            ),
            0
        );
        assert_eq!(
            count_where(
                &db,
                "SELECT COUNT(*) FROM upload_batches WHERE project_id = ?1",
                p.id
            ),
            0,
            "空批次应已清理"
        );
        // 关键词配置不受删除文件影响
        assert_eq!(
            count_where(
                &db,
                "SELECT COUNT(*) FROM extract_configs WHERE project_id = ?1",
                p.id
            ),
            1
        );
        // 磁盘：原始 PDF、解析产物目录、llm.json 均移入回收站（可恢复），原位置清空
        assert!(!raw_pdf.exists());
        assert!(!parse_dir.exists());
        assert!(!llm_json.exists());
        assert!(trashed.join("a.pdf").is_file(), "原始 PDF 应在回收站");
        assert!(trashed.join("a").is_dir(), "解析产物目录应在回收站");
        assert!(
            trashed.join("a.llm.json").is_file(),
            "llm.json 留档应在回收站"
        );
        // 非本项目文件：报错
        assert!(matches!(
            delete_files(&db, p.id, &[9999]),
            Err(AppError::NotFound(_))
        ));
    }

    #[test]
    fn reparse_files_cascades_extract_and_marks_pending() {
        let (tmp, db) = setup();
        let p = create_project(&db, tmp.path().to_str().unwrap(), "重解析", "组A").unwrap();
        let fid = fixture_with_progress(&db, p.id);

        reparse_files(&db, p.id, &[fid]).unwrap();

        assert_eq!(
            count_where(
                &db,
                "SELECT COUNT(*) FROM parse_results WHERE source_file_id = ?1",
                fid
            ),
            0
        );
        assert_eq!(
            count_where(
                &db,
                "SELECT COUNT(*) FROM extract_results WHERE project_id = ?1",
                p.id
            ),
            0,
            "提取结果应被级联删除"
        );
        assert_eq!(
            count_where(
                &db,
                "SELECT COUNT(*) FROM extract_configs WHERE project_id = ?1",
                p.id
            ),
            1,
            "关键词配置应保留"
        );
        let status: String = db
            .conn()
            .unwrap()
            .query_row(
                "SELECT status FROM source_files WHERE id = ?1",
                params![fid],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(status, "pending");
        // 非本项目文件 / 空选择：报错
        assert!(matches!(
            reparse_files(&db, p.id, &[9999]),
            Err(AppError::NotFound(_))
        ));
        assert!(matches!(
            reparse_files(&db, p.id, &[]),
            Err(AppError::InvalidInput(_))
        ));
    }

    #[test]
    fn reextract_files_only_deletes_extract_rows() {
        let (tmp, db) = setup();
        let p = create_project(&db, tmp.path().to_str().unwrap(), "重提取", "组A").unwrap();
        let fid = fixture_with_progress(&db, p.id);

        reextract_files(&db, p.id, &[fid]).unwrap();

        assert_eq!(
            count_where(
                &db,
                "SELECT COUNT(*) FROM extract_results WHERE project_id = ?1",
                p.id
            ),
            0
        );
        assert_eq!(
            count_where(
                &db,
                "SELECT COUNT(*) FROM parse_results WHERE source_file_id = ?1",
                fid
            ),
            1,
            "解析结果不受影响"
        );
        let status: String = db
            .conn()
            .unwrap()
            .query_row(
                "SELECT status FROM source_files WHERE id = ?1",
                params![fid],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(status, "done", "文件解析状态不受影响");
    }

    #[test]
    fn recover_interrupted_reverts_states_and_cleans_partial() {
        let (tmp, db) = setup();
        let p = create_project(&db, tmp.path().to_str().unwrap(), "中断", "组A").unwrap();
        let fid = fixture_with_progress(&db, p.id);
        // 模拟解析中断：项目 parsing、文件 parsing、parse_results running
        {
            let conn = db.conn().unwrap();
            conn.execute(
                "UPDATE projects SET status = 'parsing' WHERE id = ?1",
                params![p.id],
            )
            .unwrap();
            conn.execute(
                "UPDATE source_files SET status = 'parsing' WHERE id = ?1",
                params![fid],
            )
            .unwrap();
            conn.execute(
                "UPDATE parse_results SET status = 'running' WHERE source_file_id = ?1",
                params![fid],
            )
            .unwrap();
        }
        // 半成品目录（与 parse.rs 命名规则一致：SUBDIR_PARSE/<文件名主体>）
        let partial = p.full_path().join(paths::SUBDIR_PARSE).join("a");
        std::fs::create_dir_all(&partial).unwrap();
        std::fs::write(partial.join("page-1.json"), b"{}").unwrap();

        recover_interrupted(&db).unwrap();

        // running 行删除、文件回 pending、项目回 idle、半成品目录清除
        assert_eq!(
            count_where(
                &db,
                "SELECT COUNT(*) FROM parse_results WHERE source_file_id = ?1",
                fid
            ),
            0,
            "running 解析行应已删除"
        );
        let fstatus: String = db
            .conn()
            .unwrap()
            .query_row(
                "SELECT status FROM source_files WHERE id = ?1",
                params![fid],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(fstatus, "pending");
        assert_eq!(get_project(&db, p.id).unwrap().status, "idle");
        assert!(!partial.exists(), "半成品解析目录应已清除");
        // 提取结果/关键词不受影响
        assert_eq!(
            count_where(
                &db,
                "SELECT COUNT(*) FROM extract_results WHERE project_id = ?1",
                p.id
            ),
            1
        );
    }

    #[test]
    fn recover_interrupted_extracting_project_also_recovers() {
        let (tmp, db) = setup();
        let p = create_project(&db, tmp.path().to_str().unwrap(), "中断提取", "组A").unwrap();
        db.conn()
            .unwrap()
            .execute(
                "UPDATE projects SET status = 'extracting' WHERE id = ?1",
                params![p.id],
            )
            .unwrap();
        recover_interrupted(&db).unwrap();
        assert_eq!(get_project(&db, p.id).unwrap().status, "idle");
    }
}
