# 手動驗證清單

自動測試涵蓋不到真實的 Claude Code／Codex、系統工具的數字，以及各終端機的中文寬度，所以每個平台都要用真的 session 手動跑一次。把每一項標成通過、失敗或不適用；失敗的附上螢幕截圖、`cctree --version`、作業系統版本和終端機名稱，回報後回到對應的 Task 修正。

## 準備

- 執行檔：到要發佈的 commit 的 GitHub Actions 執行頁，下載 Artifacts：`cctree-aarch64-apple-darwin`（macOS）、`cctree-x86_64-pc-windows-msvc`（Windows）、`cctree-x86_64-unknown-linux-musl`（WSL／Linux）。解壓縮後，macOS 與 Linux 要 `chmod +x cctree`；macOS 另外要 `xattr -d com.apple.quarantine ./cctree`，Windows 的 SmartScreen 按「其他資訊」→「仍要執行」。
- 需要一個真的 Claude Code session（至少設定一個 MCP）和一個真的 Codex session（最好在 `~/.codex/config.toml` 設定一個 `[mcp_servers.<名稱>]`）。
- 另外開一個專案目錄名稱含中文的 Claude Code session，例如 `~/prj/測試專案`。
- 下面「請 Claude Code 跑…」都是請它用 Bash 工具在背景執行（指令結尾加 `&`），否則 Bash 工具會一直等到指令結束。這些 process 會出現在該 session 底下（「其他子程序」或「已脫離」），驗證完用 cctree 自己結束它們，這本身就是 `x` 的測試。
- 不要把 `--dump-snapshot` 的輸出貼到公開的地方：命令列已遮蔽，但專案路徑還在。

## macOS

終端機：Terminal.app 與 iTerm2 各跑一次（中文對齊那組）。

- [ ] **session 與 MCP**
  - [ ] 另開終端機啟動 Claude Code，幾秒內「Claude Code」底下出現這個 session，標籤是專案目錄（家目錄縮寫成 `~`）。
  - [ ] 展開「MCP」，每個 MCP 的名稱和 `~/.claude.json` 的 `mcpServers`、專案的 `.mcp.json` 一致。
  - [ ] 啟動 Codex，「Codex」底下出現 session，MCP 名稱和 `~/.codex/config.toml` 一致。
  - [ ] `m` 切到 MCP 總覽，同名的 MCP 跨 session 加總；再按 `m` 回來。
- [ ] **記憶體與 CPU**
  - [ ] 選三個 process（session 本體、`npm exec` 外殼、MCP server），詳細面板的「活動監視器會顯示 X」和活動監視器的「記憶體」欄一致（數字會隨時間小幅變動）。
  - [ ] 詳細面板裡「在 RAM 裡 ＋ 被擠出 RAM ＋ 其他 ＝ 記憶體」的算式成立。
  - [ ] 畫面最上方的 RAM 已用、swap 和活動監視器「記憶體」分頁的「已使用記憶體」「使用的 Swap」接近。
  - [ ] 請 Claude Code 跑 `node -e "while(1){}" &`：CPU 約 100%（單一核心為 100%），和活動監視器一致。cctree 剛啟動時 CPU 先顯示「—」，約 2 秒後有數字。
- [ ] **結束 process**
  - [ ] 請 Claude Code 跑 `sleep 600 &`，選到那一列按 `x`：確認視窗列出 PID 與名稱，Esc 取消後它還在；再按 `x`、Enter，它消失，訊息列說明結果。
  - [ ] 選一個 MCP 按 `X`：確認視窗列出外殼、server、輔助 process；Enter 後，Claude Code 裡用 `/mcp` 看到它斷線。
  - [ ] 在「MCP（n 個）」這種分組列按 `x`：訊息列說這一列是一組 process，要結束整組請按 `X`。
  - [ ] 在「Claude Code」「已失聯」「其他 Node.js」分類列按 `X`：被擋下，訊息列說範圍太大，沒有任何 process 被結束。
  - [ ] 用 `sudo -u nobody node -e "setInterval(()=>{},1000)" &` 開一個其他使用者的 node（需要 `nobody` 能執行 node，不行就略過這項）。它出現在「其他 Node.js」時按 `x`：被擋下並建議 `sudo cctree`；用 `sudo cctree` 開啟後可以結束。
