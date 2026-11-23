//! Tauri command 层：只做参数转发与错误字符串化，业务逻辑在各模块的 pub 函数中。

use tauri::State;

use crate::error::{to_string_err, AppError};
use crate::{export, files, ic, llm, parse, projects, queue, region_ocr, settings, AppState};

// ---------------------------------------------------------------------------
// 项目
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn create_project(
    state: State<'_, AppState>,
    base_path: String,
    name: String,
    group_name: String,
    // 内控测试新增；旧前端不传时缺省 'parse'（原则 2 兼容）
    task_type: Option<String>,
) -> Result<projects::Project, String> {
    projects::create_project_typed(
        &state.db,
        &base_path,
        &name,
        &group_name,
        task_type.as_deref().unwrap_or("parse"),
    )
    .map_err(to_string_err)
}

#[tauri::command]
pub fn list_projects(state: State<'_, AppState>) -> Result<Vec<projects::Project>, String> {
    projects::list_projects(&state.db).map_err(to_string_err)
}

#[tauri::command]
pub fn list_project_progress(
    state: State<'_, AppState>,
) -> Result<Vec<projects::ProjectProgress>, String> {
    projects::list_project_progress(&state.db).map_err(to_string_err)
}

#[tauri::command]
pub fn delete_project(state: State<'_, AppState>, id: i64) -> Result<(), String> {
    projects::delete_project(&state.db, id).map_err(to_string_err)
}

#[tauri::command]
pub fn reset_project(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    project_id: i64,
) -> Result<(), String> {
    projects::reset_project(&state.db, project_id).map_err(to_string_err)?;
    crate::task_log::task_log(
        Some(&app),
        &state.db,
        project_id,
        "队列",
        "全部重置：任务恢复初始状态",
    );
    Ok(())
}

#[tauri::command]
pub fn reparse_files(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    project_id: i64,
    file_ids: Vec<i64>,
) -> Result<(), String> {
    projects::reparse_files(&state.db, project_id, &file_ids).map_err(to_string_err)?;
    crate::task_log::task_log(
        Some(&app),
        &state.db,
        project_id,
        "解析",
        &format!("重新解析 {} 个文件", file_ids.len()),
    );
    state
        .queue
        .enqueue(queue::TaskKind::Parse { project_id })
        .map_err(to_string_err)
}

#[tauri::command]
pub fn reextract_files(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    project_id: i64,
    file_ids: Vec<i64>,
) -> Result<(), String> {
    projects::reextract_files(&state.db, project_id, &file_ids).map_err(to_string_err)?;
    crate::task_log::task_log(
        Some(&app),
        &state.db,
        project_id,
        "提取",
        &format!("重新提取 {} 个文件", file_ids.len()),
    );
    state
        .queue
        .enqueue(queue::TaskKind::Extract { project_id })
        .map_err(to_string_err)
}

