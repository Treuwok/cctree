# cctree

**非官方工具，與 Anthropic、OpenAI 無關。**

[English](README.md) | 繁體中文

`cctree` 是一個類似 `top` 的終端機工具。它顯示 Claude Code 和 Codex 在你電腦上啟動的 process，依「是哪個 session 啟動的」分組，並讓你安全地結束單一 process 或一整組。

```text
cctree  每 2 秒更新 · RAM 已用 27.0 GB/36.0 GB · swap 2.3 GB · cctree 本身 12 MB              ? 說明
  名稱                                                                PID     CPU      記憶體   個數
▼ Claude Code（2 個 session）                                                0.5%      2.0 GB     13
  ▼ ~/prj/cctree                                                    49049    0.5%      1.5 GB     11
      （本體）                                                      49049    0.4%      550 MB
    ▼ MCP（3 個）                                                            0.0%      958 MB      8
      ▼ chrome-live                                                 50094    0.0%      413 MB      3
          npx 外殼                                                  49815    0.0%       97 MB
          chrome-devtools-mcp                                       50094    0.0%      162 MB
          └ watchdog                                                50485    0.0%      154 MB
      ▶ chrome-cdp                                                  49722    0.0%      330 MB      3
      ▶ playwright（plugin）                                        49770    0.0%      215 MB      2
    ▶ 其他子程序（shell、LSP…）                                              0.1%       12 MB      2
  ▶ ~/prj/非常非常長的中文專案目錄名稱/子資料夾                     52091    0.0%      551 MB      2
  Codex（目前沒有在執行）
▼ ⚠ 已失聯（1）                                                              0.2%      102 MB      1
    ？ChatGPT Meetings（疑似）                                      80871    0.2%      102 MB
▶ 其他 Node.js（1）                                                          0.0%       88 MB      1





────────────────────────────────────────────────────────────────────────────────────────────────────
 chrome-live › chrome-devtools-mcp    PID 50094    父 PID 49815
 記憶體 162 MB ＝ 在 RAM 裡 112 MB ＋ 被擠出 RAM 42 MB ＋ 其他 8 MB（另有 15 MB 與其他程式共用）
 活動監視器會顯示 162 MB
 指令：chrome-devtools-mcp --autoConnect    已執行 15 分鐘

 ↑↓ 移動  ←→ 收合/展開  x 結束這個  X 結束整棵  m MCP 總覽  s 排序  空白 暫停  ? 說明  q 離開
```

（示意數字）

## 用途

用 cctree 查看 Claude Code 和 Codex 啟動了哪些 process、各用了多少 CPU 與記憶體，以及哪些可以結束。

## 解決什麼問題

Claude Code 和 Codex 每個 session 都會啟動許多 process，例如 MCP server、MCP server 外面那層 `npx`、shell 和語言伺服器（LSP）。其中有些 process 在 session 結束後還會繼續執行。

系統的 process 清單（活動監視器、工作管理員或 `top`）把這些 process 平鋪成一長串，很多名字都一樣，例如 `node`。清單看不出 process 是哪個 session 啟動的，所以很難找出是誰在吃記憶體；不知道主人是誰，也不敢隨便結束它。

cctree 把同樣這些 process 排成一棵樹，每個 process 都掛在啟動它的 session 底下。

## 功能

- **依 session 分組的樹。** 每個 process 掛在啟動它的 Claude Code 或 Codex session 底下。MCP server 用你 Claude Code 或 Codex 設定檔裡的名字顯示。
- **每一列都有數字。** 每個 process、每一組都顯示 PID、CPU 與記憶體。記憶體數字和活動監視器、工作管理員對得上。
- **MCP 總覽。** 按 `m` 看每個 MCP server 在所有 session 加起來的用量。
- **已失聯的 process。** 找出主人已經結束、卻還在執行的 process，標上「確定」或「疑似」，並說明判定原因。
- **安全結束。** 按 `x` 結束單一 process，按 `X` 結束整棵。每次都會先請你確認。cctree 不會結束它自己、執行它的 shell 與終端機、系統 process，或其他使用者的 process。
- **隱私。** 不連網、不收集遙測資料、不寫入檔案。命令列裡的 API 金鑰、token、密碼會先遮蔽再顯示。
- **平台。** macOS、Windows、Linux（包含 WSL）。不需要管理員權限。
- **語言。** 英文與繁體中文。

## 安裝

### macOS 與 Linux

