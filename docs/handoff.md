# Project Handoff

> Last updated: 2026-10-22
> Updated by: Session：cctree 設計、實作計畫、第 1 輪 adversarial review
> 現在在做: 準備用 Subagent-driven 方式從 Task 1 開始實作 cctree — 無 tracking issue（repo 剛建立，尚未開票）

## 待辦（Pending）

（無）

## 進行中（In Progress）

- [ ] **[2026-10-22]** cctree v1：依實作計畫開發跨平台 TUI（macOS／Windows／Linux）
  - Issue/PR：無（repo 剛建立；要開 tracking issue 的話，在 Task 1 push 前開）
  - 進度：spec 與 21 個 Task 的實作計畫已完成，並做過第 1 輪 adversarial review（16 條全修）；push 狀態以 `git status -sb` 為準
  - 下一步：用 superpowers:subagent-driven-development 執行 Task 1（專案骨架、核心資料型別、CI）
  - 為什麼是這一步：使用者已選擇 Subagent-driven；Task 1 要 push 到 private repo，CI 才會在三個平台上跑
  - 從哪開始：`docs/superpowers/plans/2026-10-22-cctree.md` 的 Task 1，搭配 `docs/superpowers/specs/2026-10-22-cctree-design.md`

## 已完成（Completed）

- [x] **[2026-10-22]** cctree 設計 spec 定稿，並通過 builder check — `docs/superpowers/specs/2026-10-22-cctree-design.md`（1a166b0）
- [x] **[2026-10-22]** 實作計畫（21 個 TDD Task）— `docs/superpowers/plans/2026-10-22-cctree.md`（cc4efcf）
- [x] **[2026-10-22]** 第 1 輪 adversarial review：找到 16 條（其中 P1 兩條：Linux CPU 歸零、對整個分類結束 process），全部修進文件（14ff04d）

## 備註（Notes）

- 本機資料夾仍叫 `ccwho`，但 GitHub repo 已改名為 `yenchiugu/cctree`（private），remote 也已更新。
- Task 7 需要使用者協助：在 Mac 上開一個 Codex session、在 Windows 實機與 WSL 上各跑一次 `--dump-snapshot`；fixture commit 前要先請使用者確認路徑，因為 repo 預計公開。
- Task 21 中對外公開的動作（repo 改成公開、建立 Homebrew tap、`cargo publish`）一律要先等使用者同意。
- 第 1 輪 review 沒驗證到的部分：Windows、Codex、Linux 的實機行為。這些已排進 Task 7，實作時要以實測結果為準，不要只靠 spec。
