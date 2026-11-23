//! 任务日志：每任务单文件（`<项目目录>/任务日志.log`）持续追加，
//! 同经 `queue://log` 事件推送前端（进度面板滚动日志区）。
//! 覆盖上传/解析/提取/导出/队列全链路；任何日志失败都不影响主流程。

use serde::Serialize;
use tauri::{AppHandle, Emitter};

use crate::db::Db;
use crate::error::AppError;
use crate::paths;
use crate::projects;

/// 前端监听的事件名。
pub const LOG_EVENT: &str = "queue://log";
/// 任务目录根下的日志文件名（单文件持续写入）。
pub const LOG_FILE_NAME: &str = "任务日志.log";

/// `queue://log` 事件载荷。
#[derive(Debug, Clone, Serialize)]
pub struct LogPayload {
    pub project_id: i64,
    pub line: String,
}

/// 追加一行任务日志：写任务目录日志文件 + 广播前端。
pub fn task_log(app: Option<&AppHandle>, db: &Db, project_id: i64, stage: &str, msg: &str) {
    let line = format!("[{}] [{}] {}", paths::now_display(), stage, msg);
    if let Ok(project) = projects::get_project(db, project_id) {
        use std::io::Write;
        let path = project.full_path().join(LOG_FILE_NAME);
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let _ = writeln!(f, "{line}");
        }
    }
    if let Some(app) = app {
        let _ = app.emit(LOG_EVENT, LogPayload { project_id, line });
    }
}

/// 读取日志尾部的默认字节数（read_project_log 缺省）。
pub const DEFAULT_LOG_TAIL_BYTES: u64 = 64 * 1024;

/// 执行体下沉点：阶段 + 内容。各执行体以闭包/引用形式接收。
pub type LogSink<'a> = &'a (dyn Fn(&str, &str) + Send + Sync);

/// 读取任务日志尾部（最多 max_bytes，按行对齐截断）；无日志返回空串。
pub fn read_project_log(db: &Db, project_id: i64, max_bytes: u64) -> Result<String, AppError> {
    let project = projects::get_project(db, project_id)?;
    let path = project.full_path().join(LOG_FILE_NAME);
    if !path.exists() {
        return Ok(String::new());
    }
    let data = std::fs::read(&path)?;
    let cap = max_bytes.max(1024) as usize;
    let start = if data.len() > cap {
        let mut s = data.len() - cap;
        while s < data.len() && data[s] != b'\n' {
            s += 1;
        }
        (s + 1).min(data.len())
    } else {
        0
    };
    Ok(String::from_utf8_lossy(&data[start..]).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_log_appends_and_reads_tail() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open_in_memory().unwrap();
        let p =
            projects::create_project(&db, tmp.path().to_str().unwrap(), "日志", "测试组").unwrap();
        task_log(None, &db, p.id, "解析", "第一行");
        task_log(None, &db, p.id, "提取", "第二行");
        let text = read_project_log(&db, p.id, 64 * 1024).unwrap();
        assert!(text.contains("[解析] 第一行"), "{text}");
        assert!(text.contains("[提取] 第二行"), "{text}");
        // 文件确实写在任务目录根
        assert!(p.full_path().join(LOG_FILE_NAME).exists());
        // 尾部截断：首行超长（>1KB 下限），小 max_bytes 只留末尾行
        task_log(None, &db, p.id, "提取", &"长".repeat(1500));
        task_log(None, &db, p.id, "提取", "末尾行");
        let tail = read_project_log(&db, p.id, 20).unwrap();
        assert!(tail.contains("末尾行"), "{tail}");
        assert!(!tail.contains("第一行"), "{tail}");
        // 无日志的项目返回空串
        let p2 =
            projects::create_project(&db, tmp.path().to_str().unwrap(), "空", "测试组").unwrap();
        assert_eq!(read_project_log(&db, p2.id, 1024).unwrap(), "");
    }
}
