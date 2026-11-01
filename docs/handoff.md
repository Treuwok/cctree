# Project Handoff

> Last updated: 2026-11-01
> Updated by: Session：repo 搬到 Treuwok、公開、發布 v0.1.0
> 現在在做: v0.1.0 已發布；下一步是 Windows／WSL 實機驗證 — 追蹤 issue 2（記錄、暫不修的 review 意見）

## 待辦（Pending）

- [ ] **[2026-11-01]** Windows／WSL 實機驗證
  - 從哪開始：`docs/manual-verification.md`。執行檔可直接用 README 的安裝指令，或從 Releases 頁下載。
  - 加測一項：開著 cctree，讓 Windows 睡眠再喚醒，然後在 WSL 裡結束一個 process。被跳過就代表 issue 2 留言記錄的「開機時間變動」問題會發生，要修（修法方向寫在該留言）。
- [ ] Homebrew：使用者決定 v0.1.0 先不含。要加時建 `Treuwok/homebrew-tap`、設定 `HOMEBREW_TAP_TOKEN` secret，在 `dist-workspace.toml` 加回 Homebrew 後執行 `dist generate`。
- [ ] `cargo publish`：未做。發布到 crates.io 無法撤回，要先等使用者同意。

## 進行中（In Progress）

（無）

## 已完成（Completed）

- [x] **[2026-10-23]** cctree v1 實作完成並 merge（PR 1，b0388ea）。Codex review 7 輪，判定罕見或不影響使用者的意見記在 issue 2，不修。
- [x] **[2026-10-23]** CI 只在 `main` push 與 PR 時跑，Windows CPU 測試改成等到量到空轉為止（PR 3）。
- [x] **[2026-11-01]** repo 從 `yenchiugu/cctree` 搬到 `Treuwok/cctree` 並設為公開。公開前用 gitleaks 與 trufflehog 掃過全部 git 歷史，沒有真的金鑰。預設分支改為 `main`。
- [x] **[2026-11-01]** README 重寫（英文依 ASD-STE100 寫法，另有 `README.zh-TW.md`），發布設定拿掉 Homebrew（PR 4，ec68f76）。
- [x] **[2026-11-01]** 發布 v0.1.0：https://github.com/Treuwok/cctree/releases/tag/v0.1.0 （5 個平台的壓縮檔、shell 與 PowerShell 安裝程式）。

## 備註（Notes）

- 本機資料夾仍叫 `ccwho`，remote 是 `https://github.com/Treuwok/cctree.git`。
- 發布新版本：改 `Cargo.toml` 的 `version`、寫 `CHANGELOG.md`、merge 進 `main`，再推同版本的 tag（例如 `v0.1.1`）。流程寫在 README 的〈Make a release〉。
- 公開 repo 使用 GitHub 標準 hosted runner 不收費，CI 與發布流程不耗額度。
- fixture 不 commit（含真實路徑）。`--dump-snapshot` 的輸出不要貼到公開的地方。
- 對外的動作（新 tag、Homebrew tap、`cargo publish`）每次都要先等使用者同意。
