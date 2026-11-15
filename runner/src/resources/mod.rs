mod cgroup;
mod source;
mod usage;

use std::path::Path;
use std::time::{Duration, Instant};

use serde::Serialize;

use source::{CGROUP_ROOT, Cgroup};
use usage::{Averages, Usage};

pub const SAMPLE_EVERY: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(serde::Deserialize))]
pub struct Resources {
    pub cpu_limit_millicores: Option<u32>,
    pub memory_limit_bytes: Option<u64>,
    #[serde(flatten)]
    pub usage: Averages,
}

#[derive(Debug)]
pub struct Sampler {
    cgroup: Cgroup,
    cpu_limit_millicores: Option<u32>,
    memory_limit_bytes: Option<u64>,
    usage: Usage,
}

impl Sampler {
    pub fn start() -> Option<Self> {
        Self::start_at(Path::new(CGROUP_ROOT))
    }

    fn start_at(root: &Path) -> Option<Self> {
        let cgroup = Cgroup::detect(root)?;
        let cpu_limit_millicores = cgroup.cpu_limit().or_else(source::host_cpu_millicores);
        let memory_limit_bytes = cgroup.memory_limit().or_else(source::host_memory_bytes);
        let mut sampler = Self {
            cgroup,
            cpu_limit_millicores,
            memory_limit_bytes,
            usage: Usage::default(),
        };
        sampler.sample();
        sampler.usage.averages()?;
        Some(sampler)
    }

    pub fn sample(&mut self) {
        if let Some(reading) = self.cgroup.reading() {
            self.usage.observe(reading, Instant::now());
        }
    }

    pub fn resources(&self) -> Option<Resources> {
        Some(Resources {
            cpu_limit_millicores: self.cpu_limit_millicores,
            memory_limit_bytes: self.memory_limit_bytes,
            usage: self.usage.averages()?,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use super::*;

    struct Tree(PathBuf);

    impl Tree {
        fn new(files: &[(&str, &str)]) -> Self {
            let root =
                std::env::temp_dir().join(format!("ferrfleet-cgroup-{}", uuid::Uuid::new_v4()));
            for (name, content) in files {
                let path = root.join(name);
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                fs::write(path, content).unwrap();
            }
            fs::create_dir_all(&root).unwrap();
            Self(root)
        }
    }

    impl Drop for Tree {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_v2_cgroup_reports_its_limits_and_usage() {
        let tree = Tree::new(&[
            ("cpu.stat", "usage_usec 1000\n"),
            ("memory.current", "2048\n"),
            ("cpu.max", "200000 100000\n"),
            ("memory.max", "1073741824\n"),
        ]);
        let resources = Sampler::start_at(&tree.0).unwrap().resources().unwrap();
        assert_eq!(resources.cpu_limit_millicores, Some(2000));
        assert_eq!(resources.memory_limit_bytes, Some(1_073_741_824));
        assert_eq!(resources.usage.memory_peak_bytes, 2048);
        assert_eq!(resources.usage.samples, 1);
    }

    #[test]
    fn a_v1_cgroup_reports_its_limits_and_usage() {
        let tree = Tree::new(&[
            ("cpuacct/cpuacct.usage", "5000000\n"),
            ("memory/memory.usage_in_bytes", "4096\n"),
            ("cpu/cpu.cfs_quota_us", "50000\n"),
            ("cpu/cpu.cfs_period_us", "100000\n"),
            ("memory/memory.limit_in_bytes", "268435456\n"),
        ]);
        let resources = Sampler::start_at(&tree.0).unwrap().resources().unwrap();
        assert_eq!(resources.cpu_limit_millicores, Some(500));
        assert_eq!(resources.memory_limit_bytes, Some(268_435_456));
        assert_eq!(resources.usage.memory_peak_bytes, 4096);
    }

    #[test]
    fn an_unlimited_cgroup_falls_back_to_the_host_capacity() {
        let tree = Tree::new(&[
            ("cpu.stat", "usage_usec 1000\n"),
            ("memory.current", "2048\n"),
            ("cpu.max", "max 100000\n"),
            ("memory.max", "max\n"),
        ]);
        let resources = Sampler::start_at(&tree.0).unwrap().resources().unwrap();
        assert_eq!(
            resources.cpu_limit_millicores,
            source::host_cpu_millicores()
        );
        assert_eq!(resources.memory_limit_bytes, source::host_memory_bytes());
    }

    #[test]
    fn missing_cgroup_files_mean_no_sampler() {
        let tree = Tree::new(&[]);
        assert!(Sampler::start_at(&tree.0).is_none());
        assert!(Sampler::start_at(Path::new("/definitely/not/here")).is_none());
    }

    #[test]
    fn unreadable_counters_mean_no_sampler() {
        let tree = Tree::new(&[("cpu.stat", "user_usec 1\n"), ("memory.current", "1\n")]);
        assert!(Sampler::start_at(&tree.0).is_none());
    }

    #[test]
    fn resources_serialize_to_the_contract_with_null_limits_kept() {
        let resources = Resources {
            cpu_limit_millicores: None,
            memory_limit_bytes: Some(10),
            usage: Averages {
                cpu_avg_millicores: 1,
                cpu_peak_millicores: 2,
                memory_avg_bytes: 3,
                memory_peak_bytes: 4,
                samples: 5,
            },
        };
        assert_eq!(
            serde_json::to_value(resources).unwrap(),
            serde_json::json!({
                "cpu_limit_millicores": null,
                "memory_limit_bytes": 10,
                "cpu_avg_millicores": 1,
                "cpu_peak_millicores": 2,
                "memory_avg_bytes": 3,
                "memory_peak_bytes": 4,
                "samples": 5,
            })
        );
    }
}