- [ ] **已失聯**
  - [ ] 請 Claude Code 跑 `nohup sleep 3000 >/dev/null 2>&1 &`，再結束那個 Claude Code session（`/exit`）：幾秒內 `sleep` 出現在「已失聯」，詳細面板寫出判定原因。沒有其他 Claude Code session 在執行時標「確定」，還有別的 session 在執行時標「疑似」。如果結束 session 時 `sleep` 也跟著結束了，這項不適用。
  - [ ] 在「已失聯」分類列按 `X` 被擋下；選到 `sleep` 那一列按 `x` 可以結束。
- [ ] **中文對齊**
  - [ ] 含中文專案目錄的 session：PID、CPU、記憶體欄位對齊，名稱太長時被截斷，沒有擠壞欄位。
  - [ ] 80×24 時仍然對齊；再縮小出現「視窗太小」畫面，放大後恢復。
  - [ ] 詳細面板、確認視窗、說明頁的框線與文字沒有錯位。
- [ ] **顏色**
  - [ ] `NO_COLOR=1 cctree`：沒有任何顏色；選取列反白，新出現的 process 為粗體。

## Windows

終端機：Windows Terminal 與舊的 conhost（執行 `conhost.exe` 開出的視窗）各跑一次（中文對齊那組）。在 PowerShell 裡用 `$env:NO_COLOR = "1"; .\cctree.exe`。

- [ ] **session 與 MCP**
  - [ ] Claude Code 的 session（原生版與 npm 版各一種，有的話）出現在「Claude Code」底下，標籤是專案目錄。
  - [ ] MCP 名稱和 `~\.claude.json`、`.mcp.json` 一致；`cmd.exe /c npx …` 啟動的 MCP 在同一個 MCP 節點裡，`cmd.exe`、`conhost.exe` 標為「外殼」。
  - [ ] Codex 的 session 出現在「Codex」底下，MCP 名稱和 `~\.codex\config.toml` 一致。
  - [ ] `m` 切到 MCP 總覽再切回來。
- [ ] **記憶體與 CPU**
  - [ ] **EX2 欄位有值**：「在 RAM 裡」顯示數字，不是「—」。
  - [ ] 選三個 process，「記憶體」和工作管理員「詳細資料」頁的「認可大小」一致；「在 RAM 裡」和「處理程序」頁的「記憶體」一致。詳細面板的「活動監視器」字樣在 Windows 上應該是「工作管理員」。
  - [ ] 「被擠出 RAM」前面有「約」。
  - [ ] 請 Claude Code 跑 `node -e "while(1){}" &`：CPU 約為 100 ÷ 邏輯處理器數，和工作管理員一致（Windows 以整台機器為 100%）。
  - [ ] 選做：在沒有 2023 年 9 月累積更新的舊版 Windows 上，「在 RAM 裡」顯示「—」，並提示更新。
- [ ] **結束 process**
  - [ ] 請 Claude Code 跑 `node -e "setInterval(()=>{},1000)" &`，選到那一列按 `x`：確認視窗寫明「Windows 會直接強制結束」；Esc 取消後它還在；Enter 後它消失。
  - [ ] 選一個 MCP 按 `X`：外殼、server 都被結束，Claude Code 裡 `/mcp` 顯示斷線。
  - [ ] 在分組列按 `x`，以及在「Claude Code」「已失聯」「其他 Node.js」分類列按 `X`：都被擋下並說明原因。
- [ ] **中文對齊**
  - [ ] 含中文專案目錄的 session：欄位對齊，名稱太長時被截斷。
  - [ ] 80×24 仍然對齊；再縮小出現「視窗太小」畫面。
  - [ ] 詳細面板、確認視窗、說明頁的框線與文字沒有錯位（conhost 與 Windows Terminal 都要看）。
