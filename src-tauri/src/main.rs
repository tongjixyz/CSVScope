#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use csv_workbench_core::*;
use std::{path::PathBuf, sync::Arc};
use tauri::{Manager, State};

type AppState = Arc<Engine>;
type ApiResult<T> = Result<T, String>;
fn error(e: impl std::fmt::Display) -> String { e.to_string() }

#[tauri::command]
fn sessions(engine: State<AppState>) -> Vec<SessionSnapshot> { engine.snapshots() }
#[tauri::command]
fn session(engine: State<AppState>, tab_id: String) -> ApiResult<SessionSnapshot> { engine.snapshot(&tab_id).map_err(error) }
#[tauri::command]
fn open_file(engine: State<AppState>, path: PathBuf, spec: Option<ParseSpec>) -> ApiResult<String> {
    engine.inner().open(path, spec.unwrap_or_default()).map_err(error)
}
#[tauri::command]
fn commit_view(engine: State<AppState>, tab_id: String, revision: u64, view: ViewState) -> ApiResult<SessionSnapshot> {
    engine.inner().commit(&tab_id, revision, view).map_err(error)
}
#[tauri::command]
fn history_action(engine: State<AppState>, tab_id: String, revision: u64, redo: bool) -> ApiResult<SessionSnapshot> {
    engine.inner().history(&tab_id, revision, redo).map_err(error)
}
#[tauri::command]
fn request_projection(engine: State<AppState>, tab_id: String, revision: u64, columns: Vec<usize>) -> ApiResult<String> {
    if engine.snapshot(&tab_id).map_err(error)?.revision != revision { return Err("Stale：列投影上下文已过期".into()); }
    engine.inner().start_query(&tab_id, columns).map_err(error)
}
#[tauri::command]
async fn read_page(engine: State<'_, AppState>, tab_id: String, query_id: String, start: String, count: usize, columns: Vec<usize>) -> ApiResult<tauri::ipc::Response> {
    let engine = engine.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let start = start.parse().map_err(error)?;
        engine.page(&tab_id, &query_id, start, count, columns).map(tauri::ipc::Response::new).map_err(error)
    }).await.map_err(error)?
}
#[tauri::command]
async fn full_cell(engine: State<'_, AppState>, tab_id: String, rid: String, column: usize) -> ApiResult<String> {
    let engine = engine.inner().clone();
    tauri::async_runtime::spawn_blocking(move || engine.full_cell(&tab_id, rid.parse().map_err(error)?, column).map_err(error)).await.map_err(error)?
}
#[tauri::command]
async fn copy_cells(engine: State<'_, AppState>, tab_id: String, query_id: String, start: String, count: usize, columns: Vec<usize>, include_rid: bool) -> ApiResult<Vec<Vec<String>>> {
    let engine = engine.inner().clone();
    tauri::async_runtime::spawn_blocking(move || engine.copy_cells(&tab_id, &query_id, start.parse().map_err(error)?, count, columns, include_rid).map_err(error)).await.map_err(error)?
}
#[tauri::command]
fn open_candidates(engine: State<AppState>, tab_id: String, revision: u64, column: usize, search: String, searches: Option<Vec<String>>, selected_values: Option<Vec<FilterValue>>) -> ApiResult<String> {
    engine.inner().candidates_with_rules(&tab_id, revision, column, search, searches.unwrap_or_default(), selected_values.unwrap_or_default()).map_err(error)
}
#[tauri::command]
fn candidate_status(engine: State<AppState>, candidate_id: String) -> ApiResult<CandidateSnapshot> {
    engine.candidate_snapshot(&candidate_id).map_err(error)
}
#[tauri::command]
async fn candidate_page(engine: State<'_, AppState>, candidate_id: String, cursor: String) -> ApiResult<CandidatePage> {
    let engine = engine.inner().clone();
    tauri::async_runtime::spawn_blocking(move || engine.candidate_page(&candidate_id, cursor.parse().map_err(error)?).map_err(error)).await.map_err(error)?
}
#[tauri::command]
fn close_candidates(engine: State<AppState>, candidate_id: String) { engine.close_candidate(&candidate_id); }
#[tauri::command]
fn cancel_task(engine: State<AppState>, tab_id: String) -> ApiResult<()> { engine.cancel(&tab_id).map_err(error) }
#[tauri::command]
fn close_file(engine: State<AppState>, tab_id: String) -> ApiResult<()> { engine.close(&tab_id).map_err(error) }
#[tauri::command]
fn activate_file(engine: State<AppState>, tab_id: String) -> ApiResult<()> { engine.activate(&tab_id).map_err(error) }
#[tauri::command]
fn start_export(engine: State<AppState>, tab_id: String, query_id: String, spec: ExportSpec) -> ApiResult<String> {
    engine.inner().export(&tab_id, &query_id, spec).map_err(error)
}
#[tauri::command]
fn export_status(engine: State<AppState>, export_id: String) -> ApiResult<ExportSnapshot> { engine.export_snapshot(&export_id).map_err(error) }
#[tauri::command]
fn cancel_export(engine: State<AppState>, export_id: String) -> ApiResult<()> { engine.cancel_export(&export_id).map_err(error) }
#[tauri::command]
fn cache_usage(engine: State<AppState>) -> serde_json::Value { engine.cache_usage() }
#[tauri::command]
async fn clear_cache(engine: State<'_, AppState>) -> ApiResult<String> {
    let engine = engine.inner().clone();
    tauri::async_runtime::spawn_blocking(move || engine.clear_unused_cache().map(|n| n.to_string()).map_err(error)).await.map_err(error)?
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let path = std::env::var_os("CSV_WORKBENCH_CACHE_DIR").map(PathBuf::from)
                .unwrap_or(app.path().app_cache_dir()?.join("v1"));
            app.manage(Engine::new(EngineConfig::new(path))?);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            sessions, session, open_file, commit_view, history_action, request_projection, read_page, full_cell, copy_cells,
            open_candidates, candidate_status, candidate_page, close_candidates, cancel_task, close_file, activate_file,
            start_export, export_status, cancel_export, cache_usage, clear_cache
        ])
        .build(tauri::generate_context!())
        .expect("无法启动 CSV Workbench")
        .run(|app, event| {
            if matches!(event, tauri::RunEvent::ExitRequested { .. }) {
                let engine = app.state::<AppState>();
                for tab in engine.snapshots() { let _ = engine.cancel(&tab.id); }
            }
        });
}
