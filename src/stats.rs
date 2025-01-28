use arraydeque::{ArrayDeque, Wrapping};
use std::{cell::UnsafeCell, time::Duration};

const DEQUE_SIZE: usize = 100;

pub struct PingStats<'a> {
    pub region: &'a str,
    latencies: ArrayDeque<f64, DEQUE_SIZE, Wrapping>,
    cached_stats: UnsafeCell<Option<CachedStats>>,
}

impl<'a> Clone for PingStats<'a> {
    fn clone(&self) -> Self {
        Self {
            region: self.region,
            latencies: self.latencies.clone(),
            cached_stats: UnsafeCell::new(unsafe { (*self.cached_stats.get()).clone() }),
        }
    }
}

#[derive(Clone, Default)]
struct CachedStats {
    min: Option<f64>,
    max: Option<f64>,
    avg: Option<f64>,
    stddev: Option<f64>,
    p95: Option<f64>,
    p99: Option<f64>,
}

impl<'a> PingStats<'a> {
    pub fn new(region: &'a str) -> Self {
        Self {
            region,
            latencies: ArrayDeque::new(),
            cached_stats: UnsafeCell::new(None),
        }
    }

    pub fn add_latency(&mut self, latency: Option<Duration>) {
        if let Some(lat) = latency {
            self.latencies.push_back(lat.as_secs_f64() * 1000.0);
            unsafe {
                *self.cached_stats.get() = None; // Invalidate cache
            }
        }
    }

    fn calculate_stats(&self) -> CachedStats {
        if self.latencies.is_empty() {
            return CachedStats::default();
        }

        let mut min = f64::INFINITY;
        let mut max = f64::NEG_INFINITY;
        let mut sum = 0.0;
        let mut values = Vec::with_capacity(self.latencies.len());

        for &v in self.latencies.iter() {
            min = min.min(v);
            max = max.max(v);
            sum += v;
            values.push(v);
        }

        let len = values.len();
        let avg = sum / len as f64;

        // Calculate standard deviation
        let variance = values.iter()
            .map(|x| {
                let diff = x - avg;
                diff * diff
            })
            .sum::<f64>() / (len - 1) as f64;
        let stddev = variance.sqrt();

        // Calculate percentiles
        values.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let p95_idx = (len as f64 * 0.95) as usize;
        let p99_idx = (len as f64 * 0.99) as usize;

        CachedStats {
            min: Some(min),
            max: Some(max),
            avg: Some(avg),
            stddev: Some(stddev),
            p95: Some(values[p95_idx.min(len - 1)]),
            p99: Some(values[p99_idx.min(len - 1)]),
        }
    }

    fn get_stats(&self) -> &CachedStats {
        unsafe {
            let cached_stats = &mut *self.cached_stats.get();
            if cached_stats.is_none() {
                *cached_stats = Some(self.calculate_stats());
            }
            cached_stats.as_ref().unwrap()
        }
    }

    pub fn min(&self) -> Option<f64> {
        self.get_stats().min
    }

    pub fn max(&self) -> Option<f64> {
        self.get_stats().max
    }

    pub fn avg(&self) -> Option<f64> {
        self.get_stats().avg
    }

    pub fn stddev(&self) -> Option<f64> {
        self.get_stats().stddev
    }

    pub fn last(&self) -> Option<f64> {
        self.latencies.back().copied()
    }

    pub fn p95(&self) -> Option<f64> {
        self.get_stats().p95
    }

    pub fn p99(&self) -> Option<f64> {
        self.get_stats().p99
    }
}

