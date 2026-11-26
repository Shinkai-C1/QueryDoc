//! 文件业务逻辑：多文件上传（PDF 校验 + 文件名安全化 + 落盘入库）、项目文件列表。
//! 语义沿用旧版 `backend/apps/upload/views.py`。

use std::io::Read;
use std::path::Path;

use rusqlite::params;
use serde::Serialize;

use crate::db::Db;
use crate::error::AppError;
use crate::paths;
use crate::projects;

#[derive(Debug, Clone, Serialize)]
pub struct UploadBatchInfo {
    pub id: i64,
    pub project_id: i64,
    pub folder_name: String,
    pub uploaded_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SourceFileInfo {
    pub id: i64,
    pub batch_id: i64,
    pub project_id: i64,
    pub folder_name: String,
    pub original_name: String,
    pub stored_name: String,
    pub relative_path: String,
    pub file_size: i64,
    pub status: String,
    /// 预估提取输入 Token 数（进度面板「预估Token」列；NULL=未估算/上次估算失败）
    pub estimated_tokens: Option<i64>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct UploadResult {
    pub batch: UploadBatchInfo,
    pub files: Vec<SourceFileInfo>,
}

/// 校验单个 PDF 文件：扩展名（大小写不敏感）+ 魔数 `%PDF-` + 大小上限。
/// 语义同旧版 `validate_pdf_file`（离线版无客户端 MIME，改由魔数兜底）。
pub fn validate_pdf_file(path: &Path, max_size: u64) -> Result<(), AppError> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| path.display().to_string());

    let is_pdf_ext = path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("pdf"));
    if !is_pdf_ext {
        return Err(AppError::Validation(format!("仅支持 PDF 文件: {name}")));
    }

    let meta = std::fs::metadata(path)
        .map_err(|e| AppError::Validation(format!("无法读取文件: {name} ({e})")))?;
    if !meta.is_file() {
        return Err(AppError::Validation(format!("不是普通文件: {name}")));
    }
    if meta.len() > max_size {
        return Err(AppError::Validation(format!(
            "文件超过 {} MB 限制: {name} ({} 字节)",
            max_size / 1024 / 1024,
            meta.len()
        )));
    }

    let mut file = std::fs::File::open(path)?;
    let mut header = [0u8; 5];
    if file.read_exact(&mut header).is_err() || header != *b"%PDF-" {
        return Err(AppError::Validation(format!("文件内容不是 PDF: {name}")));
    }
    Ok(())
}

/// 上传文件：逐文件校验（任一失败整批拒绝）→ 建批次目录
/// `0-原始文件集/<YYYYMMDDHHMMSS>/` → 安全化文件名复制 → 写 source_files。
pub fn upload_files(db: &Db, project_id: i64, paths: &[String]) -> Result<UploadResult, AppError> {
    let project = projects::get_project(db, project_id)?;
    if paths.is_empty() {
        return Err(AppError::InvalidInput("未上传文件".to_string()));
    }

    // 先全量校验，任何失败整批不入库（同旧版行为）
    let mut errors = Vec::new();
    for p in paths {
        if let Err(e) = validate_pdf_file(Path::new(p), paths::MAX_UPLOAD_SIZE) {
            errors.push(e.to_string());
        }
    }
    if !errors.is_empty() {
        return Err(AppError::Validation(errors.join("；")));
    }

    // 文件名唯一性校验：导出产物以文件名命名 PDF，重名会冲突——
    // 批内不得重名，且不得与项目内已有文件重名
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for p in paths {
        let name = Path::new(p)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "unnamed.pdf".to_string());
        if !seen.insert(name.clone()) {
            return Err(AppError::Validation(format!(
                "本次上传包含重名文件: {name}"
            )));
        }
    }
    {
        let conn = db.conn()?;
        for name in &seen {
            let n: i64 = conn.query_row(
                "SELECT COUNT(*) FROM source_files sf
                 JOIN upload_batches b ON b.id = sf.batch_id
                 WHERE b.project_id = ?1 AND sf.original_name = ?2",
                params![project_id, name],
                |r| r.get(0),
            )?;
            if n > 0 {
                return Err(AppError::Validation(format!(
                    "项目内已存在同名文件，请先删除后再上传: {name}"
                )));
            }
        }
    }

    let project_root = project.full_path();
    let raw_dir = paths::ensure_under_project(&project_root, Path::new(paths::SUBDIR_RAW))?;
    std::fs::create_dir_all(&raw_dir)?;
    let folder_name = paths::unique_child_name(&raw_dir, &paths::now_compact());
    let upload_dir = paths::ensure_under_project(
        &project_root,
        Path::new(paths::SUBDIR_RAW).join(&folder_name).as_path(),
    )?;
    std::fs::create_dir_all(&upload_dir)?;

    let uploaded_at = paths::now_display();
    let conn = db.conn()?;
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "INSERT INTO upload_batches(project_id, folder_name, uploaded_at) VALUES (?1, ?2, ?3)",
        params![project_id, folder_name, uploaded_at],
    )?;
    let batch_id = tx.last_insert_rowid();

    let mut files = Vec::with_capacity(paths.len());
    for src in paths {
        let src_path = Path::new(src);
        let original_name = src_path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "unnamed.pdf".to_string());
        let (stem, ext) = paths::split_filename(&original_name);
        let safe_stem = paths::safe_filename(&stem, 100);
        let stored_name = paths::unique_file_name(&upload_dir, &safe_stem, &ext);

        let dest = upload_dir.join(&stored_name);
        std::fs::copy(src_path, &dest)?;
        let file_size = std::fs::metadata(&dest)?.len() as i64;
        let relative_path = format!("{}/{folder_name}/{stored_name}", paths::SUBDIR_RAW);
        let created_at = paths::now_display();

        tx.execute(
            "INSERT INTO source_files(batch_id, original_name, stored_name, relative_path,
                                     file_size, status, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, 'pending', ?6)",
            params![
                batch_id,
                original_name,
                stored_name,
                relative_path,
                file_size,
                created_at
            ],
        )?;
        files.push(SourceFileInfo {
            id: tx.last_insert_rowid(),
            batch_id,
            project_id,
            folder_name: folder_name.clone(),
            original_name,
            stored_name,
            relative_path,
            file_size,
            status: "pending".to_string(),
            estimated_tokens: None,
            created_at,
        });
    }
    tx.commit()?;

    Ok(UploadResult {
        batch: UploadBatchInfo {
            id: batch_id,
            project_id,
            folder_name,
            uploaded_at,
        },
        files,
    })
}

