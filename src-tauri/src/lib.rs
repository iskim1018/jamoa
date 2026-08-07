mod config;
mod engine;
mod normalizer;

use config::{AppConfig, RenameRecord, WatchedFolder};
use engine::EngineState;
use serde::Serialize;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Mutex};
use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Emitter, Manager, WindowEvent, Wry};
use tauri_plugin_autostart::{MacosLauncher, ManagerExt};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons};
use tauri_plugin_updater::UpdaterExt;

struct TrayHandles {
    enabled_item: Mutex<Option<CheckMenuItem<Wry>>>,
}

#[derive(Serialize)]
struct UiState {
    config: AppConfig,
    autostart: bool,
}

fn show_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

fn apply_enabled(app: &AppHandle, enabled: bool) {
    let state = app.state::<EngineState>();
    state.enabled.store(enabled, Ordering::Relaxed);
    {
        let mut config = state.config.lock().unwrap();
        config.enabled = enabled;
        config::save_config(app, &config);
    }
    let tray = app.state::<TrayHandles>();
    if let Some(item) = tray.enabled_item.lock().unwrap().as_ref() {
        let _ = item.set_checked(enabled);
    }
    let _ = app.emit("config-changed", ());
    if enabled {
        // 꺼져 있던 동안 쌓인 파일 정리
        let app = app.clone();
        std::thread::spawn(move || {
            engine::scan_all(&app);
        });
    }
}

#[tauri::command]
fn get_state(app: AppHandle) -> UiState {
    let state = app.state::<EngineState>();
    let config = state.config.lock().unwrap().clone();
    let autostart = app.autolaunch().is_enabled().unwrap_or(false);
    UiState { config, autostart }
}

#[tauri::command]
fn get_history(app: AppHandle) -> Vec<RenameRecord> {
    app.state::<EngineState>().history.lock().unwrap().clone()
}

#[tauri::command]
fn set_enabled(app: AppHandle, enabled: bool) {
    apply_enabled(&app, enabled);
}

#[tauri::command]
async fn pick_and_add_folder(app: AppHandle) -> Result<bool, String> {
    let (tx, rx) = mpsc::channel();
    app.dialog().file().pick_folder(move |folder| {
        let _ = tx.send(folder);
    });
    let picked = tauri::async_runtime::spawn_blocking(move || rx.recv().ok().flatten())
        .await
        .map_err(|e| e.to_string())?;
    let Some(folder) = picked else {
        return Ok(false);
    };
    let path = folder.into_path().map_err(|e| e.to_string())?;
    let path_str = path.to_string_lossy().into_owned();

    let state = app.state::<EngineState>();
    {
        let mut config = state.config.lock().unwrap();
        if config.folders.iter().any(|f| f.path == path_str) {
            return Ok(false);
        }
        config.folders.push(WatchedFolder {
            path: path_str,
            recursive: false,
        });
        config::save_config(&app, &config);
    }
    engine::rebuild_watcher(&app);
    let _ = app.emit("config-changed", ());
    if state.enabled.load(Ordering::Relaxed) {
        let app2 = app.clone();
        std::thread::spawn(move || {
            engine::scan_dir(&app2, &path, false);
        });
    }
    Ok(true)
}

#[tauri::command]
fn remove_folder(app: AppHandle, path: String) {
    let state = app.state::<EngineState>();
    {
        let mut config = state.config.lock().unwrap();
        config.folders.retain(|f| f.path != path);
        config::save_config(&app, &config);
    }
    engine::rebuild_watcher(&app);
    let _ = app.emit("config-changed", ());
}

#[tauri::command]
fn set_recursive(app: AppHandle, path: String, recursive: bool) {
    let state = app.state::<EngineState>();
    {
        let mut config = state.config.lock().unwrap();
        if let Some(folder) = config.folders.iter_mut().find(|f| f.path == path) {
            folder.recursive = recursive;
        }
        config::save_config(&app, &config);
    }
    engine::rebuild_watcher(&app);
    let _ = app.emit("config-changed", ());
    if recursive && state.enabled.load(Ordering::Relaxed) {
        let app2 = app.clone();
        std::thread::spawn(move || {
            engine::scan_dir(&app2, Path::new(&path), true);
        });
    }
}

#[tauri::command]
fn set_autostart(app: AppHandle, enabled: bool) -> Result<(), String> {
    let autolaunch = app.autolaunch();
    if enabled {
        autolaunch.enable().map_err(|e| e.to_string())
    } else {
        autolaunch.disable().map_err(|e| e.to_string())
    }
}