#[tauri::command]
pub fn delete_files(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    project_id: i64,
    file_ids: Vec<i64>,
) -> Result<(), String> {
    // 先取文件名供日志记录（删除后列表即无）
    let names: Vec<String> = files::list_files(&state.db, project_id)
        .map_err(to_string_err)?
        .into_iter()
        .filter(|f| file_ids.contains(&f.id))
        .map(|f| f.original_name)
        .collect();
    projects::delete_files(&state.db, project_id, &file_ids).map_err(to_string_err)?;
    crate::task_log::task_log(
        Some(&app),
        &state.db,
        project_id,
        "上传",
        &format!("删除 {} 个文件：{}", names.len(), names.join("、")),
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// 文件
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn upload_files(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    project_id: i64,
    paths: Vec<String>,
) -> Result<files::UploadResult, String> {
    crate::task_log::task_log(
        Some(&app),
        &state.db,
        project_id,
        "上传",
        &format!("开始上传 {} 个文件", paths.len()),
    );
    match files::upload_files(&state.db, project_id, &paths) {
        Ok(r) => {
            for f in &r.files {
                crate::task_log::task_log(
                    Some(&app),
                    &state.db,
                    project_id,
                    "上传",
                    &format!("已上传 {}（{} 字节）", f.original_name, f.file_size),
                );
            }
            Ok(r)
        }
        Err(e) => {
            crate::task_log::task_log(
                Some(&app),
                &state.db,
                project_id,
                "上传",
                &format!("上传失败: {e}"),
            );
            Err(to_string_err(e))
        }
    }
}

#[tauri::command]
pub fn list_files(
    state: State<'_, AppState>,
    project_id: i64,
) -> Result<Vec<files::SourceFileInfo>, String> {
    files::list_files(&state.db, project_id).map_err(to_string_err)
}

// ---------------------------------------------------------------------------
// 设置
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn get_settings(state: State<'_, AppState>) -> Result<settings::SettingsView, String> {
    settings::get_settings(&state.db).map_err(to_string_err)
}

#[tauri::command]
pub fn save_settings(
    state: State<'_, AppState>,
    input: settings::SettingsInput,
) -> Result<settings::SettingsView, String> {
    settings::save_settings(&state.db, &input).map_err(to_string_err)
}

/// 以表单草稿配置获取模型清单（设置弹窗「刷新」按钮；不读库、不落库）。
#[tauri::command]
pub async fn fetch_models(
    provider: String,
    base_url: String,
    api_key: String,
) -> Result<settings::LlmTestResult, String> {
    Ok(settings::fetch_models(&provider, &base_url, &api_key).await)
}

// ---------------------------------------------------------------------------
// 任务队列
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn enqueue_parse(state: State<'_, AppState>, project_id: i64) -> Result<(), String> {
    projects::get_project(&state.db, project_id).map_err(to_string_err)?;
    state
        .queue
        .enqueue(queue::TaskKind::Parse { project_id })
        .map_err(to_string_err)
}

#[tauri::command]
pub fn enqueue_extract(state: State<'_, AppState>, project_id: i64) -> Result<(), String> {
    projects::get_project(&state.db, project_id).map_err(to_string_err)?;
    state
        .queue
        .enqueue(queue::TaskKind::Extract { project_id })
        .map_err(to_string_err)
}

#[tauri::command]
pub fn enqueue_export(
    state: State<'_, AppState>,
    project_id: i64,
    kind: String,
) -> Result<(), String> {
    projects::get_project(&state.db, project_id).map_err(to_string_err)?;
    state
        .queue
        .enqueue(queue::TaskKind::Export { project_id, kind })
        .map_err(to_string_err)
}

#[tauri::command]
pub fn queue_status(
    state: State<'_, AppState>,
    project_id: i64,
) -> Result<queue::ProjectQueueStatus, String> {
    queue::project_queue_status(&state.db, project_id).map_err(to_string_err)
}

/// 全部停止：中断当前解析/提取任务并取消队列中该项目任务。
/// 已完成文件与结果不受影响；未处理文件回到待解析/待提取。
#[tauri::command]
pub fn stop_project(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    project_id: i64,
) -> Result<(), String> {
    let p = projects::get_project(&state.db, project_id).map_err(to_string_err)?;
    let running = matches!(p.status.as_str(), "parsing" | "extracting");
    if !running && state.queue.pending_count() <= 0 {
        crate::task_log::task_log(
            Some(&app),
            &state.db,
            project_id,
            "队列",
            "「全部停止」：当前无进行中的任务",
        );
        return Ok(());
    }
    state.stop.request_stop(project_id);
    crate::task_log::task_log(
        Some(&app),
        &state.db,
        project_id,
        "队列",
        "用户请求「全部停止」：中断当前任务并清空队列",
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// 解析结果（Phase 3.4）
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn read_source_pdf_path(
    state: State<'_, AppState>,
    source_file_id: i64,
) -> Result<String, String> {
    parse::read_source_pdf_path(&state.db, source_file_id).map_err(to_string_err)
}

#[tauri::command]
pub fn read_file_bytes(state: State<'_, AppState>, path: String) -> Result<Vec<u8>, String> {
    parse::read_file_bytes(&state.db, &path).map_err(to_string_err)
}

// ---------------------------------------------------------------------------
// 提取配置与结果（Phase 4.5）
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn get_extract_configs(
    state: State<'_, AppState>,
    project_id: i64,
) -> Result<Vec<llm::extract::ExtractConfigInfo>, String> {
    llm::extract::get_extract_configs(&state.db, project_id).map_err(to_string_err)
}

#[tauri::command]
pub fn save_extract_configs(
    state: State<'_, AppState>,
    project_id: i64,
    configs: Vec<llm::extract::ExtractConfigInput>,
) -> Result<Vec<llm::extract::ExtractConfigInfo>, String> {
    llm::extract::save_extract_configs(&state.db, project_id, &configs).map_err(to_string_err)
}

#[tauri::command]
pub fn get_extract_results(
    state: State<'_, AppState>,
    project_id: i64,
) -> Result<Vec<llm::extract::ExtractResultInfo>, String> {
    llm::extract::get_extract_results(&state.db, project_id).map_err(to_string_err)
}

#[tauri::command]
pub fn update_extract_cell(
    state: State<'_, AppState>,
    id: i64,
    col_index: i64,
    value: String,
) -> Result<llm::extract::ExtractResultInfo, String> {
    llm::extract::update_extract_cell(&state.db, id, col_index, &value).map_err(to_string_err)
}

/// 逐文件估算提取输入 Token（进度面板「预估Token」列；仅 KIMI-API）。
#[tauri::command]
pub async fn estimate_tokens(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    project_id: i64,
) -> Result<Vec<llm::extract::TokenEstimate>, String> {
    let db = state.db.clone();
    let log = move |stage: &str, msg: &str| {
        crate::task_log::task_log(Some(&app), &db, project_id, stage, msg)
    };
    llm::extract::estimate_tokens(&state.db, project_id, &log)
        .await
        .map_err(to_string_err)
}

// ---------------------------------------------------------------------------
// 导出（Phase 6）
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn get_export_tasks(
    state: State<'_, AppState>,
    project_id: i64,
) -> Result<Vec<export::ExportTaskInfo>, String> {
    export::get_export_tasks(&state.db, project_id).map_err(to_string_err)
}

// ---------------------------------------------------------------------------
// 任务日志
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn read_project_log(
    state: State<'_, AppState>,
    project_id: i64,
    max_bytes: Option<u64>,
) -> Result<String, String> {
    crate::task_log::read_project_log(
        &state.db,
        project_id,
        max_bytes.unwrap_or(crate::task_log::DEFAULT_LOG_TAIL_BYTES),
    )
    .map_err(to_string_err)
}

// ---------------------------------------------------------------------------
// 内控测试（ic_* 系列，全部为新增命令；既有命令不动）
// ---------------------------------------------------------------------------

/// 导入制度文件：递归收集 src_dir 下 pdf 字节入库；异步执行（不阻塞主线程，
/// 逐文件独立落库）。诊断结论：命令挂起期间 emit 进度事件在 Wry 运行时下会
/// 干扰响应投递（界面持续"导入中"），故不做进度事件（小批量导入本身很快）。
#[tauri::command]
pub async fn ic_import_docs(
    state: State<'_, AppState>,
    project_id: i64,
    src_dir: String,
) -> Result<ic::IcImportDocsResult, String> {
    let db = state.db.clone();
    tokio::task::spawn_blocking(move || ic::import_docs(&db, project_id, &src_dir))
        .await
        .map_err(|e| to_string_err(AppError::Internal(format!("导入任务失败: {e}"))))?
        .map_err(to_string_err)
}

/// 生成导入模版 xlsx（roster/org）到用户选择的路径。
#[tauri::command]
pub fn ic_write_template(kind: String, path: String) -> Result<(), String> {
    ic::write_template(&kind, &path).map_err(to_string_err)
}

#[tauri::command]
pub fn ic_list_docs(
    state: State<'_, AppState>,
    project_id: i64,
) -> Result<Vec<ic::IcDocument>, String> {
    ic::list_docs(&state.db, project_id).map_err(to_string_err)
}

/// 导入人员名册 xlsx（calamine 解析，先清空再整体替换；返回导入行数）。
#[tauri::command]
pub fn ic_import_roster(
    state: State<'_, AppState>,
    project_id: i64,
    xlsx_path: String,
) -> Result<i64, String> {
    ic::import_roster(&state.db, project_id, &xlsx_path).map_err(to_string_err)
}

/// 导入组织结构 xlsx（同名册规则）。
#[tauri::command]
pub fn ic_import_org(
    state: State<'_, AppState>,
    project_id: i64,
    xlsx_path: String,
) -> Result<i64, String> {
    ic::import_org(&state.db, project_id, &xlsx_path).map_err(to_string_err)
}

#[tauri::command]
pub fn ic_list_roster(
    state: State<'_, AppState>,
    project_id: i64,
) -> Result<Vec<ic::IcRosterRow>, String> {
    ic::list_roster(&state.db, project_id).map_err(to_string_err)
}

#[tauri::command]
pub fn ic_list_org(
    state: State<'_, AppState>,
    project_id: i64,
) -> Result<Vec<ic::IcOrgRow>, String> {
    ic::list_org(&state.db, project_id).map_err(to_string_err)
}

/// 新增制度索引（源 pdf 矩形 → 目标 pdf 文件）；返回行 id。
#[tauri::command]
pub fn ic_add_doc_link(
    state: State<'_, AppState>,
    project_id: i64,
    src_doc_id: i64,
    page: i64,
    rect: String,
    target_doc_id: i64,
) -> Result<i64, String> {
    ic::add_doc_link(
        &state.db,
        project_id,
        src_doc_id,
        page,
        &rect,
        target_doc_id,
    )
    .map_err(to_string_err)
}

#[tauri::command]
pub fn ic_remove_doc_link(
    state: State<'_, AppState>,
    project_id: i64,
    link_id: i64,
) -> Result<(), String> {
    ic::remove_doc_link(&state.db, project_id, link_id).map_err(to_string_err)
}

#[tauri::command]
pub fn ic_list_doc_links(
    state: State<'_, AppState>,
    project_id: i64,
) -> Result<Vec<ic::IcDocLink>, String> {
    ic::list_doc_links(&state.db, project_id).map_err(to_string_err)
}

/// 创建流程节点（编号由 ic_counters 分配，全局唯一不复用）。
#[tauri::command]
pub fn ic_create_node(
    state: State<'_, AppState>,
    project_id: i64,
    input: ic::IcNodeInput,
) -> Result<ic::IcNode, String> {
    ic::create_node(&state.db, project_id, &input).map_err(to_string_err)
}

#[tauri::command]
pub fn ic_update_node(
    state: State<'_, AppState>,
    project_id: i64,
    node_id: i64,
    patch: ic::IcNodePatch,
) -> Result<ic::IcNode, String> {
    ic::update_node(&state.db, project_id, node_id, &patch).map_err(to_string_err)
}

/// 删除节点（级联清边，编号不复用）。
#[tauri::command]
pub fn ic_delete_node(
    state: State<'_, AppState>,
    project_id: i64,
    node_id: i64,
) -> Result<(), String> {
    ic::delete_node(&state.db, project_id, node_id).map_err(to_string_err)
}

#[tauri::command]
pub fn ic_list_nodes(
    state: State<'_, AppState>,
    project_id: i64,
) -> Result<Vec<ic::IcNode>, String> {
    ic::list_nodes(&state.db, project_id).map_err(to_string_err)
}

/// 新增连线（flow/role/data，类型校验见 ic::add_edge）。
#[tauri::command]
pub fn ic_add_edge(
    state: State<'_, AppState>,
    project_id: i64,
    from_node: i64,
    to_node: i64,
    edge_kind: String,
) -> Result<i64, String> {
    ic::add_edge(&state.db, project_id, from_node, to_node, &edge_kind).map_err(to_string_err)
}

#[tauri::command]
pub fn ic_remove_edge(
    state: State<'_, AppState>,
    project_id: i64,
    edge_id: i64,
) -> Result<(), String> {
    ic::remove_edge(&state.db, project_id, edge_id).map_err(to_string_err)
}

#[tauri::command]
pub fn ic_list_edges(
    state: State<'_, AppState>,
    project_id: i64,
) -> Result<Vec<ic::IcEdge>, String> {
    ic::list_edges(&state.db, project_id).map_err(to_string_err)
}

/// 创建环境组（it/manual；把指定节点归入新组）。
#[tauri::command]
pub fn ic_create_group(
    state: State<'_, AppState>,
    project_id: i64,
    env_type: String,
    rect: String,
    node_ids: Vec<i64>,
) -> Result<i64, String> {
    ic::create_group(&state.db, project_id, &env_type, &rect, &node_ids).map_err(to_string_err)
}

/// 删除环境组（节点保留、组归属清除）。
#[tauri::command]
pub fn ic_delete_group(
    state: State<'_, AppState>,
    project_id: i64,
    group_id: i64,
) -> Result<(), String> {
    ic::delete_group(&state.db, project_id, group_id).map_err(to_string_err)
}

/// 更新环境组包围矩形（节点拖动后回写）。
#[tauri::command]
pub fn ic_update_group_rect(
    state: State<'_, AppState>,
    project_id: i64,
    group_id: i64,
    rect: String,
) -> Result<(), String> {
    ic::update_group_rect(&state.db, project_id, group_id, &rect).map_err(to_string_err)
}

#[tauri::command]
pub fn ic_list_groups(
    state: State<'_, AppState>,
    project_id: i64,
) -> Result<Vec<ic::IcGroup>, String> {
    ic::list_groups(&state.db, project_id).map_err(to_string_err)
}

/// 区域 OCR（即用即载、空闲 5 分钟延迟卸载、10 分钟未调用强制卸载）。
/// rect 为 0–1000 归一化 [x0,y0,x1,y1]。
#[tauri::command]
pub async fn ocr_region(
    state: State<'_, AppState>,
    project_id: i64,
    doc_id: i64,
    page: i64,
    rect: [f64; 4],
) -> Result<region_ocr::OcrRegionResult, String> {
    // 校验矩形（与 ic::ensure_valid_rect 同规则，数组入参免 json 往返）
    if rect[0] >= rect[2] || rect[1] >= rect[3] || rect.iter().any(|x| !(0.0..=1000.0).contains(x))
    {
        return Err(to_string_err(AppError::InvalidInput(format!(
            "矩形坐标须为 0-1000 归一化 [x0,y0,x1,y1] 且 x0<x1、y0<y1: {rect:?}"
        ))));
    }
    let bytes = ic::doc_bytes(&state.db, project_id, doc_id).map_err(to_string_err)?;
    state
        .region_ocr
        .run(bytes, page, rect)
        .await
        .map_err(to_string_err)
}

/// 删除制度文件（节点与索引保留，跳转在运行时判定失效）。
#[tauri::command]
pub fn ic_delete_doc(
    state: State<'_, AppState>,
    project_id: i64,
    doc_id: i64,
) -> Result<(), String> {
    ic::delete_doc(&state.db, project_id, doc_id).map_err(to_string_err)
}

/// 读取制度文件字节（入库 blob；旧行回落磁盘路径受控读取）。
#[tauri::command]
pub fn ic_read_doc_bytes(
    state: State<'_, AppState>,
    project_id: i64,
    doc_id: i64,
) -> Result<Vec<u8>, String> {
    ic::doc_bytes(&state.db, project_id, doc_id).map_err(to_string_err)
}
