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
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_updater::UpdaterExt;

struct UpdateState {
    pending: Mutex<Option<tauri_plugin_updater::Update>>,
}

struct TrayHandles {
    enabled_item: Mutex<Option<CheckMenuItem<Wry>>>,
    update_item: Mutex<Option<MenuItem<Wry>>>,
    tray: Mutex<Option<tauri::tray::TrayIcon>>,
}

/// 트레이 아이콘을 업데이트 배지 유무에 맞게 설정한다.
fn set_tray_icon(app: &AppHandle, update_available: bool) {
    let handles = app.state::<TrayHandles>();
    let tray_guard = handles.tray.lock().unwrap();
    let Some(tray) = tray_guard.as_ref() else { return };
    #[cfg(target_os = "macos")]
    {
        let bytes: &[u8] = if update_available {
            include_bytes!("../icons/tray-template-update@2x.png")
        } else {
            include_bytes!("../icons/tray-template@2x.png")
        };
        if let Ok(icon) = tauri::image::Image::from_bytes(bytes) {
            let _ = tray.set_icon(Some(icon));
            let _ = tray.set_icon_as_template(true);
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        if update_available {
            if let Ok(icon) =
                tauri::image::Image::from_bytes(include_bytes!("../icons/tray-update-color.png"))
            {
                let _ = tray.set_icon(Some(icon));
            }
        } else if let Some(icon) = app.default_window_icon() {
            let _ = tray.set_icon(Some(icon.clone()));
        }
    }
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

/// 시작 시 그리고 6시간마다 새 버전을 확인한다.
fn spawn_update_check(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            check_for_update(&app, false).await;
            tokio::time::sleep(std::time::Duration::from_secs(6 * 60 * 60)).await;
        }
    });
}

/// 새 버전 확인. 발견하면 트레이 배지·메뉴·창 배너에 반영한다.
/// manual(트레이 메뉴에서 직접 확인)일 때만 "최신 버전" 안내도 보여준다.
async fn check_for_update(app: &AppHandle, manual: bool) {
    if app.state::<UpdateState>().pending.lock().unwrap().is_some() {
        return; // 이미 안내 중인 업데이트가 있음
    }
    let updater = match app.updater() {
        Ok(u) => u,
        Err(e) => {
            eprintln!("[updater] init error: {e}");
            return;
        }
    };
    match updater.check().await {
        Ok(Some(update)) => {
            eprintln!("[updater] update found: v{}", update.version);
            let version = update.version.clone();
            *app.state::<UpdateState>().pending.lock().unwrap() = Some(update);
            let handles = app.state::<TrayHandles>();
            if let Some(item) = handles.update_item.lock().unwrap().as_ref() {
                let _ = item.set_text(format!("v{version}(으)로 업데이트…"));
            }
            set_tray_icon(app, true);
            let _ = app.emit("update-available", version);
        }
        Ok(None) => {
            eprintln!("[updater] no update available");
            if manual {
                show_main_window(app);
                let _ = app.emit("update-none", ());
            }
        }
        Err(e) => {
            eprintln!("[updater] check error: {e}");
            if manual {
                show_main_window(app);
                let _ = app.emit("update-check-failed", e.to_string());
            }
        }
    }
}

#[tauri::command]
fn get_pending_update(app: AppHandle) -> Option<String> {
    app.state::<UpdateState>()
        .pending
        .lock()
        .unwrap()
        .as_ref()
        .map(|u| u.version.clone())
}

#[tauri::command]
async fn install_update(app: AppHandle) -> Result<(), String> {
    // 실패 후에도 다시 시도할 수 있도록 성공(재시작) 전까지 pending을 유지한다
    let update = app.state::<UpdateState>().pending.lock().unwrap().clone();
    let Some(update) = update else {
        return Err("설치할 업데이트가 없습니다".into());
    };
    update
        .download_and_install(|_, _| {}, || {})
        .await
        .map_err(|e| e.to_string())?;
    app.restart();
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
    let update_item = MenuItem::with_id(app, "update", "업데이트 확인", true, None::<&str>)?;
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
            &update_item,
            &quit_item,
        ],
    )?;

    *app.state::<TrayHandles>().enabled_item.lock().unwrap() = Some(enabled_item);
    *app.state::<TrayHandles>().update_item.lock().unwrap() = Some(update_item);

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

    let tray = tray_builder
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
            "update" => {
                let pending_version = app
                    .state::<UpdateState>()
                    .pending
                    .lock()
                    .unwrap()
                    .as_ref()
                    .map(|u| u.version.clone());
                show_main_window(app);
                if let Some(version) = pending_version {
                    // 창의 상단 배너로 안내 (닫았던 경우 다시 표시)
                    let _ = app.emit("update-available", version);
                } else {
                    let app = app.clone();
                    tauri::async_runtime::spawn(async move {
                        check_for_update(&app, true).await;
                    });
                }
            }
            "quit" => app.exit(0),
            _ => {}
        })
        .build(app)?;
    *app.state::<TrayHandles>().tray.lock().unwrap() = Some(tray);
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
            Some(vec!["--hidden"]),
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
            reveal_path,
            get_pending_update,
            install_update
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
                update_item: Mutex::new(None),
                tray: Mutex::new(None),
            });
            app.manage(UpdateState {
                pending: Mutex::new(None),
            });

            engine::spawn_worker(handle.clone(), job_rx);
            engine::rebuild_watcher(&handle);
            build_tray(&handle)?;
            spawn_update_check(&handle);

            // 로그인 자동 시작(--hidden)이면 트레이로만 상주하고 창은 띄우지 않는다
            let start_hidden = std::env::args().any(|arg| arg == "--hidden");
            if !start_hidden {
                show_main_window(&handle);
            }
            // 예전 버전에서 --hidden 없이 등록된 자동 시작 항목을 새 형식으로 갱신
            let autolaunch = handle.autolaunch();
            if autolaunch.is_enabled().unwrap_or(false) {
                let _ = autolaunch.enable();
            }

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
