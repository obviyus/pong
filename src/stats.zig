const std = @import("std");

const math = std.math;
const sort = std.sort;

pub const PingStats = struct {
    pub const capacity = 128;

    region: []const u8,
    buffer: [capacity]f64 = undefined,
    len: usize = 0,
    head: usize = 0,

    welford_count: u64 = 0,
    welford_mean: f64 = 0.0,
    welford_m2: f64 = 0.0,

    running_min: f64 = math.inf(f64),
    running_max: f64 = -math.inf(f64),
    min_max_valid: bool = true,

    last_value: ?f64 = null,

    percentile_cache: PercentileCache = .{},
    percentile_valid: bool = false,

    total_samples: u64 = 0,

    pub fn init(region: []const u8) PingStats {
        return .{
            .region = region,
        };
    }

    pub fn addSample(self: *PingStats, latency_ms: ?f64) void {
        const value = latency_ms orelse return;

        if (self.len == capacity) {
            const evicted = self.buffer[self.head];
            if (evicted <= self.running_min or evicted >= self.running_max) {
                self.min_max_valid = false;
            }
        }

        self.buffer[self.head] = value;
        self.head = (self.head + 1) & (capacity - 1);
        if (self.len < capacity) {
            self.len += 1;
        }

        self.welford_count +|= 1;
        const delta = value - self.welford_mean;
        self.welford_mean += delta / @as(f64, @floatFromInt(self.welford_count));
        const delta2 = value - self.welford_mean;
        self.welford_m2 += delta * delta2;

        if (value < self.running_min) self.running_min = value;
        if (value > self.running_max) self.running_max = value;

        self.last_value = value;
        self.percentile_valid = false;
        self.total_samples +|= 1;
    }

    pub fn last(self: *const PingStats) ?f64 {
        return self.last_value;
    }

    pub fn totalSamples(self: *const PingStats) u64 {
        return self.total_samples;
    }

    pub fn avg(self: *const PingStats) ?f64 {
        if (self.welford_count == 0) return null;
        return self.welford_mean;
    }

    pub fn stddev(self: *const PingStats) ?f64 {
        if (self.welford_count < 2) return null;
        const variance = self.welford_m2 / @as(f64, @floatFromInt(self.welford_count - 1));
        return math.sqrt(variance);
    }

    pub fn min(self: *PingStats) ?f64 {
        if (self.len == 0) return null;
        if (!self.min_max_valid) self.recomputeMinMax();
        return self.running_min;
    }

    pub fn max(self: *PingStats) ?f64 {
        if (self.len == 0) return null;
        if (!self.min_max_valid) self.recomputeMinMax();
        return self.running_max;
    }

    pub fn p95(self: *PingStats) ?f64 {
        if (self.len == 0) return null;
        if (!self.percentile_valid) self.recomputePercentiles();
        return self.percentile_cache.p95;
    }

    pub fn p99(self: *PingStats) ?f64 {
        if (self.len == 0) return null;
        if (!self.percentile_valid) self.recomputePercentiles();
        return self.percentile_cache.p99;
    }

    fn recomputeMinMax(self: *PingStats) void {
        var min_val = math.inf(f64);
        var max_val = -math.inf(f64);

        const start = (self.head + capacity - self.len) & (capacity - 1);
        var i: usize = 0;
        while (i < self.len) : (i += 1) {
            const idx = (start + i) & (capacity - 1);
            const val = self.buffer[idx];
            if (val < min_val) min_val = val;
            if (val > max_val) max_val = val;
        }

        self.running_min = min_val;
        self.running_max = max_val;
        self.min_max_valid = true;
    }

    fn recomputePercentiles(self: *PingStats) void {
        if (self.len == 0) return;

        var scratch: [capacity]f64 = undefined;
        const start = (self.head + capacity - self.len) & (capacity - 1);
        var i: usize = 0;
        while (i < self.len) : (i += 1) {
            const idx = (start + i) & (capacity - 1);
            scratch[i] = self.buffer[idx];
        }
        const values = scratch[0..self.len];

        sort.heap(f64, values, {}, sort.asc(f64));

        self.percentile_cache = .{
            .p95 = values[percentileIndex(self.len, 0.95)],
            .p99 = values[percentileIndex(self.len, 0.99)],
        };
        self.percentile_valid = true;
    }
};

const PercentileCache = struct {
    p95: f64 = 0.0,
    p99: f64 = 0.0,
};

fn percentileIndex(len: usize, percentile: f64) usize {
    if (len == 0) return 0;
    const count = @as(f64, @floatFromInt(len));
    const rank = math.ceil(count * percentile);
    const raw_index: usize = @intFromFloat(rank);
    var idx: usize = if (rank <= 1.0) 0 else raw_index - 1;
    if (idx >= len) idx = len - 1;
    return idx;
}
