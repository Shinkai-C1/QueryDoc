//! QueryDoc 离线版 Rust 核心：SQLite / 项目 / 文件 / 设置 / 任务队列。
//! 前端经 `invoke` 调用 [`commands`] 中的 command（JS 侧参数名为 camelCase）。

pub mod commands;
pub mod db;
pub mod error;
pub mod export;
pub mod files;
pub mod ic;
pub mod llm;
pub mod ocr;
pub mod parse;
pub mod paths;
pub mod projects;
pub mod queue;
pub mod region_ocr;
pub mod settings;
pub mod task_log;

use tauri::Manager;

/// 全局应用状态：数据库句柄 + 任务队列入队句柄 + 停止标记注册表 + 区域 OCR 引擎。
pub struct AppState {
    pub db: db::Db,
    pub queue: queue::QueueHandle,
    pub stop: queue::StopRegistry,
    /// 区域 OCR（即用即载/延迟卸载；与解析队列引擎完全隔离，原则 4）。
    pub region_ocr: region_ocr::RegionOcr,
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        // 单实例：重复启动只聚焦已有窗口，不再新开（须在 Builder 最前注册）
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.unminimize();
                let _ = w.set_focus();
            }
        }))
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            // 库文件放 app_data_dir/querydoc.db（dev / 打包模式一致可用）
            let data_dir = app.path().app_data_dir()?;
            let db = db::Db::open(&data_dir.join("querydoc.db"))?;
            // 启动恢复：上次退出时中断的解析/提取回退到未开始，清理半成品
            projects::recover_interrupted(&db)?;
            let (queue_handle, rx) = queue::QueueHandle::new();
            let stop = queue::StopRegistry::default();
            queue::spawn_worker(
                app.handle().clone(),
                db.clone(),
                queue_handle.clone(),
                stop.clone(),
                rx,
            );
            app.manage(AppState {
                db,
                queue: queue_handle,
                stop,
                region_ocr: region_ocr::RegionOcr::new(),
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::create_project,
            commands::list_projects,
            commands::list_project_progress,
            commands::delete_project,
            commands::reset_project,
            commands::reparse_files,
            commands::reextract_files,
            commands::delete_files,
            commands::upload_files,
            commands::list_files,
            commands::get_settings,
            commands::save_settings,
            commands::fetch_models,
            commands::enqueue_parse,
            commands::enqueue_extract,
            commands::enqueue_export,
            commands::queue_status,
            commands::stop_project,
            commands::read_source_pdf_path,
            commands::read_file_bytes,
            commands::get_extract_configs,
            commands::save_extract_configs,
            commands::get_extract_results,
            commands::update_extract_cell,
            commands::estimate_tokens,
            commands::get_export_tasks,
            commands::read_project_log,
            commands::ic_import_docs,
            commands::ic_list_docs,
            commands::ic_delete_doc,
            commands::ic_read_doc_bytes,
            commands::ic_write_template,
            commands::ic_import_roster,
            commands::ic_import_org,
            commands::ic_list_roster,
            commands::ic_list_org,
            commands::ic_add_doc_link,
            commands::ic_remove_doc_link,
            commands::ic_list_doc_links,
            commands::ic_create_node,
            commands::ic_update_node,
            commands::ic_delete_node,
            commands::ic_list_nodes,
            commands::ic_add_edge,
            commands::ic_remove_edge,
            commands::ic_list_edges,
            commands::ic_create_group,
            commands::ic_delete_group,
            commands::ic_update_group_rect,
            commands::ic_list_groups,
            commands::ocr_region,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
