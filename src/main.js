const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const $ = (selector) => document.querySelector(selector);

function toast(message) {
  const el = $("#toast");
  el.textContent = message;
  el.classList.remove("hidden");
  clearTimeout(toast._timer);
  toast._timer = setTimeout(() => el.classList.add("hidden"), 2600);
}

function formatTime(ms) {
  const d = new Date(ms);
  const pad = (n) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

let currentEnabled = false;

async function refreshState() {
  const state = await invoke("get_state");
  currentEnabled = state.config.enabled;
  $("#enabled-toggle").checked = state.config.enabled;
  $("#autostart-toggle").checked = state.autostart;

  const list = $("#folder-list");
  list.innerHTML = "";
  for (const folder of state.config.folders) {
    const li = document.createElement("li");

    const pathBtn = document.createElement("button");
    pathBtn.className = "path";
    // direction:rtl 말줄임 트릭이 앞의 "/"를 뒤로 밀지 않도록 LRM으로 감싼다
    pathBtn.textContent = `‎${folder.path}‎`;
    pathBtn.title = "Finder/탐색기에서 열기";
    pathBtn.addEventListener("click", () => invoke("reveal_path", { path: folder.path }));

    const recursiveLabel = document.createElement("label");
    recursiveLabel.className = "recursive";
    const recursiveCheck = document.createElement("input");
    recursiveCheck.type = "checkbox";
    recursiveCheck.checked = folder.recursive;
    recursiveCheck.addEventListener("change", async () => {
      await invoke("set_recursive", { path: folder.path, recursive: recursiveCheck.checked });
    });
    recursiveLabel.append(recursiveCheck, "하위 폴더 포함");

    const removeBtn = document.createElement("button");
    removeBtn.className = "remove";
    removeBtn.textContent = "제거";
    removeBtn.addEventListener("click", async () => {
      await invoke("remove_folder", { path: folder.path });
    });

    li.append(pathBtn, recursiveLabel, removeBtn);
    list.appendChild(li);
  }
  $("#folder-empty").classList.toggle("hidden", state.config.folders.length > 0);
}

async function refreshHistory() {
  const history = await invoke("get_history");
  const list = $("#history-list");
  list.innerHTML = "";
  for (const record of history.slice(0, 100)) {
    const li = document.createElement("li");

    const meta = document.createElement("div");
    meta.className = "meta";
    meta.textContent = `${formatTime(record.at)} · ${record.dir}`;

    const change = document.createElement("div");
    change.className = "change";
    const from = document.createElement("span");
    from.className = "from";
    from.textContent = record.from;
    const to = document.createElement("span");
    to.className = "to";
    to.textContent = record.to;
    change.append(from, " → ", to);

    li.append(change, meta);
    list.appendChild(li);
  }
  $("#history-empty").classList.toggle("hidden", history.length > 0);
}

window.addEventListener("DOMContentLoaded", async () => {
  $("#enabled-toggle").addEventListener("change", (e) => {
    if (e.target.checked) {
      // 확인을 받기 전까지는 꺼진 상태로 되돌려 둔다
      e.target.checked = false;
      $("#confirm-overlay").classList.remove("hidden");
    } else {
      invoke("set_enabled", { enabled: false });
    }
  });
  $("#confirm-ok").addEventListener("click", async () => {
    $("#confirm-overlay").classList.add("hidden");
    await invoke("set_enabled", { enabled: true });
  });
  $("#confirm-cancel").addEventListener("click", () => {
    $("#confirm-overlay").classList.add("hidden");
  });
  $("#autostart-toggle").addEventListener("change", async (e) => {
    try {
      await invoke("set_autostart", { enabled: e.target.checked });
    } catch (err) {
      toast(`자동 시작 설정 실패: ${err}`);
      e.target.checked = !e.target.checked;
    }
  });
  $("#add-folder").addEventListener("click", async () => {
    const added = await invoke("pick_and_add_folder");
    if (added) {
      toast(
        currentEnabled
          ? "폴더가 추가되었습니다. 초기 검사를 시작합니다."
          : "폴더가 추가되었습니다. 자동 정규화를 켜면 검사가 시작됩니다."
      );
    }
  });
  $("#scan-now").addEventListener("click", async () => {
    const button = $("#scan-now");
    button.disabled = true;
    try {
      const count = await invoke("scan_now");
      toast(count > 0 ? `${count}개 파일명을 정규화했습니다.` : "정규화할 파일이 없습니다.");
    } finally {
      button.disabled = false;
    }
  });

  const showUpdateModal = (version) => {
    $("#update-version").textContent = `v${version}`;
    $("#update-overlay").classList.remove("hidden");
  };
  $("#update-later").addEventListener("click", () =>
    $("#update-overlay").classList.add("hidden")
  );
  $("#update-now").addEventListener("click", async () => {
    const button = $("#update-now");
    button.disabled = true;
    button.textContent = "다운로드 중…";
    try {
      await invoke("install_update"); // 성공하면 앱이 재시작되므로 복귀하지 않는다
    } catch (err) {
      $("#update-overlay").classList.add("hidden");
      button.disabled = false;
      button.textContent = "업데이트";
      toast(`업데이트 실패: ${err}`);
    }
  });
  await listen("update-available", (event) => showUpdateModal(event.payload));
  // 웹뷰 로드 전에 확인이 끝나 이벤트를 놓친 경우를 대비한 폴백
  const pendingUpdate = await invoke("get_pending_update");
  if (pendingUpdate) showUpdateModal(pendingUpdate);

  await listen("config-changed", refreshState);
  await listen("rename-recorded", refreshHistory);
  await listen("scan-done", (event) => {
    toast(
      event.payload > 0
        ? `${event.payload}개 파일명을 정규화했습니다.`
        : "정규화할 파일이 없습니다."
    );
  });

  await refreshState();
  await refreshHistory();
});
