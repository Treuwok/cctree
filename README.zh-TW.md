# cctree

**非官方工具，與 Anthropic、OpenAI 無關。（Unofficial; not affiliated with Anthropic or OpenAI.）**

[English](README.md) | 繁體中文

`cctree` 是像 `top` 一樣的終端機工具，讓你看到 Claude Code 和 Codex 在你電腦上帶出了哪些 process。它把每個 process 依「是哪個 session 帶出來的」分組（每個 session 底下的 MCP server 用你設定檔裡的名字顯示），每一列、每一組都有 PID、CPU 與記憶體，並能安全地結束單一 process 或整組。它也會指出主人已經結束、卻還留著的 process。支援 macOS、Windows 和 Linux（包含 WSL），不需要管理員權限，執行時完全不連網。

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

## 安裝

macOS 與 Linux：

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/yenchiugu/cctree/releases/latest/download/cctree-installer.sh | sh
```

Windows（PowerShell）：

```powershell
powershell -ExecutionPolicy Bypass -c "irm https://github.com/yenchiugu/cctree/releases/latest/download/cctree-installer.ps1 | iex"
```

Homebrew：

```sh
brew install yenchiugu/tap/cctree
```

Cargo：

```sh
cargo install cctree
```

從原始碼安裝（在這個 repo 的目錄裡）：

```sh
cargo install --path .
```

也可以從 [Releases](https://github.com/yenchiugu/cctree/releases) 頁面下載 macOS（Apple silicon 與 Intel）、Windows x64、Linux（x64 與 arm64，靜態連結）的執行檔。

用瀏覽器下載壓縮檔時，因為執行檔沒有簽章，macOS 的 Gatekeeper 或 Windows 的 SmartScreen 可能會跳出警告。用上面的指令安裝不會有這個警告。

- macOS：解壓縮後執行 `xattr -d com.apple.quarantine ./cctree`。
- Windows：在 SmartScreen 視窗按「其他資訊」，再按「仍要執行」。

## 使用

在終端機（至少 80×24）裡直接執行 `cctree`，不需要任何參數。滑鼠滾輪可以捲動，點一列可以選取或展開收合。

| 按鍵 | 功能 |
|---|---|
| `↑ ↓ ／ k j` | 上下移動 |
| `PgUp PgDn Home End` | 快速移動 |
| `← ／ h` | 收合；已經收合時跳到上一層 |
| `→ ／ l` | 展開 |
| `Enter` | 切換展開或收合 |
| `x` | 結束這一個 process |
| `X` | 結束整棵或整組 |
| `m` | 切換 MCP 總覽 |
| `s` | 切換排序（記憶體 → CPU → PID → 名稱） |
| `/` | 搜尋名稱或 PID；Esc 清除 |
| `空白鍵` | 暫停或恢復更新 |
| `+ -` | 調整更新間隔，1～10 秒 |
| `?` | 說明 |
| `q ／ Ctrl+C` | 離開 |

第一次的 CPU 數字需要兩次取樣，所以一開始會短暫顯示「—」。有設定 `NO_COLOR` 時不使用顏色。介面語言依系統自動選擇（`zh_TW`、`zh_HK`、`zh_Hant*` 顯示繁體中文，其他顯示英文），也可以用 `CCTREE_LANG`（例如 `zh_TW` 或 `en`）指定。

### `x` 和 `X` 會結束什麼

- 每次結束都只確認一次：Enter 確定，Esc 取消。確認視窗會列出要結束的 process，以及約可釋放的記憶體。
- `X` 結束的是「看到的」：選取那一列底下畫出來的 process，加上它們底下 cctree 沒有追蹤的子 process。畫在別的列底下的 process 不會被動到。
- 在整個分類（Claude Code、Codex、已失聯、其他 Node.js）上按 `X` 會被擋下，因為範圍太大。沒有任何批次結束；已失聯的 process 要到它自己那一列結束。
- macOS 與 Linux：從最深的子孫開始送 SIGTERM，3 秒後還沒結束的再送 SIGKILL。Windows：直接強制結束。
- cctree 不會結束自己、執行它的 shell 和終端機、PID 0 和 1，以及 root 或 SYSTEM 的 process。其他使用者的 process 會被擋下，除非 cctree 以 root 執行（`sudo cctree`）。

## 記憶體數字的意思

每個 process 會顯示四種數字，讓你能跟系統自己的工具對照。

| 畫面名稱 | 白話意思 | macOS | Windows | Linux |
|---|---|---|---|---|
| **記憶體** | 這個 process 自己獨佔的量，結束它就能拿回來；加總與排序都用這個數字 | `phys_footprint`（活動監視器的「記憶體」） | Private Bytes（工作管理員「詳細資料」頁的「認可大小」） | `RssAnon` + `VmSwap` |
| 在 RAM 裡 | 現在實際放在 RAM 中的部分 | `internal` | Private Working Set（工作管理員「處理程序」頁的「記憶體」） | `RssAnon` |
| 被擠出 RAM | 很久沒用，被系統壓縮或搬到硬碟；不太影響速度，但仍佔資源 | `compressed` | Private Bytes − Private Working Set，標「約」 | `VmSwap` |
| 共用部分 | 程式碼、函式庫等與其他 process 共用的部分；只顯示，不加總 | `resident` − `internal` | Working Set − Private Working Set | `RssFile` + `RssShmem` |

各系統的限制：

- macOS 不提供單一 process 的 swap 用量，只能知道被壓縮了多少（被壓縮的資料可能再被搬進 swap）。系統記帳還有差額時，會多一項「其他」補上，讓詳細面板裡「＝」兩邊永遠相等。
- Windows 不提供單一 process 被壓縮多少，所以「被擠出 RAM」是分頁檔、壓縮和已預留未使用空間的合計。「在 RAM 裡」需要 Windows 10 或 11 22H2 並裝有 2023 年 9 月的累積更新；版本不足時顯示「—」。
- Linux 的資訊最完整，可以直接讀到單一 process 的 swap 用量。`htop` 的 RES 包含共用部分，所以會比「記憶體」大。

數字前面的「約」表示有些成員讀不到，實際數字可能更高。cctree 讀不到的欄位（例如其他使用者的 process）會顯示「—」並說明原因。

CPU 在 macOS 與 Linux 以單一核心為 100%，可以超過 100%（和 `top` 相同）；在 Windows 以整台機器為 100%（和工作管理員相同）。

## 已失聯的 process

「已失聯」列出主人已經結束、卻還在執行的 process。cctree 依證據判斷：帶起它的 Claude Code session id（該 session 已結束）、已經不存在的 `--parent-pid`，或位於 Codex、Claude 目錄底下的路徑（只限沒有人帶起它，而且當下沒有該工具的 session 在執行）。

每一列都會標「確定」或「疑似」，詳細面板會寫出判定原因。這些標記只是給你的提示：「確定」表示它已被 `launchd`、PID 1 或 subreaper 收養（或 parent 已不存在）、不是你在用的 app，而且沒有任何執行中的 session 可能是它的主人；比這更不確定的標「疑似」，可能是你正在用的 app。沒有任何批次結束，請看過那一列，再用 `x` 結束。

## 隱私

- cctree 執行時不連網、不收集任何遙測資料、不寫入任何檔案。
- 它讀取 process 資訊，以及（唯讀）Claude Code 與 Codex 設定檔中 MCP 的 `command` 與 `args`；設定檔中的 `env` 值一律不讀。
- 命令列一律先遮蔽再顯示（API 金鑰、token、密碼、`Bearer` 後面的值、`X-Api-Key:` 這類 header 的值、網址裡的帳號密碼），不提供顯示原文的選項。遮蔽是依常見寫法判斷的，無法保證涵蓋所有寫法。
- 從其他 process 只保留一份短的環境變數名稱清單（`CLAUDECODE`、`CLAUDE_CODE_SESSION_ID`、`CLAUDE_PROJECT_DIR`、`CLAUDE_CONFIG_DIR`、`CLAUDE_CODE_ENTRYPOINT`、`CODEX_HOME`），其他變數一律丟棄，不會儲存。

## WSL

Windows 版只看得到 Windows 的 process，WSL 裡的 Linux 版也只看得到 WSL 裡的 process。如果你在 WSL 裡使用 Claude Code 或 Codex，請在 WSL 裡安裝並執行 cctree；Windows 那邊請在 PowerShell 另外執行。cctree 偵測到這種情況時會顯示提示。

## 授權

以 [Apache License 2.0](LICENSE-APACHE) 或 [MIT 授權](LICENSE-MIT) 擇一授權。

cctree 是非官方工具，與 Anthropic、OpenAI 無關。
