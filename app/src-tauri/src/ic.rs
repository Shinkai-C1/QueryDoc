//! 内控测试（audit 任务）：制度文件导入（pdf 递归收集、字节直接入库
//! ic_documents.data，不生成本地文件夹；单文件可删，删除后节点保留但跳转失效）、
//! 人员名册/组织结构 xlsx 导入（calamine 解析，先清空再整体替换）。
//! 纯增量模块：不触碰解析提取既有逻辑（§2 原则）。

use std::path::{Path, PathBuf};

use rusqlite::{params, OptionalExtension};
use serde::Serialize;

use crate::db::Db;
use crate::error::AppError;
use crate::projects;

// ---------------------------------------------------------------------------
// 制度文件（字节入库，不落本地目录）
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct IcDocument {
    pub id: i64,
    pub project_id: i64,
    pub name: String,
    /// 相对导入文件夹的路径（`/` 分隔），用于树层级。
    pub rel_path: String,
    /// 遗留字段（v10 磁盘落盘路径）；v11 起入库恒为空串。
    pub file_path: String,
    /// pdf 字节数。
    pub size: i64,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct IcImportDocsResult {
    /// 新导入的文件数（rel_path 首次出现）。
    pub imported: i64,
    /// 同 rel_path 重复导入被覆盖更新的文件数。
    pub updated: i64,
}

fn row_to_doc(row: &rusqlite::Row<'_>) -> rusqlite::Result<IcDocument> {
    Ok(IcDocument {
        id: row.get("id")?,
        project_id: row.get("project_id")?,
        name: row.get("name")?,
        rel_path: row.get("rel_path")?,
        file_path: row.get("file_path")?,
        size: row.get("size")?,
        created_at: row.get("created_at")?,
    })
}

/// 递归收集目录下全部 pdf（扩展名大小写不敏感），按相对路径排序输出
/// （绝对路径, 相对路径 `/` 分隔）。
fn collect_pdfs(dir: &Path, root: &Path, out: &mut Vec<(PathBuf, String)>) -> Result<(), AppError> {
    let mut entries: Vec<_> = std::fs::read_dir(dir)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let path = entry.path();
        if path.is_dir() {
            collect_pdfs(&path, root, out)?;
        } else if path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("pdf"))
        {
            let rel = path
                .strip_prefix(root)
                .map_err(|e| AppError::Internal(format!("相对路径计算失败: {e}")))?;
            let rel_str = rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/");
            out.push((path, rel_str));
        }
    }
    Ok(())
}

/// 导入制度文件：递归收集 src_dir 下全部 pdf，字节直接写入 ic_documents.data
/// （按 rel_path 建树；重复导入同 rel_path 覆盖更新；逐文件独立落库）。
pub fn import_docs(
    db: &Db,
    project_id: i64,
    src_dir: &str,
) -> Result<IcImportDocsResult, AppError> {
    projects::get_project(db, project_id)?;
    let src = Path::new(src_dir);
    if !src.is_dir() {
        return Err(AppError::InvalidInput(format!(
            "制度文件源目录不存在或不是目录: {src_dir}"
        )));
    }
    let src = src.canonicalize()?;

    let mut pdfs: Vec<(PathBuf, String)> = Vec::new();
    collect_pdfs(&src, &src, &mut pdfs)?;
    if pdfs.is_empty() {
        return Err(AppError::Validation(
            "所选文件夹中没有 PDF 文件".to_string(),
        ));
    }

    let mut imported = 0i64;
    let mut updated = 0i64;
    for (abs, rel) in pdfs.into_iter() {
        let name = abs
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| rel.clone());
        let data = std::fs::read(&abs)?;
        let size = data.len() as i64;
        let conn = db.conn()?;
        let existing: Option<i64> = conn
            .query_row(
                "SELECT id FROM ic_documents WHERE project_id = ?1 AND rel_path = ?2",
                params![project_id, rel],
                |r| r.get(0),
            )
            .optional()?;
        match existing {
            Some(id) => {
                conn.execute(
                    "UPDATE ic_documents SET name = ?2, file_path = '', data = ?3, size = ?4
                     WHERE id = ?1",
                    params![id, name, data, size],
                )?;
                updated += 1;
            }
            None => {
                conn.execute(
                    "INSERT INTO ic_documents(project_id, name, rel_path, file_path, data, size)
                     VALUES (?1, ?2, ?3, '', ?4, ?5)",
                    params![project_id, name, rel, data, size],
                )?;
                imported += 1;
            }
        }
    }
    Ok(IcImportDocsResult { imported, updated })
}

