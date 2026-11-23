//! 路径与文件名工具：项目标准子目录、路径安全、文件名安全化、时间戳。
//! 语义沿用旧版 `backend/core/utils.py`（safe_filename / ensure_safe_path）。

use std::path::{Component, Path, PathBuf};

use crate::error::AppError;

/// 项目内标准子目录（带序号前缀，便于用户本地浏览时排序）。
/// 任务日志写任务目录根的 `任务日志.log`（见 task_log.rs），无独立 log 目录。
pub const SUBDIR_RAW: &str = "0-原始文件集";
pub const SUBDIR_PARSE: &str = "1-解析结果集";
pub const SUBDIR_EXTRACT: &str = "2-提取结果集";
pub const SUBDIR_EXPORT: &str = "3-导出结果集";

pub const PROJECT_SUBDIRS: [&str; 4] = [SUBDIR_RAW, SUBDIR_PARSE, SUBDIR_EXTRACT, SUBDIR_EXPORT];

/// 上传文件大小限制 100 MB。
pub const MAX_UPLOAD_SIZE: u64 = 100 * 1024 * 1024;

/// 数据库文本时间戳：`YYYY-MM-DD HH:MM:SS`（本地时间，字典序即可排序）。
pub fn now_display() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

/// 目录名时间戳：`YYYYMMDDHHMMSS`（本地时间）。
pub fn now_compact() -> String {
    chrono::Local::now().format("%Y%m%d%H%M%S").to_string()
}

/// 分离文件名主体与扩展名（扩展名含 `.`），语义同 Python `split_filename`。
pub fn split_filename(filename: &str) -> (String, String) {
    match filename.rfind('.') {
        Some(idx) => (filename[..idx].to_string(), filename[idx..].to_string()),
        None => (filename.to_string(), String::new()),
    }
}

/// 安全化文件名：保留 Unicode 字母/数字（含中文）、`-`、`_`、`.`，
/// 其余字符的连续段折叠为单个 `_`；去除 `..` 与控制字符；超长时截断主体保留扩展名。
pub fn safe_filename(name: &str, max_length: usize) -> String {
    // basename：剥掉两类路径分隔符前缀，再 trim
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name).trim();

    let mut safe = String::new();
    let mut last_was_sep = false;
    for c in base.chars() {
        if c.is_alphanumeric() || matches!(c, '-' | '_' | '.') {
            safe.push(c);
            last_was_sep = false;
        } else if !last_was_sep {
            safe.push('_');
            last_was_sep = true;
        }
    }
    // Python 端为 strip("._")：去掉首尾的 '.' 与 '_'
    let trimmed = safe.trim_matches(['.', '_']);
    let mut safe = if trimmed.is_empty() {
        "unnamed".to_string()
    } else {
        trimmed.to_string()
    };
    // 防止 `..` 序列（非重叠全局替换，与 Python str.replace 一致）
    safe = safe.replace("..", "_");

    // 超长截断：保留扩展名（按字符数，Python len 即码点数）
    if safe.chars().count() > max_length {
        let (stem, ext) = split_filename(&safe);
        let ext_len = ext.chars().count();
        let keep = max_length.saturating_sub(ext_len);
        let stem: String = stem.chars().take(keep).collect();
        safe = format!("{stem}{ext}");
    }
    safe
}

/// 在项目目录下安全拼接相对路径：拒绝 `..`、绝对路径、Windows 盘符，
/// 已存在的目标再 canonicalize 防符号链接逃逸。等价旧版 `ensure_safe_path`。
pub fn ensure_under_project(project_root: &Path, rel: &Path) -> Result<PathBuf, AppError> {
    for comp in rel.components() {
        match comp {
            Component::Normal(_) | Component::CurDir => {}
            // ParentDir("..") / RootDir("/") / Prefix("C:") 一律拒绝
            other => {
                return Err(AppError::PathTraversal(format!(
                    "{}（非法分量: {}）",
                    rel.display(),
                    other.as_os_str().to_string_lossy()
                )));
            }
        }
    }
    let root = project_root.canonicalize()?;
    let candidate = root.join(rel);
    let resolved = if candidate.exists() {
        candidate.canonicalize()?
    } else {
        candidate
    };
    if resolved != root && !resolved.starts_with(&root) {
        return Err(AppError::PathTraversal(rel.display().to_string()));
    }
    Ok(resolved)
}

