# cctree

**Unofficial tool. It is not affiliated with Anthropic or OpenAI.**

English | [繁體中文](README.zh-TW.md)

`cctree` is a terminal tool, similar to `top`. It shows the processes that Claude Code and Codex start on your computer. It groups these processes by the session that started them. It also lets you end one process or one group safely.

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

## Purpose

Use cctree to see which processes Claude Code and Codex started, how much CPU and memory they use, and which of them you can end.

## The problem

Claude Code and Codex start many processes for each session. Examples are MCP servers, the `npx` wrappers of MCP servers, shells and language servers. Some of these processes continue to run after their session ends.

The process list of your system (Activity Monitor, Task Manager or `top`) shows these processes as a flat list. Many of them have the same name, for example `node`. The list does not show which session started a process. Thus it is difficult to find which processes use your memory, and it is not safe to end a process when you do not know its owner.

cctree shows the same processes as a tree. Each process is under the session that started it.

## Features

- **Tree by session.** cctree groups each process under the Claude Code or Codex session that started it. It shows each MCP server with the name from your Claude Code or Codex config.
- **Numbers on each row.** Each process and each group shows PID, CPU and memory. The memory numbers agree with Activity Monitor and Task Manager.
- **MCP overview.** Press `m` to see the total of each MCP server across all sessions.
- **Orphaned processes.** cctree finds processes that continue to run after their owner ended. It marks each one "confirmed" or "suspected" and tells you why.
- **Safe end.** Press `x` to end one process or `X` to end a tree. cctree asks you to confirm each time. It does not end cctree itself, its shell or terminal, system processes, or the processes of other users.
- **Privacy.** cctree does not use the network, does not collect telemetry and does not write files. It masks API keys, tokens and passwords in command lines before it shows them.
- **Platforms.** macOS, Windows and Linux, including WSL. You do not need administrator rights.
- **Languages.** English and Traditional Chinese.

## Install

### macOS and Linux

