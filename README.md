# cctree

**Unofficial; not affiliated with Anthropic or OpenAI.**

English | [繁體中文](README.zh-TW.md)

`cctree` is a `top`-like terminal tool that shows what Claude Code and Codex start on your computer. It groups every process by the session that started it (and each session's MCP servers by the name in your config), shows PID, CPU and memory for every row and every group, and lets you end one process or a whole group safely. It also points out processes whose owner has already ended. It runs on macOS, Windows and Linux (including WSL), needs no administrator rights, and never uses the network.

```text
cctree  refresh every 2 s · RAM used 27.0 GB/36.0 GB · swap 2.3 GB · cctree itself 12 MB      ? help
  Name                                                                PID     CPU      Memory  Count
▼ Claude Code (2 sessions)                                                   0.5%      2.0 GB     13
  ▼ ~/prj/cctree                                                    49049    0.5%      1.5 GB     11
      (itself)                                                      49049    0.4%      550 MB
    ▼ MCP (3)                                                                0.0%      958 MB      8
      ▼ chrome-live                                                 50094    0.0%      413 MB      3
          npx wrapper                                               49815    0.0%       97 MB
          chrome-devtools-mcp                                       50094    0.0%      162 MB
          └ watchdog                                                50485    0.0%      154 MB
      ▶ chrome-cdp                                                  49722    0.0%      330 MB      3
      ▶ playwright (plugin)                                         49770    0.0%      215 MB      2
    ▶ Other child processes (shell, LSP…)                                    0.1%       12 MB      2
  ▶ ~/prj/非常非常長的中文專案目錄名稱/子資料夾                     52091    0.0%      551 MB      2
  Codex (not running)
▼ ⚠ Orphaned (1)                                                             0.2%      102 MB      1
    ？ChatGPT Meetings (suspected)                                  80871    0.2%      102 MB
▶ Other Node.js (1)                                                          0.0%       88 MB      1





────────────────────────────────────────────────────────────────────────────────────────────────────
 chrome-live › chrome-devtools-mcp    PID 50094    parent PID 49815
 Memory 162 MB = In RAM 112 MB + Pushed out of RAM 42 MB + Other 8 MB (another 15 MB is shared with
 other programs)
 Command: chrome-devtools-mcp --autoConnect    running 15 min

 ↑↓ move  ←→ fold/unfold  x end this  X end tree  m MCP overview  Space pause  ? help  q quit
```

(Sample data.)

## Install

macOS and Linux:

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/yenchiugu/cctree/releases/latest/download/cctree-installer.sh | sh
```

Windows (PowerShell):

```powershell
powershell -ExecutionPolicy Bypass -c "irm https://github.com/yenchiugu/cctree/releases/latest/download/cctree-installer.ps1 | iex"
```

Homebrew:

```sh
brew install yenchiugu/tap/cctree
```

Cargo:

```sh
cargo install cctree
```

From source, in a clone of this repository:

```sh
cargo install --path .
```

Prebuilt binaries for macOS (Apple silicon and Intel), Windows x64 and Linux (x64 and arm64, static) are also on the [Releases](https://github.com/yenchiugu/cctree/releases) page.

If you download an archive with a web browser, macOS Gatekeeper or Windows SmartScreen may warn about it, because the binaries are not signed. Installing with the commands above does not trigger the warning.

- macOS: after unpacking, run `xattr -d com.apple.quarantine ./cctree`.
- Windows: in the SmartScreen window click "More info", then "Run anyway".

## Use

Run `cctree` in a terminal (at least 80×24). No options are needed. The mouse wheel scrolls, and a click selects or folds a row.

| Key | What it does |
|---|---|
| `↑ ↓ / k j` | Move up and down |
| `PgUp PgDn Home End` | Move faster |
| `← / h` | Fold; when already folded, go to the parent |
| `→ / l` | Unfold |
| `Enter` | Fold or unfold |
| `x` | End this one process |
| `X` | End the whole tree or group |
| `m` | Switch the MCP overview on or off |
| `s` | Change the sort (memory → CPU → PID → name) |
| `/` | Search names or PIDs; Esc clears |
| `Space` | Pause or resume updates |
| `+ -` | Change the refresh interval, 1 to 10 s |
| `?` | Help |
| `q / Ctrl+C` | Quit |

The first CPU reading needs two samples, so it shows "—" for a moment. Colour is off when `NO_COLOR` is set. The language follows your system (Traditional Chinese for `zh_TW`, `zh_HK` and `zh_Hant*`, English otherwise); set `CCTREE_LANG` (for example `zh_TW` or `en`) to choose.

### What `x` and `X` end

- Every end action asks once: Enter confirms, Esc cancels. The dialog lists the processes and how much memory you get back.
- `X` ends what you see: the processes drawn under the selected row, plus their child processes that cctree does not track. Processes drawn under another row are not touched.
- `X` on a whole category (Claude Code, Codex, Orphaned, Other Node.js) is refused as too broad. Nothing is ended in a batch; an orphan is ended from its own row.
- On macOS and Linux each process gets SIGTERM, deepest child first, then SIGKILL after 3 seconds if it is still there. Windows ends them by force right away.
- cctree never ends itself or the shell and terminal it runs in, PID 0 and 1, or processes owned by root or SYSTEM. Another user's processes are refused unless cctree runs as root (`sudo cctree`).

## What the memory numbers mean

Each process shows four numbers, so the figure you see can be matched to your system's own tool.

| Name | Meaning | macOS | Windows | Linux |
|---|---|---|---|---|
| **Memory** | What the process holds by itself; ending it gives it back. Totals and sorting use this number. | `phys_footprint` (Activity Monitor "Memory") | Private Bytes (Task Manager, Details, "Commit size") | `RssAnon` + `VmSwap` |
| In RAM | The part that sits in RAM now. | `internal` | Private Working Set (Task Manager, Processes, "Memory") | `RssAnon` |
| Pushed out of RAM | Not used for a while, so the system compressed it or moved it to disk. It hardly affects speed but still takes resources. | `compressed` | Private Bytes − Private Working Set, shown with "~" | `VmSwap` |
| Shared | Code and libraries shared with other processes. Shown, never added up. | `resident` − `internal` | Working Set − Private Working Set | `RssFile` + `RssShmem` |

Limits of each system:

- macOS does not report swap per process, only how much is compressed (compressed data may later move to swap). When the system's own bookkeeping leaves a gap, an "Other" item closes it so the two sides of the "=" in the detail panel always add up.
- Windows does not report how much of a process is compressed, so "Pushed out of RAM" is the total of page file, compression and reserved-but-unused space. "In RAM" needs Windows 10 or 11 22H2 with the September 2023 update; on an older build it shows "—".
- Linux gives the most complete picture, including swap per process. `htop`'s RES also counts shared memory, so it reads larger than "Memory".

A "~" before a total means some members could not be read, so the real figure may be higher. Fields cctree cannot read (for example another user's process) show "—" with the reason.

CPU is the share of one core on macOS and Linux (it can pass 100%, like `top`), and the share of the whole machine on Windows (like Task Manager).

## Orphaned processes

"Orphaned" lists processes whose owner has already ended but that are still running. cctree judges this from evidence: the id of the Claude Code session that started the process (that session has ended), a `--parent-pid` that no longer exists, or a path under a Codex or Claude directory (only for a process nobody started, and only when no session of that tool is running).

Each row is marked **confirmed** or **suspected**, and the detail panel says why. The marks are hints for you: "confirmed" means the process has been adopted by `launchd`, PID 1 or a subreaper (or its parent is gone), is not an app you run, and no running session could still own it; anything less certain is "suspected" and may be an app you are using. Nothing is ended in a batch. Look at a row, then end it with `x`.

## Privacy

- cctree does not use the network, collects no telemetry and writes no files while it runs.
- It reads process information and, read-only, the `command` and `args` of the MCP servers in your Claude Code and Codex config files. Config `env` values are never read.
- Command lines are masked before they are shown (API keys, tokens, passwords, `Bearer` values, secret headers such as `X-Api-Key:`, credentials in URLs). There is no option to show the original. Masking works from common patterns and cannot catch every form.
- From other processes it keeps only a short list of environment variable names (`CLAUDECODE`, `CLAUDE_CODE_SESSION_ID`, `CLAUDE_PROJECT_DIR`, `CLAUDE_CONFIG_DIR`, `CLAUDE_CODE_ENTRYPOINT`, `CODEX_HOME`); every other variable is dropped without being stored.

## WSL

The Windows build sees only Windows processes, and the Linux build inside WSL sees only the processes in WSL. If you use Claude Code or Codex inside WSL, install and run cctree inside WSL; for the Windows side, run it in PowerShell. cctree shows a notice when it detects this.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT) at your option.

cctree is unofficial and not affiliated with Anthropic or OpenAI.
