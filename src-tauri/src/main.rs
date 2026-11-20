#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod diagnostics;

use csvscope_core::{diagnostics as log, *};
use serde_json::{Value, json};
use std::{path::PathBuf, sync::Arc};
use tauri::{Manager, State};

type AppState = Arc<Engine>;
type ApiResult<T> = Result<T, String>;
fn error(e: impl std::fmt::Display) -> String { format!("{e:#}") }

/// Engine calls can touch locks or disk; keep them off Tauri's UI thread.
async fn blocking<T: Send + 'static>(
    command: &'static str, context: Value, action: impl FnOnce() -> ApiResult<T> + Send + 'static,
) -> ApiResult<T> {
    let quiet = matches!(command, "sessions" | "session" | "candidate_status" | "export_status" | "read_page");
    let mut operation = log::Operation::start(command, context, quiet);
    tauri::async_runtime::spawn_blocking(move || {
        let result = action();
        operation.finish(result.as_ref().err().cloned());
        result
    }).await.map_err(|join| {
        let message = error(join);
        log::record("error", "ipc_worker_failed", json!({ "command": command, "error": message }));
        message
    })?
}

#[tauri::command]
async fn sessions(engine: State<'_, AppState>) -> ApiResult<Vec<SessionSnapshot>> {
    let engine = engine.inner().clone();
    blocking("sessions", json!({}), move || Ok(engine.snapshots())).await
}
#[tauri::command]
async fn session(engine: State<'_, AppState>, tab_id: String) -> ApiResult<SessionSnapshot> {
    let engine = engine.inner().clone();
    blocking("session", json!({ "tabId": tab_id }), move || engine.snapshot(&tab_id).map_err(error)).await
}
#[tauri::command]
async fn open_file(engine: State<'_, AppState>, path: PathBuf, spec: Option<ParseSpec>) -> ApiResult<String> {
    let engine = engine.inner().clone();
    blocking("open_file", json!({ "path": path, "parseSpec": spec }),
        move || engine.open(path, spec.unwrap_or_default()).map_err(error)).await
}
#[tauri::command]
async fn commit_view(engine: State<'_, AppState>, tab_id: String, revision: u64, view: ViewState) -> ApiResult<SessionSnapshot> {
    let engine = engine.inner().clone();
    blocking("commit_view", json!({ "tabId": tab_id, "revision": revision, "filterColumns": view.filters.len(), "sortKeys": view.sort.len() }),
        move || engine.commit(&tab_id, revision, view).map_err(error)).await
}
#[tauri::command]
async fn history_action(engine: State<'_, AppState>, tab_id: String, revision: u64, redo: bool) -> ApiResult<SessionSnapshot> {
    let engine = engine.inner().clone();
    blocking("history_action", json!({ "tabId": tab_id, "revision": revision, "redo": redo }),
        move || engine.history(&tab_id, revision, redo).map_err(error)).await
}
#[tauri::command]
async fn request_projection(engine: State<'_, AppState>, tab_id: String, revision: u64, columns: Vec<usize>) -> ApiResult<String> {
    let engine = engine.inner().clone();
    blocking("request_projection", json!({ "tabId": tab_id, "revision": revision, "columns": columns }), move || {
        if engine.snapshot(&tab_id).map_err(error)?.revision != revision { return Err("Stale：列投影上下文已过期".into()); }
        engine.start_query(&tab_id, columns).map_err(error)
    }).await
}
#[tauri::command]
async fn read_page(engine: State<'_, AppState>, tab_id: String, query_id: String, start: String, count: usize, columns: Vec<usize>) -> ApiResult<tauri::ipc::Response> {
    let engine = engine.inner().clone();
    blocking("read_page", json!({ "tabId": tab_id, "queryId": query_id, "start": start, "count": count, "columns": columns }),
        move || engine.page(&tab_id, &query_id, start.parse().map_err(error)?, count, columns)
            .map(tauri::ipc::Response::new).map_err(error)).await
}
#[tauri::command]
async fn full_cell(engine: State<'_, AppState>, tab_id: String, rid: String, column: usize) -> ApiResult<String> {
    let engine = engine.inner().clone();
    blocking("full_cell", json!({ "tabId": tab_id, "rid": rid, "column": column }),
        move || engine.full_cell(&tab_id, rid.parse().map_err(error)?, column).map_err(error)).await
}
#[tauri::command]
async fn copy_cells(engine: State<'_, AppState>, tab_id: String, query_id: String, start: String, count: usize, columns: Vec<usize>, include_rid: bool) -> ApiResult<Vec<Vec<String>>> {
    let engine = engine.inner().clone();
    blocking("copy_cells", json!({ "tabId": tab_id, "queryId": query_id, "start": start, "count": count, "columns": columns }),
        move || engine.copy_cells(&tab_id, &query_id, start.parse().map_err(error)?, count, columns, include_rid).map_err(error)).await
}
#[tauri::command]
async fn open_candidates(engine: State<'_, AppState>, tab_id: String, revision: u64, column: usize, search: String, searches: Option<Vec<String>>, selected_values: Option<Vec<FilterValue>>) -> ApiResult<String> {
    let engine = engine.inner().clone();
    blocking("open_candidates", json!({ "tabId": tab_id, "revision": revision, "column": column, "searchBytes": search.len() }),
        move || engine.candidates_with_rules(&tab_id, revision, column, search, searches.unwrap_or_default(), selected_values.unwrap_or_default()).map_err(error)).await
}
#[tauri::command]
async fn candidate_status(engine: State<'_, AppState>, candidate_id: String) -> ApiResult<CandidateSnapshot> {
    let engine = engine.inner().clone();
    blocking("candidate_status", json!({ "candidateId": candidate_id }), move || engine.candidate_snapshot(&candidate_id).map_err(error)).await
}
#[tauri::command]
async fn candidate_page(engine: State<'_, AppState>, candidate_id: String, cursor: String) -> ApiResult<CandidatePage> {
    let engine = engine.inner().clone();
    blocking("candidate_page", json!({ "candidateId": candidate_id, "cursor": cursor }),
        move || engine.candidate_page(&candidate_id, cursor.parse().map_err(error)?).map_err(error)).await
}
#[tauri::command]
async fn close_candidates(engine: State<'_, AppState>, candidate_id: String) -> ApiResult<()> {
    let engine = engine.inner().clone();
    blocking("close_candidates", json!({ "candidateId": candidate_id }), move || { engine.close_candidate(&candidate_id); Ok(()) }).await
}
#[tauri::command]
async fn cancel_task(engine: State<'_, AppState>, tab_id: String) -> ApiResult<()> {
    let engine = engine.inner().clone();
    blocking("cancel_task", json!({ "tabId": tab_id }), move || engine.cancel(&tab_id).map_err(error)).await
}
#[tauri::command]
async fn close_file(engine: State<'_, AppState>, tab_id: String) -> ApiResult<()> {
    let engine = engine.inner().clone();
    blocking("close_file", json!({ "tabId": tab_id }), move || engine.close(&tab_id).map_err(error)).await
}
#[tauri::command]
async fn activate_file(engine: State<'_, AppState>, tab_id: String) -> ApiResult<()> {
    let engine = engine.inner().clone();
    blocking("activate_file", json!({ "tabId": tab_id }), move || engine.activate(&tab_id).map_err(error)).await
}
#[tauri::command]
async fn start_export(engine: State<'_, AppState>, tab_id: String, query_id: String, spec: ExportSpec) -> ApiResult<String> {
    let engine = engine.inner().clone();
    blocking("start_export", json!({ "tabId": tab_id, "queryId": query_id, "path": spec.path, "columns": spec.columns }),
        move || engine.export(&tab_id, &query_id, spec).map_err(error)).await
}
#[tauri::command]
async fn export_status(engine: State<'_, AppState>, export_id: String) -> ApiResult<ExportSnapshot> {
    let engine = engine.inner().clone();
    blocking("export_status", json!({ "exportId": export_id }), move || engine.export_snapshot(&export_id).map_err(error)).await
}
#[tauri::command]
async fn cancel_export(engine: State<'_, AppState>, export_id: String) -> ApiResult<()> {
    let engine = engine.inner().clone();
    blocking("cancel_export", json!({ "exportId": export_id }), move || engine.cancel_export(&export_id).map_err(error)).await
}
#[tauri::command]
async fn cache_usage(engine: State<'_, AppState>) -> ApiResult<Value> {
    let engine = engine.inner().clone();
    blocking("cache_usage", json!({}), move || Ok(engine.cache_usage())).await
}
#[tauri::command]
async fn clear_cache(engine: State<'_, AppState>) -> ApiResult<String> {
    let engine = engine.inner().clone();
    blocking("clear_cache", json!({}), move || engine.clear_unused_cache().map(|n| n.to_string()).map_err(error)).await
}

