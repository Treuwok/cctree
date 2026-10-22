# cctree 設計文件

- 日期：2026-10-22
- 狀態：草稿，待確認
- Repo：https://github.com/yenchiugu/cctree （目前 private，預計公開）

## 1. 目的與背景

Claude Code 和 Codex 執行時會帶出大量子 process：每個 MCP server 通常由 `npx`／`npm exec` 啟動，再開出真正的 `node` server，有些還會再 fork 輔助 process；另外還有 shell 指令、LSP server 等。macOS 活動監視器和 Windows 工作管理員只會列出一整排 `node`，看不出：

- 這個 `node` 是誰帶出來的
- 一個 Claude Code／Codex session 連同底下所有子 process 總共吃多少記憶體與 CPU
- 總共開了幾份 MCP、各佔多少
- 有沒有主人已經結束、卻還留著的 process（使用者口中的「zombie」，嚴格說大多是孤兒／洩漏）

cctree 是一個終端機介面（TUI）工具，像 `top` 一樣持續更新，把整個系統的 process 依「是哪個工具、哪個 session 帶出來的」整理成可收合的樹，顯示每一層的 PID、CPU 與記憶體，並能安全地結束單一 process 或整棵樹。

### 1.1 實測數據（2026-10-22，作者的 macOS 26 / Darwin 25.5 機器）

- 12 個 Claude Code session，底下共 159 個 process，其中 142 個是 node／npm／MCP，RSS 加總約 7 GB。
- 每個 MCP 佔 2～3 個 process：`npm exec X`（外殼）→ 真正的 server；`chrome-devtools-mcp` 還會再 fork 一個 telemetry watchdog（用 `setsid` 脫離 session，但 PPID 仍指向 server，命令列帶 `--parent-pid=<pid>`）。
- 剛啟動的 session 中，每個 `npm exec` 外殼本身約 96 MB RSS。
- 同一個 `npm exec`（PID 1843）：RSS 7.7 MB，但 `phys_footprint` 63.6 MB，其中 56 MB 已被壓縮。只看 RSS 會嚴重低估。
- 真正的 zombie（`Z` 狀態）為 0。發現一個疑似失聯的 process：`ChatGPT Meetings`（PID 80871，約 100 MB）。它在 10/17 啟動，PPID 為 launchd（1），只能從路徑（位於 Codex 的暫存 home 目錄 `…/T/codex-home-*/…`）看出與 Codex 有關，而當下沒有任何 Codex 在執行。因為只有路徑證據，只能算「疑似」。
- 反例：Chrome 底下有一個 `ChatGPT for Chrome` native host，它的執行檔在 `~/.codex/plugins/cache/…`，但 parent（Chrome）還活著，所以不是失聯。**光看路徑不能判定失聯。**
- Claude Code 的子 process 會繼承以下環境變數（只確認名稱，未記錄值）：`CLAUDECODE`、`CLAUDE_CODE_SESSION_ID`、`CLAUDE_PROJECT_DIR`、`CLAUDE_CONFIG_DIR`、`CLAUDE_CODE_ENTRYPOINT` 等。
- Claude Code 原生安裝版的 process：`argv[0]` 是 `claude`，但 `comm`（`top` 看到的名稱）是版本號，例如 `2.1.306`；執行檔位於 `~/.local/share/claude/versions/<版本>`。**不能用 process 名稱辨識 Claude Code。**
- 有一個 MCP 的命令列直接帶著 API 金鑰（`--api-key …`），任何能列出 process 的程式都看得到。

### 1.2 成功的樣子

使用者不帶任何參數執行 `cctree`，幾秒內就能回答：

1. 每個 Claude Code／Codex session（連同子 process）各吃多少記憶體與 CPU
2. 哪些 MCP 在跑、在哪個 session 底下、同一種 MCP 總共開了幾份
3. 有沒有主人已經結束、卻還留著的 process
4. 不用查文件，就能安全地結束一個 process 或一整棵樹

## 2. 目標、非目標、反目標

### 2.1 目標（v1）

- **G1 樹狀分群**：掃描整個系統，分成「Claude Code」「Codex」「已失聯」「其他 Node.js」四大類；Claude Code／Codex 底下依 session 分，session 底下分成「MCP」與「其他子程序」。
- **G2 每個 process 都有 PID**：每個 process 一列，顯示 PID、CPU、記憶體；分組列顯示加總與 process 個數。
- **G3 白話記憶體分類**：用一般人看得懂的方式，分開「獨佔」「在 RAM 裡」「被擠出 RAM（壓縮／swap／分頁檔）」「共用」。
- **G4 MCP 分解**：每個 MCP 一個可收合的節點，用使用者設定檔裡的名字命名；另有「MCP 總覽」檢視，跨 session 依 MCP 名稱加總。
- **G5 持續監控**：預設每 2 秒更新。
- **G6 結束 process**：`x` 結束單一 process，`X` 結束整棵樹或整組；一定會先確認，並有防誤殺機制。
- **G7 懶人也能用**：不帶參數就能用；畫面上隨時有按鍵提示；錯誤訊息說明「發生什麼事、影響什麼、怎麼解決」；單一 process 讀取失敗不會讓程式停止。
- **G8 跨平台與公開發佈**：macOS（Apple silicon 與 Intel）、Windows x64、Linux（x64 與 arm64，包含 WSL）都是一級支援；提供一行指令就能安裝的方式。

### 2.2 非目標（v1 不做）

