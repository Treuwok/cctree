//! The collection side of the program (spec 4.2): one [`Worker::tick`] is
//! one refresh, and [`spawn`] runs ticks on a thread of their own so the UI
//! is never held up by collecting.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::classify::rules::detect_tool;
use crate::classify::{Classification, classify};
use crate::collect::Collector;
use crate::config::{ConfigLoader, ConfigProblem};
use crate::model::Snapshot;

/// What one refresh produced.
pub struct Frame {
    pub snap: Snapshot,
    pub cls: Classification,
    /// Config files that exist but could not be read or understood.
    pub config_problems: Vec<ConfigProblem>,
}

/// What the UI tells the worker thread.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToWorker {
    /// The refresh interval, in seconds.
    SetInterval(u64),
    /// `true` stops the ticks, `false` starts them again with one right away.
    Pause(bool),
    /// One tick now, paused or not (after ending processes).
    Refresh,
    Quit,
}

/// The collector and the config loader, which keep their caches from one
/// tick to the next.
pub struct Worker {
    collector: Collector,
    configs: ConfigLoader,
}

impl Worker {
    /// `home` is the user's home directory, where the Claude Code and Codex
    /// configs are looked for (unless this process's `CLAUDE_CONFIG_DIR` or
    /// `CODEX_HOME` says otherwise).
    pub fn new(home: PathBuf) -> Worker {
        Worker {
            collector: Collector::new(),
            configs: ConfigLoader::new(&home, &|name| std::env::var(name).ok()),
        }
    }

    /// One refresh: list the processes, read the configs they point at (the
    /// distinct `CLAUDE_CONFIG_DIR` and `CODEX_HOME` values in their
    /// environments, and the `.mcp.json` in the working directory of every
    /// Claude Code or Codex process), classify, then read memory for the
    /// processes that will be shown.
    pub fn tick(&mut self) -> Frame {
        let mut snap = self.collector.snapshot();
        let mut claude_dirs = BTreeSet::new();
        let mut codex_homes = BTreeSet::new();
        let mut cwds = BTreeSet::new();
        for p in &snap.procs {
            if let Some(dir) = p.env.get("CLAUDE_CONFIG_DIR") {
                claude_dirs.insert(dir.clone());
            }
            if let Some(home) = p.env.get("CODEX_HOME") {
                codex_homes.insert(home.clone());
            }
            if let Some(cwd) = &p.cwd
                && detect_tool(p).is_some()
            {
                cwds.insert(cwd.clone());
            }
        }
        let list = |set: BTreeSet<String>| set.into_iter().collect::<Vec<_>>();
        let configs = self
            .configs
            .load(&list(claude_dirs), &list(codex_homes), &list(cwds));
        let cls = classify(&snap, configs);
        let config_problems = configs.problems.clone();
        self.collector.fill_dynamic(&mut snap, &cls.tracked());
        Frame {
            snap,
            cls,
            config_problems,
        }
    }
}

/// Runs `w` on a thread: a tick right away, then one every `interval_secs`
/// (counted from the start of the previous tick), each [`Frame`] sent on
/// `out`. It ends on [`ToWorker::Quit`] or when either channel is closed.
pub fn spawn(
    mut w: Worker,
    interval_secs: u64,
    out: Sender<Frame>,
    cmds: Receiver<ToWorker>,
) -> JoinHandle<()> {
    thread::spawn(move || {
        let mut interval = Duration::from_secs(interval_secs);
        let mut paused = false;
        let mut last: Option<Instant> = None;
        loop {
            let cmd = if paused {
                cmds.recv().map_err(|_| RecvTimeoutError::Disconnected)
            } else {
                let due = last.map_or_else(Instant::now, |t| t + interval);
                cmds.recv_timeout(due.saturating_duration_since(Instant::now()))
            };
            match cmd {
                Err(RecvTimeoutError::Timeout) | Ok(ToWorker::Refresh) => {}
                Ok(ToWorker::SetInterval(secs)) => {
                    interval = Duration::from_secs(secs);
                    continue;
                }
                Ok(ToWorker::Pause(p)) => {
                    let resumed = paused && !p;
                    paused = p;
                    if !resumed {
                        continue;
                    }
                }
                Ok(ToWorker::Quit) | Err(RecvTimeoutError::Disconnected) => return,
            }
            last = Some(Instant::now());
            if out.send(w.tick()).is_err() {
                return;
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worker_tick_tracks_self() {
        let mut w = Worker::new(std::env::temp_dir());
        let f = w.tick();
        assert!(f.snap.get(f.snap.self_key).unwrap().mem.is_some());
    }
}
