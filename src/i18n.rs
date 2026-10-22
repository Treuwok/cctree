//! Every user-visible string, in Traditional Chinese (zh-TW) and English,
//! and the choice between the two.
//!
//! Three kinds of text live here:
//!
//! - [`Text`]: a fixed string (a column title, a key-bar item, a help
//!   paragraph). Look it up with [`tr`]. The catalog is one table, so a
//!   string cannot exist in one language only.
//! - [`Msg`]: a message with parameters (a result, a refusal, a notice).
//!   Render it with [`msg`]. Every error is written as three parts, in this
//!   order: what happened, what it means for you, what to do about it.
//! - Small functions for labels that carry a count or a name (`category_title`,
//!   `kill_total`, `header_ram`, ...). The caller passes numbers already
//!   formatted with [`crate::format`], so the wording is the only thing here.
//!   These are functions, not [`Msg`] variants, because they are pieces of a
//!   row or a dialog, not messages for the message bar.
//!
//! Wording follows the design spec (4.4 memory names, 4.6 labels and key bar,
//! 4.7 kill dialog and results, 4.8 errors). English is plain and short.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Lang {
    ZhTw,
    En,
}

/// The smallest terminal the UI draws in; below it only [`Msg::TooSmall`].
pub const MIN_WIDTH: u16 = 80;
pub const MIN_HEIGHT: u16 = 24;

/// Picks the language. The first of `CCTREE_LANG`, `LC_ALL`, `LC_MESSAGES`
/// and `LANG` that is set to a non-blank value decides: `zh_TW`, `zh_HK`,
/// `zh_Hant*` (also with `-`, any case) are Traditional Chinese, everything
/// else is English. With none of them set, the Windows UI language id
/// decides (`0x0404` zh-TW, `0x0C04` zh-HK, `0x1404` zh-MO).
pub fn detect_lang(get_env: &dyn Fn(&str) -> Option<String>, windows_langid: Option<u16>) -> Lang {
    for key in ["CCTREE_LANG", "LC_ALL", "LC_MESSAGES", "LANG"] {
        let Some(value) = get_env(key) else { continue };
        let value = value.trim();
        if value.is_empty() {
            continue;
        }
        return if is_traditional_chinese(value) {
            Lang::ZhTw
        } else {
            Lang::En
        };
    }
    match windows_langid {
        Some(0x0404 | 0x0C04 | 0x1404) => Lang::ZhTw,
        _ => Lang::En,
    }
}

fn is_traditional_chinese(locale: &str) -> bool {
    let locale = locale.to_ascii_lowercase();
    ["zh_tw", "zh-tw", "zh_hk", "zh-hk", "zh_hant", "zh-hant"]
        .iter()
        .any(|prefix| locale.starts_with(prefix))
}

