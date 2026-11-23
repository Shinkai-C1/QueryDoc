//! 任务队列框架：tokio mpsc 通道 + 单一串行 worker + Tauri event 进度推送。
//! 三阶段状态/位次语义沿用旧版 `backend/apps/queue/views.py`（差异见函数注释）。
//! 执行体：Parse（parse.rs）/ Extract（llm/extract.rs）/ Export（export.rs）已接通。

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter};
use tokio::sync::mpsc;

use crate::db::Db;
use crate::error::AppError;
use crate::ocr::models::{self, OrtEngine};
use crate::projects;
use pdfium_render::prelude::Pdfium;

/// 队列事件名（前端 @tauri-apps/api/event 监听）。
pub const PROGRESS_EVENT: &str = "queue://progress";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TaskKind {
    Parse { project_id: i64 },
    Extract { project_id: i64 },
    Export { project_id: i64, kind: String },
}

impl TaskKind {
    pub fn kind_name(&self) -> &'static str {
        match self {
            TaskKind::Parse { .. } => "parse",
            TaskKind::Extract { .. } => "extract",
            TaskKind::Export { .. } => "export",
        }
    }

    pub fn project_id(&self) -> i64 {
        match self {
            TaskKind::Parse { project_id }
            | TaskKind::Extract { project_id }
            | TaskKind::Export { project_id, .. } => *project_id,
        }
    }
}

/// 入队句柄（随 AppState 管理，command 层调用 enqueue）。
/// pending 计数供「全部停止」判断队列是否有待处理任务。
#[derive(Clone)]
pub struct QueueHandle {
    tx: mpsc::UnboundedSender<TaskKind>,
    pending: Arc<AtomicI64>,
}

impl QueueHandle {
    pub fn new() -> (Self, mpsc::UnboundedReceiver<TaskKind>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (
            QueueHandle {
                tx,
                pending: Arc::new(AtomicI64::new(0)),
            },
            rx,
        )
    }

    pub fn enqueue(&self, task: TaskKind) -> Result<(), AppError> {
        self.pending.fetch_add(1, Ordering::SeqCst);
        self.tx
            .send(task)
            .map_err(|_| AppError::Internal("任务队列已关闭".to_string()))
    }

    /// 队列中待处理任务数（入队 +1，worker 取走 -1）。
    pub fn pending_count(&self) -> i64 {
        self.pending.load(Ordering::SeqCst)
    }

    /// worker 专用：取走一个任务后递减计数。
    fn take(&self) {
        self.pending.fetch_sub(1, Ordering::SeqCst);
    }
}

/// 停止标记注册表：按项目记录「全部停止」标记（worker 与执行体协作中断）。
#[derive(Clone, Default)]
pub struct StopRegistry {
    flags: Arc<Mutex<HashMap<i64, Arc<AtomicBool>>>>,
}

impl StopRegistry {
    /// 取/建某项目的停止标记（执行体轮询用）。
    pub fn flag(&self, project_id: i64) -> Arc<AtomicBool> {
        self.flags
            .lock()
            .unwrap()
            .entry(project_id)
            .or_default()
            .clone()
    }

    /// 请求停止某项目（中断运行中任务 + 取消队列中该项目任务）。
    pub fn request_stop(&self, project_id: i64) {
        self.flag(project_id).store(true, Ordering::SeqCst);
    }

    /// 清除标记（停止已消费或新项目开始时）。
    pub fn clear(&self, project_id: i64) {
        self.flag(project_id).store(false, Ordering::SeqCst);
    }
}

/// 进度事件载荷：`queue://progress`。
#[derive(Debug, Clone, Serialize)]
pub struct ProgressPayload {
    pub project_id: i64,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_id: Option<i64>,
    pub status: String,
    pub done: u32,
    pub total: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_file: Option<String>,
}

/// 推送进度（发送失败仅意味着无监听窗口，不视为错误）。
pub fn emit_progress(app: &AppHandle, payload: &ProgressPayload) {
    let _ = app.emit(PROGRESS_EVENT, payload);
}