Run this command in a terminal:

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/Treuwok/cctree/releases/latest/download/cctree-installer.sh | sh
```

The installer puts `cctree` in `~/.cargo/bin`. If your shell cannot find `cctree`, open a new terminal.

### Windows

Run this command in PowerShell:

```powershell
powershell -ExecutionPolicy Bypass -c "irm https://github.com/Treuwok/cctree/releases/latest/download/cctree-installer.ps1 | iex"
```

The installer puts `cctree.exe` in `%USERPROFILE%\.cargo\bin`. Run `cctree` in Windows Terminal or PowerShell.

### WSL

The Windows build sees only Windows processes. The Linux build in WSL sees only the processes in WSL. If you use Claude Code or Codex in WSL, install cctree in WSL with the macOS and Linux command. To see the Windows processes, also install cctree on Windows. cctree shows a notice when it finds this condition.

### Download a binary

The [Releases](https://github.com/Treuwok/cctree/releases) page has archives for these platforms:

| Platform | Archive |
|---|---|
| macOS, Apple silicon | `cctree-aarch64-apple-darwin.tar.xz` |
| macOS, Intel | `cctree-x86_64-apple-darwin.tar.xz` |
| Windows x64 | `cctree-x86_64-pc-windows-msvc.zip` |
| Linux x64 (static) | `cctree-x86_64-unknown-linux-musl.tar.xz` |
| Linux arm64 (static) | `cctree-aarch64-unknown-linux-musl.tar.xz` |

Each archive contains the executable file, the README, the CHANGELOG and the license files. Extract the archive. Then put the executable file in a directory that is in your `PATH`.

The binaries do not have a code signature. If you download an archive with a web browser, macOS Gatekeeper or Windows SmartScreen can show a warning. The installer commands above do not cause this warning.

- macOS: after you extract the archive, run `xattr -d com.apple.quarantine ./cctree`.
- Windows: in the SmartScreen window, click "More info" and then "Run anyway".

### Install with Cargo

If you have Rust, you can build and install the latest source:

```sh
cargo install --git https://github.com/Treuwok/cctree --locked
```

## Build from source

### Prerequisites

1. Install Rust stable with [rustup](https://rustup.rs).
2. Install a C linker for your platform:
   - macOS: run `xcode-select --install`.
   - Windows: install Visual Studio Build Tools with the "Desktop development with C++" workload. rustup tells you about this step.
   - Linux: install `gcc` (for example, `sudo apt-get install build-essential`).

### Build

```sh
git clone https://github.com/Treuwok/cctree.git
cd cctree
cargo build --release
```

The executable is `target/release/cctree` (`target\release\cctree.exe` on Windows).

To install it in `~/.cargo/bin`, run `cargo install --path . --locked`.

### Static Linux build

The Linux release is a static musl build. To make the same build:

```sh
sudo apt-get install musl-tools
rustup target add x86_64-unknown-linux-musl
cargo build --release --target x86_64-unknown-linux-musl
```

### Test

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

CI runs these commands on macOS, Windows and Linux for each pull request.

### Make a release

1. Change `version` in `Cargo.toml`.
2. Add the changes to `CHANGELOG.md`.
3. Merge these changes into `main`.
4. Push a tag with the same version, for example `git tag v0.1.1` and `git push origin v0.1.1`.

The release workflow builds all platforms and publishes the archives and the installers on the Releases page.

## Use

Run `cctree` in a terminal. The terminal must be 80×24 or larger. cctree has no options. Use the mouse wheel to scroll. Click a row to select it or to fold it.

| Key | Action |
|---|---|
| `↑ ↓ / k j` | Move up and down |
| `PgUp PgDn Home End` | Move faster |
| `← / h` | Fold. If the row is folded, go to the parent row. |
| `→ / l` | Unfold |
| `Enter` | Fold or unfold |
| `x` | End this process |
| `X` | End the tree or group |
| `m` | Show or hide the MCP overview |
| `s` | Change the sort order (memory → CPU → PID → name) |
| `/` | Find a name or a PID. Esc clears the search. |
| `Space` | Pause or continue the updates |
| `+ -` | Change the refresh interval (1 to 10 seconds) |
| `?` | Help |
| `q / Ctrl+C` | Quit |

The first CPU value needs two samples. Thus cctree shows "—" for a short time. If you set `NO_COLOR`, cctree does not use color.

cctree uses the language of your system. It uses Traditional Chinese for `zh_TW`, `zh_HK` and `zh_Hant*`, and English for all other languages. To select a language, set `CCTREE_LANG` (for example, `zh_TW` or `en`).

### What `x` and `X` end

- Each end action asks you to confirm one time. Press Enter to confirm or Esc to cancel. The dialog shows the processes and the memory that you get back.
- `X` ends the processes that you see under the selected row. It also ends their child processes that cctree does not show. It does not end processes that are under a different row.
- cctree does not let you use `X` on a full category (Claude Code, Codex, Orphaned or Other Node.js). This range is too large. To end an orphaned process, select its row and press `x`.
- On macOS and Linux, cctree sends SIGTERM to each process, deepest child first. After 3 seconds, it sends SIGKILL to each process that is still running. On Windows, cctree ends each process immediately.
- cctree does not end these processes: cctree itself, the shell and terminal that run cctree, PID 0, PID 1, and processes that root or SYSTEM owns. It does not end the processes of other users, unless cctree runs as root (`sudo cctree`).

## What the memory numbers mean

cctree shows four memory numbers for each process. Use them to compare cctree with the tool of your system.

| Name | Meaning | macOS | Windows | Linux |
|---|---|---|---|---|
| **Memory** | The memory that only this process uses. You get it back when the process ends. Totals and sort order use this number. | `phys_footprint` (Activity Monitor, "Memory") | Private Bytes (Task Manager, Details, "Commit size") | `RssAnon` + `VmSwap` |
| In RAM | The part that is in RAM now. | `internal` | Private Working Set (Task Manager, Processes, "Memory") | `RssAnon` |
| Pushed out of RAM | The part that the system compressed or moved to disk, because the process did not use it for some time. It has a small effect on speed, but it still uses resources. | `compressed` | Private Bytes − Private Working Set, shown with "~" | `VmSwap` |
| Shared | Code and libraries that the process shares with other processes. cctree shows this number but does not add it to totals. | `resident` − `internal` | Working Set − Private Working Set | `RssFile` + `RssShmem` |

Limits of each system:

- macOS does not give the swap of one process. It gives only the compressed part, and compressed data can move to swap later. If the numbers of the system do not add up, cctree adds an "Other" item. Thus the two sides of "=" in the detail panel are always equal.
- Windows does not give the compressed part of one process. Thus "Pushed out of RAM" is the total of the page file, compression and reserved space that is not in use. "In RAM" needs Windows 10, or Windows 11 22H2 with the September 2023 update. On older versions, cctree shows "—".
- Linux gives the most complete data, including the swap of each process. The RES value in `htop` includes shared memory. Thus it is larger than "Memory".

A "~" before a total means that cctree cannot read some members of the group. The real value can be higher. If cctree cannot read a field (for example, for a process of a different user), it shows "—" and the reason.

On macOS and Linux, 100% CPU is one full core, and the value can be more than 100% (as in `top`). On Windows, 100% CPU is the full computer (as in Task Manager).

## Orphaned processes

"Orphaned" shows processes that continue to run after their owner ended. cctree uses this evidence:

- The process has the ID of a Claude Code session, and that session ended.
- The process has a `--parent-pid` value, and that process does not exist.
- The process runs from a Codex or Claude directory. cctree uses this evidence only when no process started it, and only when no session of that tool is running.

cctree marks each orphaned process "confirmed" or "suspected". The detail panel tells you the reason. These marks are hints only.

- "Confirmed" means that `launchd`, PID 1 or a subreaper adopted the process (or its parent does not exist), that it is not an app you use, and that no running session can own it.
- "Suspected" means that the evidence is not as strong. The process can be an app that you use.

cctree does not end orphaned processes as a batch. Examine each row, and then press `x` to end it.

## Privacy

- cctree does not use the network, does not collect telemetry and does not write files.
- It reads process data. It also reads the `command` and `args` of the MCP servers in your Claude Code and Codex config files. It does not read the `env` values in these files. It does not change these files.
- cctree masks command lines before it shows them. It masks API keys, tokens, passwords, `Bearer` values, secret headers such as `X-Api-Key:`, and credentials in URLs. You cannot show the original text. The masking uses common patterns, and it cannot find all forms of secrets.
- From other processes, cctree keeps only these environment variable names: `CLAUDECODE`, `CLAUDE_CODE_SESSION_ID`, `CLAUDE_PROJECT_DIR`, `CLAUDE_CONFIG_DIR`, `CLAUDE_CODE_ENTRYPOINT` and `CODEX_HOME`. It does not keep other variables.

## License

You can use cctree under the [Apache License, Version 2.0](LICENSE-APACHE) or the [MIT license](LICENSE-MIT). You select which one.

cctree is an unofficial tool. It is not affiliated with Anthropic or OpenAI.
