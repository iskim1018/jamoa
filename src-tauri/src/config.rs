use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use tauri::{AppHandle, Manager};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WatchedFolder {
    pub path: String,
    pub recursive: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    pub enabled: bool,
    pub folders: Vec<WatchedFolder>,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            // 첫 실행 시 사용자가 감시 폴더를 확인하고 직접 켜도록 꺼진 상태로 시작
            enabled: false,
            folders: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RenameRecord {
    /// Unix epoch milliseconds
    pub at: u64,
    pub dir: String,
    pub from: String,
    pub to: String,
}

pub const HISTORY_LIMIT: usize = 300;

fn config_dir(app: &AppHandle) -> PathBuf {
    let dir = app
        .path()
        .app_config_dir()
        .expect("cannot resolve app config dir");
    let _ = fs::create_dir_all(&dir);
    dir
}

fn config_path(app: &AppHandle) -> PathBuf {
    config_dir(app).join("config.json")
}

fn history_path(app: &AppHandle) -> PathBuf {
    config_dir(app).join("history.json")
}

pub fn load_config(app: &AppHandle) -> AppConfig {
    let path = config_path(app);
    match fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
        Err(_) => {
            // 첫 실행: 다운로드 폴더를 기본 감시 대상으로 등록
            let mut config = AppConfig::default();
            if let Ok(downloads) = app.path().download_dir() {
                config.folders.push(WatchedFolder {
                    path: downloads.to_string_lossy().into_owned(),
                    recursive: false,
                });
            }
            save_config(app, &config);
            config
        }
    }
}

pub fn save_config(app: &AppHandle, config: &AppConfig) {
    if let Ok(text) = serde_json::to_string_pretty(config) {
        let _ = fs::write(config_path(app), text);
    }
}

pub fn load_history(app: &AppHandle) -> Vec<RenameRecord> {
    fs::read_to_string(history_path(app))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

pub fn save_history(app: &AppHandle, history: &[RenameRecord]) {
    if let Ok(text) = serde_json::to_string(history) {
        let _ = fs::write(history_path(app), text);
    }
}