/// 在 app setup 时调用：spawn 唯一串行 worker，逐任务执行。
/// 单个任务 panic 不会拖垮 worker（子任务隔离）；panic/异常终止时兜底回写
/// 项目状态并发终态事件——否则前端永远等不到 done/error，节点无限"进行中"。
/// 「全部停止」：取到标记置位的项目任务直接取消；任务结束后队列一空即清标记。
/// OCR 引擎不常驻：每个 Parse 任务运行时加载、任务结束即卸载释放内存
///（见 run_parse_task），无解析任务时引擎不占用户内存。
pub fn spawn_worker(
    app: AppHandle,
    db: Db,
    queue: QueueHandle,
    stop: StopRegistry,
    mut rx: mpsc::UnboundedReceiver<TaskKind>,
) -> tauri::async_runtime::JoinHandle<()> {
    tauri::async_runtime::spawn(async move {
        while let Some(task) = rx.recv().await {
            queue.take();
            let app = app.clone();
            let db = db.clone();
            let project_id = task.project_id();
            let kind = task.kind_name().to_string();
            let flag = stop.flag(project_id);
            if flag.load(Ordering::SeqCst) {
                // 「全部停止」：队列中的该项目任务直接取消
                crate::task_log::task_log(
                    Some(&app),
                    &db,
                    project_id,
                    "队列",
                    &format!("{kind}任务已被「全部停止」取消"),
                );
                let _ = projects::set_project_status(&db, project_id, "idle");
                emit_progress(
                    &app,
                    &ProgressPayload {
                        project_id,
                        kind,
                        file_id: None,
                        status: "error".to_string(),
                        done: 0,
                        total: 0,
                        current_file: None,
                    },
                );
                if queue.pending_count() <= 0 {
                    flag.store(false, Ordering::SeqCst);
                }
                continue;
            }
            let app2 = app.clone();
            let db2 = db.clone();
            let stop2 = stop.clone();
            let joined =
                tauri::async_runtime::spawn(
                    async move { execute(&app2, &db2, &stop2, task).await },
                );
            if let Err(e) = joined.await {
                crate::task_log::task_log(
                    Some(&app),
                    &db,
                    project_id,
                    "队列",
                    &format!("任务异常终止（panic）: {e}"),
                );
                let _ = projects::set_project_status(&db, project_id, "error");
                emit_progress(
                    &app,
                    &ProgressPayload {
                        project_id,
                        kind,
                        file_id: None,
                        status: "error".to_string(),
                        done: 0,
                        total: 0,
                        current_file: None,
                    },
                );
            }
            // 任务结束（含因「全部停止」中止）：队列已空则消费标记，避免误杀后续新任务
            if queue.pending_count() <= 0 {
                stop.clear(project_id);
            }
        }
    })
}

/// 任务执行体。
async fn execute(app: &AppHandle, db: &Db, stop: &StopRegistry, task: TaskKind) {
    let project_id = task.project_id();
    let kind = task.kind_name();
    let stop_flag = stop.flag(project_id);
    let label = match kind {
        "parse" => "解析",
        "extract" => "提取",
        _ => "导出",
    };
    crate::task_log::task_log(
        Some(app),
        db,
        project_id,
        "队列",
        &format!("{label}任务开始"),
    );
    let result = match &task {
        TaskKind::Parse { project_id } => {
            run_parse_task(app, db, *project_id, stop_flag.clone()).await
        }
        TaskKind::Extract { project_id } => {
            let app2 = app.clone();
            let db2 = db.clone();
            let pid = *project_id;
            let log = move |stage: &str, msg: &str| {
                crate::task_log::task_log(Some(app), db, pid, stage, msg)
            };
            crate::llm::extract::run_project_extract(
                &db2,
                pid,
                move |p| {
                    emit_progress(&app2, &p);
                },
                &log,
                stop_flag,
            )
            .await
        }
        TaskKind::Export { project_id, kind } => {
            let app = app.clone();
            let db = db.clone();
            let db2 = db.clone();
            let kind = kind.clone();
            let project_id = *project_id;
            match tauri::async_runtime::spawn_blocking(move || {
                let log = |stage: &str, msg: &str| {
                    crate::task_log::task_log(Some(&app), &db2, project_id, stage, msg)
                };
                crate::export::run_export(
                    &db,
                    project_id,
                    &kind,
                    |p| {
                        emit_progress(&app, &p);
                    },
                    &log,
                )
            })
            .await
            {
                Ok(r) => r,
                Err(e) => Err(AppError::Internal(format!("导出线程异常结束: {e}"))),
            }
        }
    };
    let (status, done, total) = match &result {
        Ok(summary) => ("done", summary.0 + summary.1, summary.0 + summary.1),
        Err(_) => ("error", 0, 0),
    };
    if result.is_err() {
        // 兜底：执行体自身未回写状态（如阻塞线程 JoinError）时，避免项目卡在
        // parsing/extracting 导致前端无限"进行中"
        if matches!(task, TaskKind::Parse { .. } | TaskKind::Extract { .. }) {
            if let Ok(p) = projects::get_project(db, project_id) {
                if p.status == "parsing" || p.status == "extracting" {
                    let _ = projects::set_project_status(db, project_id, "error");
                }
            }
        }
    }
    match &result {
        Ok((ok, failed)) => crate::task_log::task_log(
            Some(app),
            db,
            project_id,
            label,
            &format!("{label}任务结束：成功 {ok}，失败 {failed}"),
        ),
        Err(e) => crate::task_log::task_log(
            Some(app),
            db,
            project_id,
            label,
            &format!("{label}任务失败: {e}"),
        ),
    }
    emit_progress(
        app,
        &ProgressPayload {
            project_id,
            kind: kind.to_string(),
            file_id: None,
            status: status.to_string(),
            done,
            total,
            current_file: None,
        },
    );
}

