use std::cmp::Ordering;

use crate::StatsSnapshot;

const CAPACITY: usize = 128;

#[derive(Clone, Copy, Default)]
struct PercentileCache {
    p95: f64,
    p99: f64,
}

pub struct PingStats {
    pub region: &'static str,
    buffer: [f64; CAPACITY],
    len: usize,
    head: usize,
    running_min: f64,
    running_max: f64,
    min_max_valid: bool,
    last_value: Option<f64>,
    percentile_cache: PercentileCache,
    percentile_valid: bool,
    total_samples: u64,
}

impl PingStats {
    pub fn new(region: &'static str) -> Self {
        Self {
            region,
            buffer: [0.0; CAPACITY],
            len: 0,
            head: 0,
            running_min: f64::INFINITY,
            running_max: -f64::INFINITY,
            min_max_valid: true,
            last_value: None,
            percentile_cache: PercentileCache::default(),
            percentile_valid: false,
            total_samples: 0,
        }
    }

    pub fn add_sample(&mut self, latency_ms: Option<f64>) {
        self.last_value = latency_ms;
        let value = match latency_ms {
            Some(v) => v,
            None => return,
        };

        if self.len == CAPACITY {
            let evicted = self.buffer[self.head];
            if evicted <= self.running_min || evicted >= self.running_max {
                self.min_max_valid = false;
            }
        }

        self.buffer[self.head] = value;
        self.head = (self.head + 1) & (CAPACITY - 1);
        if self.len < CAPACITY {
            self.len += 1;
        }

        if value < self.running_min {
            self.running_min = value;
        }
        if value > self.running_max {
            self.running_max = value;
        }

        self.percentile_valid = false;
        self.total_samples = self.total_samples.saturating_add(1);
    }

    pub fn last(&self) -> Option<f64> {
        self.last_value
    }

    pub fn avg(&self) -> Option<f64> {
        if self.len == 0 {
            return None;
        }
        Some(self.buffer[..self.len].iter().sum::<f64>() / self.len as f64)
    }

    pub fn stddev(&self) -> Option<f64> {
        if self.len < 2 {
            return None;
        }
        let mean = self.avg()?;
        let variance = self.buffer[..self.len]
            .iter()
            .map(|value| (value - mean).powi(2))
            .sum::<f64>()
            / (self.len - 1) as f64;
        Some(variance.sqrt())
    }

    pub fn min(&mut self) -> Option<f64> {
        if self.len == 0 {
            return None;
        }
        if !self.min_max_valid {
            self.recompute_min_max();
        }
        Some(self.running_min)
    }

    pub fn max(&mut self) -> Option<f64> {
        if self.len == 0 {
            return None;
        }
        if !self.min_max_valid {
            self.recompute_min_max();
        }
        Some(self.running_max)
    }

    pub fn p95(&mut self) -> Option<f64> {
        if self.len == 0 {
            return None;
        }
        if !self.percentile_valid {
            self.recompute_percentiles();
        }
        Some(self.percentile_cache.p95)
    }

    pub fn p99(&mut self) -> Option<f64> {
        if self.len == 0 {
            return None;
        }
        if !self.percentile_valid {
            self.recompute_percentiles();
        }
        Some(self.percentile_cache.p99)
    }

    pub fn snapshot(&mut self) -> StatsSnapshot {
        StatsSnapshot {
            region: self.region,
            last: self.last(),
            min: self.min(),
            avg: self.avg(),
            max: self.max(),
            stddev: self.stddev(),
            p95: self.p95(),
            p99: self.p99(),
            samples: self.total_samples,
        }
    }

    fn recompute_min_max(&mut self) {
        let mut min_val = f64::INFINITY;
        let mut max_val = -f64::INFINITY;
        let start = (self.head + CAPACITY - self.len) & (CAPACITY - 1);
        for i in 0..self.len {
            let idx = (start + i) & (CAPACITY - 1);
            let val = self.buffer[idx];
            if val < min_val {
                min_val = val;
            }
            if val > max_val {
                max_val = val;
            }
        }
        self.running_min = min_val;
        self.running_max = max_val;
        self.min_max_valid = true;
    }

