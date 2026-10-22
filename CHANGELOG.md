# Changelog

## [0.1.0] - 2026-10-22

First release.

- Shows the processes started by Claude Code and Codex as a tree, grouped by session. Each session's MCP servers are named after your Claude Code or Codex config; its other child processes and its detached processes are listed apart. "Other Node.js" and "Orphaned" (owner ended, still running) are separate groups.
- PID, CPU and memory on every row and group. Memory is split into the four names of the help page (Memory, In RAM, Pushed out of RAM, Shared) and matches Activity Monitor and Task Manager. An MCP overview adds up each MCP across sessions.
- `x` ends one process and `X` ends a tree or group, after one confirmation. Protected against PID reuse, and refuses cctree itself and its ancestors, PID 0 and 1, root and SYSTEM processes, and a whole category.
- Orphans are marked "confirmed" or "suspected" as a hint; nothing is ended in a batch.
- No network, no telemetry and no files written. Command lines are masked, and only a short list of environment variable names is read from other processes.
- Traditional Chinese and English (`CCTREE_LANG`), `NO_COLOR`, and a notice for WSL.
- Builds for macOS (Apple silicon and Intel), Windows x64 and Linux (x64 and arm64, static), with shell, PowerShell and Homebrew installers.