/// Parse 臂（3.3）：引擎随任务加载、任务结束即卸载（内存不常驻），
/// 在阻塞线程中跑项目级解析（CPU 密集）。worker 串行执行，无并发访问。
async fn run_parse_task(
    app: &AppHandle,
    db: &Db,
    project_id: i64,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> Result<(u32, u32), AppError> {
    let app = app.clone();
    let db = db.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let log = |stage: &str, msg: &str| {
            crate::task_log::task_log(Some(&app), &db, project_id, stage, msg)
        };
        log("解析", "OCR 引擎加载中…");
        let t0 = std::time::Instant::now();
        let mut engine = OrtEngine::load()?;
        log(
            "解析",
            &format!("OCR 引擎加载完成，耗时 {:.1}s", t0.elapsed().as_secs_f64()),
        );
        let resources = models::resolve_resources_dir()?;
        let pdfium = Pdfium::new(
            Pdfium::bind_to_library(models::pdfium_dylib(&resources))
                .map_err(|e| AppError::Internal(format!("绑定 pdfium 失败: {e}")))?,
        );
        let result = crate::parse::run_project_parse_blocking(
            &db,
            project_id,
            &mut engine,
            &pdfium,
            |p| {
                emit_progress(&app, &p);
            },
            &log,
            stop,
        );
        // 任务结束（成功/失败/全部停止）即卸载：4 个 ONNX 会话析构释放模型内存，
        // Pdfium 随作用域结束卸载动态库；下次解析任务再重新加载
        drop(engine);
        drop(pdfium);
        log("解析", "OCR 引擎已卸载，内存已释放");
        result
    })
    .await
    .map_err(|e| AppError::Internal(format!("解析线程异常结束: {e}")))?
}

