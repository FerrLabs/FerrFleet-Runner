const UNLIMITED_FLOOR: u64 = 1 << 60;

pub fn cpu_limit_v2(cpu_max: &str) -> Option<u32> {
    let mut fields = cpu_max.split_whitespace();
    let quota = fields.next()?.parse::<u64>().ok()?;
    let period = fields.next()?.parse::<u64>().ok()?;
    millicores(quota, period)
}

pub fn cpu_limit_v1(quota: &str, period: &str) -> Option<u32> {
    let quota = quota.trim().parse::<u64>().ok()?;
    let period = period.trim().parse::<u64>().ok()?;
    millicores(quota, period)
}

fn millicores(quota: u64, period: u64) -> Option<u32> {
    if period == 0 {
        return None;
    }
    let millicores = quota.saturating_mul(1000) / period;
    u32::try_from(millicores).ok().filter(|m| *m > 0)
}

pub fn memory_limit(raw: &str) -> Option<u64> {
    let bytes = raw.trim().parse::<u64>().ok()?;
    (bytes < UNLIMITED_FLOOR).then_some(bytes)
}

pub fn cpu_usage_usec_v2(cpu_stat: &str) -> Option<u64> {
    cpu_stat.lines().find_map(|line| {
        let (key, value) = line.split_once(' ')?;
        (key == "usage_usec").then(|| value.trim().parse().ok())?
    })
}

pub fn cpu_usage_usec_v1(cpuacct_usage_nanos: &str) -> Option<u64> {
    cpuacct_usage_nanos
        .trim()
        .parse::<u64>()
        .ok()
        .map(|nanos| nanos / 1000)
}

pub fn memory_bytes(raw: &str) -> Option<u64> {
    raw.trim().parse().ok()
}

pub fn host_memory_total(meminfo: &str) -> Option<u64> {
    let kib = meminfo.lines().find_map(|line| {
        let rest = line.strip_prefix("MemTotal:")?;
        rest.split_whitespace().next()?.parse::<u64>().ok()
    })?;
    kib.checked_mul(1024)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v2_cpu_max_with_a_quota_is_millicores() {
        assert_eq!(cpu_limit_v2("200000 100000\n"), Some(2000));
        assert_eq!(cpu_limit_v2("50000 100000"), Some(500));
    }

    #[test]
    fn v2_cpu_max_without_a_quota_is_unlimited() {
        assert_eq!(cpu_limit_v2("max 100000\n"), None);
    }

    #[test]
    fn v2_cpu_max_garbage_or_zero_period_is_unlimited() {
        assert_eq!(cpu_limit_v2(""), None);
        assert_eq!(cpu_limit_v2("200000"), None);
        assert_eq!(cpu_limit_v2("200000 0"), None);
        assert_eq!(cpu_limit_v2("0 100000"), None);
    }

    #[test]
    fn v1_quota_of_minus_one_is_unlimited() {
        assert_eq!(cpu_limit_v1("-1\n", "100000\n"), None);
        assert_eq!(cpu_limit_v1("150000\n", "100000\n"), Some(1500));
    }

    #[test]
    fn memory_limit_max_and_the_v1_sentinel_are_unlimited() {
        assert_eq!(memory_limit("max\n"), None);
        assert_eq!(memory_limit("9223372036854771712\n"), None);
        assert_eq!(memory_limit("536870912\n"), Some(536_870_912));
    }

    #[test]
    fn usage_usec_is_read_from_the_v2_cpu_stat() {
        let stat = "usage_usec 12345\nuser_usec 10000\nsystem_usec 2345\n";
        assert_eq!(cpu_usage_usec_v2(stat), Some(12345));
    }

    #[test]
    fn usage_usec_is_not_confused_with_user_usec() {
        assert_eq!(cpu_usage_usec_v2("user_usec 5\nsystem_usec 6\n"), None);
        assert_eq!(cpu_usage_usec_v2(""), None);
    }

    #[test]
    fn v1_cpuacct_nanoseconds_become_microseconds() {
        assert_eq!(cpu_usage_usec_v1("5000000\n"), Some(5000));
        assert_eq!(cpu_usage_usec_v1("nope"), None);
    }

    #[test]
    fn memory_bytes_rejects_non_numbers() {
        assert_eq!(memory_bytes("4096\n"), Some(4096));
        assert_eq!(memory_bytes("max"), None);
    }

    #[test]
    fn meminfo_total_is_converted_from_kib() {
        let meminfo = "MemFree:  100 kB\nMemTotal:       16384 kB\nBuffers: 1 kB\n";
        assert_eq!(host_memory_total(meminfo), Some(16_777_216));
        assert_eq!(host_memory_total("MemFree: 1 kB\n"), None);
    }
}
