use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use unicode_normalization::UnicodeNormalization;

/// 다운로드 중이거나 프로그램이 임시로 쓰는 파일은 건드리지 않는다.
const TEMP_EXTENSIONS: &[&str] = &[
    "crdownload", // Chrome/Edge
    "download",   // Safari
    "part",       // Firefox
    "partial",    // IE/legacy
    "opdownload", // Opera
    "aria2",
    "tmp",
    "temp",
];

pub fn nfc(name: &str) -> String {
    name.nfc().collect()
}

/// 파일명이 NFC 정규화가 필요한지 (NFD 자모분리 상태인지) 판별.
pub fn needs_normalization(name: &str) -> bool {
    nfc(name) != name
}

/// 정규화 대상에서 제외할 파일명인지 판별.
pub fn should_skip(name: &str) -> bool {
    if name.starts_with('.') || name.starts_with("~$") {
        return true;
    }
    if let Some((_, ext)) = name.rsplit_once('.') {
        if TEMP_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str()) {
            return true;
        }
    }
    false
}

/// 같은 폴더에 NFC 이름이 이미 (다른 파일로) 존재하면 " (1)" 식 접미사를 붙인다.
fn unique_target(parent: &Path, nfc_name: &str) -> PathBuf {
    let (stem, ext) = match nfc_name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s.to_string(), format!(".{e}")),
        _ => (nfc_name.to_string(), String::new()),
    };
    for i in 1..1000 {
        let candidate = parent.join(format!("{stem} ({i}){ext}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    parent.join(format!("{stem} (dup){ext}"))
}

/// 디렉터리에서 `path`와 같은 파일을 찾아 실제 저장된 이름을 돌려준다.
/// APFS는 정규화 무시 조회를 하므로 NFD 경로 문자열이 이미 NFC로 저장된
/// 파일에 닿을 수 있다 — 경로 문자열이 아니라 저장된 이름으로 판단해야 한다.
fn stored_name_of(parent: &Path, path: &Path) -> io::Result<Option<String>> {
    for entry in fs::read_dir(parent)?.filter_map(|e| e.ok()) {
        if same_file::is_same_file(entry.path(), path).unwrap_or(false) {
            return Ok(entry.file_name().to_str().map(String::from));
        }
    }
    Ok(None)
}

/// 이름 변경 성공 시 (변경 전, 변경 후) 파일명을 돌려준다.
/// 이미 정규화되어 있거나 파일이 사라졌으면 Ok(None).
pub fn try_normalize(path: &Path) -> io::Result<Option<(String, String)>> {
    let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
        return Ok(None);
    };
    if should_skip(name) || !needs_normalization(name) {
        return Ok(None);
    }
    if fs::symlink_metadata(path).is_err() {
        return Ok(None); // 이벤트 처리 전에 파일이 사라짐
    }
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("no parent directory"))?;

    // 저장된 실제 이름 기준으로 재판단 (자기 자신을 재개명하는 루프 방지)
    let mut path = path.to_path_buf();
    let mut name = name.to_string();
    if let Some(stored) = stored_name_of(parent, &path)? {
        if stored != name {
            if should_skip(&stored) || !needs_normalization(&stored) {
                return Ok(None);
            }
            path = parent.join(&stored);
            name = stored;
        }
    }
    let path = path.as_path();
    let name = name.as_str();

    let nfc_name = nfc(name);
    let mut dst = parent.join(&nfc_name);

    // macOS APFS는 정규화 무시(normalization-insensitive) 조회를 하므로
    // NFD 경로와 NFC 경로가 "같은 파일"로 보인다. 이 경우 POSIX rename은
    // 아무 일도 하지 않으므로 임시 이름을 거쳐 2단계로 바꾼다.
    let same = same_file::is_same_file(path, &dst).unwrap_or(false);
    if !same && dst.exists() {
        dst = unique_target(parent, &nfc_name);
    }

    if same {
        let tmp = parent.join(format!(".nfc-tmp-{}", std::process::id()));
        fs::rename(path, &tmp)?;
        if let Err(e) = fs::rename(&tmp, &dst) {
            let _ = fs::rename(&tmp, path); // 원복 시도
            return Err(e);
        }
    } else {
        fs::rename(path, &dst)?;
    }

    // 파일시스템이 NFC를 보존했는지 확인 (HFS+는 강제로 NFD 저장 → 무한 재시도 방지)
    let stored_ok = fs::read_dir(parent)?
        .filter_map(|e| e.ok())
        .any(|e| e.file_name().to_str() == dst.file_name().and_then(|n| n.to_str()));
    if !stored_ok {
        return Err(io::Error::other(
            "filesystem does not preserve NFC names (HFS+?)",
        ));
    }

    Ok(Some((
        name.to_string(),
        dst.file_name().unwrap().to_string_lossy().into_owned(),
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    // "한글" 을 NFD(자모분리)로 표기
    fn nfd(s: &str) -> String {
        use unicode_normalization::UnicodeNormalization;
        s.nfd().collect()
    }

    #[test]
    fn detects_nfd_names() {
        let decomposed = nfd("한글 문서.pdf");
        assert!(needs_normalization(&decomposed));
        assert!(!needs_normalization("한글 문서.pdf"));
        assert!(!needs_normalization("plain-ascii.txt"));
    }

    #[test]
    fn nfc_roundtrip() {
        let decomposed = nfd("보고서 최종.hwp");
        assert_eq!(nfc(&decomposed), "보고서 최종.hwp");
    }

    #[test]
    fn skips_temp_and_hidden() {
        assert!(should_skip(".DS_Store"));
        assert!(should_skip("~$문서.docx"));
        assert!(should_skip("받는중.pdf.crdownload"));
        assert!(should_skip("영화.PART"));
        assert!(!should_skip("보고서.pdf"));
    }

    #[test]
    fn renames_nfd_file_on_disk() {
        let dir = std::env::temp_dir().join(format!("nfc-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let src = dir.join(nfd("테스트 파일.txt"));
        fs::write(&src, b"hello").unwrap();

        let result = try_normalize(&src).unwrap();
        let (from, to) = result.expect("should rename");
        assert!(needs_normalization(&from));
        assert_eq!(to, "테스트 파일.txt");

        // 결과 파일이 실제로 NFC 이름으로 존재하는지 (바이트 단위 비교)
        let stored: Vec<String> = fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert!(stored.contains(&"테스트 파일.txt".to_string()));
        let _ = fs::remove_dir_all(&dir);
    }

    /// APFS 재개명 루프 회귀 테스트: 이미 NFC로 바뀐 파일을
    /// 예전 NFD 경로로 다시 처리해도 no-op 이어야 한다.
    #[test]
    fn stale_nfd_path_is_noop_after_rename() {
        let dir = std::env::temp_dir().join(format!("nfc-test3-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let nfd_path = dir.join(nfd("루프 테스트.txt"));
        fs::write(&nfd_path, b"hi").unwrap();

        assert!(try_normalize(&nfd_path).unwrap().is_some()); // 1차: 실제 개명
        // 2차: 옛 NFD 경로로 재시도 — APFS에선 같은 파일에 닿지만 no-op이어야 함
        #[cfg(target_os = "macos")]
        assert!(try_normalize(&nfd_path).unwrap().is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn already_nfc_is_noop() {
        let dir = std::env::temp_dir().join(format!("nfc-test2-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let src = dir.join("정상 파일.txt");
        fs::write(&src, b"hi").unwrap();
        assert!(try_normalize(&src).unwrap().is_none());
        let _ = fs::remove_dir_all(&dir);
    }
}
