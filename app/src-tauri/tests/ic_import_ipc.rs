//! ic_import_docs 的真实 IPC 路径回归测试（用户报障：导入后 invoke 不返回、
//! 界面持续"导入中"）。经 mock runtime + get_ipc_response 走完整命令分发链，
//! 锁定 async 命令 + spawn_blocking 组合路径必须正常返回。

use app_lib::{commands, db::Db, projects, queue, region_ocr, AppState};
use tauri::Manager;

/// 构造挂有真实 invoke handler 与 AppState 的 mock 应用（内存库）。
fn mock_app_with_state() -> (tauri::App<tauri::test::MockRuntime>, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let db = Db::open_in_memory().unwrap();
    let (queue_handle, _rx) = queue::QueueHandle::new();
    let app = tauri::test::mock_builder()
        .invoke_handler(tauri::generate_handler![commands::ic_import_docs])
        .manage(AppState {
            db,
            queue: queue_handle,
            stop: queue::StopRegistry::default(),
            region_ocr: region_ocr::RegionOcr::new(),
        })
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .expect("failed to build mock app");
    (app, tmp)
}

#[test]
fn ic_import_docs_via_ipc_returns_result() {
    let (app, tmp) = mock_app_with_state();
    // 造一个含 7 个假 pdf 的源文件夹（导入只读字节，无需真实 pdf 结构）
    let src = tmp.path().join("制度源");
    std::fs::create_dir_all(src.join("sub")).unwrap();
    for i in 1..=6 {
        std::fs::write(src.join(format!("f{i}.pdf")), format!("%PDF fake {i}")).unwrap();
    }
    std::fs::write(src.join("sub/f7.pdf"), b"%PDF fake 7").unwrap();
    // 内控任务（不落盘）
    let db = app.state::<AppState>().db.clone();
    let project = projects::create_project_typed(&db, "", "内控", "组A", "audit").unwrap();

    let webview = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
        .build()
        .unwrap();

    let res = tauri::test::get_ipc_response(
        &webview,
        tauri::webview::InvokeRequest {
            cmd: "ic_import_docs".into(),
            callback: tauri::ipc::CallbackFn(0),
            error: tauri::ipc::CallbackFn(1),
            // IPC origin 分平台（macOS/Linux: tauri://localhost，Windows: http://tauri.localhost），
            // 写错会被 ACL 以 "not allowed. Plugin not found" 拒绝
            url: if cfg!(windows) {
                "http://tauri.localhost".parse().unwrap()
            } else {
                "tauri://localhost".parse().unwrap()
            },
            body: tauri::ipc::InvokeBody::Json(serde_json::json!({
                "projectId": project.id,
                "srcDir": src.to_string_lossy(),
            })),
            headers: Default::default(),
            invoke_key: tauri::test::INVOKE_KEY.to_string(),
        },
    );
    let body = res.unwrap_or_else(|e| panic!("命令返回错误: {e:?}"));
    let v: serde_json::Value = body.deserialize().unwrap();
    assert_eq!(v["imported"], 7, "应导入 7 个文件: {v}");
    assert_eq!(v["updated"], 0);
    // 落库可见
    assert_eq!(app_lib::ic::list_docs(&db, project.id).unwrap().len(), 7);
}
