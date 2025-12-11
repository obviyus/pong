const std = @import("std");
const vaxis = @import("vaxis");

pub const std_options = std.Options{
    .log_level = .info,
};

const regions = @import("regions.zig");
const stats = @import("stats.zig");
const renderer = @import("render.zig");

const time = std.time;
const retry_delay_ns: u64 = 500 * time.ns_per_ms;

const log = std.log.scoped(.pong);

const PingStats = stats.PingStats;
const Region = regions.Region;

const EventLoop = vaxis.Loop(Event);

const Event = union(enum) {
    key_press: vaxis.Key,
    winsize: vaxis.Winsize,
    data_update,
};

const StatsSnapshot = struct {
    region: []const u8,
    last: ?f64,
    min: ?f64,
    avg: ?f64,
    max: ?f64,
    stddev: ?f64,
    p95: ?f64,
    p99: ?f64,
    samples: u64,
};

const SharedStat = struct {
    snapshots: [2]StatsSnapshot,
    current: std.atomic.Value(u8) = std.atomic.Value(u8).init(0),
    region: []const u8,

    pub fn init(region: []const u8) SharedStat {
        const empty_snapshot = StatsSnapshot{
            .region = region,
            .last = null,
            .min = null,
            .avg = null,
            .max = null,
            .stddev = null,
            .p95 = null,
            .p99 = null,
            .samples = 0,
        };
        return .{
            .snapshots = .{ empty_snapshot, empty_snapshot },
            .region = region,
        };
    }

    pub fn publish(self: *SharedStat, data: *PingStats) void {
        const current_idx = self.current.load(.acquire);
        const write_idx: u8 = 1 - current_idx;

        self.snapshots[write_idx] = StatsSnapshot{
            .region = data.region,
            .last = data.last(),
            .min = data.min(),
            .avg = data.avg(),
            .max = data.max(),
            .stddev = data.stddev(),
            .p95 = data.p95(),
            .p99 = data.p99(),
            .samples = data.totalSamples(),
        };

        self.current.store(write_idx, .release);
    }

    pub fn read(self: *const SharedStat) StatsSnapshot {
        const idx = self.current.load(.acquire);
        return self.snapshots[idx];
    }

    pub fn readAvg(self: *const SharedStat) ?f64 {
        const idx = self.current.load(.acquire);
        return self.snapshots[idx].avg;
    }
};

const WorkerContext = struct {
    region: Region,
    stats: *SharedStat,
    shutdown: *std.atomic.Value(bool),
    collect: *std.atomic.Value(bool),
    loop: *EventLoop,
};

pub fn main() !void {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    defer {
        const leak = gpa.deinit();
        if (leak == .leak) log.err("memory leak detected", .{});
    }
    const allocator = gpa.allocator();

    const warmup_ns = parseWarmup(allocator) catch |err| switch (err) {
        error.InvalidArgument => {
            std.process.exit(1);
        },
    };
    const warmup_total_seconds: u64 = if (warmup_ns == 0)
        0
    else
        (warmup_ns + time.ns_per_s - 1) / time.ns_per_s;
    var warmup_timer = try time.Timer.start();
    var warmup_ready = warmup_ns == 0;
    var collect_samples = std.atomic.Value(bool).init(warmup_ready);

    var tty_buffer: [1024]u8 = undefined;
    var tty = try vaxis.Tty.init(&tty_buffer);
    defer tty.deinit();

    var vx = try vaxis.init(allocator, .{});
    defer vx.deinit(allocator, tty.writer());

    var loop: EventLoop = .{ .tty = &tty, .vaxis = &vx };
    try loop.init();
    try loop.start();
    defer loop.stop();

    try vx.enterAltScreen(tty.writer());
    try vx.queryTerminal(tty.writer(), 250 * time.ns_per_ms);

    var shutdown = std.atomic.Value(bool).init(false);

    var shared_stats_storage: [regions.region_count]SharedStat = undefined;
    const shared_stats = shared_stats_storage[0..];

    var worker_threads: [regions.region_count]std.Thread = undefined;
    var worker_count: usize = 0;

    defer {
        shutdown.store(true, .seq_cst);
        var i: usize = 0;
        while (i < worker_count) : (i += 1) {
            worker_threads[i].join();
        }
    }

    for (shared_stats, regions.REGIONS_LIST, 0..) |*shared, region, idx| {
        shared.* = SharedStat.init(region.name);
        const ctx = WorkerContext{
            .region = region,
            .stats = shared,
            .shutdown = &shutdown,
            .collect = &collect_samples,
            .loop = &loop,
        };
        const thread = try std.Thread.spawn(.{}, pingWorker, .{ctx});
        worker_threads[idx] = thread;
        worker_count = idx + 1;
    }

    loop.postEvent(.data_update);

    var sorted_index_buffer: [regions.region_count]usize = undefined;

    var running = true;
    while (running) {
        const event = loop.nextEvent();
        var needs_render = switch (event) {
            .key_press => |key| blk: {
                const quit = key.matches('q', .{}) or key.matches('c', .{ .ctrl = true });
                if (quit) running = false;
                break :blk true;
            },
            .winsize => |ws| blk: {
                try vx.resize(allocator, tty.writer(), ws);
                break :blk true;
            },
            .data_update => true,
        };

        if (!warmup_ready) {
            needs_render = true;
        }

        if (!needs_render or !running) continue;

        const elapsed_ns = warmup_timer.read();
        if (!warmup_ready) {
            if (elapsed_ns >= warmup_ns) {
                warmup_ready = true;
                collect_samples.store(true, .release);
            } else {
                const remaining_ns = warmup_ns - elapsed_ns;
                try renderer.renderWarmup(&vx, &tty, elapsed_ns, remaining_ns, warmup_total_seconds);

                const spinner_sleep_ns: u64 = 150 * time.ns_per_ms;
                const sleep_ns = if (remaining_ns > spinner_sleep_ns) spinner_sleep_ns else remaining_ns;
                if (sleep_ns > 0) {
                    std.Thread.sleep(sleep_ns);
                }

                notify(&loop);
                continue;
            }
        }

        const sorted_indices = renderer.collectSortedIndices(SharedStat, shared_stats, sorted_index_buffer[0..]);
        try renderer.render(SharedStat, &vx, &tty, shared_stats, sorted_indices);
    }
}