/// 目录子项重名时追加 `_1`、`_2` …（用于项目目录名、上传批次目录名）。
pub fn unique_child_name(dir: &Path, desired: &str) -> String {
    if !dir.join(desired).exists() {
        return desired.to_string();
    }
    let mut counter = 1;
    loop {
        let candidate = format!("{desired}_{counter}");
        if !dir.join(&candidate).exists() {
            return candidate;
        }
        counter += 1;
    }
}

/// 文件名重名时追加序号：`stem.ext` → `stem_1.ext` → `stem_2.ext` …
pub fn unique_file_name(dir: &Path, stem: &str, ext: &str) -> String {
    let first = format!("{stem}{ext}");
    if !dir.join(&first).exists() {
        return first;
    }
    let mut counter = 1;
    loop {
        let candidate = format!("{stem}_{counter}{ext}");
        if !dir.join(&candidate).exists() {
            return candidate;
        }
        counter += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_filename_keeps_chinese_and_common_chars() {
        assert_eq!(safe_filename("合同扫描件.pdf", 120), "合同扫描件.pdf");
        assert_eq!(
            safe_filename("report-2024_final.pdf", 120),
            "report-2024_final.pdf"
        );
        assert_eq!(
            safe_filename("报告 （终稿） v2.pdf", 120),
            "报告_终稿_v2.pdf"
        );
    }

    #[test]
    fn safe_filename_strips_path_and_dangerous_chars() {
        // 路径分隔符：取 basename
        assert_eq!(safe_filename("../../etc/passwd", 120), "passwd");
        assert_eq!(safe_filename("..\\..\\evil.pdf", 120), "evil.pdf");
        // 控制字符与空白折叠为单个 _
        assert_eq!(safe_filename("a\u{0}\u{1}b.pdf", 120), "a_b.pdf");
        // 纯危险字符 → unnamed
        assert_eq!(safe_filename("...", 120), "unnamed");
        assert_eq!(safe_filename("   ", 120), "unnamed");
        assert_eq!(safe_filename("", 120), "unnamed");
        // 内部 `..` 序列
        assert_eq!(safe_filename("a..b.pdf", 120), "a_b.pdf");
    }

    #[test]
    fn safe_filename_truncates_keeping_extension() {
        let long = format!("{}.pdf", "合".repeat(200));
        let out = safe_filename(&long, 100);
        assert_eq!(out.chars().count(), 100);
        assert!(out.ends_with(".pdf"));
    }

    #[test]
    fn split_filename_matches_python_semantics() {
        assert_eq!(split_filename("file.pdf"), ("file".into(), ".pdf".into()));
        assert_eq!(split_filename("file"), ("file".into(), "".into()));
        assert_eq!(split_filename(".hidden"), ("".into(), ".hidden".into()));
        assert_eq!(split_filename("a.b.c"), ("a.b".into(), ".c".into()));
    }

    #[test]
    fn ensure_under_project_accepts_normal_relative_paths() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("proj");
        std::fs::create_dir_all(root.join(SUBDIR_RAW)).unwrap();
        let p = ensure_under_project(&root, Path::new(SUBDIR_RAW).join("a.pdf").as_path()).unwrap();
        assert!(p.starts_with(root.canonicalize().unwrap()));
        assert!(p.ends_with("a.pdf"));
    }

    #[test]
    fn ensure_under_project_rejects_traversal() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("proj");
        std::fs::create_dir_all(&root).unwrap();
        for bad in ["../../etc", "../outside.pdf", "/etc/passwd", "a/../../b"] {
            let r = ensure_under_project(&root, Path::new(bad));
            assert!(
                matches!(r, Err(AppError::PathTraversal(_))),
                "应拒绝 {bad}: {r:?}"
            );
        }
    }

    #[test]
    fn unique_names_append_counter() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(unique_child_name(tmp.path(), "批次"), "批次");
        std::fs::create_dir_all(tmp.path().join("批次")).unwrap();
        assert_eq!(unique_child_name(tmp.path(), "批次"), "批次_1");

        assert_eq!(unique_file_name(tmp.path(), "a", ".pdf"), "a.pdf");
        std::fs::write(tmp.path().join("a.pdf"), b"x").unwrap();
        assert_eq!(unique_file_name(tmp.path(), "a", ".pdf"), "a_1.pdf");
        std::fs::write(tmp.path().join("a_1.pdf"), b"x").unwrap();
        assert_eq!(unique_file_name(tmp.path(), "a", ".pdf"), "a_2.pdf");
    }
}