// ---------------------------------------------------------------------------
// 三阶段状态与位次查询
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct FileQueueStatus {
    pub file_id: i64,
    pub file_name: String,
    pub upload_status: String,
    pub upload_position: Option<u32>,
    pub upload_files_ahead: u32,
    pub parse_status: String,
    pub parse_position: Option<u32>,
    pub parse_files_ahead: u32,
    pub extract_status: String,
    pub extract_position: Option<u32>,
    pub extract_files_ahead: u32,
    /// parse_results.error_msg（仅 status=error 时有值）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parse_error: Option<String>,
    /// extract_results.error_msg（提取失败行的错误信息）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extract_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct StageCounts {
    pub pending: i64,
    pub running: i64,
    pub done: i64,
    pub error: i64,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct QueueSummary {
    pub upload: StageCounts,
    pub parse: StageCounts,
    pub extract: StageCounts,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProjectQueueStatus {
    pub project_id: i64,
    pub project_status: String,
    pub files: Vec<FileQueueStatus>,
    pub summary: QueueSummary,
}

fn count_by_status(counts: &mut StageCounts, status: &str) {
    match status {
        "pending" => counts.pending += 1,
        "running" => counts.running += 1,
        "done" | "completed" => counts.done += 1,
        "error" => counts.error += 1,
        _ => {}
    }
}

/// 项目维度三阶段状态与位次。
///
/// 与旧版语义的对应关系（旧版 `backend/apps/queue/views.py::_build_file_status`）：
/// - upload：恒 completed（本地上传为同步复制），位次 null、前方 0；
/// - parse：无 parse_results 行 → pending/位次 null；行 pending → 全局待解析队列
///   （按源文件创建时间升序）内 1 起始位次，前方排队 = 位次 - 1；running → 位次 0；
///   done/error → 位次 null；
/// - extract：成功行（error_msg 为空）→ done；失败行（非空）→ error
///   （错误信息随 extract_error 返回）；项目提取中且文件已解析完成 →
///   未成功文件（无行或失败行）进入待提取队列：最早 running（位次 0），
///   其余 pending 按创建时间给 1 起始位次，失败行的错误文本此时隐藏（视为排队重提）；
///   项目未在提取 → 无行 pending/位次 null。
pub fn project_queue_status(db: &Db, project_id: i64) -> Result<ProjectQueueStatus, AppError> {
    let project = projects::get_project(db, project_id)?;
    let conn = db.conn()?;

    // 项目文件（创建时间升序，同刻按 id）
    let mut stmt = conn.prepare(
        "SELECT sf.id, sf.original_name FROM source_files sf
         JOIN upload_batches b ON b.id = sf.batch_id
         WHERE b.project_id = ?1
         ORDER BY sf.created_at ASC, sf.id ASC",
    )?;
    let files: Vec<(i64, String)> = stmt
        .query_map(rusqlite::params![project_id], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    drop(stmt);

    // file_id → (parse_results.status, error_msg)
    let mut stmt = conn.prepare(
        "SELECT pr.source_file_id, pr.status, pr.error_msg FROM parse_results pr
         JOIN source_files sf ON sf.id = pr.source_file_id
         JOIN upload_batches b ON b.id = sf.batch_id
         WHERE b.project_id = ?1",
    )?;
    let parse_map: std::collections::HashMap<i64, (String, String)> = stmt
        .query_map(rusqlite::params![project_id], |row| {
            Ok((row.get(0)?, (row.get(1)?, row.get(2)?)))
        })?
        .collect::<Result<_, _>>()?;
    drop(stmt);

    // file_id → extract_results.error_msg（行存在且 error_msg 为空 = 提取成功）
    let mut stmt = conn
        .prepare("SELECT source_file_id, error_msg FROM extract_results WHERE project_id = ?1")?;
    let extract_map: std::collections::HashMap<i64, String> = stmt
        .query_map(rusqlite::params![project_id], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })?
        .collect::<Result<_, _>>()?;
    drop(stmt);

    // 全局待解析队列（与串行 worker 消费顺序一致：按源文件创建时间）
    let mut stmt = conn.prepare(
        "SELECT pr.source_file_id FROM parse_results pr
         JOIN source_files sf ON sf.id = pr.source_file_id
         WHERE pr.status = 'pending'
         ORDER BY sf.created_at ASC, sf.id ASC",
    )?;
    let global_pending_parse: Vec<i64> = stmt
        .query_map([], |row| row.get(0))?
        .collect::<Result<Vec<_>, _>>()?;
    drop(stmt);

    let is_extracting = project.status == "extracting";
    // 待提取（未成功）集合 = 已解析完成 且（无提取行 或 提取失败行）——与执行体
    // eligible_files 口径一致；提取中失败行视为排队重提（pending/running，错误文本隐藏），
    // 非提取中失败行才显 error。
    let not_succeeded: Vec<i64> = files
        .iter()
        .map(|(id, _)| *id)
        .filter(|id| {
            matches!(parse_map.get(id), Some((s, _)) if s == "done")
                && !matches!(extract_map.get(id), Some(m) if m.is_empty())
        })
        .collect();

    let mut file_statuses = Vec::with_capacity(files.len());
    let mut summary = QueueSummary::default();
    for (file_id, file_name) in &files {
        // upload：同步完成
        let upload_status = "completed".to_string();

        // parse
        let (parse_status, parse_position, parse_files_ahead, parse_error) =
            match parse_map.get(file_id) {
                None => ("pending".to_string(), None, 0, None),
                Some((s, _)) if s == "pending" => {
                    let pos = global_pending_parse
                        .iter()
                        .position(|id| id == file_id)
                        .map(|i| i as u32 + 1);
                    (
                        "pending".to_string(),
                        pos,
                        pos.map(|p| p - 1).unwrap_or(0),
                        None,
                    )
                }
                Some((s, _)) if s == "running" => ("running".to_string(), Some(0), 0, None),
                Some((s, msg)) => {
                    let err = (s == "error" && !msg.is_empty()).then(|| msg.clone());
                    (s.clone(), None, 0, err)
                }
            };

        // extract：成功行（error_msg 为空）→ done；提取中且已解析 → 未成功文件
        // 按待提取队列给位次（最早 running(0)，失败行错误文本隐藏）；非提取中 →
        // 失败行 error（带错误信息），无行 pending。
        let parse_done = matches!(parse_map.get(file_id), Some((s, _)) if s == "done");
        let (extract_status, extract_position, extract_files_ahead, extract_error) = if matches!(extract_map.get(file_id), Some(m) if m.is_empty())
        {
            ("done".to_string(), None, 0, None)
        } else if is_extracting && parse_done {
            let idx = not_succeeded.iter().position(|id| id == file_id);
            match idx {
                Some(0) => ("running".to_string(), Some(0), 0, None),
                Some(i) => ("pending".to_string(), Some(i as u32 + 1), i as u32, None),
                None => ("pending".to_string(), None, 0, None),
            }
        } else {
            match extract_map.get(file_id) {
                Some(msg) => ("error".to_string(), None, 0, Some(msg.clone())),
                None => ("pending".to_string(), None, 0, None),
            }
        };

        count_by_status(&mut summary.upload, &upload_status);
        count_by_status(&mut summary.parse, &parse_status);
        count_by_status(&mut summary.extract, &extract_status);

        file_statuses.push(FileQueueStatus {
            file_id: *file_id,
            file_name: file_name.clone(),
            upload_status,
            upload_position: None,
            upload_files_ahead: 0,
            parse_status,
            parse_position,
            parse_files_ahead,
            extract_status,
            extract_position,
            extract_files_ahead,
            parse_error,
            extract_error,
        });
    }

    Ok(ProjectQueueStatus {
        project_id,
        project_status: project.status,
        files: file_statuses,
        summary,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture {
        _tmp: tempfile::TempDir,
        db: Db,
        project: projects::Project,
        file_ids: Vec<i64>,
    }

    /// 项目 + 3 个已上传文件；返回句柄便于插各种状态行。
    fn fixture() -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open_in_memory().unwrap();
        let project =
            projects::create_project(&db, tmp.path().to_str().unwrap(), "队列项目", "测试组")
                .unwrap();
        let src_dir = tmp.path().join("src");
        std::fs::create_dir_all(&src_dir).unwrap();
        let mut paths = Vec::new();
        for name in ["a.pdf", "b.pdf", "c.pdf"] {
            let p = src_dir.join(name);
            std::fs::write(&p, b"%PDF-1.7\n").unwrap();
            paths.push(p.to_str().unwrap().to_string());
        }
        let result = crate::files::upload_files(&db, project.id, &paths).unwrap();
        let file_ids = result.files.iter().map(|f| f.id).collect();
        Fixture {
            _tmp: tmp,
            db,
            project,
            file_ids,
        }
    }

    fn insert_parse(db: &Db, file_id: i64, status: &str) {
        db.conn()
            .unwrap()
            .execute(
                "INSERT INTO parse_results(source_file_id, status) VALUES (?1, ?2)",
                rusqlite::params![file_id, status],
            )
            .unwrap();
    }

    fn insert_extract(db: &Db, project_id: i64, file_id: i64) {
        db.conn()
            .unwrap()
            .execute(
                "INSERT INTO extract_results(project_id, source_file_id, cell_data,
                                            is_edited, created_at, updated_at)
                 VALUES (?1, ?2, '{}', 0, '2026-01-01 00:00:00', '2026-01-01 00:00:00')",
                rusqlite::params![project_id, file_id],
            )
            .unwrap();
    }

    fn set_project_status(db: &Db, project_id: i64, status: &str) {
        db.conn()
            .unwrap()
            .execute(
                "UPDATE projects SET status = ?2 WHERE id = ?1",
                rusqlite::params![project_id, status],
            )
            .unwrap();
    }

    #[test]
    fn queue_handle_enqueue_and_close() {
        let (handle, mut rx) = QueueHandle::new();
        handle.enqueue(TaskKind::Parse { project_id: 7 }).unwrap();
        handle
            .enqueue(TaskKind::Export {
                project_id: 7,
                kind: "table".to_string(),
            })
            .unwrap();
        assert_eq!(rx.try_recv().unwrap(), TaskKind::Parse { project_id: 7 });
        assert_eq!(
            rx.try_recv().unwrap(),
            TaskKind::Export {
                project_id: 7,
                kind: "table".to_string()
            }
        );
        drop(rx);
        assert!(matches!(
            handle.enqueue(TaskKind::Extract { project_id: 1 }),
            Err(AppError::Internal(_))
        ));
    }

    #[test]
    fn parse_positions_running_zero_pending_global() {
        let fx = fixture();
        // file1 running、file2 pending、file3 done
        insert_parse(&fx.db, fx.file_ids[0], "running");
        insert_parse(&fx.db, fx.file_ids[1], "pending");
        insert_parse(&fx.db, fx.file_ids[2], "done");

        let st = project_queue_status(&fx.db, fx.project.id).unwrap();
        let f = &st.files;
        assert_eq!(f[0].parse_status, "running");
        assert_eq!(f[0].parse_position, Some(0));
        assert_eq!(f[1].parse_status, "pending");
        assert_eq!(f[1].parse_position, Some(1));
        assert_eq!(f[1].parse_files_ahead, 0);
        assert_eq!(f[2].parse_status, "done");
        assert_eq!(f[2].parse_position, None);
        assert_eq!(f[0].upload_status, "completed");

        assert_eq!(st.summary.parse.running, 1);
        assert_eq!(st.summary.parse.pending, 1);
        assert_eq!(st.summary.parse.done, 1);
    }

    #[test]
    fn parse_pending_position_is_global_across_projects() {
        let fx = fixture();
        let tmp2 = tempfile::tempdir().unwrap();
        let p2 =
            projects::create_project(&fx.db, tmp2.path().to_str().unwrap(), "第二项目", "测试组")
                .unwrap();
        let src_dir = tmp2.path().join("src");
        std::fs::create_dir_all(&src_dir).unwrap();
        let p = src_dir.join("x.pdf");
        std::fs::write(&p, b"%PDF-1.7\n").unwrap();
        let up2 =
            crate::files::upload_files(&fx.db, p2.id, &[p.to_str().unwrap().to_string()]).unwrap();
        // 强制第二项目的文件更早入队（全局队列按源文件创建时间排序）
        fx.db
            .conn()
            .unwrap()
            .execute(
                "UPDATE source_files SET created_at = '2000-01-01 00:00:00' WHERE id = ?1",
                rusqlite::params![up2.files[0].id],
            )
            .unwrap();
        insert_parse(&fx.db, up2.files[0].id, "pending");
        insert_parse(&fx.db, fx.file_ids[0], "pending");

        let st = project_queue_status(&fx.db, fx.project.id).unwrap();
        // 全局队列中 p2 的文件在前 → 本项目的 pending 位次为 2，前方 1
        assert_eq!(st.files[0].parse_position, Some(2));
        assert_eq!(st.files[0].parse_files_ahead, 1);
    }

    #[test]
    fn parse_pending_without_row_has_null_position() {
        let fx = fixture();
        // 无 parse_results 行：pending 但位次 null
        let st = project_queue_status(&fx.db, fx.project.id).unwrap();
        assert_eq!(st.files[0].parse_status, "pending");
        assert_eq!(st.files[0].parse_position, None);
        assert_eq!(st.files[0].parse_files_ahead, 0);
    }

    #[test]
    fn extract_status_done_running_head_pending_rest() {
        let fx = fixture();
        // 未提取且项目未在提取：全部 pending/位次 null
        let st = project_queue_status(&fx.db, fx.project.id).unwrap();
        assert!(st
            .files
            .iter()
            .all(|f| f.extract_status == "pending" && f.extract_position.is_none()));

        // 项目提取中：最早未提取文件 running(0)，其余 pending 位次 2/3
        for fid in &fx.file_ids {
            insert_parse(&fx.db, *fid, "done");
        }
        set_project_status(&fx.db, fx.project.id, "extracting");
        let st = project_queue_status(&fx.db, fx.project.id).unwrap();
        assert_eq!(st.files[0].extract_status, "running");
        assert_eq!(st.files[0].extract_position, Some(0));
        assert_eq!(st.files[1].extract_status, "pending");
        assert_eq!(st.files[1].extract_position, Some(2));
        assert_eq!(st.files[1].extract_files_ahead, 1);
        assert_eq!(st.files[2].extract_position, Some(3));
        assert_eq!(st.files[2].extract_files_ahead, 2);

        // file1 提取完成 → done；running 顺延给 file2
        insert_extract(&fx.db, fx.project.id, fx.file_ids[0]);
        let st = project_queue_status(&fx.db, fx.project.id).unwrap();
        assert_eq!(st.files[0].extract_status, "done");
        assert_eq!(st.files[1].extract_status, "running");
        assert_eq!(st.files[1].extract_position, Some(0));
        assert_eq!(st.files[2].extract_position, Some(2));
        assert_eq!(st.summary.extract.done, 1);
        assert_eq!(st.summary.extract.running, 1);
        assert_eq!(st.summary.extract.pending, 1);
    }

    #[test]
    fn extract_error_row_shows_error_without_position() {
        let fx = fixture();
        // file1 提取失败行；file2 无行
        fx.db
            .conn()
            .unwrap()
            .execute(
                "INSERT INTO extract_results(project_id, source_file_id, cell_data,
                                            is_edited, error_msg, created_at, updated_at)
                 VALUES (?1, ?2, '{}', 0, 'LLM 超时',
                         '2026-01-01 00:00:00', '2026-01-01 00:00:00')",
                rusqlite::params![fx.project.id, fx.file_ids[0]],
            )
            .unwrap();
        let st = project_queue_status(&fx.db, fx.project.id).unwrap();
        assert_eq!(st.files[0].extract_status, "error");
        assert_eq!(st.files[0].extract_error.as_deref(), Some("LLM 超时"));
        assert_eq!(st.files[0].extract_position, None);
        assert_eq!(st.summary.extract.error, 1);

        // 项目提取中：失败行视为排队重提（running/pending，错误文本隐藏）
        for fid in &fx.file_ids {
            insert_parse(&fx.db, *fid, "done");
        }
        set_project_status(&fx.db, fx.project.id, "extracting");
        let st = project_queue_status(&fx.db, fx.project.id).unwrap();
        assert_eq!(st.files[0].extract_status, "running");
        assert_eq!(st.files[0].extract_position, Some(0));
        assert_eq!(st.files[0].extract_error, None, "重提中失败文本应隐藏");
        assert_eq!(st.files[1].extract_status, "pending");
        assert_eq!(st.files[1].extract_position, Some(2));
        assert_eq!(st.files[2].extract_position, Some(3));
    }

    #[test]
    fn parse_error_carries_message() {
        let fx = fixture();
        fx.db
            .conn()
            .unwrap()
            .execute(
                "INSERT INTO parse_results(source_file_id, status, error_msg)
                 VALUES (?1, 'error', 'pdf 渲染失败')",
                rusqlite::params![fx.file_ids[0]],
            )
            .unwrap();
        let st = project_queue_status(&fx.db, fx.project.id).unwrap();
        assert_eq!(st.files[0].parse_status, "error");
        assert_eq!(st.files[0].parse_error.as_deref(), Some("pdf 渲染失败"));
        assert!(st.files[1].parse_error.is_none());
    }
}