- 跨越 WSL 邊界：Windows 版看不到 WSL 裡的 process，WSL 裡的 Linux 版也看不到 Windows 的 process。兩邊各自執行，畫面上會提示（§4.8）。
- 把 Claude Desktop、Codex 桌面 app、Cursor、VS Code 等當成「已知工具」分類。它們帶出的 node／MCP 會出現在「其他 Node.js」，並依最上層的 app 祖先分組，所以不會漏掉。
- 遠端／HTTP 型 MCP（本機沒有 process，看不到）
- 歷史紀錄、趨勢圖、警報、通知
- GUI、選單列／系統匣圖示（核心邏輯與 UI 分離，保留日後加上的可能）
- 網路、磁碟 I/O 統計
- 修改 MCP 設定、重啟 MCP
- 自動更新（交給套件管理工具）
- 監控遠端機器

### 2.3 反目標（就算功能都能動，出現這些也算失敗）

- **結束到錯的 process**：例如 PID 被重用、畫面重新排序導致游標跳到別列。
- **洩漏機密**：在畫面上顯示命令列或環境變數裡的金鑰；或把任何資料傳出本機（不連網、不收集任何遙測資料）。
- **監控工具本身很重**：預設設定下，單一核心的平均 CPU 超過 2%，或記憶體超過約 50 MB（§4.9）。
- **一定要管理員權限才有用**。
- **要先讀文件或記參數才有用**。
- **數字跟系統工具對不上，卻沒有解釋**。
- **看起來像 Anthropic 或 OpenAI 的官方工具**：README 必須寫明「非官方，與 Anthropic、OpenAI 無關」，也不使用它們的商標或視覺。

## 3. 限制

- 語言：Rust stable（作者機器為 rustc 1.98）。
- 主要套件：`sysinfo`（列舉 process、PPID、命令列、環境變數、cwd、CPU、啟動時間）、`ratatui` + `crossterm`（TUI）。平台記憶體細節需自行讀取：macOS 用 `libc`／`mach2` 呼叫系統 API，Windows 用 `windows` crate，Linux 讀 `/proc/<pid>/status`。
- 不需要管理員權限：目前使用者自己的 process 要能拿到完整資訊；其他使用者或系統 process 拿不到的欄位顯示「—」。
- 執行時完全不連網。
- CPU% 需要兩次取樣，第一個畫面的 CPU 顯示「—」，不顯示 0。
- macOS 實測：一般權限可對同一使用者的 process 使用 `proc_pid_rusage`（取得 footprint、resident）與 `task_name_for_pid` + `task_info(TASK_VM_INFO)`（取得 internal、compressed）；對 root 的 process（例如 PID 1）會失敗。
- Windows：`PROCESS_MEMORY_COUNTERS_EX2`（含 Private Working Set）需要 Windows 10 或 11 22H2，並裝有 2023 年 9 月累積更新；版本不足時，該欄顯示「—」並提示更新。
- Windows 的 parent 結束後，子 process 記錄的 PPID 不會更新，而且該 PID 可能被重用。所以 parent 關係一律要用啟動時間驗證（parent 必須比 child 早啟動）。
- Linux：`/proc/<pid>/status` 的 `RssAnon`、`RssFile`、`RssShmem`、`VmSwap` 讀取成本低，而且提供單一 process 的 swap 用量。parent 結束後，子 process 會被交給 PID 1 或最近的 subreaper，桌面環境常見的是 `systemd --user`，WSL 則是 `/init`。
- 終端機：要能在 macOS Terminal、iTerm2、Windows Terminal、Windows 10 以上的 conhost，以及常見的 Linux 終端機（GNOME Terminal、Konsole 等）正常運作。中文是雙寬字元，欄位要用顯示寬度對齊。最小尺寸 80×24。

## 4. 已決定的設計

### 4.1 名稱與平台

- 工具、執行檔、crate 的名稱都是 `cctree`。
- 支援平台：
  - macOS：`aarch64-apple-darwin`、`x86_64-apple-darwin`
  - Windows：`x86_64-pc-windows-msvc`
  - Linux：`x86_64-unknown-linux-musl`、`aarch64-unknown-linux-musl`。用靜態連結，同一個執行檔在各種發行版都能跑。

### 4.2 架構

單一 Cargo package：`src/lib.rs` 放核心邏輯，不依賴任何 UI 套件；`src/main.rs` 與 `src/ui/` 放 TUI。日後要做 GUI 時，再把核心拆成獨立 crate。

每次更新的流程：

1. **Collector**：列舉所有 process，產出 `RawProc { pid, ppid, start_time, name, exe, cmd, user, status, cpu, mem }`。不會變的資訊（`exe`、`cmd`、`cwd`、允許清單內的環境變數）以 `(pid, start_time)` 為 key 快取，只在第一次看到時讀取；每次更新只重讀 CPU、記憶體、狀態。
2. **平台記憶體讀取**：見 §4.4。
3. **Classifier**：判斷每個 process 屬於哪個工具、哪個 session，以及扮演什麼角色（session 本體、MCP 外殼／server／輔助、其他子程序、已失聯、其他 Node.js、不相關）。
4. **Tree builder**：組成顯示用的樹並計算加總。每個節點都有穩定的 ID：process 用 `(pid, start_time)`，分組用語意路徑，例如 `claude/<session>/mcp/chrome-cdp`。
5. **UI**：負責畫面與 UI 狀態，包括哪些節點展開、選取哪個節點（以節點 ID 記錄）、排序方式、檢視模式、是否暫停。
6. **Actions**：結束 process 的規劃、驗證、執行與結果回報。

資料收集在背景執行緒進行，透過 channel 把 snapshot 交給 UI 執行緒。UI 執行緒只負責處理輸入和畫面，不會被收集工作卡住。

### 4.3 辨識規則

規則寫成程式內的資料表，新增一種工具時只要加一筆。v1 不提供使用者自訂規則。

**工具本體（session 的根）**

