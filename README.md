# Jamoa (자모아)

> 자모(Jamo)를 모아(Moa) — 풀어쓰기(NFD)된 한글 파일명을 모아쓰기(NFC)로.

Windows ↔ macOS 파일 교환 시 발생하는 **한글 자모분리(NFD) 파일명**을 자동으로 정규화(NFC)해주는 트레이 상주 앱입니다.

맥에서 만든 파일을 윈도우로 옮기면 `ㅂㅗㄱㅗㅅㅓ.hwp`처럼 자모가 분리되어 보이는 문제를, 지정한 폴더를 실시간 감시하면서 자동으로 고쳐줍니다.

## 동작 방식

- **실시간 감시**: 주기적 폴링이 아니라 OS 파일시스템 이벤트(macOS FSEvents / Windows ReadDirectoryChangesW)를 사용합니다. 폴더에 파일이 아무리 많아도 부하가 거의 없습니다.
- **시작 시 1회 스캔**: 앱이 꺼져 있던 동안 들어온 파일도 시작할 때 한 번 훑어서 정리합니다. 파일명 문자열 비교만 하므로 수만 개 폴더도 1초 미만입니다.
- **정규화 방향**: 양쪽 OS 모두 NFC로 통일합니다 (Windows 표준이며, macOS APFS도 NFC를 보존).

## 주요 기능

- 메뉴바(macOS) / 트레이(Windows) 상주, 창을 닫아도 백그라운드 동작
- 감시 폴더 추가/제거, 폴더별 하위 폴더 포함 여부 설정 (첫 실행 시 다운로드 폴더 자동 등록)
- 로그인 시 자동 시작 옵션
- 최근 변환 기록 확인 (변경 전 → 변경 후)
- 안전장치:
  - 다운로드 중 임시 파일(`.crdownload`, `.download`, `.part` 등)과 숨김 파일(`.*`, `~$*`)은 건드리지 않음
  - 대상 이름이 이미 존재하면 `이름 (1).ext` 식으로 충돌 회피
  - macOS APFS의 정규화 무시 조회 특성(NFD/NFC가 같은 파일로 보임)을 감안한 2단계 개명
  - NFC를 저장할 수 없는 파일시스템(HFS+)에서는 재시도 루프에 빠지지 않도록 자동 제외
  - 쓰기 중인 파일의 개명 실패 시(주로 Windows) 1초 간격 재시도

## 개발

요구사항: Rust(stable), Node.js 18+

```bash
npm install
npm run tauri dev     # 개발 실행
npm run tauri build   # 설치본 빌드 (.dmg / .msi·.exe)
```

Rust 코어 테스트:

```bash
cd src-tauri && cargo test
```

Windows 설치본은 Windows에서 빌드해야 합니다 (또는 GitHub Actions 사용 — `.github/workflows/release.yml` 참고, 태그 push 시 양쪽 OS 설치본이 만들어집니다).

## 구조

```
src/                  # 프런트엔드 (설정 창 — 순수 HTML/CSS/JS, 빌드 단계 없음)
src-tauri/src/
  normalizer.rs       # NFC 판별·개명 핵심 로직 (+ 단위 테스트)
  engine.rs           # 파일시스템 감시(notify) + 워커 + 폴더 스캔
  config.rs           # 설정·기록 저장 (앱 설정 디렉터리의 JSON)
  lib.rs              # 트레이 메뉴, Tauri 명령, 앱 초기화
```
