use crate::config::{AppConfig, RenameRecord, WatchedFolder, HISTORY_LIMIT};
use crate::normalizer;
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter, Manager};
use walkdir::WalkDir;

pub struct EngineState {
    pub config: Mutex<AppConfig>,
    pub history: Mutex<Vec<RenameRecord>>,
    pub enabled: AtomicBool,
    pub job_tx: Sender<PathBuf>,
    pub watcher: Mutex<Option<RecommendedWatcher>>,
    /// NFC 저장이 불가능한 경로(HFS+ 등) — 이벤트 무한루프 방지용
    pub failed: Mutex<HashSet<PathBuf>>,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub fn record_rename(app: &AppHandle, dir: &Path, from: String, to: String) {
    let state = app.state::<EngineState>();
    let record = RenameRecord {
        at: now_ms(),
        dir: dir.to_string_lossy().into_owned(),
        from,
        to,
    };
    {
        let mut history = state.history.lock().unwrap();
        history.insert(0, record.clone());
        history.truncate(HISTORY_LIMIT);
        crate::config::save_history(app, &history);
    }
    let _ = app.emit("rename-recorded", &record);
}

/// 감시 폴더 목록에 비추어 이 경로를 처리해야 하는지 판별.
/// (macOS FSEvents는 항상 재귀적으로 이벤트를 주므로, 비재귀 폴더는 직계 자식만 허용)
fn is_in_scope(folders: &[WatchedFolder], path: &Path) -> bool {
    folders.iter().any(|f| {
        let root = Path::new(&f.path);
        if f.recursive {
            path.starts_with(root) && path != root
        } else {
            path.parent() == Some(root)
        }
    })
}

/// 단일 경로 처리 (재시도 포함). 디렉터리를 개명한 경우 내부를 추가 스캔한다.
fn process_path(app: &AppHandle, path: &Path) {
    let state = app.state::<EngineState>();
    if !state.enabled.load(Ordering::Relaxed) {
        return;
    }
    let (in_scope, recursive_scope) = {
        let config = state.config.lock().unwrap();
        let scope = is_in_scope(&config.folders, path);
        let recursive = config.folders.iter().any(|f| {
            f.recursive && path.starts_with(Path::new(&f.path)) && path != Path::new(&f.path)
        });
        (scope, recursive)
    };
    if !in_scope {
        return;
    }
    if state.failed.lock().unwrap().contains(path) {
        return;
    }

    for attempt in 0..3 {
        match normalizer::try_normalize(path) {
            Ok(None) => return,
            Ok(Some((from, to))) => {
                let parent = path.parent().unwrap_or(Path::new(""));
                let new_path = parent.join(&to);
                record_rename(app, parent, from, to.clone());
                // NFD 이름의 폴더가 통째로 들어온 경우: 폴더 개명 후 내부 항목도 정리
                if recursive_scope && new_path.is_dir() {
                    scan_dir(app, &new_path, true);
                }
                return;
            }
            Err(_) if attempt < 2 => {
                // Windows에서 쓰기 중인 파일은 개명이 실패할 수 있으므로 잠시 후 재시도
                std::thread::sleep(Duration::from_secs(1));
            }
            Err(_) => {
                let mut failed = state.failed.lock().unwrap();
                if failed.len() < 200 {
                    failed.insert(path.to_path_buf());
                }
                return;
            }
        }
    }
}

/// 폴더 하나를 스캔해서 정규화. 반환값은 변경 건수.
/// contents_first: 하위 항목을 먼저 개명한 뒤 상위 폴더를 개명해야 경로가 꼬이지 않는다.
pub fn scan_dir(app: &AppHandle, root: &Path, recursive: bool) -> usize {
    let mut count = 0;
    let max_depth = if recursive { usize::MAX } else { 1 };
    let walker = WalkDir::new(root)
        .min_depth(1)
        .max_depth(max_depth)
        .contents_first(true)
        .into_iter()
        .filter_entry(|e| {
            e.file_name()
                .to_str()
                .map(|n| !n.starts_with('.'))
                .unwrap_or(false)
        });
    for entry in walker.filter_map(|e| e.ok()) {
        if let Ok(Some((from, to))) = normalizer::try_normalize(entry.path()) {
            let parent = entry.path().parent().unwrap_or(root);
            record_rename(app, parent, from, to);
            count += 1;
        }
    }
    count
}

/// 등록된 모든 감시 폴더를 스캔.
pub fn scan_all(app: &AppHandle) -> usize {
    let folders = {
        let state = app.state::<EngineState>();
        state.failed.lock().unwrap().clear();
        let folders = state.config.lock().unwrap().folders.clone();
        folders
    };
    folders
        .iter()
        .map(|f| scan_dir(app, Path::new(&f.path), f.recursive))
        .sum()
}

/// 파일시스템 이벤트를 받아 작업 큐로 넘기는 워처를 (재)구성한다.
pub fn rebuild_watcher(app: &AppHandle) {
    let state = app.state::<EngineState>();
    let folders = state.config.lock().unwrap().folders.clone();

    // 기존 워처 해제
    *state.watcher.lock().unwrap() = None;
    if folders.is_empty() {
        return;
    }

    let tx = state.job_tx.clone();
    let mut watcher = match notify::recommended_watcher(move |result: notify::Result<notify::Event>| {
        if let Ok(event) = result {
            use notify::EventKind;
            match event.kind {
                EventKind::Create(_) | EventKind::Modify(_) | EventKind::Any => {
                    for path in event.paths {
                        let _ = tx.send(path);
                    }
                }
                _ => {}
            }
        }
    }) {
        Ok(w) => w,
        Err(_) => return,
    };

    for folder in &folders {
        let mode = if folder.recursive {
            RecursiveMode::Recursive
        } else {
            RecursiveMode::NonRecursive
        };
        let _ = watcher.watch(Path::new(&folder.path), mode);
    }
    *state.watcher.lock().unwrap() = Some(watcher);
}

/// 이벤트 큐를 소비하는 워커 스레드 시작.
pub fn spawn_worker(app: AppHandle, rx: Receiver<PathBuf>) {
    std::thread::spawn(move || {
        while let Ok(path) = rx.recv() {
            process_path(&app, &path);
        }
    });
}