- **Claude Code**：符合下列任一條件。
  - `argv[0]` 的檔名是 `claude` 或 `claude.exe`。
  - 執行檔路徑符合 `*/claude/versions/*`（原生安裝版），**而且** `argv[0]` 的檔名和該執行檔的檔名相同。原生版是 multi-call 執行檔，`ugrep`、`rg` 等輔助指令會從同一個檔案以別的名稱執行，它們不是 session。
  - 是 `node`，且執行的是 `@anthropic-ai/claude-code/cli.js`（npm 安裝版）。
- **Codex**：符合下列任一條件。
  - 執行檔名稱是 `codex` 或 `codex.exe`，包含 `codex app-server` 這類子命令。
  - 是 `node`，且執行的是 `@openai/codex/bin/codex.js`（npm 外殼）。npm 外殼和它帶出的原生 binary 合併成同一個 session。
- **依執行檔判斷歸屬，誰帶起來的只用來標註**：
  - 只要符合上面的特徵，不論是誰帶起來的，都算該工具的 session。終端機、VS Code／Cursor extension、Claude Desktop 內建的 Claude Code、Agent SDK 寫的程式都一樣。
  - 「啟動者」（最近一個不是 shell 的祖先 app）顯示在 session 標籤上，例如「（由 VS Code 啟動）」；從終端機啟動的不另外標註。
  - 桌面 app 的主程式本身不能被誤認成 CLI，所以要依安裝路徑排除。macOS 只排除 `<名稱>.app` **直接位於** `Applications` 資料夾（`/Applications`、`/System/Applications`、`~/Applications`）內的主程式；Claude Desktop 內建的 Claude Code 位於 `~/Library/Application Support/Claude/claude-code/<版本>/`，是真正的 session，不排除。Windows 排除 Claude Desktop 等 app 的安裝目錄，實際路徑待實測（§6）。
- **巢狀**：
  - 工具 process 的祖先如果也是工具 session，不論是不是同一種工具（例如 Claude Code 透過 plugin 或 Bash 帶起 `codex exec`），都只算在**最外層**的 session 底下，顯示成它的子 session，加總也只算一次。
  - 「Codex」大類另外顯示一列不加總的提示：「另有 N 個 Codex session 在 Claude Code 底下」。反過來也一樣。
  - 例外：如果工具 process 是另一個 session 的直接子 process，而且符合該 session 的 MCP 設定（例如 `codex mcp-server` 被設成 Claude Code 的 MCP），就當成 MCP 節點，不當成子 session。
- **Session 的標籤**：本體 process 的 cwd（家目錄縮寫成 `~`），再加上已執行多久。如果 cwd 沒有意義（例如 `codex app-server` 同時服務多個對話），改用「Codex app-server（服務 <啟動者>）」。

**MCP（session 的子 process）**

主要依據是比對設定檔。以下來源都只讀不寫，檔案修改時間改變時才重新讀取：

- Claude Code：
  - `~/.claude.json` 的 `mcpServers`（使用者層級）
  - `~/.claude.json` 的 `projects[<路徑>].mcpServers`（專案層級）
  - session cwd 下的 `.mcp.json`
  - 已安裝 plugin 內的 `.mcp.json`
  - 如果有設定 `CLAUDE_CONFIG_DIR`，改從該目錄讀取。
- Codex：`~/.codex/config.toml` 的 `[mcp_servers.<名稱>]`；如果有設定 `CODEX_HOME`，改從該目錄讀取。另外，snapshot 中各 process 帶的 `CODEX_HOME` 值（例如 plugin 帶起的 Codex 使用暫存的 `codex-home-*`）也要逐一讀取，跟 `CLAUDE_CONFIG_DIR` 的處理方式相同。

比對方式：把設定檔裡的 `command` + `args`，和 process 的 argv 正規化後比對。正規化包括把 `npx -y X` 和 `npm exec X` 視為相同、去掉 `cmd.exe /c` 外殼、忽略路徑差異。例如設定 `npx -y chrome-devtools-mcp@latest --browserUrl …` 對應到 process `npm exec chrome-devtools-mcp@latest --browserUrl …`，節點名稱就是設定檔裡的 `chrome-cdp`。

比對不到時的備案：如果 session 的直接子 process 的 argv 符合啟動器模式（`npx`、`npm exec`、`pnpm dlx`、`bunx`、`uvx`、`uv run`、`docker run`、`deno run`），或者套件名稱、執行檔名稱含有 `mcp`，就視為 MCP，並用套件名稱命名（例如 `@playwright/mcp` 命名為 `playwright`）。

一個 MCP 節點包含那個直接子 process 和它所有的子孫，例如外殼、server、watchdog。節點內的每個 process 都會標上角色：`npm exec`、`npx`、`cmd.exe /c`、`conhost.exe` 標為「外殼」，其餘標為 server 或輔助。

**其他子程序**：session 底下不是 MCP 的子 process，例如 Bash 工具執行的指令、LSP（如 pyright）、`caffeinate`、hooks，統一放在「其他子程序」分組。

**已失聯與已脫離**

「已失聯」的意思是**主人已經結束，它卻還留著**。

有些 process 雖然脫離了 process 樹，主人卻還在執行，例如 Claude Code 的 Bash 工具用 `nohup` 或 `setsid` 啟動的 dev server。這類 process 叫「已脫離」，**不算已失聯**。

判斷 process 屬於誰的證據分成三種強度：

| 證據 | 強度 | 範例 |
|---|---|---|
| 環境變數能指出是哪一個 session | 強 | Claude Code 的 `CLAUDE_CODE_SESSION_ID`（另可搭配 `CLAUDE_PROJECT_DIR` 顯示路徑），已在 macOS 實測。Codex 的對應變數待實測（§6）。 |
| 命令列帶 `--parent-pid=<pid>` | 中 | `chrome-devtools-mcp` 的 telemetry watchdog |
| 只有路徑 | 弱 | 執行檔或 cwd 位於 `codex-home-*` 暫存目錄、`~/.codex` 或 `~/.claude` 底下 |

判定規則：

