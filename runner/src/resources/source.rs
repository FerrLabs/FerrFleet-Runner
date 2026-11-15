use std::fs;
use std::path::{Path, PathBuf};

use super::cgroup;
use super::usage::Reading;

pub const CGROUP_ROOT: &str = "/sys/fs/cgroup";
const MEMINFO: &str = "/proc/meminfo";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Layout {
    V2,
    V1,
}

#[derive(Debug)]
pub struct Cgroup {
    root: PathBuf,
    layout: Layout,
}

fn read(path: &Path) -> Option<String> {
    fs::read_to_string(path).ok()
}

impl Cgroup {
    pub fn detect(root: &Path) -> Option<Self> {
        let layout = if root.join("cpu.stat").is_file() && root.join("memory.current").is_file() {
            Layout::V2
        } else if root.join("cpuacct/cpuacct.usage").is_file()
            && root.join("memory/memory.usage_in_bytes").is_file()
        {
            Layout::V1
        } else {
            return None;
        };
        Some(Self {
            root: root.to_path_buf(),
            layout,
        })
    }

    pub fn reading(&self) -> Option<Reading> {
        let (cpu_usec, memory_bytes) = match self.layout {
            Layout::V2 => (
                cgroup::cpu_usage_usec_v2(&read(&self.root.join("cpu.stat"))?)?,
                cgroup::memory_bytes(&read(&self.root.join("memory.current"))?)?,
            ),
            Layout::V1 => (
                cgroup::cpu_usage_usec_v1(&read(&self.root.join("cpuacct/cpuacct.usage"))?)?,
                cgroup::memory_bytes(&read(&self.root.join("memory/memory.usage_in_bytes"))?)?,
            ),
        };
        Some(Reading {
            cpu_usec,
            memory_bytes,
        })
    }

    pub fn cpu_limit(&self) -> Option<u32> {
        match self.layout {
            Layout::V2 => cgroup::cpu_limit_v2(&read(&self.root.join("cpu.max"))?),
            Layout::V1 => cgroup::cpu_limit_v1(
                &read(&self.root.join("cpu/cpu.cfs_quota_us"))?,
                &read(&self.root.join("cpu/cpu.cfs_period_us"))?,
            ),
        }
    }

    pub fn memory_limit(&self) -> Option<u64> {
        let file = match self.layout {
            Layout::V2 => "memory.max",
            Layout::V1 => "memory/memory.limit_in_bytes",
        };
        cgroup::memory_limit(&read(&self.root.join(file))?)
    }
}

pub fn host_cpu_millicores() -> Option<u32> {
    let cpus = std::thread::available_parallelism().ok()?;
    u32::try_from(cpus.get()).ok()?.checked_mul(1000)
}

pub fn host_memory_bytes() -> Option<u64> {
    cgroup::host_memory_total(&read(Path::new(MEMINFO))?)
}