在終端機執行：

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/Treuwok/cctree/releases/latest/download/cctree-installer.sh | sh
```

安裝程式會把 `cctree` 放在 `~/.cargo/bin`。如果 shell 找不到 `cctree`，請開一個新的終端機。

### Windows

在 PowerShell 執行：

```powershell
powershell -ExecutionPolicy Bypass -c "irm https://github.com/Treuwok/cctree/releases/latest/download/cctree-installer.ps1 | iex"
```

安裝程式會把 `cctree.exe` 放在 `%USERPROFILE%\.cargo\bin`。請在 Windows Terminal 或 PowerShell 裡執行 `cctree`。

### WSL

Windows 版只看得到 Windows 的 process，WSL 裡的 Linux 版也只看得到 WSL 裡的 process。如果你在 WSL 裡使用 Claude Code 或 Codex，請在 WSL 裡用上面 macOS 與 Linux 的指令安裝。要看 Windows 那邊的 process，請另外在 Windows 安裝。cctree 偵測到這種情況時會顯示提示。

### 直接下載執行檔

[Releases](https://github.com/Treuwok/cctree/releases) 頁面有以下平台的壓縮檔：

| 平台 | 壓縮檔 |
|---|---|
| macOS，Apple silicon | `cctree-aarch64-apple-darwin.tar.xz` |
| macOS，Intel | `cctree-x86_64-apple-darwin.tar.xz` |
| Windows x64 | `cctree-x86_64-pc-windows-msvc.zip` |
| Linux x64（靜態連結） | `cctree-x86_64-unknown-linux-musl.tar.xz` |
| Linux arm64（靜態連結） | `cctree-aarch64-unknown-linux-musl.tar.xz` |

每個壓縮檔裡有執行檔、README、CHANGELOG 與授權檔。解壓縮後，把執行檔放進 `PATH` 裡的資料夾。

執行檔沒有程式碼簽章。用瀏覽器下載壓縮檔時，macOS 的 Gatekeeper 或 Windows 的 SmartScreen 可能會跳出警告。用上面的安裝指令不會有這個警告。

- macOS：解壓縮後執行 `xattr -d com.apple.quarantine ./cctree`。
- Windows：在 SmartScreen 視窗按「其他資訊」，再按「仍要執行」。

### 用 Cargo 安裝

如果你已經裝了 Rust，可以直接從最新的原始碼編譯並安裝：

```sh
cargo install --git https://github.com/Treuwok/cctree --locked
```

## 從原始碼編譯

### 事前準備

1. 用 [rustup](https://rustup.rs) 安裝 Rust stable。
2. 安裝你平台的 C linker：
   - macOS：執行 `xcode-select --install`。
   - Windows：安裝 Visual Studio Build Tools，並勾選「使用 C++ 的桌面開發」。rustup 安裝時會提示這一步。
   - Linux：安裝 `gcc`（例如 `sudo apt-get install build-essential`）。

### 編譯

```sh
git clone https://github.com/Treuwok/cctree.git
cd cctree
cargo build --release
```

執行檔在 `target/release/cctree`（Windows 是 `target\release\cctree.exe`）。

要安裝到 `~/.cargo/bin`，執行 `cargo install --path . --locked`。

### 靜態連結的 Linux 版

Linux 的發行版是靜態連結的 musl 版本。要編出一樣的版本：

```sh
sudo apt-get install musl-tools
rustup target add x86_64-unknown-linux-musl
cargo build --release --target x86_64-unknown-linux-musl
```

### 測試

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

每個 pull request 都會在 macOS、Windows、Linux 上由 CI 跑這三個指令。

### 發布新版本

1. 修改 `Cargo.toml` 的 `version`。
2. 在 `CHANGELOG.md` 寫下這一版的改動。
3. 把這些改動合併進 `main`。
4. 推一個同版本的 tag，例如 `git tag v0.1.1` 與 `git push origin v0.1.1`。

發布流程會編譯所有平台，並把壓縮檔與安裝程式發布到 Releases 頁面。

## 使用

在終端機（至少 80×24）裡執行 `cctree`，不需要任何參數。滑鼠滾輪可以捲動，點一列可以選取或展開收合。

| 按鍵 | 功能 |
|---|---|
| `↑ ↓ ／ k j` | 上下移動 |
| `PgUp PgDn Home End` | 快速移動 |
| `← ／ h` | 收合。已經收合時，跳到上一層。 |
| `→ ／ l` | 展開 |
| `Enter` | 展開或收合 |
| `x` | 結束這一個 process |
| `X` | 結束整棵或整組 |
| `m` | 開關 MCP 總覽 |
| `s` | 切換排序（記憶體 → CPU → PID → 名稱） |
| `/` | 搜尋名稱或 PID。Esc 清除搜尋。 |
| `空白鍵` | 暫停或恢復更新 |
| `+ -` | 調整更新間隔（1～10 秒） |
| `?` | 說明 |
| `q ／ Ctrl+C` | 離開 |

第一次的 CPU 數字需要兩次取樣，所以一開始會短暫顯示「—」。有設定 `NO_COLOR` 時不使用顏色。

介面語言跟著系統：`zh_TW`、`zh_HK`、`zh_Hant*` 顯示繁體中文，其他語言顯示英文。要指定語言，設定 `CCTREE_LANG`（例如 `zh_TW` 或 `en`）。

### `x` 和 `X` 會結束什麼

- 每次結束都只確認一次：Enter 確定，Esc 取消。確認視窗會列出要結束的 process，以及可以拿回的記憶體。
- `X` 結束你在選取那一列底下看到的 process，加上它們底下 cctree 沒有顯示的子 process。畫在別的列底下的 process 不會被動到。
- 在整個分類（Claude Code、Codex、已失聯、其他 Node.js）上按 `X` 會被擋下，因為範圍太大。要結束已失聯的 process，請選它自己那一列再按 `x`。
- macOS 與 Linux：從最深的子孫開始送 SIGTERM，3 秒後還在執行的再送 SIGKILL。Windows：直接強制結束。
- cctree 不會結束這些 process：cctree 自己、執行它的 shell 和終端機、PID 0、PID 1，以及 root 或 SYSTEM 擁有的 process。其他使用者的 process 也不會結束，除非 cctree 以 root 執行（`sudo cctree`）。

## 記憶體數字的意思

每個 process 會顯示四種記憶體數字，讓你能和系統自己的工具對照。

| 畫面名稱 | 意思 | macOS | Windows | Linux |
|---|---|---|---|---|
| **記憶體** | 只有這個 process 在用的記憶體，結束它就能拿回來。加總與排序都用這個數字。 | `phys_footprint`（活動監視器的「記憶體」） | Private Bytes（工作管理員「詳細資料」頁的「認可大小」） | `RssAnon` + `VmSwap` |
| 在 RAM 裡 | 現在實際放在 RAM 中的部分。 | `internal` | Private Working Set（工作管理員「處理程序」頁的「記憶體」） | `RssAnon` |
| 被擠出 RAM | 一段時間沒用，被系統壓縮或搬到硬碟的部分。不太影響速度，但仍佔資源。 | `compressed` | Private Bytes − Private Working Set，標「約」 | `VmSwap` |
| 共用部分 | 與其他 process 共用的程式碼與函式庫。只顯示，不加進總計。 | `resident` − `internal` | Working Set − Private Working Set | `RssFile` + `RssShmem` |

各系統的限制：

- macOS 不提供單一 process 的 swap 用量，只提供被壓縮的部分，而被壓縮的資料之後可能再搬進 swap。系統的數字加不起來時，cctree 會多一項「其他」補上，讓詳細面板裡「＝」兩邊永遠相等。
- Windows 不提供單一 process 被壓縮的部分，所以「被擠出 RAM」是分頁檔、壓縮和已預留但沒用到的空間的合計。「在 RAM 裡」需要 Windows 10，或裝有 2023 年 9 月更新的 Windows 11 22H2；版本較舊時顯示「—」。
- Linux 的資料最完整，可以讀到每個 process 的 swap 用量。`htop` 的 RES 包含共用部分，所以會比「記憶體」大。

總計前面的「約」表示有些成員讀不到，實際數字可能更高。cctree 讀不到的欄位（例如其他使用者的 process）會顯示「—」並說明原因。

CPU 在 macOS 與 Linux 以一個核心為 100%，可以超過 100%（和 `top` 相同）；在 Windows 以整台電腦為 100%（和工作管理員相同）。

## 已失聯的 process

「已失聯」列出主人已經結束、卻還在執行的 process。cctree 依這些證據判斷：

- process 帶有某個 Claude Code session 的 ID，而那個 session 已經結束。
- process 帶有 `--parent-pid`，而那個 process 已經不存在。
- process 從 Codex 或 Claude 的目錄執行。這條證據只在沒有任何 process 啟動它、而且當下沒有該工具的 session 在執行時才使用。

每個已失聯的 process 都會標「確定」或「疑似」，詳細面板會寫出原因。這些標記只是提示：

- 「確定」表示它已被 `launchd`、PID 1 或 subreaper 收養（或 parent 已不存在）、不是你在用的 app，而且沒有任何執行中的 session 可能是它的主人。
- 「疑似」表示證據沒那麼強，它可能是你正在用的 app。

cctree 不會批次結束已失聯的 process。請逐列看過，再按 `x` 結束。

## 隱私

- cctree 不連網、不收集遙測資料、不寫入任何檔案。
- 它讀取 process 資訊，以及 Claude Code 與 Codex 設定檔裡 MCP server 的 `command` 與 `args`。它不讀設定檔裡的 `env` 值，也不修改這些檔案。
- 命令列一律先遮蔽再顯示，包括 API 金鑰、token、密碼、`Bearer` 後面的值、`X-Api-Key:` 這類 header 的值，以及網址裡的帳號密碼。沒有顯示原文的選項。遮蔽依常見寫法判斷，無法保證涵蓋所有寫法。
- 從其他 process 只保留這幾個環境變數名稱：`CLAUDECODE`、`CLAUDE_CODE_SESSION_ID`、`CLAUDE_PROJECT_DIR`、`CLAUDE_CONFIG_DIR`、`CLAUDE_CODE_ENTRYPOINT`、`CODEX_HOME`。其他變數一律不保留。

## 授權

你可以在 [Apache License 2.0](LICENSE-APACHE) 或 [MIT 授權](LICENSE-MIT) 中擇一使用 cctree。

cctree 是非官方工具，與 Anthropic、OpenAI 無關。