#[tauri::command]
async fn scan_now(app: AppHandle) -> Result<usize, String> {
    tauri::async_runtime::spawn_blocking(move || engine::scan_all(&app))
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn reveal_path(path: String) {
    let _ = tauri_plugin_opener::open_path(path, None::<String>);
}

/// 시작 시 새 버전을 확인하고, 있으면 사용자 확인 후 설치·재시작한다.
fn spawn_update_check(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let Ok(updater) = app.updater() else { return };
        let Ok(Some(update)) = updater.check().await else {
            return;
        };
        let version = update.version.clone();
        let app2 = app.clone();
        app.dialog()
            .message(format!(
                "새 버전 v{version}이 나왔습니다. 지금 업데이트할까요?\n(다운로드 후 자동으로 재시작됩니다)"
            ))
            .title("Jamoa 업데이트")
            .buttons(MessageDialogButtons::OkCancelCustom(
                "업데이트".into(),
                "나중에".into(),
            ))
            .show(move |confirmed| {
                if !confirmed {
                    return;
                }
                tauri::async_runtime::spawn(async move {
                    match update.download_and_install(|_, _| {}, || {}).await {
                        Ok(()) => app2.restart(),
                        Err(e) => {
                            app2.dialog()
                                .message(format!("업데이트에 실패했습니다: {e}"))
                                .title("Jamoa 업데이트")
                                .show(|_| {});
                        }
                    }
                });
            });
    });
}

fn build_tray(app: &AppHandle) -> tauri::Result<()> {
    let enabled = app
        .state::<EngineState>()
        .enabled
        .load(Ordering::Relaxed);

    let open_item = MenuItem::with_id(app, "open", "설정 열기…", true, None::<&str>)?;
    let scan_item = MenuItem::with_id(app, "scan", "지금 전체 검사", true, None::<&str>)?;
    let enabled_item =
        CheckMenuItem::with_id(app, "enabled", "자동 정규화", true, enabled, None::<&str>)?;
    let quit_item = MenuItem::with_id(app, "quit", "종료", true, None::<&str>)?;
    let separator1 = PredefinedMenuItem::separator(app)?;
    let separator2 = PredefinedMenuItem::separator(app)?;
    let menu = Menu::with_items(
        app,
        &[
            &open_item,
            &scan_item,
            &separator1,
            &enabled_item,
            &separator2,
            &quit_item,
        ],
    )?;

    *app.state::<TrayHandles>().enabled_item.lock().unwrap() = Some(enabled_item);

    let tray_builder = TrayIconBuilder::with_id("main-tray");
    // macOS 메뉴바는 단색 템플릿 아이콘 관례를 따른다 (시스템이 라이트/다크 자동 반전)
    #[cfg(target_os = "macos")]
    let tray_builder = tray_builder
        .icon(tauri::image::Image::from_bytes(include_bytes!(
            "../icons/tray-template@2x.png"
        ))?)
        .icon_as_template(true);
    #[cfg(not(target_os = "macos"))]
    let tray_builder = tray_builder
        .icon(app.default_window_icon().unwrap().clone())
        .icon_as_template(false);

    tray_builder
        .tooltip("Jamoa — 한글 파일명 자동 정규화")
        .menu(&menu)
        .show_menu_on_left_click(true)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "open" => show_main_window(app),
            "scan" => {
                let app = app.clone();
                std::thread::spawn(move || {
                    let count = engine::scan_all(&app);
                    let _ = app.emit("scan-done", count);
                });
            }
            "enabled" => {
                let current = app
                    .state::<EngineState>()
                    .enabled
                    .load(Ordering::Relaxed);
                apply_enabled(app, !current);
            }
            "quit" => app.exit(0),
            _ => {}
        })
        .build(app)?;
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            None,
        ))
        .invoke_handler(tauri::generate_handler![
            get_state,
            get_history,
            set_enabled,
            pick_and_add_folder,
            remove_folder,
            set_recursive,
            set_autostart,
            scan_now,
            reveal_path
        ])
        .setup(|app| {
            let handle = app.handle().clone();

            // macOS: Dock 아이콘 없이 메뉴바 상주 앱으로 동작
            #[cfg(target_os = "macos")]
            let _ = handle.set_activation_policy(tauri::ActivationPolicy::Accessory);

            let config = config::load_config(&handle);
            let history = config::load_history(&handle);
            let enabled = config.enabled;
            let (job_tx, job_rx) = mpsc::channel();

            app.manage(EngineState {
                config: Mutex::new(config),
                history: Mutex::new(history),
                enabled: AtomicBool::new(enabled),
                job_tx,
                watcher: Mutex::new(None),
                failed: Mutex::new(Default::default()),
            });
            app.manage(TrayHandles {
                enabled_item: Mutex::new(None),
            });

            engine::spawn_worker(handle.clone(), job_rx);
            engine::rebuild_watcher(&handle);
            build_tray(&handle)?;
            spawn_update_check(&handle);

            // 시작 시 밀린 파일 1회 정리
            if enabled {
                let scan_handle = handle.clone();
                std::thread::spawn(move || {
                    engine::scan_all(&scan_handle);
                });
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            // 창을 닫아도 종료하지 않고 트레이로 숨긴다
            if let WindowEvent::CloseRequested { api, .. } = event {
                let _ = window.hide();
                api.prevent_close();
            }
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|_app, event| {
            if let tauri::RunEvent::ExitRequested { api, code, .. } = event {
                // 마지막 창이 닫혀도 백그라운드 상주 유지 (명시적 종료만 허용)
                if code.is_none() {
                    api.prevent_exit();
                }
            }
        });
}
