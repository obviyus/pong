const std = @import("std");

const math = std.math;
const sort = std.sort;

pub const PingStats = struct {
    pub const capacity = 100;

    region: []const u8,
    buffer: [capacity]f64 = undefined,
    len: usize = 0,
    head: usize = 0,
    cached: CachedStats = .{},
    cache_valid: bool = false,

    pub fn init(region: []const u8) PingStats {
        return .{
            .region = region,
            .buffer = undefined,
        };
    }

    pub fn addSample(self: *PingStats, latency_ms: ?f64) void {
        if (latency_ms) |value| {
            self.buffer[self.head] = value;
            self.head = (self.head + 1) % capacity;
            if (self.len < capacity) {
                self.len += 1;
            }
            self.cache_valid = false;
        }
    }

    pub fn last(self: *const PingStats) ?f64 {
        if (self.len == 0) return null;
        const index = if (self.head == 0) capacity - 1 else self.head - 1;
        return self.buffer[index];
    }

    pub fn min(self: *PingStats) ?f64 {
        if (self.len == 0) return null;
        return self.ensureStats().min;
    }

    pub fn max(self: *PingStats) ?f64 {
        if (self.len == 0) return null;
        return self.ensureStats().max;
    }

    pub fn avg(self: *PingStats) ?f64 {
        if (self.len == 0) return null;
        return self.ensureStats().avg;
    }

    pub fn stddev(self: *PingStats) ?f64 {
        if (self.len == 0) return null;
        return self.ensureStats().stddev;
    }

    pub fn p95(self: *PingStats) ?f64 {
        if (self.len == 0) return null;
        return self.ensureStats().p95;
    }

    pub fn p99(self: *PingStats) ?f64 {
        if (self.len == 0) return null;
        return self.ensureStats().p99;
    }

    fn ensureStats(self: *PingStats) CachedStats {
        if (!self.cache_valid) {
            self.cached = self.computeStats();
            self.cache_valid = true;
        }
        return self.cached;
    }

    fn computeStats(self: *const PingStats) CachedStats {
        if (self.len == 0) return .{};

        var scratch: [capacity]f64 = undefined;
        const values = self.copyValues(&scratch);

        var min_val = math.inf(f64);
        var max_val = -math.inf(f64);
        var sum: f64 = 0.0;

        for (values) |value| {
            min_val = @min(min_val, value);
            max_val = @max(max_val, value);
            sum += value;
        }

        const count = @as(f64, @floatFromInt(values.len));
        const avg_val = sum / count;

        var variance_sum: f64 = 0.0;
        if (values.len > 1) {
            for (values) |value| {
                const diff = value - avg_val;
                variance_sum += diff * diff;
            }
        }
        const variance = if (values.len > 1)
            variance_sum / @as(f64, @floatFromInt(values.len - 1))
        else
            0.0;

        sort.heap(f64, values, {}, sort.asc(f64));
        const idx95 = percentileIndex(values.len, 0.95);
        const idx99 = percentileIndex(values.len, 0.99);

        return .{
            .min = min_val,
            .max = max_val,
            .avg = avg_val,
            .stddev = math.sqrt(variance),
            .p95 = values[idx95],
            .p99 = values[idx99],
        };
    }

    fn copyValues(self: *const PingStats, dest: *[capacity]f64) []f64 {
        var out_index: usize = 0;
        if (self.len == 0) return dest[0..0];

        const start = (self.head + capacity - self.len) % capacity;
        var idx = start;
        while (out_index < self.len) : (out_index += 1) {
            dest[out_index] = self.buffer[idx];
            idx = (idx + 1) % capacity;
        }
        return dest[0..self.len];
    }
};

const CachedStats = struct {
    min: f64 = 0.0,
    max: f64 = 0.0,
    avg: f64 = 0.0,
    stddev: f64 = 0.0,
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