1. **強證據**
   - 指向的 session 還活著，而這個 process 不在它的樹裡：放進該 session 底下的「已脫離（仍屬於這個 session）」分組，並算進該 session 的加總。出現在活著的 session 成員（MCP 成員、其他子程序；不含本體）身上的每一個 session id，都對應到那個 session（session 中途換 id 時，MCP 仍帶舊 id）。
   - 指向的 session 已經結束（沒有任何活著的成員帶著這個 id）：判定為**已失聯**，理由寫「原本屬於 Claude Code session（~/prj/foo），該 session 已結束」。
2. **中證據**：`--parent-pid` 指向的 PID 已不存在，或已經不是同一個 process（用啟動時間判斷）時，判定為**已失聯**，理由寫「它記錄的主人 PID N 已不存在」。
3. **弱證據**：只有在 parent 是 launchd／PID 1（macOS），或 parent 已不存在、未通過驗證（Windows），**而且**當下沒有任何該工具的 session 在執行時，才判定為**疑似失聯**。parent 還活著的（例如上面 Chrome 帶起的 native host）不算。

**確定與疑似**：強、中證據判定為已失聯後，只有同時符合以下全部條件才標「確定」，否則標「疑似」。弱證據一律是「疑似」。

- 它已被收養：parent 是 launchd／PID 1／subreaper，或 parent 已不存在。parent 還活著時，可能是 tmux 這類 cctree 不認得的程式在管它。
- 它不是 app：執行檔不在 `.app` 套件、`/Applications/`，或（Windows）`Program Files`、`Program Files (x86)`、`WindowsApps` 底下。這些是使用者自己開的程式。
- 強證據另外要求：沒有任何同工具、啟動時間不晚於它的活著的 session **可能是它的主人**。比對專案只用 `CLAUDE_PROJECT_DIR`：有這個變數時，session 的工作目錄要與它相同或互為上下層才算可能；沒有時（Bash 工具帶起的子 process 不會有，而且 Bash 工具可以 `cd` 到任何地方），任何較早啟動的同工具 session 都可能是主人。

**「確定」和「疑似」只是給人看的提示，不會觸發任何批次動作**（§4.7）。

說明：

- macOS 的一般 GUI app 本來 PPID 就是 1，所以 **PPID = 1 本身不算證據**，只是弱證據的前提條件。
- Linux 上，「parent 是 launchd／PID 1」這個條件改成「parent 是 PID 1，或是 subreaper（`systemd --user`、WSL 的 `/init`）」。
- 「已失聯」大類裡，「確定」和「疑似」要用不同標記區分。每一列都要在詳細面板寫出判定理由。
- 已失聯 process 的子孫，跟著它一起算已失聯。仍在使用中的 process（活著的 session 本體及其祖先、cctree 自己，以及帶著活 session id 的 process）永遠不算已失聯。

**環境變數讀取規則**：只讀目前使用者的 process，每個 process 只在第一次看到時讀一次；只取出允許清單內的變數名稱，其他變數一律不儲存、不顯示。

「不儲存」也包含記憶體。2026-10-22 實測，MCP 子 process 的環境變數裡就有 `CLAUDE_CODE_MESSAGING_TOKEN` 這類機密。sysinfo 會把讀到的完整環境變數一直留在它的 process 快取裡，所以讀環境變數時要用一個**用完就丟的獨立 sysinfo 實例**：取出允許清單內的變數後立刻釋放，主實例永遠不讀環境變數。

**命令列還原**：Node 的 process 若設定 `process.title`，會覆寫自己的參數區，sysinfo 讀到的 `argv[0]` 就變成標題，後面還可能接著環境變數字串。收集時要還原這種改寫過的標題：把標題拆回參數，並丟掉標題後面接著的環境變數字串（否則允許清單以外的環境變數值會被存下來）。

**其他 Node.js**：沒被歸到上面任何一類的 `node`、`npm`、`npx`、`bun`、`deno`，以及 argv 含 `mcp` 的 process，依它最上層的非系統祖先 app 分組，例如「Claude」「Code」「Hermes」。

**殭屍（Z 狀態）**：留在原本所屬的位置，加上「殭屍」標記；畫面最上方顯示殭屍數量。

**Windows 特有處理**：

- parent 關係一律用啟動時間驗證。
- MCP 常見的啟動方式是 `cmd.exe /c npx …` → `node.exe`，旁邊還有 `conhost.exe`，這些都收進 MCP 節點，標為「外殼」。

### 4.4 記憶體模型

每個 process 會顯示以下幾種數字：

| 畫面名稱 | 白話意思 | macOS 來源 | Windows 來源 | Linux 來源 |
|---|---|---|---|---|
| **記憶體**（主欄位，用於加總與排序） | 這個 process 自己獨佔的量，結束它就能拿回來 | `phys_footprint`（等於活動監視器的「記憶體」） | Private Bytes（`PrivateUsage`，等於工作管理員「詳細資料」頁的「認可大小」） | `RssAnon + VmSwap` |
| ├ 在 RAM 裡 | 現在實際放在 RAM 中的部分 | `TASK_VM_INFO.internal` | `PrivateWorkingSetSize`（等於工作管理員「處理程序」頁的「記憶體」） | `RssAnon` |
| └ 被擠出 RAM | 很久沒用，被系統壓縮或搬到硬碟；不太影響速度，但仍佔資源 | `TASK_VM_INFO.compressed` | `PrivateUsage − PrivateWorkingSetSize`，標「約」 | `VmSwap`（包含 zram／zswap 壓縮的部分） |
| 共用部分（只顯示，不加總） | 程式碼、函式庫等與其他 process 共用的部分 | `resident − internal` | `WorkingSetSize − PrivateWorkingSetSize` | `RssFile + RssShmem`（`htop` 的 RES 等於 `VmRSS`，包含共用部分） |