- [ ] **顏色**
  - [ ] 設定 `NO_COLOR` 後沒有任何顏色；選取列反白。
- [ ] **WSL 提示**
  - [ ] 關掉 Windows 上的 Claude Code／Codex，但讓 WSL 裡的 Claude Code 或任何 WSL 分發版在執行：畫面上方出現「如果你是在 WSL 裡用 Claude Code／Codex，請在 WSL 裡執行 cctree」。

## WSL／Linux

WSL 用 `cctree-x86_64-unknown-linux-musl` 在 WSL 裡執行；也可以在原生 Linux 上跑同一份清單。

- [ ] **session 與 MCP**
  - [ ] Claude Code 與 Codex 的 session 出現在對應分類，標籤是專案目錄。
  - [ ] MCP 名稱和 `~/.claude.json`、`.mcp.json`、`~/.codex/config.toml` 一致。
  - [ ] `m` 切到 MCP 總覽再切回來。
- [ ] **記憶體與 CPU**
  - [ ] 選三個 process，用 `grep -E 'RssAnon|VmSwap' /proc/<PID>/status` 對照：「記憶體」＝ `RssAnon` ＋ `VmSwap`，「在 RAM 裡」＝ `RssAnon`，「被擠出 RAM」＝ `VmSwap`。
  - [ ] `htop` 的 RES 比「記憶體」大（RES 包含共用部分），詳細面板的「共用部分」大約等於差額。
  - [ ] 請 Claude Code 跑 `node -e "while(1){}" &`：CPU 約 100%（單一核心為 100%），和 `top`、`htop` 一致。cctree 剛啟動時 CPU 先顯示「—」。
- [ ] **結束 process**
  - [ ] 請 Claude Code 跑 `sleep 600 &`，選到那一列按 `x`：確認視窗列出 PID 與名稱，Esc 取消後它還在；Enter 後 3 秒內消失，訊息列說明結果。
  - [ ] 選一個 MCP 按 `X`，Claude Code 裡 `/mcp` 顯示斷線。
  - [ ] 在分組列按 `x`，以及在「Claude Code」「已失聯」「其他 Node.js」分類列按 `X`：都被擋下並說明原因。
  - [ ] 用 `sudo -u nobody node -e "setInterval(()=>{},1000)" &` 開一個其他使用者的 node（需要 `nobody` 能執行 node，不行就略過這項）。它出現後按 `x`：被擋下並建議 `sudo cctree`；用 `sudo cctree` 開啟後可以結束。
- [ ] **已失聯**
  - [ ] 請 Claude Code 跑 `setsid nohup sleep 3000 >/dev/null 2>&1 &`，再結束那個 session：幾秒內 `sleep` 出現在「已失聯」，詳細面板寫出判定原因。沒有其他 Claude Code session 在執行時標「確定」，還有別的 session 在執行時標「疑似」。`ps -o pid,ppid,comm -p <PID>` 的 PPID 是 1，或是 `systemd --user`、`/init` 這類 subreaper。
  - [ ] 在「已失聯」分類列按 `X` 被擋下；選到 `sleep` 那一列按 `x` 可以結束。
- [ ] **中文對齊**
  - [ ] 含中文專案目錄的 session：欄位對齊，名稱太長時被截斷。
  - [ ] 80×24 仍然對齊；再縮小出現「視窗太小」畫面。
  - [ ] 詳細面板、確認視窗、說明頁的框線與文字沒有錯位（GNOME Terminal、Konsole，或 WSL 裡的 Windows Terminal）。
- [ ] **顏色**
  - [ ] `NO_COLOR=1 cctree`：沒有任何顏色；選取列反白。
- [ ] **WSL 提示**（只在 WSL 裡）
  - [ ] 畫面上方常駐「這裡只看得到 WSL 裡的 process；Windows 那邊請在 PowerShell 另外執行 cctree」。
- [ ] 選做：在 Mac 的 Docker 容器裡執行。容器裡的 PID 1 不是 init，孤兒的收養行為和桌面 Linux 不同，所以只用來驗證記憶體與畫面，不驗證「已失聯」。