/// Defines [`Text`] and its catalog from one table, so every variant has
/// both languages and [`Text::ALL`] cannot miss one.
macro_rules! catalog {
    ($( $(#[$doc:meta])* $name:ident => ($zh:expr, $en:expr $(,)?), )*) => {
        /// A fixed string. See [`tr`].
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum Text {
            $( $(#[$doc])* $name, )*
        }

        impl Text {
            /// Every variant, for tests and tools.
            pub const ALL: &'static [Text] = &[ $( Text::$name, )* ];
        }

        /// The fixed string `t` in `lang`.
        pub fn tr(lang: Lang, t: Text) -> &'static str {
            match t {
                $( Text::$name => match lang { Lang::ZhTw => $zh, Lang::En => $en }, )*
            }
        }
    };
}

catalog! {
    // ---- Categories (the four top-level rows) ----
    CatClaudeCode => ("Claude Code", "Claude Code"),
    CatCodex => ("Codex", "Codex"),
    CatOrphaned => ("已失聯", "Orphaned"),
    CatOtherNode => ("其他 Node.js", "Other Node.js"),
    /// Goes after Claude Code / Codex when there is no session.
    CatNotRunning => ("目前沒有在執行", "not running"),

    // ---- Table column titles ----
    ColName => ("名稱", "Name"),
    ColPid => ("PID", "PID"),
    ColCpu => ("CPU", "CPU"),
    ColMem => ("記憶體", "Memory"),
    ColCount => ("個數", "Count"),

    // ---- Row names ----
    /// The first child of a process row that has children: its own numbers.
    RowSelf => ("（本體）", "(itself)"),
    RowNpxShell => ("npx 外殼", "npx wrapper"),
    /// Roles of the members of an MCP node.
    PartWrapper => ("外殼", "wrapper"),
    PartServer => ("server", "server"),
    PartHelper => ("輔助", "helper"),
    GroupOtherChildren => ("其他子程序（shell、LSP…）", "Other child processes (shell, LSP…)"),
    GroupDetached => ("已脫離（仍屬於這個 session）", "Detached (still part of this session)"),

    // ---- Row marks ----
    MarkZombie => ("殭屍", "zombie"),
    MarkConfirmed => ("確定", "confirmed"),
    MarkSuspected => ("疑似", "suspected"),
    MarkPlugin => ("plugin", "plugin"),

    // ---- Header line (the "? 說明" at its right end is `KeyHelp`) ----
    HeaderPaused => ("已暫停", "Paused"),
    ViewBySession => ("依 session", "By session"),
    ViewMcpOverview => ("MCP 總覽", "MCP overview"),
    SortMem => ("記憶體", "memory"),
    SortCpu => ("CPU", "CPU"),
    SortPid => ("PID", "PID"),
    SortName => ("名稱", "name"),
    /// Windows shows the page file where the other systems show swap.
    LabelSwap => ("swap", "swap"),
    LabelPageFile => ("分頁檔", "page file"),

    // ---- Detail panel ----
    DetailCommand => ("指令：", "Command: "),
    DetailReason => ("判定原因：", "Why: "),
    /// What the system's own tool is called, for "the tool shows X".
    OsToolMac => ("活動監視器", "Activity Monitor"),
    OsToolWindows => ("工作管理員", "Task Manager"),
    OsToolLinux => ("htop 的 RES", "htop RES"),
    /// Why a memory field shows "—".
    UnavailPermission => ("權限不足，讀不到", "no permission to read it"),
    UnavailOsTooOld => ("系統版本太舊，讀不到", "the OS is too old to report it"),
    UnavailGone => ("這個 process 已經結束", "the process has ended"),
    UnavailUnsupported => ("這個 process 不提供這項資料", "not available for this process"),

    // ---- Memory names (spec 4.4) and what they mean ----
    MemName => ("記憶體", "Memory"),
    MemNameDesc => (
        "這個 process 自己獨佔的量，結束它就能拿回來；加總與排序都用這個數字",
        "What this process holds all by itself; you get it back by ending the process. Totals and sorting use this number.",
    ),
    MemInRam => ("在 RAM 裡", "In RAM"),
    MemInRamDesc => ("現在實際放在 RAM 中的部分", "The part that sits in RAM right now."),
    MemSwapped => ("被擠出 RAM", "Pushed out of RAM"),
    MemSwappedDesc => (
        "很久沒用，被系統壓縮或搬到硬碟；不太影響速度，但仍佔資源",
        "Not used for a while, so the system compressed it or moved it to disk; it hardly affects speed but still takes resources.",
    ),
    MemShared => ("共用部分", "Shared"),
    MemSharedDesc => (
        "程式碼、函式庫等與其他 process 共用的部分；只顯示，不加總",
        "Code, libraries and the like shared with other processes; shown only, never added up.",
    ),
    /// The remainder that makes both sides of the "＝" equal (macOS).
    MemOther => ("其他", "Other"),
    MemOtherDesc => (
        "系統記帳的其他部分；macOS 的「記憶體」不完全等於前兩項相加，差額另外列出，讓等號兩邊相等",
        "Other parts the system accounts for; on macOS \"Memory\" is not exactly the first two added up, so the difference is listed to make both sides equal.",
    ),
    /// Goes in front of a number that is a lower bound or an estimate.
    ApproxPrefix => ("約 ", "~"),

    // ---- Help page (spec 4.4, 4.6) ----
    HelpTitle => ("cctree 說明", "cctree help"),
    HelpIntro => (
        "cctree 把 Claude Code／Codex 帶起來的 process 依 session 分組，讓你看出誰佔了多少記憶體，並可以安全地結束不需要的。",
        "cctree groups the processes started by Claude Code and Codex by session, shows how much memory each one uses, and lets you safely end the ones you do not need.",
    ),
    HelpKeysTitle => ("按鍵", "Keys"),
    HelpMemoryTitle => ("記憶體名詞", "Memory terms"),
    HelpNotesTitle => ("注意事項", "Good to know"),
    HelpNoteApprox => (
        "數字前面的「約」表示有些成員讀不到，實際數字可能更高。",
        "A \"~\" before a number means some members could not be read, so the real figure may be higher.",
    ),
    HelpNoteCpu => (
        "CPU：macOS 與 Linux 以單一核心為 100%，可以超過 100%（和活動監視器、top 相同）；Windows 以整台機器為 100%（和工作管理員相同）。",
        "CPU: on macOS and Linux 100% is one core and it can go above 100% (like Activity Monitor and top); on Windows 100% is the whole machine (like Task Manager).",
    ),
    HelpNoteOrphan => (
        "已失聯：主人已經結束，它卻還留著。標「確定」的可以整批清理；標「疑似」的可能是你正在用的 app，請個別確認後再用 x 結束。",
        "Orphaned: its owner has ended but it is still there. Ones marked \"confirmed\" can be cleaned up as a batch; ones marked \"suspected\" may be an app you are using, so check each one and end it with x.",
    ),
    HelpNoteKill => (
        "按 x 或 X 結束 process 前，會先列出要結束的項目，按 Enter 確定、Esc 取消；只需要確認這一次。",
        "Before x or X ends anything, the list of processes is shown first: Enter confirms, Esc cancels. You confirm only once.",
    ),
    HelpNoteMouse => (
        "滑鼠：滾輪捲動，點一列選取，點 ▶／▼ 展開或收合。",
        "Mouse: the wheel scrolls, a click selects a row, and a click on ▶ or ▼ folds or unfolds it.",
    ),
    HelpPlatformMac => (
        "macOS：「記憶體」等於活動監視器的「記憶體」。macOS 不提供單一 process 的 swap 用量，只能知道被壓縮了多少，被壓縮的資料可能再被搬進 swap。",
        "macOS: \"Memory\" is the same as \"Memory\" in Activity Monitor. macOS does not report swap use per process, only how much is compressed, and compressed data may later be moved to swap.",
    ),
    HelpPlatformWindows => (
        "Windows：「記憶體」等於工作管理員「詳細資料」頁的「認可大小」，「在 RAM 裡」等於「處理程序」頁的「記憶體」。Windows 不提供單一 process 被壓縮多少，所以「被擠出 RAM」是分頁檔、壓縮和已預留未使用空間的合計，數字前標「約」。",
        "Windows: \"Memory\" is the same as \"Commit size\" on the Details tab of Task Manager, and \"In RAM\" is the same as \"Memory\" on the Processes tab. Windows does not report how much of a process is compressed, so \"Pushed out of RAM\" is the total of the page file, compression and reserved-but-unused space, and is marked \"~\".",
    ),
    HelpPlatformLinux => (
        "Linux：「記憶體」是 RssAnon ＋ VmSwap。三個平台中資訊最完整，可以直接讀到單一 process 的 swap 用量。htop 的 RES 包含共用部分，所以會比這裡的數字大。",
        "Linux: \"Memory\" is RssAnon + VmSwap. Of the three systems it gives the most complete picture and reports swap use per process directly. The RES column in htop includes shared memory, so it reads larger than the numbers here.",
    ),
    HelpFooter => (
        "非官方工具，與 Anthropic、OpenAI 無關。",
        "Unofficial tool; not affiliated with Anthropic or OpenAI.",
    ),
    HelpClose => ("按任意鍵關閉說明", "Press any key to close this help"),
    /// At the foot of the help page while there is more below; the arrow
    /// keys scroll it.
    HelpMore => ("↓ 還有更多（↑↓ 捲動）", "↓ more (↑↓ scroll)"),

    // ---- Key bar (spec 4.6 mockup; the order there is the order to use) ----
    KeyMove => ("↑↓ 移動", "↑↓ move"),
    KeyFold => ("←→ 收合/展開", "←→ fold/unfold"),
    KeyKillOne => ("x 結束這個", "x end this"),
    KeyKillTree => ("X 結束整棵", "X end tree"),
    KeyMcp => ("m MCP 總覽", "m MCP overview"),
    KeySort => ("s 排序", "s sort"),
    KeySearch => ("/ 搜尋", "/ search"),
    KeyPause => ("空白 暫停", "Space pause"),
    KeyResume => ("空白 恢復", "Space resume"),
    KeyHelp => ("? 說明", "? help"),
    KeyQuit => ("q 離開", "q quit"),
    /// The key bar while the search box is open.
    KeySearching => ("輸入文字搜尋　Enter 完成　Esc 清除", "Type to search   Enter done   Esc clear"),
    SearchPrompt => ("搜尋：", "Search: "),
    SearchNoMatch => ("沒有符合的項目", "No match"),

    // ---- Kill dialog (spec 4.7) ----
    DialogTitleOne => ("結束這個 process？", "End this process?"),
    DialogTitleTree => ("結束整組 process？", "End this whole group of processes?"),
    DialogConfirm => ("Enter 確定結束", "Enter to end"),
    DialogCancel => ("Esc 取消", "Esc to cancel"),
    DialogForceOnly => (
        "Windows 會直接強制結束，不會先請它們自己關閉。",
        "Windows ends them by force right away; they get no chance to shut down cleanly.",
    ),
    DialogDetachedHeader => (
        "已脫離（仍屬於這個 session）：",
        "Detached (still part of this session):",
    ),
    DialogMcpClaude => (
        "結束後 Claude Code 會顯示這個 MCP 已斷線，需要時可在 Claude Code 裡用 /mcp 重新連線。",
        "Claude Code will show this MCP as disconnected afterwards. Reconnect it with /mcp in Claude Code when you need it.",
    ),
    DialogMcpCodex => (
        "結束後 Codex 裡的這個 MCP 會斷線，需要時重新開啟 Codex 的 session。",
        "This MCP will be disconnected in Codex afterwards. Reopen the Codex session when you need it.",
    ),

    // ---- Crash report (printed as plain text after the terminal is restored) ----
    PanicReport => (
        "cctree 發生了預期之外的錯誤而停止；終端機已恢復原狀。請到 https://github.com/yenchiugu/cctree/issues 回報，並附上下面的訊息：",
        "cctree stopped because of an unexpected error; your terminal has been restored. Please report it at https://github.com/yenchiugu/cctree/issues and include the details below:",
    ),
}

/// How long a process has been running.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Age {
    UnderMinute,
    Minutes(u64),
    Hours(u64),
    Days(u64),
}

/// "已執行 15 分鐘" / "running 15 min".
pub fn age_text(lang: Lang, age: Age) -> String {
    match (lang, age) {
        (Lang::ZhTw, Age::UnderMinute) => "已執行不到 1 分鐘".to_string(),
        (Lang::ZhTw, Age::Minutes(n)) => format!("已執行 {n} 分鐘"),
        (Lang::ZhTw, Age::Hours(n)) => format!("已執行 {n} 小時"),
        (Lang::ZhTw, Age::Days(n)) => format!("已執行 {n} 天"),
        (Lang::En, Age::UnderMinute) => "running <1 min".to_string(),
        (Lang::En, Age::Minutes(n)) => format!("running {n} min"),
        (Lang::En, Age::Hours(n)) => format!("running {n} h"),
        (Lang::En, Age::Days(n)) => format!("running {n} d"),
    }
}

/// "1 session" / "2 sessions".
fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

// ---- Labels with a count or a name -------------------------------------------

/// The row of a category. `cat` is one of `Text::CatClaudeCode`,
/// `CatCodex`, `CatOrphaned`, `CatOtherNode`; `n` is the number of rows that
/// count under it (sessions for the first two).
///
/// "Claude Code（12 個 session）", "Codex（目前沒有在執行）", "已失聯（1）".
pub fn category_title(lang: Lang, cat: Text, n: usize) -> String {
    let name = tr(lang, cat);
    let is_tool = matches!(cat, Text::CatClaudeCode | Text::CatCodex);
    match lang {
        Lang::ZhTw if is_tool && n == 0 => {
            format!("{name}（{}）", tr(lang, Text::CatNotRunning))
        }
        Lang::ZhTw if is_tool => format!("{name}（{n} 個 session）"),
        Lang::ZhTw => format!("{name}（{n}）"),
        Lang::En if is_tool && n == 0 => format!("{name} ({})", tr(lang, Text::CatNotRunning)),
        Lang::En if is_tool => format!("{name} ({})", plural(n, "session", "sessions")),
        Lang::En => format!("{name} ({n})"),
    }
}

/// The MCP group of a session: "MCP（5 個）" / "MCP (5)".
pub fn mcp_group_title(lang: Lang, n: usize) -> String {
    match lang {
        Lang::ZhTw => format!("MCP（{n} 個）"),
        Lang::En => format!("MCP ({n})"),
    }
}

/// How many sessions run one MCP, in the overview: "12 份" / "12 copies".
pub fn mcp_instances(lang: Lang, n: usize) -> String {
    match lang {
        Lang::ZhTw => format!("{n} 份"),
        Lang::En => plural(n, "copy", "copies"),
    }
}

/// The note under a tool's category for sessions that run inside the other
/// tool (and are counted there): "另有 2 個 Codex session 在 Claude Code 底下".
pub fn nested_note(lang: Lang, n: usize, tool: &str, inside: &str) -> String {
    match lang {
        Lang::ZhTw => format!("另有 {n} 個 {tool} session 在 {inside} 底下"),
        Lang::En => format!(
            "{} more {tool} {} inside {inside}",
            n,
            if n == 1 { "session" } else { "sessions" }
        ),
    }
}

/// Added to a session's label when an app started it: "（由 VS Code 啟動）".
pub fn launched_by(lang: Lang, launcher: &str) -> String {
    match lang {
        Lang::ZhTw => format!("（由 {launcher} 啟動）"),
        Lang::En => format!("(started by {launcher})"),
    }
}

/// The label of a `codex app-server` session, which serves many
/// conversations and so has no useful working directory.
pub fn app_server_title(lang: Lang, launcher: Option<&str>) -> String {
    match (lang, launcher) {
        (Lang::ZhTw, Some(l)) => format!("Codex app-server（服務 {l}）"),
        (Lang::En, Some(l)) => format!("Codex app-server (serving {l})"),
        (_, None) => "Codex app-server".to_string(),
    }
}

/// Why a process counts as orphaned, for the detail panel.
pub fn orphan_reason_session_ended(lang: Lang, tool: &str, project: Option<&str>) -> String {
    match (lang, project) {
        (Lang::ZhTw, Some(p)) => format!("原本屬於 {tool} session（{p}），該 session 已結束"),
        (Lang::ZhTw, None) => format!("原本屬於某個 {tool} session，該 session 已結束"),
        (Lang::En, Some(p)) => format!("It belonged to a {tool} session ({p}) that has ended"),
        (Lang::En, None) => format!("It belonged to a {tool} session that has ended"),
    }
}

/// See [`orphan_reason_session_ended`].
pub fn orphan_reason_parent_gone(lang: Lang, pid: u32) -> String {
    match lang {
        Lang::ZhTw => format!("它記錄的主人 PID {pid} 已不存在"),
        Lang::En => format!("The owner PID {pid} it recorded no longer exists"),
    }
}

/// See [`orphan_reason_session_ended`].
pub fn orphan_reason_path_only(lang: Lang, tool: &str) -> String {
    match lang {
        Lang::ZhTw => format!("它位於 {tool} 的資料夾底下，而且目前沒有任何 {tool} session 在執行"),
        Lang::En => format!("It lives under the {tool} folders and no {tool} session is running"),
    }
}

// ---- Header line: "cctree  每 2 秒更新 · RAM 已用 27/36 GB · swap 2.3 GB · ..." --------

pub fn header_interval(lang: Lang, secs: u64) -> String {
    match lang {
        Lang::ZhTw => format!("每 {secs} 秒更新"),
        Lang::En => format!("refresh every {secs} s"),
    }
}

pub fn header_ram(lang: Lang, used: &str, total: &str) -> String {
    match lang {
        Lang::ZhTw => format!("RAM 已用 {used}/{total}"),
        Lang::En => format!("RAM used {used}/{total}"),
    }
}

/// Swap, or the page file on Windows (`windows`).
pub fn header_swap(lang: Lang, windows: bool, used: &str) -> String {
    let label = tr(
        lang,
        if windows {
            Text::LabelPageFile
        } else {
            Text::LabelSwap
        },
    );
    format!("{label} {used}")
}

pub fn header_self(lang: Lang, mem: &str) -> String {
    match lang {
        Lang::ZhTw => format!("cctree 本身 {mem}"),
        Lang::En => format!("cctree itself {mem}"),
    }
}

pub fn header_zombies(lang: Lang, n: usize) -> String {
    match lang {
        Lang::ZhTw => format!("殭屍 {n} 個"),
        Lang::En => plural(n, "zombie", "zombies"),
    }
}

/// `sort` is one of `Text::SortMem`, `SortCpu`, `SortPid`, `SortName`.
pub fn header_sort(lang: Lang, sort: Text) -> String {
    match lang {
        Lang::ZhTw => format!("排序：{}", tr(lang, sort)),
        Lang::En => format!("sort: {}", tr(lang, sort)),
    }
}

// ---- Detail panel ------------------------------------------------------------

/// "PID 50094    父 PID 49815".
pub fn detail_ids(lang: Lang, pid: u32, ppid: Option<u32>) -> String {
    match (lang, ppid) {
        (Lang::ZhTw, Some(pp)) => format!("PID {pid}    父 PID {pp}"),
        (Lang::ZhTw, None) => format!("PID {pid}"),
        (Lang::En, Some(pp)) => format!("PID {pid}    parent PID {pp}"),
        (Lang::En, None) => format!("PID {pid}"),
    }
}

/// The plain-language memory sentence:
/// "記憶體 162 MB ＝ 在 RAM 裡 112 MB ＋ 被擠出 RAM 42 MB ＋ 其他 8 MB（另有 15 MB 與其他程式共用）".
///
/// `split` is (in RAM, pushed out) when both are known; without it only the
/// total is said. `other` is the remainder to add so that both sides of the
/// "＝" are equal (give it only when it is more than 1% of the total).
/// All numbers arrive formatted.
pub fn detail_mem_line(
    lang: Lang,
    total: &str,
    split: Option<(&str, &str)>,
    other: Option<&str>,
    shared: Option<&str>,
) -> String {
    let zh = lang == Lang::ZhTw;
    let (eq, plus) = if zh { ("＝", "＋") } else { ("=", "+") };
    let mut s = format!("{} {total}", tr(lang, Text::MemName));
    if let Some((in_ram, swapped)) = split {
        s.push_str(&format!(
            " {eq} {} {in_ram} {plus} {} {swapped}",
            tr(lang, Text::MemInRam),
            tr(lang, Text::MemSwapped)
        ));
        if let Some(o) = other {
            s.push_str(&format!(" {plus} {} {o}", tr(lang, Text::MemOther)));
        }
    }
    if let Some(sh) = shared {
        s.push_str(&match lang {
            Lang::ZhTw => format!("（另有 {sh} 與其他程式共用）"),
            Lang::En => format!(" (another {sh} is shared with other programs)"),
        });
    }
    s
}

/// "活動監視器會顯示 162 MB". `tool` is `Text::OsToolMac`, `OsToolWindows`
/// or `OsToolLinux`.
pub fn detail_os_tool(lang: Lang, tool: Text, value: &str) -> String {
    match lang {
        Lang::ZhTw => {
            // A space between Latin letters and the Chinese that follows.
            let name = tr(lang, tool);
            let gap = if name.ends_with(|c: char| c.is_ascii_alphanumeric()) {
                " "
            } else {
                ""
            };
            format!("{name}{gap}會顯示 {value}")
        }
        Lang::En => format!("{} shows {value}", tr(lang, tool)),
    }
}

/// The detail of a group row: "共 12 個 process，記憶體 1.3 GB".
pub fn detail_group(lang: Lang, count: usize, mem: &str) -> String {
    match lang {
        Lang::ZhTw => format!("共 {count} 個 process，記憶體 {mem}"),
        Lang::En => format!("{}, memory {mem}", plural(count, "process", "processes")),
    }
}

// ---- Kill dialog and progress (spec 4.7) ----------------------------------------

/// "共 3 個，約可釋放 413 MB". `freed` is a plain size (no "約"); with
/// `approx` some members could not be read, so it is a lower bound.
pub fn kill_total(lang: Lang, n: usize, freed: &str, approx: bool) -> String {
    match (lang, approx) {
        (Lang::ZhTw, false) => format!("共 {n} 個，約可釋放 {freed}"),
        (Lang::ZhTw, true) => format!("共 {n} 個，約可釋放 {freed} 以上（有些讀不到，實際會更多）"),
        (Lang::En, false) => format!("{n} in total, about {freed} to free"),
        (Lang::En, true) => {
            format!("{n} in total, at least {freed} to free (some could not be read)")
        }
    }
}

/// "還有 4 個": the processes left out of the list of the first ten.
pub fn kill_more(lang: Lang, n: usize) -> String {
    match lang {
        Lang::ZhTw => format!("還有 {n} 個"),
        Lang::En => format!("{n} more"),
    }
}

/// "另有 2 個疑似失聯的沒有包含": batch cleanup of orphans skips the suspected.
pub fn kill_excluded_suspected(lang: Lang, n: usize) -> String {
    match lang {
        Lang::ZhTw => format!("另有 {n} 個疑似失聯的沒有包含"),
        Lang::En => format!(
            "{n} suspected orphan{} not included",
            if n == 1 { " is" } else { "s are" }
        ),
    }
}

/// How to get a session back after it is ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resume<'a> {
    /// A command to run in a terminal, e.g. `claude --resume`.
    Command(&'a str),
    /// The app that started the session, to open it from again.
    ReopenFrom(&'a str),
}

/// The red warning for ending a session: "這會中斷 ~/prj/foo 的對話；對話紀錄不會遺失，
/// 之後可以用 `claude --resume` 接續". `path` is the session's working directory.
pub fn kill_interrupts(lang: Lang, path: Option<&str>, resume: Resume<'_>) -> String {
    match lang {
        Lang::ZhTw => {
            let what = match path {
                Some(p) => format!("這會中斷 {p} 的對話"),
                None => "這會中斷這個 session 的對話".to_string(),
            };
            let how = match resume {
                Resume::Command(c) => format!("之後可以用 `{c}` 接續"),
                Resume::ReopenFrom(app) => format!("之後可以從 {app} 重新開啟"),
            };
            format!("{what}；對話紀錄不會遺失，{how}")
        }
        Lang::En => {
            let what = match path {
                Some(p) => format!("This interrupts the conversation in {p}"),
                None => "This interrupts the conversation of this session".to_string(),
            };
            let how = match resume {
                Resume::Command(c) => format!("you can pick it up again with `{c}`"),
                Resume::ReopenFrom(app) => format!("you can reopen it from {app}"),
            };
            format!("{what}; the history is not lost and {how}")
        }
    }
}

/// The message bar while ending: "正在結束… 2/3".
pub fn kill_progress(lang: Lang, done: usize, total: usize) -> String {
    match lang {
        Lang::ZhTw => format!("正在結束… {done}/{total}"),
        Lang::En => format!("Ending… {done}/{total}"),
    }
}

/// The key bar of the main view, in the order of the spec mockup.
/// `s` (sort) and `/` (search) are not in it: they are in the help page and
/// can be added when the terminal is wide enough ([`Text::KeySort`],
/// [`Text::KeySearch`]).
pub fn key_bar(paused: bool) -> [Text; 8] {
    [
        Text::KeyMove,
        Text::KeyFold,
        Text::KeyKillOne,
        Text::KeyKillTree,
        Text::KeyMcp,
        if paused {
            Text::KeyResume
        } else {
            Text::KeyPause
        },
        Text::KeyHelp,
        Text::KeyQuit,
    ]
}

/// The key table of the help page: (keys, what they do), in the order of
/// spec 4.6.
pub fn help_keys(lang: Lang) -> &'static [(&'static str, &'static str)] {
    const ZH: &[(&str, &str)] = &[
        ("↑ ↓ ／ k j", "上下移動"),
        ("PgUp PgDn Home End", "快速移動"),
        ("← ／ h", "收合；已經收合時跳到上一層"),
        ("→ ／ l", "展開"),
        ("Enter", "切換展開或收合"),
        ("x", "結束這一個 process"),
        ("X", "結束整棵或整組"),
        ("m", "切換 MCP 總覽"),
        ("s", "切換排序（記憶體 → CPU → PID → 名稱）"),
        ("/", "搜尋名稱、命令列或 PID；Esc 清除"),
        ("空白鍵", "暫停或恢復更新"),
        ("+ -", "調整更新間隔，1～10 秒"),
        ("?", "說明"),
        ("q ／ Ctrl+C", "離開"),
    ];
    const EN: &[(&str, &str)] = &[
        ("↑ ↓ / k j", "Move up and down"),
        ("PgUp PgDn Home End", "Move faster"),
        ("← / h", "Fold; when already folded, go to the parent"),
        ("→ / l", "Unfold"),
        ("Enter", "Fold or unfold"),
        ("x", "End this one process"),
        ("X", "End the whole tree or group"),
        ("m", "Switch the MCP overview on or off"),
        ("s", "Change the sort (memory → CPU → PID → name)"),
        ("/", "Search names, command lines or PIDs; Esc clears"),
        ("Space", "Pause or resume updates"),
        ("+ -", "Change the refresh interval, 1 to 10 s"),
        ("?", "Help"),
        ("q / Ctrl+C", "Quit"),
    ];
    match lang {
        Lang::ZhTw => ZH,
        Lang::En => EN,
    }
}

/// The four memory names with what they mean, in the order of spec 4.4.
pub fn help_memory_terms(lang: Lang) -> [(&'static str, &'static str); 4] {
    [
        (tr(lang, Text::MemName), tr(lang, Text::MemNameDesc)),
        (tr(lang, Text::MemInRam), tr(lang, Text::MemInRamDesc)),
        (tr(lang, Text::MemSwapped), tr(lang, Text::MemSwappedDesc)),
        (tr(lang, Text::MemShared), tr(lang, Text::MemSharedDesc)),
    ]
}

// ---- Messages with parameters --------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub enum Msg {
    KillDone {
        graceful: usize,
        forced: usize,
        gone: usize,
        reused: usize,
        failed: usize,
    },
    KillFailed {
        pid: u32,
        permission: bool,
    },
    GroupNeedsX,
    KillRefusedCategory,
    KillRefusedSelf,
    KillRefusedSystem {
        pid: u32,
    },
    KillRefusedOtherUser {
        pid: u32,
    },
    KillRefusedZombie {
        pid: u32,
        parent: Option<u32>,
    },
    KillNothing,
    SelectionGone,
    UnreadableProcs {
        n: usize,
    },
    WindowsTooOld,
    ConfigUnreadable {
        path: String,
    },
    TooSmall {
        w: u16,
        h: u16,
    },
    NotATty,
    WslBanner,
    WslHint,
}

impl Msg {
    /// One example of every variant, for tests and previews.
    pub fn samples() -> Vec<Msg> {
        vec![
            Msg::KillDone {
                graceful: 1,
                forced: 1,
                gone: 1,
                reused: 1,
                failed: 1,
            },
            Msg::KillFailed {
                pid: 4242,
                permission: true,
            },
            Msg::GroupNeedsX,
            Msg::KillRefusedCategory,
            Msg::KillRefusedSelf,
            Msg::KillRefusedSystem { pid: 1 },
            Msg::KillRefusedOtherUser { pid: 777 },
            Msg::KillRefusedZombie {
                pid: 250,
                parent: Some(200),
            },
            Msg::KillNothing,
            Msg::SelectionGone,
            Msg::UnreadableProcs { n: 37 },
            Msg::WindowsTooOld,
            Msg::ConfigUnreadable {
                path: "~/.claude.json".to_string(),
            },
            Msg::TooSmall { w: 60, h: 18 },
            Msg::NotATty,
            Msg::WslBanner,
            Msg::WslHint,
        ]
    }
}

/// The text of `m` in `lang`: one line, no trailing newline.
pub fn msg(lang: Lang, m: &Msg) -> String {
    match m {
        Msg::KillDone {
            graceful,
            forced,
            gone,
            reused,
            failed,
        } => kill_done(lang, *graceful, *forced, *gone, *reused, *failed),
        Msg::KillFailed {
            pid,
            permission: true,
        } => match lang {
            Lang::ZhTw => format!(
                "PID {pid} 沒能結束：權限不足，它還在執行；請改用 sudo cctree（Windows 請以系統管理員身分執行）重新啟動再試"
            ),
            Lang::En => format!(
                "PID {pid} could not be ended: permission denied, so it is still running; restart with sudo cctree (on Windows, run as administrator) and try again"
            ),
        },
        Msg::KillFailed {
            pid,
            permission: false,
        } => match lang {
            Lang::ZhTw => format!(
                "PID {pid} 沒能結束：作業系統回報失敗，它可能還在執行；請稍後再試一次"
            ),
            Lang::En => format!(
                "PID {pid} could not be ended: the OS reported a failure and it may still be running; try again in a moment"
            ),
        },
        Msg::GroupNeedsX => match lang {
            Lang::ZhTw => "這一列是一組 process，要結束整組請按 X".to_string(),
            Lang::En => "This row is a group of processes; press X to end the whole group".to_string(),
        },
        Msg::KillRefusedCategory => match lang {
            Lang::ZhTw => "這一列是整個分類，範圍太大；請到個別 session 或分組上按 X".to_string(),
            Lang::En => "This row is a whole category, which is too broad; press X on an individual session or group instead".to_string(),
        },
        Msg::KillRefusedSelf => match lang {
            Lang::ZhTw => "不能結束這一個：它是 cctree 自己，或是執行 cctree 的 shell／終端機，結束它會讓 cctree 一起關閉；要離開請按 q".to_string(),
            Lang::En => "Cannot end this one: it is cctree itself, or the shell or terminal running it, and ending it would close cctree too; press q to quit instead".to_string(),
        },
        Msg::KillRefusedSystem { pid } => match lang {
            Lang::ZhTw => format!(
                "PID {pid} 是系統 process，結束它可能讓電腦不穩，所以 cctree 不會動它；真的需要的話，請在 cctree 以外用管理員權限處理"
            ),
            Lang::En => format!(
                "PID {pid} is a system process and ending it could make the computer unstable, so cctree leaves it alone; if you really need to, do it outside cctree with administrator rights"
            ),
        },
        Msg::KillRefusedOtherUser { pid } => match lang {
            Lang::ZhTw => format!(
                "PID {pid} 屬於其他使用者，目前的權限無法結束它；真的需要的話，請改用 sudo cctree（Windows 請以系統管理員身分執行）"
            ),
            Lang::En => format!(
                "PID {pid} belongs to another user and your permissions are not enough to end it; if you really need to, restart with sudo cctree (on Windows, run as administrator)"
            ),
        },
        Msg::KillRefusedZombie {
            pid,
            parent: Some(parent),
        } => match lang {
            Lang::ZhTw => format!(
                "PID {pid} 已經結束，只是還沒被它的 parent 回收，所以不佔記憶體，也不需要結束；要讓它從列表消失，請結束它的 parent（PID {parent}）"
            ),
            Lang::En => format!(
                "PID {pid} has already ended and is only waiting for its parent to collect it, so it uses no memory and needs no action; to make it disappear, end its parent (PID {parent})"
            ),
        },
        Msg::KillRefusedZombie { pid, parent: None } => match lang {
            Lang::ZhTw => format!(
                "PID {pid} 已經結束，只是還沒被回收，所以不佔記憶體，也不需要結束；它的 parent 已經不在，系統很快就會把它清掉"
            ),
            Lang::En => format!(
                "PID {pid} has already ended and is only waiting to be collected, so it uses no memory and needs no action; its parent is gone, so the system will clear it soon"
            ),
        },
        Msg::KillNothing => match lang {
            Lang::ZhTw => "沒有可以結束的 process：它們可能已經自己結束了；請等畫面更新後再看".to_string(),
            Lang::En => "There is nothing to end: the processes may have already ended on their own; check again after the next refresh".to_string(),
        },
        Msg::SelectionGone => match lang {
            Lang::ZhTw => "剛才選取的 process 已結束".to_string(),
            Lang::En => "The process you had selected has ended".to_string(),
        },
        Msg::UnreadableProcs { n } => match lang {
            Lang::ZhTw => format!(
                "有 {n} 個 process 讀不到命令列；Claude／Codex 的統計不受影響，想看全部請用 sudo cctree 或以系統管理員身分執行"
            ),
            Lang::En => format!(
                "Could not read the command line of {}; the Claude and Codex figures are not affected, and to see all of them run sudo cctree or run as administrator",
                plural(*n, "process", "processes")
            ),
        },
        Msg::WindowsTooOld => match lang {
            Lang::ZhTw => "系統版本太舊，讀不到部分記憶體數字（例如「在 RAM 裡」），讀不到的欄位先顯示「—」；請更新作業系統（例如 Windows 10／11 22H2 以上，或 Linux 核心 4.5 以上）".to_string(),
            Lang::En => "Your OS version is too old to report some memory figures (such as \"In RAM\"), so those fields show \"—\"; please update your OS (for example Windows 10/11 22H2 or later, or Linux kernel 4.5 or later)".to_string(),
        },
        Msg::ConfigUnreadable { path } => match lang {
            Lang::ZhTw => format!(
                "讀不懂設定檔 {path}，所以 MCP 改用套件名稱命名；其他功能不受影響，修正這個檔案的格式後就會恢復"
            ),
            Lang::En => format!(
                "Could not parse the config file {path}, so MCP servers are named after their packages instead; everything else works, and the names return once the file is fixed"
            ),
        },
        Msg::TooSmall { w, h } => match lang {
            Lang::ZhTw => format!(
                "視窗太小（目前 {w}×{h}），畫面放不下；請放大到至少 {MIN_WIDTH}×{MIN_HEIGHT}"
            ),
            Lang::En => format!(
                "The window is too small ({w}×{h}), so the view does not fit; please enlarge it to at least {MIN_WIDTH}×{MIN_HEIGHT}"
            ),
        },
        Msg::NotATty => match lang {
            Lang::ZhTw => "cctree 需要在終端機裡互動執行，但現在的輸出不是終端機（可能被導向檔案或管線）；請直接在終端機裡執行 cctree".to_string(),
            Lang::En => "cctree needs to run interactively in a terminal, but its output is not a terminal (it may be redirected to a file or a pipe); run cctree directly in a terminal".to_string(),
        },
        Msg::WslBanner => match lang {
            Lang::ZhTw => "這裡只看得到 WSL 裡的 process；Windows 那邊請在 PowerShell 另外執行 cctree".to_string(),
            Lang::En => "Only processes inside WSL are visible here; run cctree separately in PowerShell for the Windows side".to_string(),
        },
        Msg::WslHint => match lang {
            Lang::ZhTw => "這裡沒有找到 Claude Code／Codex，但偵測到 WSL 正在執行；如果你是在 WSL 裡用 Claude Code／Codex，請在 WSL 裡執行 cctree".to_string(),
            Lang::En => "No Claude Code or Codex was found here, but WSL is running; if you use Claude Code or Codex inside WSL, run cctree inside WSL".to_string(),
        },
    }
}

/// The one-line result of a kill. The total counts everything that is no
/// longer running (ended by SIGTERM, force-ended, or already gone); PIDs that
/// were reused and failures are named after it, because they are not in it.
fn kill_done(
    lang: Lang,
    graceful: usize,
    forced: usize,
    gone: usize,
    reused: usize,
    failed: usize,
) -> String {
    let ended = graceful + forced + gone;
    let zh = lang == Lang::ZhTw;
    let sep = if zh { "；" } else { "; " };
    // The parts inside the parentheses explain the total.
    let mut inner: Vec<String> = Vec::new();
    if forced > 0 {
        inner.push(if zh {
            format!("{forced} 個沒回應，已強制結束")
        } else if forced == 1 {
            "1 did not respond and was force-ended".to_string()
        } else {
            format!("{forced} did not respond and were force-ended")
        });
    }
    if gone > 0 {
        inner.push(if zh {
            format!("{gone} 個已經自己結束")
        } else {
            format!("{gone} had already ended on its own")
        });
    }
    let mut s = match (zh, ended) {
        (true, 0) => "沒有結束任何 process".to_string(),
        (true, n) => format!("已結束 {n} 個"),
        (false, 0) => "Nothing was ended".to_string(),
        (false, n) => format!("Ended {n}"),
    };
    if !inner.is_empty() {
        let (open, close) = if zh {
            ("（其中 ", "）")
        } else {
            (" (", ")")
        };
        s.push_str(open);
        s.push_str(&inner.join(sep));
        s.push_str(close);
    }
    // Not part of the total: left alone for safety, or could not be ended.
    if reused > 0 {
        s.push_str(sep);
        s.push_str(&if zh {
            format!("{reused} 個的 PID 已被其他程式重用，為了安全沒有動它")
        } else if reused == 1 {
            "1 PID was reused by another program, so it was left alone for safety".to_string()
        } else {
            format!(
                "{reused} PIDs were reused by another program, so they were left alone for safety"
            )
        });
    }
    if failed > 0 {
        s.push_str(sep);
        s.push_str(&if zh {
            format!("{failed} 個沒能結束")
        } else {
            format!("{failed} could not be ended")
        });
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |k| {
            pairs
                .iter()
                .find(|(a, _)| *a == k)
                .map(|(_, v)| v.to_string())
        }
    }

    #[test]
    fn language_detection_precedence() {
        assert_eq!(
            detect_lang(
                &env(&[("CCTREE_LANG", "en"), ("LANG", "zh_TW.UTF-8")]),
                None
            ),
            Lang::En
        );
        assert_eq!(
            detect_lang(&env(&[("LANG", "zh_TW.UTF-8")]), None),
            Lang::ZhTw
        );
        assert_eq!(
            detect_lang(&env(&[("LC_ALL", "zh_HK.UTF-8"), ("LANG", "en_US")]), None),
            Lang::ZhTw
        );
        assert_eq!(
            detect_lang(&env(&[("LANG", "zh_CN.UTF-8")]), None),
            Lang::En
        );
        assert_eq!(
            detect_lang(&env(&[("CCTREE_LANG", "zh-Hant")]), None),
            Lang::ZhTw
        );
        assert_eq!(detect_lang(&env(&[]), Some(0x0404)), Lang::ZhTw);
        assert_eq!(detect_lang(&env(&[]), Some(0x0804)), Lang::En);
    }

    #[test]
    fn language_detection_edges() {
        let d = |pairs: &[(&str, &str)], id| detect_lang(&env(pairs), id);
        // Full chain: CCTREE_LANG > LC_ALL > LC_MESSAGES > LANG > LANGID.
        assert_eq!(
            d(&[("LC_ALL", "en_US"), ("LC_MESSAGES", "zh_TW")], None),
            Lang::En
        );
        assert_eq!(
            d(&[("LC_MESSAGES", "zh_TW"), ("LANG", "en_US")], None),
            Lang::ZhTw
        );
        assert_eq!(
            d(&[("LC_MESSAGES", "en_US"), ("LANG", "zh_TW")], None),
            Lang::En
        );
        assert_eq!(
            d(&[("CCTREE_LANG", "zh_TW"), ("LC_ALL", "en_US")], None),
            Lang::ZhTw
        );
        // Any set variable beats the Windows UI language.
        assert_eq!(d(&[("LANG", "en_US.UTF-8")], Some(0x0404)), Lang::En);
        assert_eq!(d(&[("LANG", "zh_TW.UTF-8")], Some(0x0409)), Lang::ZhTw);
        // Empty (or blank) values count as unset and fall through.
        assert_eq!(
            d(&[("CCTREE_LANG", ""), ("LANG", "zh_TW")], None),
            Lang::ZhTw
        );
        assert_eq!(
            d(
                &[("LC_ALL", ""), ("LC_MESSAGES", ""), ("LANG", "zh_TW")],
                None
            ),
            Lang::ZhTw
        );
        assert_eq!(d(&[("LANG", "  ")], Some(0x0404)), Lang::ZhTw);
        assert_eq!(d(&[("LANG", "")], Some(0x0409)), Lang::En);
        // Case-insensitive, every accepted spelling.
        for v in [
            "zh_TW",
            "ZH_tw.UTF-8",
            "zh-TW",
            "zh_HK.UTF-8",
            "zh-hk",
            "zh-Hant",
            "zh_Hant_TW",
            "ZH-HANT-TW",
            " zh_TW ",
        ] {
            assert_eq!(d(&[("LANG", v)], None), Lang::ZhTw, "{v}");
        }
        // Everything else is English.
        for v in [
            "en_US.UTF-8",
            "C",
            "POSIX",
            "zh_CN",
            "zh-Hans",
            "zh",
            "ja_JP",
            "zh_SG",
            "xzh_TW",
        ] {
            assert_eq!(d(&[("LANG", v)], None), Lang::En, "{v}");
        }
        // Windows LANGIDs: zh-TW, zh-HK, zh-MO.
        for id in [0x0404, 0x0C04, 0x1404] {
            assert_eq!(d(&[], Some(id)), Lang::ZhTw, "{id:#x}");
        }
        for id in [0x0804, 0x1004, 0x0409, 0x0411, 0] {
            assert_eq!(d(&[], Some(id)), Lang::En, "{id:#x}");
        }
        assert_eq!(d(&[], None), Lang::En);
    }

    #[test]
    fn spec_fixed_copy() {
        assert_eq!(
            msg(Lang::ZhTw, &Msg::GroupNeedsX),
            "這一列是一組 process，要結束整組請按 X"
        );
        assert_eq!(
            msg(Lang::ZhTw, &Msg::SelectionGone),
            "剛才選取的 process 已結束"
        );
        assert_eq!(
            msg(
                Lang::ZhTw,
                &Msg::KillDone {
                    graceful: 1,
                    forced: 1,
                    gone: 1,
                    reused: 0,
                    failed: 0
                }
            ),
            "已結束 3 個（其中 1 個沒回應，已強制結束；1 個已經自己結束）"
        );
        assert_eq!(
            msg(Lang::ZhTw, &Msg::WslBanner),
            "這裡只看得到 WSL 裡的 process；Windows 那邊請在 PowerShell 另外執行 cctree"
        );
        let small = msg(Lang::ZhTw, &Msg::TooSmall { w: 60, h: 18 });
        assert!(small.contains("60×18") && small.contains("80×24"));
    }

    #[test]
    fn controller_fixed_copy() {
        assert_eq!(
            msg(Lang::ZhTw, &Msg::KillRefusedCategory),
            "這一列是整個分類，範圍太大；請到個別 session 或分組上按 X"
        );
        assert!(
            msg(Lang::ZhTw, &Msg::WslHint)
                .contains("如果你是在 WSL 裡用 Claude Code／Codex，請在 WSL 裡執行 cctree")
        );
    }

    #[test]
    fn every_message_has_both_languages() {
        for m in Msg::samples() {
            assert!(
                !msg(Lang::ZhTw, &m).is_empty() && !msg(Lang::En, &m).is_empty(),
                "{m:?}"
            );
        }
    }

    /// Exhaustive on purpose: adding a variant breaks this until `samples()`
    /// is updated too.
    fn variant_index(m: &Msg) -> usize {
        match m {
            Msg::KillDone { .. } => 0,
            Msg::KillFailed { .. } => 1,
            Msg::GroupNeedsX => 2,
            Msg::KillRefusedCategory => 3,
            Msg::KillRefusedSelf => 4,
            Msg::KillRefusedSystem { .. } => 5,
            Msg::KillRefusedOtherUser { .. } => 6,
            Msg::KillRefusedZombie { .. } => 7,
            Msg::KillNothing => 8,
            Msg::SelectionGone => 9,
            Msg::UnreadableProcs { .. } => 10,
            Msg::WindowsTooOld => 11,
            Msg::ConfigUnreadable { .. } => 12,
            Msg::TooSmall { .. } => 13,
            Msg::NotATty => 14,
            Msg::WslBanner => 15,
            Msg::WslHint => 16,
        }
    }

    #[test]
    fn samples_cover_each_variant_once() {
        let mut seen = [0usize; 17];
        for m in Msg::samples() {
            seen[variant_index(&m)] += 1;
        }
        assert_eq!(seen, [1; 17]);
    }

    #[test]
    fn messages_are_single_line_and_fully_filled_in() {
        for lang in [Lang::ZhTw, Lang::En] {
            for m in Msg::samples() {
                let s = msg(lang, &m);
                assert!(
                    !s.contains('\n') && !s.contains('{') && !s.contains('}'),
                    "{lang:?} {m:?}: {s}"
                );
                assert_eq!(s, s.trim(), "{lang:?} {m:?}");
            }
        }
    }

    #[test]
    fn parameters_show_up_in_both_languages() {
        for lang in [Lang::ZhTw, Lang::En] {
            let has = |m: &Msg, needle: &str| {
                let s = msg(lang, m);
                assert!(s.contains(needle), "{lang:?} {m:?} lacks {needle:?}: {s}");
            };
            let zombie = Msg::KillRefusedZombie {
                pid: 250,
                parent: Some(200),
            };
            has(
                &Msg::KillFailed {
                    pid: 4242,
                    permission: true,
                },
                "4242",
            );
            has(
                &Msg::KillFailed {
                    pid: 4242,
                    permission: false,
                },
                "4242",
            );
            has(&Msg::KillRefusedSystem { pid: 1 }, "PID 1");
            has(&Msg::KillRefusedOtherUser { pid: 777 }, "777");
            has(&zombie, "250");
            has(&zombie, "200");
            has(
                &Msg::KillRefusedZombie {
                    pid: 250,
                    parent: None,
                },
                "250",
            );
            has(&Msg::UnreadableProcs { n: 37 }, "37");
            has(
                &Msg::ConfigUnreadable {
                    path: "/home/u/.claude.json".into(),
                },
                "/home/u/.claude.json",
            );
            has(&Msg::TooSmall { w: 60, h: 18 }, "60×18");
            has(&Msg::TooSmall { w: 60, h: 18 }, "80×24");
        }
    }

    #[test]
    fn unreadable_procs_points_to_elevated_run() {
        let zh = msg(Lang::ZhTw, &Msg::UnreadableProcs { n: 3 });
        assert!(zh.contains("sudo cctree") && zh.contains("系統管理員"));
        let en = msg(Lang::En, &Msg::UnreadableProcs { n: 3 });
        assert!(en.contains("sudo cctree") && en.contains("administrator"));
    }

    #[test]
    fn refusals_for_other_user_suggest_elevation() {
        let zh = msg(Lang::ZhTw, &Msg::KillRefusedOtherUser { pid: 9 });
        assert!(zh.contains("sudo cctree") && zh.contains("系統管理員"));
        let en = msg(Lang::En, &Msg::KillRefusedOtherUser { pid: 9 });
        assert!(en.contains("sudo cctree") && en.contains("administrator"));
    }

    #[test]
    fn too_old_message_is_not_windows_specific() {
        // The same reason is produced by old Linux kernels (no RssAnon).
        let zh = msg(Lang::ZhTw, &Msg::WindowsTooOld);
        assert!(zh.starts_with("系統版本太舊") && zh.contains("Windows"));
        assert!(zh.contains("Linux"));
        let en = msg(Lang::En, &Msg::WindowsTooOld);
        assert!(en.contains("too old") && !en.starts_with("Windows"));
        assert!(en.contains("Windows") && en.contains("Linux"));
    }

    #[test]
    fn kill_done_wording_covers_each_outcome() {
        let done = |graceful, forced, gone, reused, failed| {
            let m = Msg::KillDone {
                graceful,
                forced,
                gone,
                reused,
                failed,
            };
            (msg(Lang::ZhTw, &m), msg(Lang::En, &m))
        };
        assert_eq!(done(3, 0, 0, 0, 0).0, "已結束 3 個");
        assert_eq!(done(3, 0, 0, 0, 0).1, "Ended 3");
        assert_eq!(
            done(0, 2, 0, 0, 0).0,
            "已結束 2 個（其中 2 個沒回應，已強制結束）"
        );
        assert_eq!(
            done(1, 1, 1, 0, 0).1,
            "Ended 3 (1 did not respond and was force-ended; 1 had already ended on its own)"
        );
        assert_eq!(
            done(2, 0, 0, 1, 0).0,
            "已結束 2 個；1 個的 PID 已被其他程式重用，為了安全沒有動它"
        );
        assert_eq!(
            done(0, 0, 0, 1, 0).0,
            "沒有結束任何 process；1 個的 PID 已被其他程式重用，為了安全沒有動它"
        );
        assert_eq!(done(1, 0, 0, 0, 2).0, "已結束 1 個；2 個沒能結束");
        assert_eq!(
            done(1, 0, 0, 1, 1).1,
            "Ended 1; 1 PID was reused by another program, so it was left alone for safety; 1 could not be ended"
        );
        assert_eq!(done(0, 0, 0, 0, 0).0, "沒有結束任何 process");
        assert_eq!(done(0, 0, 0, 0, 0).1, "Nothing was ended");
        // The total counts everything that is no longer running.
        assert!(done(0, 0, 4, 0, 0).0.starts_with("已結束 4 個"));
    }

    fn has_cjk(s: &str) -> bool {
        s.chars()
            .any(|c| matches!(c, '\u{3000}'..='\u{9FFF}' | '\u{FF00}'..='\u{FFEF}'))
    }

    #[test]
    fn catalog_is_complete_and_each_language_stays_in_its_own() {
        let mut seen = std::collections::HashSet::new();
        for &t in Text::ALL {
            assert!(seen.insert(t), "{t:?} is listed twice");
            let (zh, en) = (tr(Lang::ZhTw, t), tr(Lang::En, t));
            assert!(!zh.trim().is_empty() && !en.trim().is_empty(), "{t:?}");
            assert!(!zh.contains('\n') && !en.contains('\n'), "{t:?}");
            assert!(!has_cjk(en), "{t:?} has Chinese in the English text: {en}");
        }
        for m in Msg::samples() {
            assert!(!has_cjk(&msg(Lang::En, &m)), "{m:?}");
        }
        assert!(Text::ALL.len() > 60);
    }

    #[test]
    fn fixed_names_follow_the_spec() {
        let zh = |t| tr(Lang::ZhTw, t);
        assert_eq!(zh(Text::RowSelf), "（本體）");
        assert_eq!(zh(Text::RowNpxShell), "npx 外殼");
        assert_eq!(zh(Text::PartWrapper), "外殼");
        assert_eq!(zh(Text::CatOrphaned), "已失聯");
        assert_eq!(zh(Text::CatOtherNode), "其他 Node.js");
        assert_eq!(zh(Text::HeaderPaused), "已暫停");
        assert_eq!(zh(Text::MarkSuspected), "疑似");
        assert_eq!(zh(Text::ColMem), "記憶體");
        assert_eq!(tr(Lang::En, Text::RowSelf), "(itself)");
        assert_eq!(tr(Lang::En, Text::ColMem), "Memory");
        assert_eq!(tr(Lang::En, Text::HeaderPaused), "Paused");
    }

    #[test]
    fn help_page_explains_the_four_memory_names_and_the_limits() {
        let names: Vec<_> = help_memory_terms(Lang::ZhTw).iter().map(|t| t.0).collect();
        assert_eq!(names, ["記憶體", "在 RAM 裡", "被擠出 RAM", "共用部分"]);
        let names: Vec<_> = help_memory_terms(Lang::En).iter().map(|t| t.0).collect();
        assert_eq!(names, ["Memory", "In RAM", "Pushed out of RAM", "Shared"]);
        let desc = |i: usize| help_memory_terms(Lang::ZhTw)[i].1;
        assert!(desc(0).contains("結束它就能拿回來"));
        assert!(desc(1).contains("實際放在 RAM"));
        assert!(desc(2).contains("壓縮") && desc(2).contains("搬到硬碟"));
        assert!(desc(3).contains("與其他 process 共用") && desc(3).contains("不加總"));
        // The limits of each system (spec 4.4).
        let zh = |t| tr(Lang::ZhTw, t);
        assert!(zh(Text::HelpPlatformMac).contains("不提供單一 process 的 swap"));
        assert!(zh(Text::HelpPlatformWindows).contains("分頁檔、壓縮和已預留未使用空間"));
        assert!(zh(Text::HelpPlatformLinux).contains("RssAnon ＋ VmSwap"));
        // The unofficial line, in both languages.
        assert!(zh(Text::HelpFooter).contains("非官方"));
        for lang in [Lang::ZhTw, Lang::En] {
            let f = tr(lang, Text::HelpFooter);
            assert!(f.contains("Anthropic") && f.contains("OpenAI"), "{f}");
        }
    }

    #[test]
    fn key_table_and_key_bar() {
        assert_eq!(help_keys(Lang::ZhTw).len(), 14);
        assert_eq!(help_keys(Lang::En).len(), 14);
        for lang in [Lang::ZhTw, Lang::En] {
            assert!(
                help_keys(lang)
                    .iter()
                    .all(|(k, d)| !k.is_empty() && !d.is_empty())
            );
        }
        let bar = |lang, paused| {
            key_bar(paused)
                .iter()
                .map(|t| tr(lang, *t))
                .collect::<Vec<_>>()
                .join("  ")
        };
        // The key bar of the spec mockup.
        assert_eq!(
            bar(Lang::ZhTw, false),
            "↑↓ 移動  ←→ 收合/展開  x 結束這個  X 結束整棵  m MCP 總覽  空白 暫停  ? 說明  q 離開"
        );
        assert!(
            bar(Lang::ZhTw, true).contains("空白 恢復") && !bar(Lang::ZhTw, true).contains("暫停")
        );
        assert!(bar(Lang::En, true).contains("Space resume"));
    }

    #[test]
    fn counted_labels() {
        use Text::*;
        let (zh, en) = (Lang::ZhTw, Lang::En);
        assert_eq!(
            category_title(zh, CatClaudeCode, 12),
            "Claude Code（12 個 session）"
        );
        assert_eq!(category_title(zh, CatCodex, 0), "Codex（目前沒有在執行）");
        assert_eq!(category_title(zh, CatOrphaned, 1), "已失聯（1）");
        assert_eq!(category_title(zh, CatOtherNode, 1), "其他 Node.js（1）");
        assert_eq!(
            category_title(en, CatClaudeCode, 1),
            "Claude Code (1 session)"
        );
        assert_eq!(
            category_title(en, CatClaudeCode, 12),
            "Claude Code (12 sessions)"
        );
        assert_eq!(category_title(en, CatCodex, 0), "Codex (not running)");
        assert_eq!(category_title(en, CatOrphaned, 3), "Orphaned (3)");
        assert_eq!(mcp_group_title(zh, 5), "MCP（5 個）");
        assert_eq!(mcp_group_title(en, 5), "MCP (5)");
        assert_eq!(mcp_instances(zh, 12), "12 份");
        assert_eq!(mcp_instances(en, 1), "1 copy");
        assert_eq!(mcp_instances(en, 12), "12 copies");
        assert_eq!(
            nested_note(zh, 2, "Codex", "Claude Code"),
            "另有 2 個 Codex session 在 Claude Code 底下"
        );
        assert_eq!(
            nested_note(en, 1, "Codex", "Claude Code"),
            "1 more Codex session inside Claude Code"
        );
        assert_eq!(
            nested_note(en, 2, "Codex", "Claude Code"),
            "2 more Codex sessions inside Claude Code"
        );
        assert_eq!(launched_by(zh, "VS Code"), "（由 VS Code 啟動）");
        assert_eq!(launched_by(en, "VS Code"), "(started by VS Code)");
        assert_eq!(
            app_server_title(zh, Some("VS Code")),
            "Codex app-server（服務 VS Code）"
        );
        assert_eq!(
            app_server_title(en, Some("VS Code")),
            "Codex app-server (serving VS Code)"
        );
        assert_eq!(app_server_title(zh, None), "Codex app-server");
    }

    #[test]
    fn orphan_reasons_follow_the_spec_wording() {
        assert_eq!(
            orphan_reason_session_ended(Lang::ZhTw, "Claude Code", Some("~/prj/foo")),
            "原本屬於 Claude Code session（~/prj/foo），該 session 已結束"
        );
        assert_eq!(
            orphan_reason_parent_gone(Lang::ZhTw, 123),
            "它記錄的主人 PID 123 已不存在"
        );
        for lang in [Lang::ZhTw, Lang::En] {
            assert!(orphan_reason_session_ended(lang, "Codex", None).contains("Codex"));
            assert!(orphan_reason_path_only(lang, "Codex").contains("Codex"));
        }
    }

    #[test]
    fn header_pieces() {
        let (zh, en) = (Lang::ZhTw, Lang::En);
        assert_eq!(header_interval(zh, 2), "每 2 秒更新");
        assert_eq!(header_interval(en, 2), "refresh every 2 s");
        assert_eq!(header_ram(zh, "27 GB", "36 GB"), "RAM 已用 27 GB/36 GB");
        assert_eq!(header_swap(zh, false, "2.3 GB"), "swap 2.3 GB");
        assert_eq!(header_swap(zh, true, "2.3 GB"), "分頁檔 2.3 GB");
        assert_eq!(header_swap(en, true, "2.3 GB"), "page file 2.3 GB");
        assert_eq!(header_self(zh, "12 MB"), "cctree 本身 12 MB");
        assert_eq!(header_zombies(zh, 2), "殭屍 2 個");
        assert_eq!(header_zombies(en, 1), "1 zombie");
        assert_eq!(header_zombies(en, 2), "2 zombies");
        assert_eq!(header_sort(zh, Text::SortMem), "排序：記憶體");
        assert_eq!(header_sort(en, Text::SortName), "sort: name");
    }

    #[test]
    fn detail_panel_lines() {
        let (zh, en) = (Lang::ZhTw, Lang::En);
        assert_eq!(
            detail_ids(zh, 50094, Some(49815)),
            "PID 50094    父 PID 49815"
        );
        assert_eq!(detail_ids(en, 50094, None), "PID 50094");
        // The sentence of the spec mockup, word for word.
        assert_eq!(
            detail_mem_line(
                zh,
                "162 MB",
                Some(("112 MB", "42 MB")),
                Some("8 MB"),
                Some("15 MB")
            ),
            "記憶體 162 MB ＝ 在 RAM 裡 112 MB ＋ 被擠出 RAM 42 MB ＋ 其他 8 MB（另有 15 MB 與其他程式共用）"
        );
        assert_eq!(
            detail_mem_line(
                en,
                "162 MB",
                Some(("112 MB", "42 MB")),
                Some("8 MB"),
                Some("15 MB")
            ),
            "Memory 162 MB = In RAM 112 MB + Pushed out of RAM 42 MB + Other 8 MB (another 15 MB is shared with other programs)"
        );
        assert_eq!(
            detail_mem_line(zh, "162 MB", Some(("112 MB", "50 MB")), None, None),
            "記憶體 162 MB ＝ 在 RAM 裡 112 MB ＋ 被擠出 RAM 50 MB"
        );
        // Without the split the equation is left out rather than half said.
        assert_eq!(
            detail_mem_line(zh, "162 MB", None, Some("8 MB"), None),
            "記憶體 162 MB"
        );
        assert_eq!(
            detail_os_tool(zh, Text::OsToolMac, "162 MB"),
            "活動監視器會顯示 162 MB"
        );
        assert_eq!(
            detail_os_tool(zh, Text::OsToolLinux, "162 MB"),
            "htop 的 RES 會顯示 162 MB"
        );
        assert_eq!(
            detail_os_tool(en, Text::OsToolWindows, "97 MB"),
            "Task Manager shows 97 MB"
        );
        assert_eq!(
            detail_group(zh, 12, "1.3 GB"),
            "共 12 個 process，記憶體 1.3 GB"
        );
        assert_eq!(detail_group(en, 1, "9 MB"), "1 process, memory 9 MB");
        assert_eq!(
            detail_group(en, 12, "1.3 GB"),
            "12 processes, memory 1.3 GB"
        );
    }

    #[test]
    fn kill_dialog_pieces() {
        let (zh, en) = (Lang::ZhTw, Lang::En);
        assert_eq!(
            kill_total(zh, 3, "413 MB", false),
            "共 3 個，約可釋放 413 MB"
        );
        assert_eq!(
            kill_total(zh, 3, "413 MB", true),
            "共 3 個，約可釋放 413 MB 以上（有些讀不到，實際會更多）"
        );
        assert_eq!(
            kill_total(en, 3, "413 MB", false),
            "3 in total, about 413 MB to free"
        );
        assert_eq!(kill_more(zh, 4), "還有 4 個");
        assert_eq!(kill_more(en, 4), "4 more");
        assert_eq!(
            kill_excluded_suspected(zh, 2),
            "另有 2 個疑似失聯的沒有包含"
        );
        assert_eq!(
            kill_excluded_suspected(en, 1),
            "1 suspected orphan is not included"
        );
        assert_eq!(
            kill_excluded_suspected(en, 2),
            "2 suspected orphans are not included"
        );
        assert_eq!(
            kill_interrupts(zh, Some("~/prj/cctree"), Resume::Command("claude --resume")),
            "這會中斷 ~/prj/cctree 的對話；對話紀錄不會遺失，之後可以用 `claude --resume` 接續"
        );
        assert_eq!(
            kill_interrupts(zh, Some("~/prj/cctree"), Resume::ReopenFrom("VS Code")),
            "這會中斷 ~/prj/cctree 的對話；對話紀錄不會遺失，之後可以從 VS Code 重新開啟"
        );
        assert_eq!(
            kill_interrupts(en, None, Resume::Command("codex resume")),
            "This interrupts the conversation of this session; the history is not lost and you can pick it up again with `codex resume`"
        );
        assert_eq!(kill_progress(zh, 2, 3), "正在結束… 2/3");
        assert_eq!(kill_progress(en, 2, 3), "Ending… 2/3");
        for (title, one) in [
            (Text::DialogTitleOne, "結束這個 process？"),
            (Text::DialogTitleTree, "結束整組 process？"),
        ] {
            assert_eq!(tr(zh, title), one);
        }
        assert!(tr(zh, Text::DialogForceOnly).contains("Windows 會直接強制結束"));
        assert!(tr(zh, Text::DialogMcpClaude).contains("/mcp"));
    }
}