/// 项目全部文件（含批次信息），按创建时间倒序。
pub fn list_files(db: &Db, project_id: i64) -> Result<Vec<SourceFileInfo>, AppError> {
    projects::get_project(db, project_id)?;
    let conn = db.conn()?;
    let mut stmt = conn.prepare(
        "SELECT sf.id, sf.batch_id, b.project_id, b.folder_name, sf.original_name,
                sf.stored_name, sf.relative_path, sf.file_size, sf.status,
                sf.estimated_tokens, sf.created_at
         FROM source_files sf
         JOIN upload_batches b ON b.id = sf.batch_id
         WHERE b.project_id = ?1
         ORDER BY sf.created_at DESC, sf.id DESC",
    )?;
    let rows = stmt
        .query_map(params![project_id], |row| {
            Ok(SourceFileInfo {
                id: row.get("id")?,
                batch_id: row.get("batch_id")?,
                project_id: row.get("project_id")?,
                folder_name: row.get("folder_name")?,
                original_name: row.get("original_name")?,
                stored_name: row.get("stored_name")?,
                relative_path: row.get("relative_path")?,
                file_size: row.get("file_size")?,
                status: row.get("status")?,
                estimated_tokens: row.get("estimated_tokens")?,
                created_at: row.get("created_at")?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PDF_BYTES: &[u8] = b"%PDF-1.7\n%%fake-for-test\n";

    fn setup() -> (tempfile::TempDir, Db, projects::Project) {
        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open_in_memory().unwrap();
        let p = projects::create_project(&db, tmp.path().to_str().unwrap(), "文件项目", "测试组")
            .unwrap();
        (tmp, db, p)
    }

    fn make_pdf(dir: &Path, name: &str) -> String {
        let path = dir.join(name);
        std::fs::write(&path, PDF_BYTES).unwrap();
        path.to_str().unwrap().to_string()
    }

    #[test]
    fn validate_pdf_accepts_real_pdf_case_insensitive_ext() {
        let tmp = tempfile::tempdir().unwrap();
        let upper = make_pdf(tmp.path(), "A.PDF");
        validate_pdf_file(Path::new(&upper), paths::MAX_UPLOAD_SIZE).unwrap();
    }

    #[test]
    fn validate_pdf_rejects_bad_ext_magic_and_size() {
        let tmp = tempfile::tempdir().unwrap();
        // 非 pdf 扩展名
        let txt = tmp.path().join("a.txt");
        std::fs::write(&txt, PDF_BYTES).unwrap();
        let e = validate_pdf_file(&txt, paths::MAX_UPLOAD_SIZE).unwrap_err();
        assert!(e.to_string().contains("仅支持 PDF"), "{e}");

        // 魔数不对
        let fake = tmp.path().join("fake.pdf");
        std::fs::write(&fake, b"not-a-pdf").unwrap();
        let e = validate_pdf_file(&fake, paths::MAX_UPLOAD_SIZE).unwrap_err();
        assert!(e.to_string().contains("不是 PDF"), "{e}");

        // 超限（用小上限模拟）
        let big = make_pdf(tmp.path(), "big.pdf");
        let e = validate_pdf_file(Path::new(&big), 4).unwrap_err();
        assert!(e.to_string().contains("限制"), "{e}");
    }

    #[test]
    fn upload_files_copies_and_records() {
        let (tmp, db, p) = setup();
        let src_dir = tmp.path().join("src");
        std::fs::create_dir_all(&src_dir).unwrap();
        let f1 = make_pdf(&src_dir, "合同 （终版）.pdf");
        // Windows 不允许文件名含控制字符（无法创建源文件），该平台仅测 ".." 净化
        let dangerous = if cfg!(windows) {
            "..奇怪.pdf"
        } else {
            "..\u{7}奇怪.pdf"
        };
        let f2 = make_pdf(&src_dir, dangerous);

        let result = upload_files(&db, p.id, &[f1, f2]).unwrap();
        assert_eq!(result.files.len(), 2);

        // 批次目录在项目 0-原始文件集 下
        let batch_dir = p
            .full_path()
            .join(paths::SUBDIR_RAW)
            .join(&result.batch.folder_name);
        assert!(batch_dir.is_dir());

        let f0 = &result.files[0];
        assert_eq!(f0.original_name, "合同 （终版）.pdf");
        assert_eq!(f0.stored_name, "合同_终版.pdf");
        assert_eq!(f0.status, "pending");
        assert_eq!(f0.file_size, PDF_BYTES.len() as i64);
        assert!(batch_dir.join(&f0.stored_name).exists());
        assert_eq!(
            f0.relative_path,
            format!(
                "{}/{}/{}",
                paths::SUBDIR_RAW,
                result.batch.folder_name,
                f0.stored_name
            )
        );

        // 危险字符被安全化
        let f1 = &result.files[1];
        assert!(!f1.stored_name.contains(".."));
        assert!(!f1.stored_name.chars().any(|c| c.is_control()));
        assert!(batch_dir.join(&f1.stored_name).exists());
    }

    #[test]
    fn upload_files_rejects_same_name_in_one_batch() {
        let (tmp, db, p) = setup();
        let d1 = tmp.path().join("d1");
        let d2 = tmp.path().join("d2");
        std::fs::create_dir_all(&d1).unwrap();
        std::fs::create_dir_all(&d2).unwrap();
        let f1 = make_pdf(&d1, "同名.pdf");
        let f2 = make_pdf(&d2, "同名.pdf");

        let e = upload_files(&db, p.id, &[f1, f2]).unwrap_err();
        assert!(e.to_string().contains("重名文件"), "{e}");
        assert!(list_files(&db, p.id).unwrap().is_empty(), "整批应被拒绝");
    }

    #[test]
    fn upload_files_rejects_name_existing_in_project() {
        let (tmp, db, p) = setup();
        let d1 = tmp.path().join("d1");
        let d2 = tmp.path().join("d2");
        std::fs::create_dir_all(&d1).unwrap();
        std::fs::create_dir_all(&d2).unwrap();
        let f1 = make_pdf(&d1, "合同.pdf");
        upload_files(&db, p.id, &[f1]).unwrap();

        // 不同目录、同名文件再次上传 → 拒绝
        let f2 = make_pdf(&d2, "合同.pdf");
        let e = upload_files(&db, p.id, &[f2]).unwrap_err();
        assert!(e.to_string().contains("同名文件"), "{e}");
        assert_eq!(list_files(&db, p.id).unwrap().len(), 1);
    }

    #[test]
    fn upload_files_rejects_invalid_batch_atomically() {
        let (tmp, db, p) = setup();
        let src_dir = tmp.path().join("src");
        std::fs::create_dir_all(&src_dir).unwrap();
        let good = make_pdf(&src_dir, "good.pdf");
        let bad = src_dir.join("bad.pdf");
        std::fs::write(&bad, b"garbage").unwrap();

        let e = upload_files(&db, p.id, &[good, bad.to_str().unwrap().to_string()]).unwrap_err();
        assert!(matches!(e, AppError::Validation(_)), "{e}");
        // 整批拒绝：无批次、无文件记录
        {
            let conn = db.conn().unwrap();
            let batches: i64 = conn
                .query_row("SELECT COUNT(*) FROM upload_batches", [], |r| r.get(0))
                .unwrap();
            assert_eq!(batches, 0);
        }
        assert!(list_files(&db, p.id).unwrap().is_empty());
    }

    #[test]
    fn upload_files_rejects_empty_and_missing_project() {
        let (_tmp, db, p) = setup();
        assert!(matches!(
            upload_files(&db, p.id, &[]),
            Err(AppError::InvalidInput(_))
        ));
        assert!(matches!(
            upload_files(&db, 9999, &["x.pdf".to_string()]),
            Err(AppError::NotFound(_))
        ));
    }

    #[test]
    fn list_files_returns_batch_info_desc() {
        let (tmp, db, p) = setup();
        let src_dir = tmp.path().join("src");
        std::fs::create_dir_all(&src_dir).unwrap();
        let f1 = make_pdf(&src_dir, "a.pdf");
        let f2 = make_pdf(&src_dir, "b.pdf");
        upload_files(&db, p.id, &[f1, f2]).unwrap();

        let files = list_files(&db, p.id).unwrap();
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].original_name, "b.pdf");
        assert_eq!(files[1].original_name, "a.pdf");
        assert!(!files[0].folder_name.is_empty());
        assert_eq!(files[0].project_id, p.id);
    }
}