- **一定要說清楚的限制**：
  - macOS 不提供單一 process 的 swap 用量，只能知道被壓縮了多少，被壓縮的資料可能再被搬進 swap。
  - Windows 不提供單一 process 被壓縮多少，所以「被擠出 RAM」是分頁檔、壓縮和已預留未使用空間的合計。
  - Linux 是三個平台中資訊最完整的，可以直接讀到單一 process 的 swap 用量。
  - 以上限制都要寫在 `?` 說明頁。
- **畫面最上方**顯示系統整體：RAM 已用／總量、swap 已用（Windows 顯示分頁檔／認可）、cctree 自己用了多少。
- **加總**：分組列與有子孫的 process 列，加總的是主欄位「記憶體」。只要有任何成員讀不到，就在數字前標「約」。共用部分不加總。
- **下方詳細面板**：選到某個 process 時，顯示上表全部數字，並註明「活動監視器／工作管理員會顯示 X」，方便使用者對照。
- **白話句要加得起來**：macOS 的 footprint 不完全等於「internal ＋ compressed」。2026-10-22 實測一個 `npm exec`：footprint 63.6 MB，但 internal 0.1 ＋ compressed 56.0，還差 7.5 MB。因此差額大於主數字的 1% 時，要加一項「其他」（系統記帳的其他部分），讓「＝」兩邊永遠相等。Windows、Linux 的對應方式本身就加得起來，不需要這一項。

### 4.5 CPU

數字跟該平台的系統工具一致：

- macOS 與 Linux 以單一核心為 100%，可以超過 100%，跟活動監視器、`top`、`htop` 相同。
- Windows 以整台機器為 100%，跟工作管理員相同。

最上方另外顯示整體 CPU 使用率。

### 4.6 介面

**版面**，由上到下：

1. 標題列，1 行
2. 樹狀表格
3. 詳細面板，3～4 行
4. 訊息列，1 行，顯示錯誤或操作結果
5. 按鍵提示列，1 行

```
cctree  每 2 秒更新 · RAM 已用 27/36 GB · swap 2.3 GB · cctree 本身 12 MB     ? 說明
 名稱                                  PID     CPU    記憶體   個數
▼ Claude Code（12 個 session）                  1.2%   7.4 GB    159
  ▼ ~/prj/cctree                       49049    0.4%   1.9 GB     17
      （本體）                         49049    0.4%   550 MB
    ▼ MCP（5 個）                              0.0%   1.3 GB     12
      ▶ chrome-cdp                    49721    0.0%   330 MB      3
      ▼ chrome-live                   50094    0.0%   413 MB      3
          npx 外殼                     49815    0.0%    97 MB
          chrome-devtools-mcp         50094    0.0%   162 MB
          └ telemetry watchdog        50485    0.0%   154 MB
      ▶ playwright（plugin）           49769    0.0%   215 MB      2
    ▶ 其他子程序（shell、LSP…）                 0.1%    12 MB      4
  ▶ ~/prj/另一個專案                   52091    0.0%   545 MB     12
▶ Codex（目前沒有在執行）
▼ ⚠ 已失聯（1）
  ？ChatGPT Meetings（疑似）           80871    0.2%   102 MB
▶ 其他 Node.js（1）
──────────────────────────────────────────────────────────────────
 chrome-live › chrome-devtools-mcp    PID 50094    父 PID 49815
 記憶體 162 MB ＝ 在 RAM 裡 112 MB ＋ 被擠出 RAM 42 MB ＋ 其他 8 MB（另有 15 MB 與其他程式共用）
 指令：chrome-devtools-mcp --autoConnect    已執行 15 分鐘
──────────────────────────────────────────────────────────────────
 ↑↓ 移動  ←→ 收合/展開  x 結束這個  X 結束整棵  m MCP 總覽  空白 暫停  ? 說明  q 離開
```

（數字為示意）

**列的種類**

- **process 列**：有自己的 PID。session 列就是 Claude Code／Codex 本體 process。
- **分組列**：四大類、「MCP（n 個）」、各個 MCP、「其他子程序」。分組列沒有自己的 PID，但 MCP 列在 PID 欄顯示 MCP 主程式（server）的 PID，以灰色表示。

**加總方式**（類似 ncdu）：每一列顯示「自己加上底下全部」的總和。有子孫的 process 列展開後，第一個子列是「（本體）」，顯示它自己的數字，所以畫面上的數字永遠加得起來。

**例外：MCP 底下的成員平鋪顯示**。一個 MCP 展開後，成員 process（外殼、server、輔助）依啟動順序排成同一層，每列只顯示自己的數字，輔助 process 前面加 `└`，跟上方 mockup 一樣。這裡不做層層巢狀，也不加「（本體）」列，因為 MCP 那一列本身就是這組的加總。

**預設展開**

- 預設展開：Claude Code、Codex、已失聯三大類。
- 預設收起：每個 session、其他 Node.js。

**排序**

- 預設依記憶體由大到小。按 `s` 依序切換：記憶體 → CPU → PID → 名稱。
- 只在同一層的兄弟節點之間排序；四大類的順序固定不變。

**游標穩定**

- 游標以節點 ID 記錄，重新排序或更新時都會停在同一個節點上。
- 如果選取的 process 已經結束，游標移到它的 parent 或最近的兄弟節點，並在訊息列顯示「剛才選取的 process 已結束」。

**變化標示**：新出現的 process 以綠色顯示約 5 秒；剛結束的 process 以灰色保留一次更新後才移除。

**MCP 總覽**：按 `m` 切換。改成依 MCP 名稱跨所有 session 加總，例如「playwright：12 份、24 個 process、1.2 GB」；展開後依序是每個 session 的那一份，再往下是各個 process。再按一次 `m` 切回原本的檢視。

**按鍵**