/// 项目全部制度文件（不含字节列），按 rel_path 字典序（树构建顺序稳定）。
pub fn list_docs(db: &Db, project_id: i64) -> Result<Vec<IcDocument>, AppError> {
    projects::get_project(db, project_id)?;
    let conn = db.conn()?;
    let mut stmt = conn.prepare(
        "SELECT id, project_id, name, rel_path, file_path, size, created_at
         FROM ic_documents WHERE project_id = ?1 ORDER BY rel_path",
    )?;
    let rows = stmt
        .query_map(params![project_id], row_to_doc)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

// ---------------------------------------------------------------------------
// 人员名册 / 组织结构（xlsx 导入，先清空再整体替换）
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct IcRosterRow {
    pub id: i64,
    pub project_id: i64,
    pub name: Option<String>,
    pub dept: Option<String>,
    pub position: Option<String>,
    pub duty: Option<String>,
    pub superior: Option<String>,
    pub subordinate: Option<String>,
    pub remark: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct IcOrgRow {
    pub id: i64,
    pub project_id: i64,
    pub dept: Option<String>,
    pub duty: Option<String>,
    pub superior: Option<String>,
    pub subordinate: Option<String>,
    pub remark: Option<String>,
}

/// 人员名册模板表头（7 列，全字符串；名册模板规范）。
const ROSTER_HEADERS: [&str; 7] = [
    "姓名",
    "部门组织",
    "岗位",
    "岗位职责描述",
    "上级",
    "下级",
    "备注",
];
/// 组织结构模板表头（5 列，全字符串；组织结构模板规范）。
const ORG_HEADERS: [&str; 5] = ["部门组织", "部门职责描述", "上级", "下级", "备注"];

/// 生成导入模版 xlsx 到指定路径（表头加粗 + 一行示例，供用户手动编辑后上传）。
pub fn write_template(kind: &str, path: &str) -> Result<(), AppError> {
    let (headers, example): (&[&str], &[&str]) = match kind {
        "roster" => (
            &ROSTER_HEADERS,
            &[
                "张三",
                "财务部",
                "会计",
                "负责日常记账与凭证审核",
                "李四",
                "",
                "示例行，可整行删除",
            ],
        ),
        "org" => (
            &ORG_HEADERS,
            &[
                "财务部",
                "负责资金收付与账务处理",
                "总经理室",
                "会计科",
                "示例行，可整行删除",
            ],
        ),
        other => {
            return Err(AppError::InvalidInput(format!(
                "未知模板类型: {other}（合法: roster/org）"
            )));
        }
    };
    let mut wb = rust_xlsxwriter::Workbook::new();
    let ws = wb.add_worksheet();
    let header_fmt = rust_xlsxwriter::Format::new().set_bold();
    for (j, h) in headers.iter().enumerate() {
        ws.write_string_with_format(0, j as u16, *h, &header_fmt)?;
        ws.set_column_width(j as u16, 16)?;
    }
    for (j, v) in example.iter().enumerate() {
        ws.write_string(1, j as u16, *v)?;
    }
    wb.save(path)?;
    Ok(())
}

/// 读取 xlsx 第一个工作表为字符串矩阵（空单元格 → ""，首尾 trim）。
fn read_xlsx_rows(xlsx_path: &str) -> Result<Vec<Vec<String>>, AppError> {
    use calamine::{open_workbook_auto, Reader};
    let mut wb = open_workbook_auto(xlsx_path)
        .map_err(|e| AppError::Validation(format!("无法读取 Excel 文件 {xlsx_path}: {e}")))?;
    let range = wb
        .worksheet_range_at(0)
        .ok_or_else(|| AppError::Validation("Excel 文件中没有工作表".to_string()))?
        .map_err(|e| AppError::Validation(format!("读取工作表失败: {e}")))?;
    let rows = range
        .rows()
        .map(|r| {
            let mut cells: Vec<String> = r
                .iter()
                .map(|c| match c {
                    calamine::Data::Empty => String::new(),
                    other => other.to_string().trim().to_string(),
                })
                .collect();
            // 去掉行尾空单元格
            while cells.last().is_some_and(|c| c.is_empty()) {
                cells.pop();
            }
            cells
        })
        .collect();
    Ok(rows)
}

/// 表头校验：首行须与模板列完全一致（顺序、名称）。
fn check_headers(rows: &[Vec<String>], expected: &[&str], what: &str) -> Result<(), AppError> {
    let Some(header) = rows.first() else {
        return Err(AppError::Validation(format!("{what}文件为空（无表头行）")));
    };
    let got: Vec<&str> = header.iter().map(|s| s.as_str()).collect();
    if got != expected {
        return Err(AppError::Validation(format!(
            "{what}表头不符：期望 [{}]，实际 [{}]",
            expected.join("、"),
            got.join("、")
        )));
    }
    Ok(())
}

/// 单元格取值：空串 → None。
fn cell(row: &[String], idx: usize) -> Option<String> {
    row.get(idx).filter(|s| !s.is_empty()).cloned()
}

/// 导入人员名册（先清空该项目名册再整体替换；返回导入行数）。
pub fn import_roster(db: &Db, project_id: i64, xlsx_path: &str) -> Result<i64, AppError> {
    projects::get_project(db, project_id)?;
    let rows = read_xlsx_rows(xlsx_path)?;
    check_headers(&rows, &ROSTER_HEADERS, "人员名册")?;
    let data = &rows[1..];
    let conn = db.conn()?;
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "DELETE FROM ic_roster WHERE project_id = ?1",
        params![project_id],
    )?;
    for r in data {
        tx.execute(
            "INSERT INTO ic_roster(project_id, name, dept, position, duty, superior, subordinate, remark)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                project_id,
                cell(r, 0),
                cell(r, 1),
                cell(r, 2),
                cell(r, 3),
                cell(r, 4),
                cell(r, 5),
                cell(r, 6)
            ],
        )?;
    }
    tx.commit()?;
    Ok(data.len() as i64)
}

/// 导入组织结构（先清空再整体替换；返回导入行数）。
pub fn import_org(db: &Db, project_id: i64, xlsx_path: &str) -> Result<i64, AppError> {
    projects::get_project(db, project_id)?;
    let rows = read_xlsx_rows(xlsx_path)?;
    check_headers(&rows, &ORG_HEADERS, "组织结构")?;
    let data = &rows[1..];
    let conn = db.conn()?;
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "DELETE FROM ic_org WHERE project_id = ?1",
        params![project_id],
    )?;
    for r in data {
        tx.execute(
            "INSERT INTO ic_org(project_id, dept, duty, superior, subordinate, remark)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                project_id,
                cell(r, 0),
                cell(r, 1),
                cell(r, 2),
                cell(r, 3),
                cell(r, 4)
            ],
        )?;
    }
    tx.commit()?;
    Ok(data.len() as i64)
}