    fn recompute_percentiles(&mut self) {
        if self.len == 0 {
            return;
        }

        let mut scratch = [0.0_f64; CAPACITY];
        let start = (self.head + CAPACITY - self.len) & (CAPACITY - 1);
        for (i, value) in scratch.iter_mut().take(self.len).enumerate() {
            let idx = (start + i) & (CAPACITY - 1);
            *value = self.buffer[idx];
        }

        let values = &mut scratch[..self.len];
        let p95_idx = percentile_index(self.len, 95, 100);
        let p99_idx = percentile_index(self.len, 99, 100);

        let (p95, p99) = if p95_idx == p99_idx {
            let (_, value, _) = values.select_nth_unstable_by(p99_idx, compare_latency);
            (*value, *value)
        } else {
            let (left, p99, _) = values.select_nth_unstable_by(p99_idx, compare_latency);
            let p99_value = *p99;
            let (_, p95, _) = left.select_nth_unstable_by(p95_idx, compare_latency);
            (*p95, p99_value)
        };

        self.percentile_cache = PercentileCache { p95, p99 };
        self.percentile_valid = true;
    }
}

fn percentile_index(len: usize, numerator: usize, denominator: usize) -> usize {
    if len == 0 {
        return 0;
    }

    let rank = len.saturating_mul(numerator).div_ceil(denominator);
    rank.saturating_sub(1).min(len - 1)
}

fn compare_latency(lhs: &f64, rhs: &f64) -> Ordering {
    lhs.partial_cmp(rhs).unwrap_or(Ordering::Equal)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_close(actual: Option<f64>, expected: f64) {
        let actual = actual.expect("expected a measurement");
        assert!((actual - expected).abs() < 1e-9, "{actual} != {expected}");
    }

    #[test]
    fn empty_and_single_sample_statistics() {
        let mut stats = PingStats::new("test");
        let empty = stats.snapshot();
        assert_eq!(empty.last, None);
        assert_eq!(empty.min, None);
        assert_eq!(empty.avg, None);
        assert_eq!(empty.max, None);
        assert_eq!(empty.stddev, None);
        assert_eq!(empty.p95, None);
        assert_eq!(empty.p99, None);
        assert_eq!(empty.samples, 0);

        stats.add_sample(Some(12.5));
        let single = stats.snapshot();
        for metric in [
            single.last,
            single.min,
            single.avg,
            single.max,
            single.p95,
            single.p99,
        ] {
            assert_eq!(metric, Some(12.5));
        }
        assert_eq!(single.stddev, None);
        assert_eq!(single.samples, 1);
    }

    #[test]
    fn discarded_samples_do_not_affect_any_statistic() {
        let mut stats = PingStats::new("test");
        for _ in 0..128 {
            stats.add_sample(Some(100.0));
        }
        for _ in 0..128 {
            stats.add_sample(Some(10.0));
        }
        let snapshot = stats.snapshot();
        for metric in [
            snapshot.min,
            snapshot.avg,
            snapshot.max,
            snapshot.p95,
            snapshot.p99,
        ] {
            assert_close(metric, 10.0);
        }
        assert_close(snapshot.stddev, 0.0);
        assert_eq!(snapshot.samples, 256);
    }

    #[test]
    fn statistics_follow_the_window_through_repeated_wraps() {
        let mut stats = PingStats::new("test");
        for value in 1..=512 {
            stats.add_sample(Some(f64::from(value)));
            let snapshot = stats.snapshot();
            let count = value.min(128);
            let min = f64::from(value - count + 1);
            assert_close(snapshot.min, min);
            assert_close(snapshot.max, f64::from(value));
            assert_close(snapshot.avg, (min + f64::from(value)) / 2.0);
            assert_close(snapshot.p95, min + (f64::from(count) * 0.95).ceil() - 1.0);
            assert_close(snapshot.p99, min + (f64::from(count) * 0.99).ceil() - 1.0);
            if count > 1 {
                assert_close(
                    snapshot.stddev,
                    (f64::from(count * (count + 1)) / 12.0).sqrt(),
                );
            }
            assert_eq!(snapshot.samples, value as u64);
        }
    }

    #[test]
    fn failure_clears_last_and_recovery_preserves_successful_history() {
        let mut stats = PingStats::new("test");
        stats.add_sample(Some(10.0));
        stats.add_sample(None);
        stats.add_sample(None);
        let failed = stats.snapshot();
        assert_eq!(failed.last, None);
        assert_eq!(failed.avg, Some(10.0));
        assert_eq!(failed.samples, 1);

        stats.add_sample(Some(20.0));
        let recovered = stats.snapshot();
        assert_eq!(recovered.last, Some(20.0));
        assert_eq!(recovered.avg, Some(15.0));
        assert_eq!(recovered.samples, 2);
    }
}
