use arraydeque::{ArrayDeque, Wrapping};
use std::{cell::Cell, time::Duration};

const DEQUE_SIZE: usize = 100;

#[derive(Clone)]
pub struct PingStats<'a> {
    pub region: &'a str,
    latencies: ArrayDeque<f64, DEQUE_SIZE, Wrapping>,
    cached_stats: Cell<Option<CachedStats>>,
}

#[derive(Copy, Clone, Default)]
struct CachedStats {
    min: f64,
    max: f64,
    avg: f64,
    stddev: f64,
    p95: f64,
    p99: f64,
    is_valid: bool,
}

impl<'a> PingStats<'a> {
    pub fn new(region: &'a str) -> Self {
        Self {
            region,
            latencies: ArrayDeque::new(),
            cached_stats: Cell::new(None),
        }
    }

    pub fn add_latency(&mut self, latency: Option<Duration>) {
        if let Some(lat) = latency {
            self.latencies.push_back(lat.as_secs_f64() * 1000.0);
            self.cached_stats.set(None);
        }
    }

    fn calculate_stats(&self) -> CachedStats {
        let len = self.latencies.len();
        if len == 0 {
            return CachedStats::default();
        }

        let mut min = f64::INFINITY;
        let mut max = f64::NEG_INFINITY;
        let mut sum = 0.0;

        // AIDEV-NOTE: Single pass for min/max/sum - zero heap allocation
        for &value in self.latencies.iter() {
            min = min.min(value);
            max = max.max(value);
            sum += value;
        }

        let avg = sum / len as f64;

        // Calculate standard deviation in second pass
        let variance = if len > 1 {
            let mut variance_sum = 0.0;
            for &value in self.latencies.iter() {
                let diff = value - avg;
                variance_sum += diff * diff;
            }
            variance_sum / (len - 1) as f64
        } else {
            0.0
        };
        let stddev = variance.sqrt();

        // Calculate percentiles using stack allocation
        let (p95, p99) = self.calculate_percentiles_efficient();

        CachedStats {
            min,
            max,
            avg,
            stddev,
            p95,
            p99,
            is_valid: true,
        }
    }

    #[inline]
    fn calculate_percentiles_efficient(&self) -> (f64, f64) {
        let len = self.latencies.len();
        if len == 0 {
            return (0.0, 0.0);
        }

        // AIDEV-NOTE: Use stack-allocated array for better cache locality
        let mut sorted: [f64; DEQUE_SIZE] = [0.0; DEQUE_SIZE];
        for (i, &value) in self.latencies.iter().enumerate() {
            sorted[i] = value;
        }

        // Sort only the portion we need
        sorted[..len]
            .sort_unstable_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

        let p95_idx = ((len as f64 * 0.95).ceil() as usize - 1).min(len - 1);
        let p99_idx = ((len as f64 * 0.99).ceil() as usize - 1).min(len - 1);

        (sorted[p95_idx], sorted[p99_idx])
    }

    fn get_stats(&self) -> CachedStats {
        if let Some(stats) = self.cached_stats.get() {
            if stats.is_valid {
                return stats;
            }
        }

        let stats = self.calculate_stats();
        self.cached_stats.set(Some(stats));
        stats
    }

    #[inline]
    pub fn min(&self) -> Option<f64> {
        if self.latencies.is_empty() {
            None
        } else {
            Some(self.get_stats().min)
        }
    }

    #[inline]
    pub fn max(&self) -> Option<f64> {
        if self.latencies.is_empty() {
            None
        } else {
            Some(self.get_stats().max)
        }
    }

    #[inline]
    pub fn avg(&self) -> Option<f64> {
        if self.latencies.is_empty() {
            None
        } else {
            Some(self.get_stats().avg)
        }
    }

    #[inline]
    pub fn stddev(&self) -> Option<f64> {
        if self.latencies.is_empty() {
            None
        } else {
            Some(self.get_stats().stddev)
        }
    }

    #[inline]
    pub fn last(&self) -> Option<f64> {
        self.latencies.back().copied()
    }

    #[inline]
    pub fn p95(&self) -> Option<f64> {
        if self.latencies.is_empty() {
            None
        } else {
            Some(self.get_stats().p95)
        }
    }

    #[inline]
    pub fn p99(&self) -> Option<f64> {
        if self.latencies.is_empty() {
            None
        } else {
            Some(self.get_stats().p99)
        }
    }
}