pub fn list_roster(db: &Db, project_id: i64) -> Result<Vec<IcRosterRow>, AppError> {
    projects::get_project(db, project_id)?;
    let conn = db.conn()?;
    let mut stmt = conn.prepare(
        "SELECT id, project_id, name, dept, position, duty, superior, subordinate, remark
         FROM ic_roster WHERE project_id = ?1 ORDER BY id",
    )?;
    let rows = stmt
        .query_map(params![project_id], |row| {
            Ok(IcRosterRow {
                id: row.get("id")?,
                project_id: row.get("project_id")?,
                name: row.get("name")?,
                dept: row.get("dept")?,
                position: row.get("position")?,
                duty: row.get("duty")?,
                superior: row.get("superior")?,
                subordinate: row.get("subordinate")?,
                remark: row.get("remark")?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn list_org(db: &Db, project_id: i64) -> Result<Vec<IcOrgRow>, AppError> {
    projects::get_project(db, project_id)?;
    let conn = db.conn()?;
    let mut stmt = conn.prepare(
        "SELECT id, project_id, dept, duty, superior, subordinate, remark
         FROM ic_org WHERE project_id = ?1 ORDER BY id",
    )?;
    let rows = stmt
        .query_map(params![project_id], |row| {
            Ok(IcOrgRow {
                id: row.get("id")?,
                project_id: row.get("project_id")?,
                dept: row.get("dept")?,
                duty: row.get("duty")?,
                superior: row.get("superior")?,
                subordinate: row.get("subordinate")?,
                remark: row.get("remark")?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

// ---------------------------------------------------------------------------
// 制度索引（源 pdf 矩形 → 目标 pdf 文件）
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct IcDocLink {
    pub id: i64,
    pub project_id: i64,
    pub src_doc_id: i64,
    pub page: i64,
    /// 0–1000 归一化矩形坐标 json（[x0,y0,x1,y1]）。
    pub rect: String,
    pub target_doc_id: i64,
}

fn row_to_doc_link(row: &rusqlite::Row<'_>) -> rusqlite::Result<IcDocLink> {
    Ok(IcDocLink {
        id: row.get("id")?,
        project_id: row.get("project_id")?,
        src_doc_id: row.get("src_doc_id")?,
        page: row.get("page")?,
        rect: row.get("rect")?,
        target_doc_id: row.get("target_doc_id")?,
    })
}

/// 校验 doc 属于该项目；返回行 id。
fn ensure_doc_in_project(db: &Db, project_id: i64, doc_id: i64) -> Result<(), AppError> {
    let conn = db.conn()?;
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM ic_documents WHERE id = ?1 AND project_id = ?2",
        params![doc_id, project_id],
        |r| r.get(0),
    )?;
    if n == 0 {
        return Err(AppError::NotFound(format!(
            "制度文件不属于该任务或不存在: {doc_id}"
        )));
    }
    Ok(())
}

/// 校验 rect 为 0–1000 归一化 [x0,y0,x1,y1] 且 x0<x1, y0<y1。
pub fn ensure_valid_rect(rect: &str) -> Result<(), AppError> {
    let v: Vec<f64> = serde_json::from_str(rect)
        .map_err(|_| AppError::InvalidInput(format!("矩形坐标格式非法: {rect}")))?;
    if v.len() != 4 || v[0] >= v[2] || v[1] >= v[3] || v.iter().any(|x| *x < 0.0 || *x > 1000.0) {
        return Err(AppError::InvalidInput(format!(
            "矩形坐标须为 0-1000 归一化 [x0,y0,x1,y1] 且 x0<x1、y0<y1: {rect}"
        )));
    }
    Ok(())
}

/// 读取制度文件字节（含归属校验；v11 起从 ic_documents.data 读，
/// 旧行 data 为 NULL 时回落磁盘 file_path 并走受控路径校验）。
pub fn doc_bytes(db: &Db, project_id: i64, doc_id: i64) -> Result<Vec<u8>, AppError> {
    ensure_doc_in_project(db, project_id, doc_id)?;
    let conn = db.conn()?;
    let (data, file_path): (Option<Vec<u8>>, String) = conn.query_row(
        "SELECT data, file_path FROM ic_documents WHERE id = ?1",
        params![doc_id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    if let Some(bytes) = data {
        return Ok(bytes);
    }
    // 旧行回落：磁盘路径受控读取
    if file_path.is_empty() {
        return Err(AppError::NotFound(format!("制度文件内容不存在: {doc_id}")));
    }
    crate::parse::read_file_bytes(db, &file_path)
}

/// 删除制度文件（仅限本项目行）。节点与索引行保留：
/// 流程图不受影响，仅指向该文件的跳转/索引在运行时判定为失效。
pub fn delete_doc(db: &Db, project_id: i64, doc_id: i64) -> Result<(), AppError> {
    let conn = db.conn()?;
    let n = conn.execute(
        "DELETE FROM ic_documents WHERE id = ?1 AND project_id = ?2",
        params![doc_id, project_id],
    )?;
    if n == 0 {
        return Err(AppError::NotFound(format!("制度文件不存在: {doc_id}")));
    }
    Ok(())
}

/// 新增制度索引；返回行 id。
pub fn add_doc_link(
    db: &Db,
    project_id: i64,
    src_doc_id: i64,
    page: i64,
    rect: &str,
    target_doc_id: i64,
) -> Result<i64, AppError> {
    projects::get_project(db, project_id)?;
    ensure_doc_in_project(db, project_id, src_doc_id)?;
    ensure_doc_in_project(db, project_id, target_doc_id)?;
    if page < 1 {
        return Err(AppError::InvalidInput(format!("页码须从 1 开始: {page}")));
    }
    ensure_valid_rect(rect)?;
    let conn = db.conn()?;
    conn.execute(
        "INSERT INTO ic_doc_links(project_id, src_doc_id, page, rect, target_doc_id)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![project_id, src_doc_id, page, rect, target_doc_id],
    )?;
    Ok(conn.last_insert_rowid())
}

/// 删除制度索引（仅限本项目行）。
pub fn remove_doc_link(db: &Db, project_id: i64, link_id: i64) -> Result<(), AppError> {
    let conn = db.conn()?;
    let n = conn.execute(
        "DELETE FROM ic_doc_links WHERE id = ?1 AND project_id = ?2",
        params![link_id, project_id],
    )?;
    if n == 0 {
        return Err(AppError::NotFound(format!("制度索引不存在: {link_id}")));
    }
    Ok(())
}

pub fn list_doc_links(db: &Db, project_id: i64) -> Result<Vec<IcDocLink>, AppError> {
    projects::get_project(db, project_id)?;
    let conn = db.conn()?;
    let mut stmt = conn.prepare(
        "SELECT id, project_id, src_doc_id, page, rect, target_doc_id
         FROM ic_doc_links WHERE project_id = ?1 ORDER BY id",
    )?;
    let rows = stmt
        .query_map(params![project_id], row_to_doc_link)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

// ---------------------------------------------------------------------------
// 流程节点（code 由 ic_counters 分配：前缀+序号，全局唯一，删除不复用）
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct IcNode {
    pub id: i64,
    pub project_id: i64,
    pub code: String,
    pub node_type: String,
    pub subtype: Option<String>,
    pub name: String,
    pub content: String,
    pub doc_id: Option<i64>,
    pub page: Option<i64>,
    pub rect: Option<String>,
    pub canvas_x: f64,
    pub canvas_y: f64,
    /// 画布尺寸；0 = 内容自适应（v12 起）。
    pub canvas_w: f64,
    pub canvas_h: f64,
    pub group_id: Option<i64>,
    pub props: String,
    pub created_at: String,
}

fn row_to_node(row: &rusqlite::Row<'_>) -> rusqlite::Result<IcNode> {
    Ok(IcNode {
        id: row.get("id")?,
        project_id: row.get("project_id")?,
        code: row.get("code")?,
        node_type: row.get("node_type")?,
        subtype: row.get("subtype")?,
        name: row.get("name")?,
        content: row.get("content")?,
        doc_id: row.get("doc_id")?,
        page: row.get("page")?,
        rect: row.get("rect")?,
        canvas_x: row.get("canvas_x")?,
        canvas_y: row.get("canvas_y")?,
        canvas_w: row.get("canvas_w")?,
        canvas_h: row.get("canvas_h")?,
        group_id: row.get("group_id")?,
        props: row.get("props")?,
        created_at: row.get("created_at")?,
    })
}

/// 节点类型 → 合法子类型（ACT/LOG 无子类型；存量 LOG 节点的历史子类型不校验、仅展示）。
fn ensure_type_subtype(node_type: &str, subtype: Option<&str>) -> Result<(), AppError> {
    let allowed: &[&str] = match node_type {
        "ACT" => &[],
        "EVT" => &["time", "message"],
        "ROL" => &["person", "dept", "it"],
        "DAT" => &["electronic", "paper", "record"],
        "LOG" => &[],
        other => {
            return Err(AppError::InvalidInput(format!(
                "未知节点类型: {other}（合法: ACT/EVT/ROL/DAT/LOG）"
            )));
        }
    };
    match subtype {
        None => Ok(()),
        Some(s) if allowed.contains(&s) => Ok(()),
        Some(s) => Err(AppError::InvalidInput(format!(
            "节点类型 {node_type} 不支持子类型 {s}（合法: {}）",
            allowed.join("/")
        ))),
    }
}

/// 分配节点编号：前缀+序号（ic_counters 自增，全局唯一，删除不复用）。调用方须持事务。
fn next_code(
    conn: &rusqlite::Connection,
    project_id: i64,
    prefix: &str,
) -> Result<String, AppError> {
    conn.execute(
        "INSERT INTO ic_counters(project_id, prefix, next_val) VALUES (?1, ?2, 1)
         ON CONFLICT(project_id, prefix) DO NOTHING",
        params![project_id, prefix],
    )?;
    let n: i64 = conn.query_row(
        "SELECT next_val FROM ic_counters WHERE project_id = ?1 AND prefix = ?2",
        params![project_id, prefix],
        |r| r.get(0),
    )?;
    conn.execute(
        "UPDATE ic_counters SET next_val = next_val + 1 WHERE project_id = ?1 AND prefix = ?2",
        params![project_id, prefix],
    )?;
    Ok(format!("{prefix}-{n}"))
}

/// 创建节点的可选字段入参（command 层 serde 透传）。
#[derive(Debug, Clone, serde::Deserialize)]
pub struct IcNodeInput {
    pub node_type: String,
    pub subtype: Option<String>,
    pub name: Option<String>,
    pub content: Option<String>,
    pub doc_id: Option<i64>,
    pub page: Option<i64>,
    pub rect: Option<String>,
    pub canvas_x: Option<f64>,
    pub canvas_y: Option<f64>,
    pub group_id: Option<i64>,
    pub props: Option<String>,
}

/// 创建流程节点：分配编号并落库，返回完整行。
pub fn create_node(db: &Db, project_id: i64, input: &IcNodeInput) -> Result<IcNode, AppError> {
    projects::get_project(db, project_id)?;
    ensure_type_subtype(&input.node_type, input.subtype.as_deref())?;
    // 来源 pdf 校验（三项要么全有要么全无）
    match (input.doc_id, input.page, &input.rect) {
        (Some(doc_id), Some(page), Some(rect)) => {
            ensure_doc_in_project(db, project_id, doc_id)?;
            if page < 1 {
                return Err(AppError::InvalidInput(format!("页码须从 1 开始: {page}")));
            }
            ensure_valid_rect(rect)?;
        }
        (None, None, None) => {}
        _ => {
            return Err(AppError::InvalidInput(
                "来源 pdf 的 doc_id/page/rect 须同时提供或同时缺省".to_string(),
            ));
        }
    }
    if let Some(props) = &input.props {
        // props 须为 json 对象（ACT 扩展属性）
        match serde_json::from_str::<serde_json::Value>(props) {
            Ok(serde_json::Value::Object(_)) => {}
            _ => {
                return Err(AppError::InvalidInput(format!(
                    "props 须为 json 对象: {props}"
                )))
            }
        }
    }
    let conn = db.conn()?;
    let tx = conn.unchecked_transaction()?;
    let code = next_code(&tx, project_id, &input.node_type)?;
    tx.execute(
        "INSERT INTO ic_nodes(project_id, code, node_type, subtype, name, content,
                              doc_id, page, rect, canvas_x, canvas_y, group_id, props)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        params![
            project_id,
            code,
            input.node_type,
            input.subtype,
            input.name.as_deref().unwrap_or(""),
            input.content.as_deref().unwrap_or(""),
            input.doc_id,
            input.page,
            input.rect,
            input.canvas_x.unwrap_or(0.0),
            input.canvas_y.unwrap_or(0.0),
            input.group_id,
            input.props.as_deref().unwrap_or("{}"),
        ],
    )?;
    let id = tx.last_insert_rowid();
    tx.commit()?;
    let node = conn.query_row(
        "SELECT * FROM ic_nodes WHERE id = ?1",
        params![id],
        row_to_node,
    )?;
    Ok(node)
}

/// 更新节点的可选补丁（None = 不动该列；group_id 外层 Some(None) = 清除所属组）。
#[derive(Debug, Clone, serde::Deserialize)]
pub struct IcNodePatch {
    pub name: Option<String>,
    pub content: Option<String>,
    pub canvas_x: Option<f64>,
    pub canvas_y: Option<f64>,
    pub canvas_w: Option<f64>,
    pub canvas_h: Option<f64>,
    pub group_id: Option<Option<i64>>,
    pub props: Option<String>,
}

pub fn update_node(
    db: &Db,
    project_id: i64,
    node_id: i64,
    patch: &IcNodePatch,
) -> Result<IcNode, AppError> {
    // 归属校验
    let conn = db.conn()?;
    let exists: i64 = conn.query_row(
        "SELECT COUNT(*) FROM ic_nodes WHERE id = ?1 AND project_id = ?2",
        params![node_id, project_id],
        |r| r.get(0),
    )?;
    if exists == 0 {
        return Err(AppError::NotFound(format!("节点不存在: {node_id}")));
    }
    if let Some(props) = &patch.props {
        match serde_json::from_str::<serde_json::Value>(props) {
            Ok(serde_json::Value::Object(_)) => {}
            _ => {
                return Err(AppError::InvalidInput(format!(
                    "props 须为 json 对象: {props}"
                )))
            }
        }
    }
    if let Some(name) = &patch.name {
        conn.execute(
            "UPDATE ic_nodes SET name = ?2 WHERE id = ?1",
            params![node_id, name],
        )?;
    }
    if let Some(content) = &patch.content {
        conn.execute(
            "UPDATE ic_nodes SET content = ?2 WHERE id = ?1",
            params![node_id, content],
        )?;
    }
    if let Some(x) = patch.canvas_x {
        conn.execute(
            "UPDATE ic_nodes SET canvas_x = ?2 WHERE id = ?1",
            params![node_id, x],
        )?;
    }
    if let Some(y) = patch.canvas_y {
        conn.execute(
            "UPDATE ic_nodes SET canvas_y = ?2 WHERE id = ?1",
            params![node_id, y],
        )?;
    }
    if let Some(w) = patch.canvas_w {
        conn.execute(
            "UPDATE ic_nodes SET canvas_w = ?2 WHERE id = ?1",
            params![node_id, w],
        )?;
    }
    if let Some(h) = patch.canvas_h {
        conn.execute(
            "UPDATE ic_nodes SET canvas_h = ?2 WHERE id = ?1",
            params![node_id, h],
        )?;
    }
    if let Some(g) = &patch.group_id {
        conn.execute(
            "UPDATE ic_nodes SET group_id = ?2 WHERE id = ?1",
            params![node_id, g],
        )?;
    }
    if let Some(props) = &patch.props {
        conn.execute(
            "UPDATE ic_nodes SET props = ?2 WHERE id = ?1",
            params![node_id, props],
        )?;
    }
    let node = conn.query_row(
        "SELECT * FROM ic_nodes WHERE id = ?1",
        params![node_id],
        row_to_node,
    )?;
    Ok(node)
}

/// 删除节点：级联清边（from/to 引用），编号废弃不复用。
pub fn delete_node(db: &Db, project_id: i64, node_id: i64) -> Result<(), AppError> {
    let conn = db.conn()?;
    let tx = conn.unchecked_transaction()?;
    let n = tx.execute(
        "DELETE FROM ic_nodes WHERE id = ?1 AND project_id = ?2",
        params![node_id, project_id],
    )?;
    if n == 0 {
        return Err(AppError::NotFound(format!("节点不存在: {node_id}")));
    }
    tx.execute(
        "DELETE FROM ic_edges WHERE project_id = ?1 AND (from_node = ?2 OR to_node = ?2)",
        params![project_id, node_id],
    )?;
    tx.commit()?;
    Ok(())
}

pub fn list_nodes(db: &Db, project_id: i64) -> Result<Vec<IcNode>, AppError> {
    projects::get_project(db, project_id)?;
    let conn = db.conn()?;
    let mut stmt = conn.prepare("SELECT * FROM ic_nodes WHERE project_id = ?1 ORDER BY id")?;
    let rows = stmt
        .query_map(params![project_id], row_to_node)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

// ---------------------------------------------------------------------------
// 连线（flow 上下流 | role 角色 | data 数据）与环境组
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct IcEdge {
    pub id: i64,
    pub project_id: i64,
    pub from_node: i64,
    pub to_node: i64,
    /// flow(上下流)|role(角色)|data(数据)
    pub edge_kind: String,
}

fn row_to_edge(row: &rusqlite::Row<'_>) -> rusqlite::Result<IcEdge> {
    Ok(IcEdge {
        id: row.get("id")?,
        project_id: row.get("project_id")?,
        from_node: row.get("from_node")?,
        to_node: row.get("to_node")?,
        edge_kind: row.get("edge_kind")?,
    })
}

/// 校验节点属于该项目并返回其类型。
fn node_type_in_project(db: &Db, project_id: i64, node_id: i64) -> Result<String, AppError> {
    let conn = db.conn()?;
    conn.query_row(
        "SELECT node_type FROM ic_nodes WHERE id = ?1 AND project_id = ?2",
        params![node_id, project_id],
        |r| r.get(0),
    )
    .optional()?
    .ok_or_else(|| AppError::NotFound(format!("节点不存在: {node_id}")))
}

/// 连线校验（按用户最新规则）：
/// - 任意两类节点均可连接（flow）；
/// - 仅当一端为 控制活动(ACT)、另一端为 角色(ROL) 时，连线语义为 role（角色关联，
///   方向不限）；同理 {ACT, DAT} 为 data（数据关联）；
/// - 除自连与同向同类型重复连线外不做其他限制。
pub fn add_edge(
    db: &Db,
    project_id: i64,
    from_node: i64,
    to_node: i64,
    edge_kind: &str,
) -> Result<i64, AppError> {
    projects::get_project(db, project_id)?;
    if from_node == to_node {
        return Err(AppError::InvalidInput("不允许节点自连".to_string()));
    }
    let from_type = node_type_in_project(db, project_id, from_node)?;
    let to_type = node_type_in_project(db, project_id, to_node)?;
    match edge_kind {
        "flow" => {} // 任意节点均可
        "role" => {
            if !((from_type == "ACT" && to_type == "ROL")
                || (from_type == "ROL" && to_type == "ACT"))
            {
                return Err(AppError::InvalidInput(
                    "角色关联须发生在 控制活动 与 角色节点 之间".to_string(),
                ));
            }
        }
        "data" => {
            if !((from_type == "ACT" && to_type == "DAT")
                || (from_type == "DAT" && to_type == "ACT"))
            {
                return Err(AppError::InvalidInput(
                    "数据关联须发生在 控制活动 与 数据资料节点 之间".to_string(),
                ));
            }
        }
        other => return Err(AppError::InvalidInput(format!("未知连线类型: {other}"))),
    }
    let conn = db.conn()?;
    // 同向同类型重复连线拒绝
    let dup: i64 = conn.query_row(
        "SELECT COUNT(*) FROM ic_edges
         WHERE project_id = ?1 AND from_node = ?2 AND to_node = ?3 AND edge_kind = ?4",
        params![project_id, from_node, to_node, edge_kind],
        |r| r.get(0),
    )?;
    if dup > 0 {
        return Err(AppError::InvalidInput("连线已存在".to_string()));
    }
    conn.execute(
        "INSERT INTO ic_edges(project_id, from_node, to_node, edge_kind) VALUES (?1, ?2, ?3, ?4)",
        params![project_id, from_node, to_node, edge_kind],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn remove_edge(db: &Db, project_id: i64, edge_id: i64) -> Result<(), AppError> {
    let conn = db.conn()?;
    let n = conn.execute(
        "DELETE FROM ic_edges WHERE id = ?1 AND project_id = ?2",
        params![edge_id, project_id],
    )?;
    if n == 0 {
        return Err(AppError::NotFound(format!("连线不存在: {edge_id}")));
    }
    Ok(())
}

pub fn list_edges(db: &Db, project_id: i64) -> Result<Vec<IcEdge>, AppError> {
    projects::get_project(db, project_id)?;
    let conn = db.conn()?;
    let mut stmt = conn.prepare(
        "SELECT id, project_id, from_node, to_node, edge_kind
         FROM ic_edges WHERE project_id = ?1 ORDER BY id",
    )?;
    let rows = stmt
        .query_map(params![project_id], row_to_edge)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct IcGroup {
    pub id: i64,
    pub project_id: i64,
    /// it(IT环境)|manual(人工环境)
    pub env_type: String,
    /// 画布包围矩形 json。
    pub rect: String,
}

fn row_to_group(row: &rusqlite::Row<'_>) -> rusqlite::Result<IcGroup> {
    Ok(IcGroup {
        id: row.get("id")?,
        project_id: row.get("project_id")?,
        env_type: row.get("env_type")?,
        rect: row.get("rect")?,
    })
}

/// 校验画布包围矩形 json：[x0,y0,x1,y1] 且 x0<x1、y0<y1（画布坐标，范围不限）。
fn ensure_valid_bbox(rect: &str) -> Result<(), AppError> {
    let v: Vec<f64> = serde_json::from_str(rect)
        .map_err(|_| AppError::InvalidInput(format!("矩形坐标格式非法: {rect}")))?;
    if v.len() != 4 || v[0] >= v[2] || v[1] >= v[3] {
        return Err(AppError::InvalidInput(format!(
            "矩形须为 [x0,y0,x1,y1] 且 x0<x1、y0<y1: {rect}"
        )));
    }
    Ok(())
}

/// 创建环境组：把指定节点归入新组（节点原所属组被替换）。
pub fn create_group(
    db: &Db,
    project_id: i64,
    env_type: &str,
    rect: &str,
    node_ids: &[i64],
) -> Result<i64, AppError> {
    projects::get_project(db, project_id)?;
    if !matches!(env_type, "it" | "manual") {
        return Err(AppError::InvalidInput(format!(
            "环境组类型须为 it(IT环境) 或 manual(人工环境): {env_type}"
        )));
    }
    ensure_valid_bbox(rect)?;
    if node_ids.is_empty() {
        return Err(AppError::InvalidInput("环境组至少包含一个节点".to_string()));
    }
    for nid in node_ids {
        node_type_in_project(db, project_id, *nid)?;
    }
    let conn = db.conn()?;
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "INSERT INTO ic_groups(project_id, env_type, rect) VALUES (?1, ?2, ?3)",
        params![project_id, env_type, rect],
    )?;
    let gid = tx.last_insert_rowid();
    for nid in node_ids {
        tx.execute(
            "UPDATE ic_nodes SET group_id = ?2 WHERE id = ?1",
            params![nid, gid],
        )?;
    }
    tx.commit()?;
    Ok(gid)
}

/// 删除环境组：组内节点 group_id 清除（节点本身保留）。
pub fn delete_group(db: &Db, project_id: i64, group_id: i64) -> Result<(), AppError> {
    let conn = db.conn()?;
    let tx = conn.unchecked_transaction()?;
    let n = tx.execute(
        "DELETE FROM ic_groups WHERE id = ?1 AND project_id = ?2",
        params![group_id, project_id],
    )?;
    if n == 0 {
        return Err(AppError::NotFound(format!("环境组不存在: {group_id}")));
    }
    tx.execute(
        "UPDATE ic_nodes SET group_id = NULL WHERE group_id = ?1",
        params![group_id],
    )?;
    tx.commit()?;
    Ok(())
}

/// 更新环境组包围矩形（节点拖动后由前端重算回写）。
pub fn update_group_rect(
    db: &Db,
    project_id: i64,
    group_id: i64,
    rect: &str,
) -> Result<(), AppError> {
    ensure_valid_bbox(rect)?;
    let conn = db.conn()?;
    let n = conn.execute(
        "UPDATE ic_groups SET rect = ?3 WHERE id = ?1 AND project_id = ?2",
        params![group_id, project_id, rect],
    )?;
    if n == 0 {
        return Err(AppError::NotFound(format!("环境组不存在: {group_id}")));
    }
    Ok(())
}

pub fn list_groups(db: &Db, project_id: i64) -> Result<Vec<IcGroup>, AppError> {
    projects::get_project(db, project_id)?;
    let conn = db.conn()?;
    let mut stmt = conn.prepare(
        "SELECT id, project_id, env_type, rect FROM ic_groups WHERE project_id = ?1 ORDER BY id",
    )?;
    let rows = stmt
        .query_map(params![project_id], row_to_group)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (tempfile::TempDir, Db, projects::Project) {
        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open_in_memory().unwrap();
        let p = projects::create_project_typed(
            &db,
            tmp.path().to_str().unwrap(),
            "内控",
            "组A",
            "audit",
        )
        .unwrap();
        (tmp, db, p)
    }

    #[test]
    fn import_docs_collects_nested_pdfs_into_blob_and_is_idempotent() {
        let (tmp, db, p) = setup();
        let src = tmp.path().join("src");
        std::fs::create_dir_all(src.join("子目录/深层")).unwrap();
        std::fs::write(src.join("a.pdf"), b"%PDF-1.7 a").unwrap();
        std::fs::write(src.join("子目录/b.PDF"), b"%PDF-1.7 b").unwrap();
        std::fs::write(src.join("子目录/深层/c.pdf"), b"%PDF-1.7 c").unwrap();
        std::fs::write(src.join("忽略.txt"), b"x").unwrap();

        let r = import_docs(&db, p.id, src.to_str().unwrap()).unwrap();
        assert_eq!((r.imported, r.updated), (3, 0));

        let docs = list_docs(&db, p.id).unwrap();
        assert_eq!(docs.len(), 3);
        let rels: Vec<&str> = docs.iter().map(|d| d.rel_path.as_str()).collect();
        assert_eq!(rels, ["a.pdf", "子目录/b.PDF", "子目录/深层/c.pdf"]);
        // 字节直接入库（不落本地目录；内控任务本身也无项目目录）
        assert!(docs.iter().all(|d| d.file_path.is_empty()));
        assert_eq!(docs[0].size, 10);
        assert_eq!(doc_bytes(&db, p.id, docs[0].id).unwrap(), b"%PDF-1.7 a");
        assert_eq!(doc_bytes(&db, p.id, docs[1].id).unwrap(), b"%PDF-1.7 b");
        // 项目目录不存在（audit 不落盘）
        assert!(!p.full_path().exists());

        // 重复导入：同 rel_path 覆盖更新，不产生重复行
        std::fs::write(src.join("a.pdf"), b"%PDF-1.7 a2").unwrap();
        let r2 = import_docs(&db, p.id, src.to_str().unwrap()).unwrap();
        assert_eq!((r2.imported, r2.updated), (0, 3));
        let docs2 = list_docs(&db, p.id).unwrap();
        assert_eq!(docs2.len(), 3);
        assert_eq!(doc_bytes(&db, p.id, docs2[0].id).unwrap(), b"%PDF-1.7 a2");
    }

    #[test]
    fn delete_doc_removes_row_keeps_nodes_and_links() {
        let (tmp, db, p) = setup();
        let (a, b) = fixture_two_docs(&db, &p, tmp.path());
        let link = add_doc_link(&db, p.id, a, 1, "[0,0,10,10]", b).unwrap();
        let mut input = node_input("ACT", None);
        input.doc_id = Some(a);
        input.page = Some(1);
        input.rect = Some("[0,0,10,10]".into());
        let node = create_node(&db, p.id, &input).unwrap();

        delete_doc(&db, p.id, a).unwrap();
        assert!(matches!(
            doc_bytes(&db, p.id, a),
            Err(AppError::NotFound(_))
        ));
        assert_eq!(list_docs(&db, p.id).unwrap().len(), 1);
        // 流程图节点与索引行保留（跳转在运行时判定失效）
        assert_eq!(list_nodes(&db, p.id).unwrap().len(), 1);
        assert_eq!(list_nodes(&db, p.id).unwrap()[0].id, node.id);
        assert_eq!(list_doc_links(&db, p.id).unwrap().len(), 1);
        assert_eq!(list_doc_links(&db, p.id).unwrap()[0].id, link);
        // 重复删除报错
        assert!(matches!(
            delete_doc(&db, p.id, a),
            Err(AppError::NotFound(_))
        ));
    }

    #[test]
    fn import_docs_rejects_empty_dir() {
        let (tmp, db, p) = setup();
        let empty = tmp.path().join("empty");
        std::fs::create_dir_all(&empty).unwrap();
        assert!(matches!(
            import_docs(&db, p.id, empty.to_str().unwrap()),
            Err(AppError::Validation(_))
        ));
        assert!(matches!(
            import_docs(&db, p.id, "/nonexistent/xyz"),
            Err(AppError::InvalidInput(_))
        ));
    }

    /// 用 rust_xlsxwriter 造测试 xlsx。
    fn write_xlsx(path: &Path, rows: &[&[&str]]) {
        let mut wb = rust_xlsxwriter::Workbook::new();
        let ws = wb.add_worksheet();
        for (i, row) in rows.iter().enumerate() {
            for (j, v) in row.iter().enumerate() {
                ws.write_string(i as u32, j as u16, *v).unwrap();
            }
        }
        wb.save(path).unwrap();
    }

    #[test]
    fn import_roster_parses_and_replaces() {
        let (tmp, db, p) = setup();
        let xlsx = tmp.path().join("roster.xlsx");
        write_xlsx(
            &xlsx,
            &[
                &[
                    "姓名",
                    "部门组织",
                    "岗位",
                    "岗位职责描述",
                    "上级",
                    "下级",
                    "备注",
                ],
                &["张三", "财务部", "会计", "记账", "李四", "", "备注1"],
                &["李四", "财务部", "经理", "审批", "", "张三", ""],
            ],
        );
        let n = import_roster(&db, p.id, xlsx.to_str().unwrap()).unwrap();
        assert_eq!(n, 2);
        let rows = list_roster(&db, p.id).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].name.as_deref(), Some("张三"));
        assert_eq!(rows[0].subordinate, None, "空单元格应为 None");
        assert_eq!(rows[1].superior, None);

        // 再导入整体替换
        write_xlsx(
            &xlsx,
            &[
                &[
                    "姓名",
                    "部门组织",
                    "岗位",
                    "岗位职责描述",
                    "上级",
                    "下级",
                    "备注",
                ],
                &["王五", "IT", "运维", "值班", "", "", ""],
            ],
        );
        let n2 = import_roster(&db, p.id, xlsx.to_str().unwrap()).unwrap();
        assert_eq!(n2, 1);
        let rows2 = list_roster(&db, p.id).unwrap();
        assert_eq!(rows2.len(), 1);
        assert_eq!(rows2[0].name.as_deref(), Some("王五"));
    }

    #[test]
    fn import_roster_rejects_bad_header() {
        let (tmp, db, p) = setup();
        let xlsx = tmp.path().join("bad.xlsx");
        write_xlsx(&xlsx, &[&["名字", "部门"], &["张三", "财务部"]]);
        let err = import_roster(&db, p.id, xlsx.to_str().unwrap()).unwrap_err();
        assert!(matches!(err, AppError::Validation(_)));
        assert!(err.to_string().contains("姓名"), "错误应含期望表头: {err}");
    }

    #[test]
    fn import_org_parses_and_replaces() {
        let (tmp, db, p) = setup();
        let xlsx = tmp.path().join("org.xlsx");
        write_xlsx(
            &xlsx,
            &[
                &["部门组织", "部门职责描述", "上级", "下级", "备注"],
                &["财务部", "资金管理", "总经理室", "会计科", ""],
            ],
        );
        let n = import_org(&db, p.id, xlsx.to_str().unwrap()).unwrap();
        assert_eq!(n, 1);
        let rows = list_org(&db, p.id).unwrap();
        assert_eq!(rows[0].dept.as_deref(), Some("财务部"));
        assert_eq!(rows[0].remark, None);
    }

    #[test]
    fn template_roundtrips_through_import() {
        let (tmp, db, p) = setup();
        // 名册模版：写出 → 直接导入应成功（示例行入库）
        let roster = tmp.path().join("roster_tpl.xlsx");
        write_template("roster", roster.to_str().unwrap()).unwrap();
        let n = import_roster(&db, p.id, roster.to_str().unwrap()).unwrap();
        assert_eq!(n, 1, "模版示例行应可导入");
        assert_eq!(
            list_roster(&db, p.id).unwrap()[0].name.as_deref(),
            Some("张三")
        );
        // 组织模版同理
        let org = tmp.path().join("org_tpl.xlsx");
        write_template("org", org.to_str().unwrap()).unwrap();
        let n2 = import_org(&db, p.id, org.to_str().unwrap()).unwrap();
        assert_eq!(n2, 1);
        // 非法模版类型拒绝
        assert!(matches!(
            write_template("bad", tmp.path().join("x.xlsx").to_str().unwrap()),
            Err(AppError::InvalidInput(_))
        ));
    }

    // ---- 制度索引 ----

    /// 造两个制度文件，返回 (doc1, doc2) id。
    fn fixture_two_docs(db: &Db, p: &projects::Project, tmp: &Path) -> (i64, i64) {
        let src = tmp.join("docs_src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("a.pdf"), b"%PDF-1.7 a").unwrap();
        std::fs::write(src.join("b.pdf"), b"%PDF-1.7 b").unwrap();
        import_docs(db, p.id, src.to_str().unwrap()).unwrap();
        let docs = list_docs(db, p.id).unwrap();
        (docs[0].id, docs[1].id)
    }

    #[test]
    fn doc_link_add_list_remove() {
        let (tmp, db, p) = setup();
        let (a, b) = fixture_two_docs(&db, &p, tmp.path());
        let id = add_doc_link(&db, p.id, a, 2, "[100,100,300,200]", b).unwrap();
        let links = list_doc_links(&db, p.id).unwrap();
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].id, id);
        assert_eq!(links[0].src_doc_id, a);
        assert_eq!(links[0].target_doc_id, b);
        assert_eq!(links[0].page, 2);
        remove_doc_link(&db, p.id, id).unwrap();
        assert!(list_doc_links(&db, p.id).unwrap().is_empty());
        assert!(matches!(
            remove_doc_link(&db, p.id, id),
            Err(AppError::NotFound(_))
        ));
    }

    #[test]
    fn doc_link_validates_rect_and_docs() {
        let (tmp, db, p) = setup();
        let (a, b) = fixture_two_docs(&db, &p, tmp.path());
        // 非法 rect
        for bad in ["[300,100,100,200]", "[0,0,1001,5]", "not-json", "[1,2,3]"] {
            assert!(
                matches!(
                    add_doc_link(&db, p.id, a, 1, bad, b),
                    Err(AppError::InvalidInput(_))
                ),
                "应拒绝 rect {bad}"
            );
        }
        // 页码从 1 开始
        assert!(matches!(
            add_doc_link(&db, p.id, a, 0, "[0,0,10,10]", b),
            Err(AppError::InvalidInput(_))
        ));
        // 不存在的 doc
        assert!(matches!(
            add_doc_link(&db, p.id, 9999, 1, "[0,0,10,10]", b),
            Err(AppError::NotFound(_))
        ));
    }

    // ---- 流程节点 ----

    fn node_input(node_type: &str, subtype: Option<&str>) -> IcNodeInput {
        IcNodeInput {
            node_type: node_type.to_string(),
            subtype: subtype.map(|s| s.to_string()),
            name: None,
            content: None,
            doc_id: None,
            page: None,
            rect: None,
            canvas_x: None,
            canvas_y: None,
            group_id: None,
            props: None,
        }
    }

    #[test]
    fn node_create_allocates_unique_codes_never_reused() {
        let (tmp, db, p) = setup();
        let (a, _b) = fixture_two_docs(&db, &p, tmp.path());
        let mut in1 = node_input("ACT", None);
        in1.name = Some("审批".into());
        in1.doc_id = Some(a);
        in1.page = Some(1);
        in1.rect = Some("[10,20,100,60]".into());
        let n1 = create_node(&db, p.id, &in1).unwrap();
        assert_eq!(n1.code, "ACT-1");
        assert_eq!(n1.doc_id, Some(a));
        let n2 = create_node(&db, p.id, &node_input("ACT", None)).unwrap();
        assert_eq!(n2.code, "ACT-2");
        let e1 = create_node(&db, p.id, &node_input("EVT", Some("time"))).unwrap();
        assert_eq!(e1.code, "EVT-1");
        // 删除 n2 后编号不复用
        delete_node(&db, p.id, n2.id).unwrap();
        let n3 = create_node(&db, p.id, &node_input("ACT", None)).unwrap();
        assert_eq!(n3.code, "ACT-3", "删除后编号不复用");
        // 删除不存在的节点
        assert!(matches!(
            delete_node(&db, p.id, 9999),
            Err(AppError::NotFound(_))
        ));
    }

    #[test]
    fn node_type_subtype_validation() {
        let (_tmp, db, p) = setup();
        // 合法组合
        for (t, s) in [
            ("ACT", None),
            ("EVT", Some("time")),
            ("EVT", Some("message")),
            ("ROL", Some("person")),
            ("DAT", Some("record")),
            ("LOG", None),
        ] {
            create_node(&db, p.id, &node_input(t, s)).unwrap();
        }
        // 非法类型/子类型
        assert!(create_node(&db, p.id, &node_input("XXX", None)).is_err());
        assert!(create_node(&db, p.id, &node_input("ACT", Some("time"))).is_err());
        assert!(create_node(&db, p.id, &node_input("EVT", Some("person"))).is_err());
        // 来源 pdf 三项不齐
        let mut bad = node_input("ACT", None);
        bad.doc_id = Some(1);
        assert!(matches!(
            create_node(&db, p.id, &bad),
            Err(AppError::InvalidInput(_))
        ));
        // props 非 json 对象
        let mut bad2 = node_input("ACT", None);
        bad2.props = Some("[1,2]".into());
        assert!(matches!(
            create_node(&db, p.id, &bad2),
            Err(AppError::InvalidInput(_))
        ));
    }

    #[test]
    fn node_update_patch_and_group_clear() {
        let (_tmp, db, p) = setup();
        let n = create_node(&db, p.id, &node_input("ACT", None)).unwrap();
        let updated = update_node(
            &db,
            p.id,
            n.id,
            &IcNodePatch {
                name: Some("新名".into()),
                content: Some("内容".into()),
                canvas_x: Some(12.5),
                canvas_y: None,
                canvas_w: None,
                canvas_h: None,
                group_id: Some(Some(7)),
                props: Some(r#"{"is_key":"Y"}"#.into()),
            },
        )
        .unwrap();
        assert_eq!(updated.name, "新名");
        assert_eq!(updated.canvas_x, 12.5);
        assert_eq!(updated.canvas_y, 0.0, "未提供的列不动");
        assert_eq!(updated.group_id, Some(7));
        // 清除组归属
        let cleared = update_node(
            &db,
            p.id,
            n.id,
            &IcNodePatch {
                name: None,
                content: None,
                canvas_x: None,
                canvas_y: None,
                canvas_w: None,
                canvas_h: None,
                group_id: Some(None),
                props: None,
            },
        )
        .unwrap();
        assert_eq!(cleared.group_id, None);
        // 他项目节点拒绝
        assert!(matches!(
            update_node(
                &db,
                p.id,
                9999,
                &IcNodePatch {
                    name: None,
                    content: None,
                    canvas_x: None,
                    canvas_y: None,
                    canvas_w: None,
                    canvas_h: None,
                    group_id: None,
                    props: None,
                }
            ),
            Err(AppError::NotFound(_))
        ));
    }

    // ---- 连线与环境组 ----

    #[test]
    fn edge_validation_rules() {
        let (_tmp, db, p) = setup();
        let act = create_node(&db, p.id, &node_input("ACT", None)).unwrap();
        let evt = create_node(&db, p.id, &node_input("EVT", Some("time"))).unwrap();
        let log = create_node(&db, p.id, &node_input("LOG", None)).unwrap();
        let rol = create_node(&db, p.id, &node_input("ROL", Some("person"))).unwrap();
        let dat = create_node(&db, p.id, &node_input("DAT", Some("paper"))).unwrap();

        // flow：任意两类节点均可连
        add_edge(&db, p.id, act.id, evt.id, "flow").unwrap();
        add_edge(&db, p.id, evt.id, log.id, "flow").unwrap();
        add_edge(&db, p.id, rol.id, dat.id, "flow").unwrap();
        add_edge(&db, p.id, dat.id, rol.id, "flow").unwrap();
        // role：ACT↔ROL 双向均可，其他组合拒绝
        add_edge(&db, p.id, act.id, rol.id, "role").unwrap();
        assert!(add_edge(&db, p.id, evt.id, rol.id, "role").is_err());
        assert!(add_edge(&db, p.id, act.id, dat.id, "role").is_err());
        // data：ACT↔DAT 双向均可，其他组合拒绝
        add_edge(&db, p.id, dat.id, act.id, "data").unwrap();
        assert!(add_edge(&db, p.id, evt.id, dat.id, "data").is_err());
        // 重复连线拒绝；自连拒绝；未知类型拒绝
        assert!(add_edge(&db, p.id, act.id, evt.id, "flow").is_err());
        assert!(add_edge(&db, p.id, act.id, act.id, "flow").is_err());
        assert!(add_edge(&db, p.id, act.id, evt.id, "other").is_err());

        let edges = list_edges(&db, p.id).unwrap();
        assert_eq!(edges.len(), 6);
        // 删节点级联清边
        delete_node(&db, p.id, act.id).unwrap();
        let edges2 = list_edges(&db, p.id).unwrap();
        assert_eq!(edges2.len(), 3, "ACT 相关的 3 条被级联清除");
        assert!(edges2
            .iter()
            .all(|e| e.from_node != act.id && e.to_node != act.id));
    }

    #[test]
    fn group_create_and_dissolve() {
        let (_tmp, db, p) = setup();
        let n1 = create_node(&db, p.id, &node_input("ACT", None)).unwrap();
        let n2 = create_node(&db, p.id, &node_input("EVT", Some("time"))).unwrap();
        let gid = create_group(&db, p.id, "it", "[0,0,500,300]", &[n1.id, n2.id]).unwrap();
        let nodes = list_nodes(&db, p.id).unwrap();
        assert!(nodes.iter().all(|n| n.group_id == Some(gid)));
        // 非法类型 / 空节点集
        assert!(create_group(&db, p.id, "cloud", "[0,0,9,9]", &[n1.id]).is_err());
        assert!(create_group(&db, p.id, "it", "[0,0,9,9]", &[]).is_err());
        // 解散：节点保留、组归属清除
        delete_group(&db, p.id, gid).unwrap();
        let nodes2 = list_nodes(&db, p.id).unwrap();
        assert_eq!(nodes2.len(), 2);
        assert!(nodes2.iter().all(|n| n.group_id.is_none()));
        assert!(matches!(
            delete_group(&db, p.id, gid),
            Err(AppError::NotFound(_))
        ));
    }
}