| 按鍵 | 功能 |
|---|---|
| `↑` `↓`／`k` `j` | 上下移動 |
| `PgUp` `PgDn` `Home` `End` | 快速移動 |
| `←`／`h` | 收合；已經收合時跳到 parent |
| `→`／`l` | 展開 |
| `Enter` | 切換展開或收合 |
| `x` | 結束這一個 process |
| `X` | 結束整棵或整組 |
| `m` | 切換 MCP 總覽 |
| `s` | 切換排序方式 |
| `/` | 搜尋名稱或 PID；`Esc` 清除 |
| `空白鍵` | 暫停或恢復更新；暫停時標題列顯示「已暫停」 |
| `+` `-` | 調整更新間隔，1～10 秒 |
| `?` | 說明，包含按鍵與記憶體名詞解釋 |
| `q`／`Ctrl+C` | 離開 |

另外支援滑鼠滾輪捲動和點選。

**遮蔽機密**：命令列一律先遮蔽再顯示，不提供顯示原文的選項。遮蔽依常見寫法判斷，不承諾涵蓋所有寫法。至少要遮蔽以下內容：

- `--api-key`、`--token`、`--password`、`--secret`、`--key` 後面接的值，以及 `--x=值` 形式中名稱含 key／token／secret／password／auth 的值。
- 參數中任何位置的 `名稱=值`（名稱含上述字樣），不只是 `--x=值`。
- 常見金鑰前綴：`sk-`、`ctx7sk-`、`ghp_`、`gho_`、`xox`、`AKIA`。
- 網址裡的帳號密碼（`https://user:pass@`），以及像 token 的網址使用者名稱（`https://<token>@`）。
- `Bearer` 後面的值。
- HTTP header 的值，名稱含上述字樣時（`X-Api-Key: 值`、`Authorization: Basic 值`），驗證方式的字（`Basic` 等）保留。
- JSON 裡名稱含上述字樣的字串值（`"API_KEY":"值"`，例如 `--mcp-config` 帶的整段設定）。

**路徑**：家目錄一律縮寫成 `~`。

**介面語言**：

- 依系統語言自動選擇：macOS 與 Linux 讀 `LC_ALL`／`LC_MESSAGES`／`LANG`，Windows 讀使用者介面語言。
- `zh_TW`、`zh_HK`、`zh_Hant*` 使用繁體中文，其他一律英文。
- 可用環境變數 `CCTREE_LANG` 強制指定。
- v1 只支援這兩種語言。

**顏色**：深色、淺色終端機都要清楚可讀；有設定 `NO_COLOR` 時不使用顏色。

### 4.7 結束 process

**範圍**

- `x` 用在 process 列：只結束該 process。
- `x` 用在分組列：不做任何事，訊息列顯示「這一列是一組 process，要結束整組請按 X」。
- `X` 用在 process 列：結束該 process 和它所有子孫。子孫依目前 snapshot 中經過驗證的 parent 關係判定。
  - 如果是 session 本體，也包含巢狀的子 session，以及「已脫離（仍屬於這個 session）」的 process。已脫離的在確認視窗中另外列一段。
- `X` 用在分類列（「Claude Code」「Codex」「已失聯」「其他 Node.js」）：一律擋下，訊息列顯示「這一列是整個分類，範圍太大；請到它底下的個別列上按 X」。**沒有任何分類層級的批次結束**。
  - 原因：分類底下是所有 session，或所有和 AI 工具無關的 node process（包括使用者自己在終端機開的 dev server、其他 app 的 node 服務）。一個 `X` 加一個 `Enter` 就全部結束，風險太高。
  - 已失聯的 process 也一樣，一律從它自己那一列結束。「疑似」的可能是使用者正在用的 app（例如 `ChatGPT Meetings`），就算「確定」，也要使用者看過那一列再決定。
- `X` 用在其他分組列（session、MCP 總類、單一 MCP、其他子程序、已脫離、Node.js 分組）：結束組內所有 process 和它們的子孫。
- **看到什麼，就結束什麼**：`X` 結束的是畫面上選取那一列底下畫出來的 process，加上它們沒有被分類追蹤的子 process（沒有被辨識成 session、MCP 等的後代）。畫在別的列底下的 process 一律不動，例如畫在「其他子程序」底下的巢狀 session，或在已脫離的 tmux server 裡啟動的 session，連同它們的子孫都跳過。

**確認視窗**

- 列出每個要結束的 process 的 PID 與名稱，超過 10 個時顯示「還有 N 個」。
- 顯示「約可釋放 X MB」，用主欄位「記憶體」加總。
- `Enter` 確定，`Esc` 取消；只需要確認這一次。
- 打開確認視窗時，把目標清單記錄為 `(pid, start_time)`。之後畫面繼續更新，但不會改變這份清單。

```
┌ 結束整組 process？ ──────────────────────────────┐
│ chrome-live（MCP，屬於 ~/prj/cctree 的 Claude Code）│
│   49815  npx 外殼                                │
│   50094  chrome-devtools-mcp                     │
│   50485  telemetry watchdog                      │
│ 共 3 個，約可釋放 413 MB                          │
│                                                  │
│ 結束後 Claude Code 會顯示這個 MCP 已斷線，          │
│ 需要時可在 Claude Code 裡用 /mcp 重新連線。         │
│                                                  │
│      Enter 確定結束          Esc 取消              │
└──────────────────────────────────────────────────┘
```

**執行**

