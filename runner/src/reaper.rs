use std::collections::HashMap;
use std::time::{Duration, Instant};

use rustix::process::{Pid, Signal, WaitOptions, getpid, kill_process, waitpid};
use tracing::warn;

const SETTLE_DEADLINE: Duration = Duration::from_secs(5);
const SETTLE_POLL: Duration = Duration::from_millis(10);

#[derive(Clone, Copy)]
pub(crate) struct Reaper {
    root: Pid,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Alive,
    Zombie,
}

#[derive(Clone, Copy)]
struct Entry {
    pid: Pid,
    parent: i32,
    state: State,
}

impl Reaper {
    pub(crate) fn runner() -> Self {
        let root = getpid();
        adopt_orphans(root);
        Self { root }
    }

    #[cfg(test)]
    pub(crate) fn under(root: Pid) -> Self {
        Self { root }
    }

    pub(crate) fn kill_descendants(self) {
        for entry in self.descendants() {
            if entry.state == State::Alive {
                let _ = kill_process(entry.pid, Signal::KILL);
            }
        }
    }

    pub(crate) async fn settle(self) -> Vec<i32> {
        let deadline = Instant::now() + SETTLE_DEADLINE;
        loop {
            let descendants = self.descendants();
            let mut alive = Vec::new();
            for entry in descendants {
                match entry.state {
                    State::Alive => {
                        let _ = kill_process(entry.pid, Signal::KILL);
                        alive.push(entry.pid.as_raw_nonzero().get());
                    }
                    State::Zombie if entry.parent == getpid().as_raw_nonzero().get() => {
                        let _ = waitpid(Some(entry.pid), WaitOptions::NOHANG);
                    }
                    State::Zombie => {}
                }
            }
            if alive.is_empty() || Instant::now() >= deadline {
                if !alive.is_empty() {
                    warn!(pids = ?alive, "processes of the run survived SIGKILL");
                }
                return alive;
            }
            tokio::time::sleep(SETTLE_POLL).await;
        }
    }

    fn descendants(self) -> Vec<Entry> {
        let mut children: HashMap<i32, Vec<Entry>> = HashMap::new();
        for entry in process_table() {
            children.entry(entry.parent).or_default().push(entry);
        }
        let mut found = Vec::new();
        let mut pending = vec![self.root.as_raw_nonzero().get()];
        while let Some(parent) = pending.pop() {
            for entry in children.remove(&parent).unwrap_or_default() {
                pending.push(entry.pid.as_raw_nonzero().get());
                found.push(entry);
            }
        }
        found
    }
}

#[cfg(target_os = "linux")]
fn adopt_orphans(me: Pid) {
    if let Err(err) = rustix::process::set_child_subreaper(Some(me)) {
        warn!(%err, "could not become a child subreaper; processes that detach from a run may escape its cleanup");
    }
}

#[cfg(not(target_os = "linux"))]
fn adopt_orphans(_me: Pid) {}

fn process_table() -> Vec<Entry> {
    let Ok(dir) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    dir.flatten()
        .filter_map(|item| {
            let pid = item.file_name().to_str()?.parse::<i32>().ok()?;
            let stat = std::fs::read_to_string(item.path().join("stat")).ok()?;
            parse_stat(pid, &stat)
        })
        .collect()
}

fn parse_stat(pid: i32, stat: &str) -> Option<Entry> {
    let (_, rest) = stat.rsplit_once(')')?;
    let mut fields = rest.split_whitespace();
    let state = match fields.next()? {
        "Z" | "X" => State::Zombie,
        _ => State::Alive,
    };
    let parent = fields.next()?.parse().ok()?;
    Some(Entry {
        pid: Pid::from_raw(pid)?,
        parent,
        state,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_command_name_with_spaces_and_parentheses_does_not_shift_the_fields() {
        let entry = parse_stat(42, "42 (a) b (c) S 7 42 42 0 -1").expect("a valid stat line");
        assert_eq!(entry.parent, 7);
        assert!(entry.state == State::Alive);
    }

    #[test]
    fn zombies_and_dead_processes_are_not_alive() {
        for state in ["Z", "X"] {
            let entry = parse_stat(1, &format!("1 (x) {state} 0")).expect("a valid stat line");
            assert!(entry.state == State::Zombie, "{state}");
        }
    }

    #[test]
    fn a_truncated_stat_line_is_ignored() {
        assert!(parse_stat(1, "1 (x) S").is_none());
        assert!(parse_stat(1, "garbage").is_none());
    }
}
