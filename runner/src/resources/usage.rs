use std::time::Instant;

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reading {
    pub cpu_usec: u64,
    pub memory_bytes: u64,
}

#[derive(Debug, Default)]
pub struct Usage {
    previous: Option<(Instant, u64)>,
    cpu_usec_total: u64,
    wall_usec_total: u128,
    cpu_peak_millicores: u32,
    memory_sum: u128,
    memory_peak: u64,
    samples: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(serde::Deserialize))]
pub struct Averages {
    pub cpu_avg_millicores: u32,
    pub cpu_peak_millicores: u32,
    pub memory_avg_bytes: u64,
    pub memory_peak_bytes: u64,
    pub samples: u32,
}

impl Usage {
    pub fn observe(&mut self, reading: Reading, at: Instant) {
        self.samples = self.samples.saturating_add(1);
        self.memory_sum += u128::from(reading.memory_bytes);
        self.memory_peak = self.memory_peak.max(reading.memory_bytes);
        if let Some((then, cpu_before)) = self.previous {
            self.observe_cpu(
                cpu_before,
                reading.cpu_usec,
                at.saturating_duration_since(then),
            );
        }
        self.previous = Some((at, reading.cpu_usec));
    }

    fn observe_cpu(&mut self, before: u64, after: u64, wall: std::time::Duration) {
        let wall_usec = wall.as_micros();
        let Some(delta) = after.checked_sub(before) else {
            return;
        };
        if wall_usec == 0 {
            return;
        }
        self.cpu_usec_total = self.cpu_usec_total.saturating_add(delta);
        self.wall_usec_total += wall_usec;
        self.cpu_peak_millicores = self
            .cpu_peak_millicores
            .max(millicores(delta.into(), wall_usec));
    }

    pub fn averages(&self) -> Option<Averages> {
        if self.samples == 0 {
            return None;
        }
        let cpu_avg = if self.wall_usec_total == 0 {
            0
        } else {
            millicores(self.cpu_usec_total.into(), self.wall_usec_total)
        };
        let memory_avg = self.memory_sum / u128::from(self.samples);
        Some(Averages {
            cpu_avg_millicores: cpu_avg,
            cpu_peak_millicores: self.cpu_peak_millicores,
            memory_avg_bytes: u64::try_from(memory_avg).unwrap_or(u64::MAX),
            memory_peak_bytes: self.memory_peak,
            samples: self.samples,
        })
    }
}

fn millicores(cpu_usec: u128, wall_usec: u128) -> u32 {
    u32::try_from(cpu_usec * 1000 / wall_usec).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn reading(cpu_usec: u64, memory_bytes: u64) -> Reading {
        Reading {
            cpu_usec,
            memory_bytes,
        }
    }

    #[test]
    fn nothing_observed_reports_nothing() {
        assert_eq!(Usage::default().averages(), None);
    }

    #[test]
    fn a_single_reading_has_memory_but_no_cpu() {
        let mut usage = Usage::default();
        usage.observe(reading(1_000_000, 100), Instant::now());
        let averages = usage.averages().unwrap();
        assert_eq!(averages.cpu_avg_millicores, 0);
        assert_eq!(averages.memory_avg_bytes, 100);
        assert_eq!(averages.samples, 1);
    }

    #[test]
    fn cpu_is_the_counter_delta_over_wall_time() {
        let start = Instant::now();
        let mut usage = Usage::default();
        usage.observe(reading(0, 10), start);
        usage.observe(reading(5_000_000, 30), start + Duration::from_secs(5));
        usage.observe(reading(5_500_000, 20), start + Duration::from_secs(10));
        let averages = usage.averages().unwrap();
        assert_eq!(averages.cpu_peak_millicores, 1000);
        assert_eq!(averages.cpu_avg_millicores, 550);
        assert_eq!(averages.memory_avg_bytes, 20);
        assert_eq!(averages.memory_peak_bytes, 30);
        assert_eq!(averages.samples, 3);
    }

    #[test]
    fn a_counter_reset_skips_that_interval() {
        let start = Instant::now();
        let mut usage = Usage::default();
        usage.observe(reading(4_000_000, 1), start);
        usage.observe(reading(100, 1), start + Duration::from_secs(5));
        usage.observe(reading(1_000_100, 1), start + Duration::from_secs(10));
        let averages = usage.averages().unwrap();
        assert_eq!(averages.cpu_peak_millicores, 200);
        assert_eq!(averages.cpu_avg_millicores, 200);
    }

    #[test]
    fn zero_wall_time_is_ignored_without_dividing() {
        let start = Instant::now();
        let mut usage = Usage::default();
        usage.observe(reading(0, 1), start);
        usage.observe(reading(1_000_000, 1), start);
        let averages = usage.averages().unwrap();
        assert_eq!(averages.cpu_avg_millicores, 0);
        assert_eq!(averages.cpu_peak_millicores, 0);
        assert_eq!(averages.samples, 2);
    }
}