#[tauri::command]
async fn frontend_heartbeat(health: State<'_, Arc<diagnostics::Health>>, visible: bool, pending: Value) -> ApiResult<()> {
    health.frontend_heartbeat(visible, if pending.to_string().len() <= 8192 { pending } else { json!({ "truncated": true }) });
    Ok(())
}
#[tauri::command]
async fn frontend_log(level: String, event: String, details: Value) {
    if event.starts_with("frontend_") && event.len() <= 80 && details.to_string().len() <= 16 * 1024 {
        log::record(if level == "error" { "error" } else { "warn" }, &event, details);
    }
}
#[tauri::command]
async fn diagnostic_info() -> Value {
    json!({ "directory": log::directory(), "activeOperations": log::active_operations() })
}
#[tauri::command]
async fn open_log_directory() -> ApiResult<String> {
    blocking("open_log_directory", json!({}), || {
        let directory = log::directory().ok_or("日志目录不可用")?;
        #[cfg(target_os = "windows")]
        std::process::Command::new("explorer.exe").arg(&directory).spawn().map_err(error)?;
        Ok(directory.to_string_lossy().into_owned())
    }).await
}

fn main() {
    diagnostics::init_logging();
    let health = diagnostics::Health::new();
    tauri::Builder::default()
        .manage(health.clone())
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let path = std::env::var_os("CSVSCOPE_CACHE_DIR").map(PathBuf::from)
                .unwrap_or(app.path().app_cache_dir()?.join("v1"));
            let config = EngineConfig::new(path);
            log::record("info", "engine_config", json!({
                "cacheDirectory": config.cache_dir, "memoryLimitMb": config.memory_limit_mb,
                "threads": config.threads, "reserveBytes": config.reserve_bytes,
                "cacheLimitBytes": config.cache_limit_bytes,
            }));
            let engine = log::measure("engine.initialize", json!({}), false, || Engine::new(config))?;
            diagnostics::start_watchdog(app.handle().clone(), app.state::<Arc<diagnostics::Health>>().inner().clone(), &engine);
            app.manage(engine);
            log::record("info", "app_ready", json!({}));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            sessions, session, open_file, commit_view, history_action, request_projection, read_page, full_cell, copy_cells,
            open_candidates, candidate_status, candidate_page, close_candidates, cancel_task, close_file, activate_file,
            start_export, export_status, cancel_export, cache_usage, clear_cache,
            frontend_heartbeat, frontend_log, diagnostic_info, open_log_directory
        ])
        .build(tauri::generate_context!())
        .expect("无法启动 CSVScope")
        .run(|app, event| {
            if matches!(event, tauri::RunEvent::ExitRequested { .. }) {
                log::record("info", "app_exit_requested", json!({}));
                let engine = app.state::<AppState>().inner().clone();
                std::thread::spawn(move || {
                    for tab in engine.snapshots() { let _ = engine.cancel(&tab.id); }
                });
            }
        });
    health.stop();
    log::record("info", "app_exit", json!({}));
    log::flush();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocking_commands_run_on_a_worker_and_keep_error_details() {
        let caller = std::thread::current().id();
        let worker = tauri::async_runtime::block_on(blocking("test", json!({}), || {
            Ok(std::thread::current().id())
        })).unwrap();
        assert_ne!(worker, caller);
        let result: ApiResult<()> = tauri::async_runtime::block_on(blocking("test_error", json!({}), || {
            Err("磁盘暂时不可用：测试错误".into())
        }));
        assert_eq!(result.unwrap_err(), "磁盘暂时不可用：测试错误");
    }
}
