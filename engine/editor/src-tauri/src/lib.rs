pub mod project;
pub mod supervisor;

use std::path::Path;

use project::ProjectSummary;
use supervisor::{EngineHostStatus, EngineLogMessage, EngineSupervisor};
use tauri::{AppHandle, Manager, RunEvent, State};

#[tauri::command]
fn list_sample_projects() -> Vec<ProjectSummary> {
    project::list_sample_projects()
}

#[tauri::command]
fn load_project_metadata(path: String) -> Result<ProjectSummary, String> {
    project::load_project_summary(Path::new(&path)).map_err(|err| format!("{err:#}"))
}

#[tauri::command]
fn get_engine_status(supervisor: State<'_, EngineSupervisor>) -> EngineHostStatus {
    supervisor.status()
}

#[tauri::command]
fn get_engine_logs(supervisor: State<'_, EngineSupervisor>) -> Vec<EngineLogMessage> {
    supervisor.recent_logs()
}

#[tauri::command]
fn clear_engine_logs(supervisor: State<'_, EngineSupervisor>) {
    supervisor.clear_logs();
}

#[tauri::command]
async fn start_engine_host(
    app: AppHandle,
    supervisor: State<'_, EngineSupervisor>,
    project_path: String,
) -> Result<EngineHostStatus, String> {
    let sup = supervisor.inner().clone();
    tauri::async_runtime::spawn_blocking(move || sup.start(&app, &project_path))
        .await
        .map_err(|err| format!("Failed to join start_engine_host task: {err}"))?
}

#[tauri::command]
async fn stop_engine_host(
    app: AppHandle,
    supervisor: State<'_, EngineSupervisor>,
) -> Result<EngineHostStatus, String> {
    let sup = supervisor.inner().clone();
    tauri::async_runtime::spawn_blocking(move || sup.stop(&app))
        .await
        .map_err(|err| format!("Failed to join stop_engine_host task: {err}"))
}

#[tauri::command]
async fn restart_engine_host(
    app: AppHandle,
    supervisor: State<'_, EngineSupervisor>,
) -> Result<EngineHostStatus, String> {
    let sup = supervisor.inner().clone();
    tauri::async_runtime::spawn_blocking(move || sup.restart(&app))
        .await
        .map_err(|err| format!("Failed to join restart_engine_host task: {err}"))?
}

/// Boots the Bevy9 Tauri v2 desktop application.
pub fn run() {
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            if let Some(win) = app.get_webview_window("main") {
                let _ = win.set_zoom(1.0);
            }
            Ok(())
        })
        .manage(EngineSupervisor::new())
        .invoke_handler(tauri::generate_handler![
            list_sample_projects,
            load_project_metadata,
            get_engine_status,
            get_engine_logs,
            clear_engine_logs,
            start_engine_host,
            stop_engine_host,
            restart_engine_host,
        ])
        .build(tauri::generate_context!())
        .expect("error while building Bevy9 Editor Tauri application");

    app.run(|app_handle, event| {
        if let RunEvent::Exit = event {
            let supervisor = app_handle.state::<EngineSupervisor>();
            let _ = supervisor.stop(app_handle);
        }
    });
}