1. 按下確定時，逐一核對每個目標的 `(pid, start_time)`。不符合的跳過不動，並在結果中註明「PID 已被其他程式重用，為了安全沒有動它」。
2. macOS 與 Linux：從最深的子孫往上，依序送出 `SIGTERM`；最多等 3 秒，期間在訊息列顯示進度；還沒結束的再送 `SIGKILL`。
3. Windows：從最深的子孫往上，依序用 `TerminateProcess` 直接強制結束。確認視窗要事先寫明「Windows 會直接強制結束」。
4. 結果用一句話顯示在訊息列，例如「已結束 3 個（其中 1 個沒回應，已強制結束；1 個已經自己結束）」。
5. 判斷「已經結束」時，殭屍狀態也算已結束。被結束的 process 在 parent 回收之前會短暫變成殭屍，不能因此等滿 3 秒再誤報「沒回應，已強制結束」。送訊號時如果發現 process 已經不在，算進「已經自己結束」，不算失敗。

**會擋下來，並說明原因的情況**

- cctree 自己，以及它的所有祖先（例如執行它的 shell、終端機）。
- PID 0、1，以及 root／SYSTEM 擁有的 process，一律擋下，包括用 `sudo cctree` 執行時。
- 其他使用者的 process（Windows 上是存取被拒）。訊息在 macOS／Linux 建議改用 `sudo cctree`；在 Windows 建議到 cctree 以外（工作管理員）結束，因為以系統管理員身分執行並不等於 SYSTEM，cctree 仍會擋下。cctree 以 root 執行時，其他使用者（非 root）的 process 可以結束；但不讀它們的環境變數與工作目錄，因為設定檔的位置是從這些值找出來的，root 不該去開其他使用者決定的路徑。
- 殭屍 process：說明它已經結束、不佔記憶體，要結束它的 parent 才會消失，並顯示 parent 的 PID。

**確認視窗中的警告**

- 目標包含還在執行的 Claude Code session 本體：用紅字提醒「這會中斷 <路徑> 的對話；對話紀錄不會遺失，之後可以用 `claude --resume` 接續」。
- 目標包含 Codex session 本體：同樣提醒，接續指令改為 `codex resume`。
- 接續指令只在 session 是從終端機啟動時顯示。如果是由 app 啟動（VS Code、Claude Desktop 等），改成「可以從 <啟動者> 重新開啟」。
- 目標包含 MCP：Claude Code 提示可以用 `/mcp` 重新連線；Codex 提示需要時重新開啟 session。

### 4.8 錯誤訊息

固定格式：**發生什麼事 → 影響什麼 → 怎麼解決**。至少涵蓋以下情況：

| 情況 | 訊息重點 |
|---|---|
| 有些 process 讀不到細節 | 只計算「名稱看起來是 node／npm／npx／bun／deno／claude／codex，但讀不到命令列」的 process。macOS 上本來就有幾百個系統 process 讀不到，全部算進來的話，這則訊息會永遠掛著，沒有意義。訊息內容：顯示數量；Claude／Codex 的統計不受影響；想看全部的話，用 `sudo cctree` 或以系統管理員身分執行 |
| Windows 版本太舊，讀不到 Private Working Set | 「在 RAM 裡」欄顯示「—」；請更新 Windows |
| 結束 process 時權限不足 | 說明原因與替代做法 |
| 要結束的 process 已經不存在 | 說明它已經自己結束了 |
| PID 已被重用 | 說明為了安全沒有動它 |
| 對殭屍 process 按結束 | 說明它不佔資源，要結束 parent，並附上 parent 的 PID |
| 想結束 cctree 自己或它的祖先 | 說明為什麼不行 |
| 終端機視窗太小 | 顯示目前尺寸，請放大到至少 80×24 |
| 輸出不是終端機（例如被導向檔案） | 說明 cctree 需要在終端機裡互動執行 |
| 讀不懂 Claude／Codex 設定檔 | 說明 MCP 改用套件名稱命名，並附上檔案路徑 |
| 在 WSL 裡執行 | 偵測方式：`WSL_DISTRO_NAME` 環境變數，或 `/proc/sys/kernel/osrelease` 含 `microsoft`。標題列常駐提示「這裡只看得到 WSL 裡的 process；Windows 那邊請在 PowerShell 另外執行 cctree」 |
| Windows 上找不到 Claude／Codex，但 WSL 正在執行 | 偵測方式：有 `vmmemWSL` 這類 process。提示「如果你是在 WSL 裡用 Claude Code／Codex，請在 WSL 裡執行 cctree」 |

- 訊息列只顯示最新一則訊息。和單一 process 有關的問題，在選到那一列時顯示在詳細面板。
- 一定要安裝 panic hook：程式當掉時要先把終端機恢復原狀，再用純文字印出錯誤與回報方式。

### 4.9 效能預算

- **CPU 的計算單位**：以單一核心為 100%，跟 macOS 標題列顯示的單位相同。
- **驗收條件**：在作者的 Apple silicon Mac 上（約 1000 個 process）、預設 2 秒更新一次時，cctree 自身 1 分鐘的平均 CPU 不超過 2%，記憶體低於 50 MB；單次收集工作在 40 ms 內完成。
- **兩層收集**：
  - 每次更新時，全部 process 只列出 PID、PPID、啟動時間這些便宜的資訊，用來判斷哪些是新出現、哪些已經結束。
  - 新出現的 process 才讀一次 argv、exe、cwd 和環境變數，然後快取起來。
  - **CPU 在第一層一起算**，所有 process 都算。CPU = 兩次 snapshot 之間累計 CPU 時間的變化 ÷ 經過的實際時間，不用 sysinfo 自己的 `cpu_usage()`（它第一次的讀數不可靠）。原因（2026-10-22 查 sysinfo 原始碼）：sysinfo 在 Linux 上每次 refresh process，不管有沒有要求 CPU，都會呼叫 `set_time` 把「上次的 CPU 時間」換成這次的值。如果第二層才另外讀 CPU，比較的對象是幾毫秒前第一層剛記下的數字，CPU 會永遠接近 0。而且第一層本來就會讀到算 CPU 需要的資料，一起算幾乎不增加成本。
  - **平台記憶體 API 只對會顯示在畫面上的 process 讀取**，也就是 Claude Code、Codex、已失聯、其他 Node.js 這四類。系統整體數字用全系統的 API 取得。
