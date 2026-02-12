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
    welford_count: u64,
    welford_mean: f64,
    welford_m2: f64,
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
            welford_count: 0,
            welford_mean: 0.0,
            welford_m2: 0.0,
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

        self.welford_count = self.welford_count.saturating_add(1);
        let delta = value - self.welford_mean;
        self.welford_mean += delta / self.welford_count as f64;
        let delta2 = value - self.welford_mean;
        self.welford_m2 += delta * delta2;

        if value < self.running_min {
            self.running_min = value;
        }
        if value > self.running_max {
            self.running_max = value;
        }

        self.last_value = Some(value);
        self.percentile_valid = false;
        self.total_samples = self.total_samples.saturating_add(1);
    }

    pub fn last(&self) -> Option<f64> {
        self.last_value
    }

    pub fn total_samples(&self) -> u64 {
        self.total_samples
    }

    pub fn avg(&self) -> Option<f64> {
        if self.welford_count == 0 {
            return None;
        }
        Some(self.welford_mean)
    }

    pub fn stddev(&self) -> Option<f64> {
        if self.welford_count < 2 {
            return None;
        }
        let variance = self.welford_m2 / (self.welford_count - 1) as f64;
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
        for i in 0..self.len {
            let idx = (start + i) & (CAPACITY - 1);
            scratch[i] = self.buffer[idx];
        }

        let values = &mut scratch[..self.len];
        values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

        self.percentile_cache = PercentileCache {
            p95: values[percentile_index(self.len, 0.95)],
            p99: values[percentile_index(self.len, 0.99)],
        };
        self.percentile_valid = true;
    }
}

fn percentile_index(len: usize, percentile: f64) -> usize {
    if len == 0 {
        return 0;
    }

    let count = len as f64;
    let rank = (count * percentile).ceil();
    let raw_index = rank as usize;
    let mut idx = if rank <= 1.0 { 0 } else { raw_index - 1 };
    if idx >= len {
        idx = len - 1;
    }
    idx
}