fn notify(loop: *EventLoop) void {
    if (!loop.tryPostEvent(.data_update)) {
        loop.postEvent(.data_update);
    }
}

fn pingWorker(ctx: WorkerContext) void {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    defer _ = gpa.deinit();

    var client = std.http.Client{ .allocator = gpa.allocator() };
    defer client.deinit();

    const uri = std.Uri.parse(ctx.region.url) catch |err| {
        log.err("invalid region url {s}: {s}", .{ ctx.region.url, @errorName(err) });
        return;
    };

    var redirect_buf: [2048]u8 = undefined;
    var local_stats = PingStats.init(ctx.region.name);

    while (!ctx.shutdown.load(.acquire)) {
        const measurement = takeMeasurement(&ctx, &client, uri, &redirect_buf);

        if (ctx.collect.load(.acquire)) {
            local_stats.addSample(measurement);
            ctx.stats.publish(&local_stats);
            notify(ctx.loop);
        }

        sleepWithShutdown(ctx.shutdown, time.ns_per_s);
    }
}

fn takeMeasurement(
    ctx: *const WorkerContext,
    client: *std.http.Client,
    uri: std.Uri,
    redirect_buf: *[2048]u8,
) ?f64 {
    var retries: u8 = 3;
    while (retries > 0 and !ctx.shutdown.load(.acquire)) : (retries -= 1) {
        const result = pingOnce(client, uri, redirect_buf) catch |err| blk: {
            log.debug("ping failed for {s}: {s}", .{ ctx.region.name, @errorName(err) });
            break :blk null;
        };
        if (result) |value| return value;
        sleepWithShutdown(ctx.shutdown, retry_delay_ns);
    }
    return null;
}

fn pingOnce(
    client: *std.http.Client,
    uri: std.Uri,
    redirect_buf: *[2048]u8,
) !?f64 {
    var request = try client.request(.HEAD, uri, .{
        .headers = .{
            .user_agent = .{ .override = "pong" },
        },
        .redirect_behavior = .unhandled,
    });
    defer request.deinit();

    try request.sendBodiless();

    var timer = try time.Timer.start();

    _ = try request.receiveHead(redirect_buf);

    const elapsed_ns = timer.read();
    return @as(f64, @floatFromInt(elapsed_ns)) / @as(f64, @floatFromInt(time.ns_per_ms));
}

fn sleepWithShutdown(flag: *std.atomic.Value(bool), total_ns: u64) void {
    const quantum: u64 = 25 * time.ns_per_ms;
    var remaining = total_ns;
    while (remaining > 0 and !flag.load(.acquire)) {
        const step = if (remaining < quantum) remaining else quantum;
        std.Thread.sleep(step);
        remaining -= step;
    }
}

fn parseWarmup(allocator: std.mem.Allocator) !u64 {
    var args = try std.process.argsWithAllocator(allocator);
    defer args.deinit();

    const program_name = args.next() orelse "pong";

    var warmup_ns: u64 = 0;

    while (args.next()) |arg| {
        if (std.mem.eql(u8, arg, "--warmup")) {
            const value = args.next() orelse {
                log.err("--warmup expects a time in seconds", .{});
                return error.InvalidArgument;
            };
            const seconds = std.fmt.parseUnsigned(u64, value, 10) catch |parse_err| {
                log.err("invalid --warmup value '{s}': {s}", .{ value, @errorName(parse_err) });
                return error.InvalidArgument;
            };
            warmup_ns = std.math.mul(u64, seconds, time.ns_per_s) catch {
                log.err("--warmup value too large", .{});
                return error.InvalidArgument;
            };
        } else if (std.mem.eql(u8, arg, "--help")) {
            showHelp(program_name);
            std.process.exit(0);
        } else {
            log.err("unrecognized argument '{s}'", .{arg});
            return error.InvalidArgument;
        }
    }

    return warmup_ns;
}

fn showHelp(program_name: []const u8) void {
    std.debug.print(
        \\Usage: {s} [--warmup <seconds>] [--help]
        \\
        \\Options:
        \\  --warmup <seconds>  delay rendering to allow initial pings to settle
        \\  --help              display this help and exit
        \\
    , .{program_name});
}