- 設定檔只在修改時間改變時重讀；畫面只在資料更新或有輸入時重畫。

### 4.10 隱私與安全

- 執行時不連網、不收集任何遙測資料、不寫入任何檔案。
- 只讀取以下資料：process 資訊；Claude／Codex 設定檔中 MCP 的 `command` 與 `args`。設定檔中的 `env` 值一律不讀出、不顯示。
- 環境變數只取允許清單內的名稱。
- 命令列一律先遮蔽再顯示（§4.6）。
- README 寫明：非官方，與 Anthropic、OpenAI 無關。

### 4.11 發佈

- 用 `dist`（原名 cargo-dist）在 GitHub Actions 上產生 GitHub Releases，提供以下安裝方式：
  - macOS／Linux 風格的一行安裝：`curl … | sh`
  - Windows 的一行安裝：`irm … | iex`
  - Homebrew formula，放在作者自己的 tap
- 另外發佈到 crates.io，可以用 `cargo install cctree` 安裝。
- v1 不做 macOS notarization 和 Windows 程式碼簽章。透過 Homebrew、`curl`、`irm` 安裝不會被加上「從網路下載」的標記，所以不會跳出安全警告；用瀏覽器下載 zip 的使用者，README 要寫出如何解除警告。
- 授權：MIT OR Apache-2.0（Rust 慣例）。
- 版本：semver，並維護 CHANGELOG。
- CI：在 macOS、Windows、Linux（Ubuntu）runner 上跑 `fmt`、`clippy` 和測試；推送 tag 時用 `dist` 發佈。`curl … | sh` 和 Homebrew 在 Linux 上也適用。

### 4.12 測試策略

- **核心邏輯**：辨識、建樹、加總、結束 process 的規劃、遮蔽機密、設定檔比對都是純函式，用 snapshot fixture（JSON）做單元測試。第一份 fixture 來自 §1.1 的實測機器（已遮蔽機密）；Windows 與 Linux 的 fixture 由實機實測補上。
- **開發者用的匯出指令**：提供一個隱藏參數，例如 `cctree --dump-snapshot <檔案>`，匯出已遮蔽機密的 snapshot，用來製作 fixture，也方便日後的貢獻者回報問題。一般使用者不需要知道這個參數。
- **fixture 公開前要給作者確認**：遮蔽與匿名化只處理機密與家目錄，專案目錄名稱（可能是客戶或私人專案）仍會留在 cwd 與路徑裡，而 repo 預計公開。所以 commit fixture 前，要把裡面出現的所有路徑列給作者確認；作者指定的名稱改成佔位名稱。
- **平台記憶體讀取**：在三個平台的 CI runner 上讀取自己這個 process，做合理性檢查，例如 footprint ≥ internal、`VmRSS` ≥ `RssAnon`。
- **結束 process**：整合測試會實際啟動一棵假的 process 樹，再用 actions 模組結束它；PID 重用的防護另外做單元測試。
- **介面**：用 ratatui 的 `TestBackend` 做畫面快照測試，涵蓋中文對齊、窄視窗、確認視窗。
- **手動驗證清單**：在每個平台上，搭配真的 Claude Code 和 Codex 各跑過一次。
  - macOS：作者的 Mac。
  - Windows：作者的 Windows 實機或 VM。
  - Linux：WSL，以及作者 Mac 上的 Docker 容器。

## 5. 留給實作者決定

- 模組的細部切分、內部型別、執行緒與 channel 的實作方式
- 樹狀表格要自己做（攤平的樹放進 `Table`，方便加欄位；建議這樣做），還是改用 `tui-tree-widget`
- 具體的顏色與樣式（須符合 §4.6 的可讀性要求）
- 遮蔽機密規則中，§4.6 列出的必要項目以外的部分
- 訊息的實際用字（須符合 §4.8 的格式）與英文翻譯
- 滑鼠操作的細節
- 「新出現的 process」綠色標示的確切持續時間

## 6. 待實測與風險

1. **Codex 的 process 結構**：CLI、`app-server`、npm 外殼各自長什麼樣子，子 process 會帶哪些環境變數，MCP 怎麼啟動。作者的 Mac 有安裝 Codex，可以直接實測。
2. **Windows 實際情況**：作者有 Windows 實機或 VM，所以開發計畫一開始就要排 Windows 實測，跟 macOS 並行，不要等 macOS 做完才開始。要確認的項目：
   - Claude Code 原生版與 npm 版的 process 長相
   - MCP 是否透過 `cmd /c npx` 啟動
   - 能否讀取其他 process 的環境變數
   - `PROCESS_MEMORY_COUNTERS_EX2` 是否可用
   - conhost 和 Windows Terminal 顯示中文是否對齊
   - Codex 在 Windows 沙箱裡的 process
3. **Linux 與 WSL 實際情況**：
   - Claude Code、Codex 在 Linux 上的 process 長相
   - 桌面環境中，孤兒是否交給 `systemd --user` 收養
   - WSL 的 `/init` 行為
   - 用 Docker 容器測試時，容器裡的 PID 1 不是 init
4. **Claude Code npm 安裝版**的 process 特徵。
5. **名稱**：2026-10-22 查詢時，`cctree` 在 crates.io、Homebrew、npm 都還沒被使用。GitHub 上有幾個同名的小型 repo，其中包含幾個 Claude Code session 瀏覽工具（各 0～2 顆星），另有一個舊的 Vim 外掛 `CCTree`。這些都不會在套件管理工具上衝突。
6. **Claude Code／Codex 改版風險**：兩者改版可能改變 process 結構或環境變數。辨識規則要集中在一處，並有 fixture 測試保護。
