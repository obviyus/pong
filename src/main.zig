const std = @import("std");
const vaxis = @import("vaxis");

const regions = @import("regions.zig");
const stats = @import("stats.zig");
const renderer = @import("render.zig");

const time = std.time;

const log = std.log.scoped(.pong);

const PingStats = stats.PingStats;
const Region = regions.Region;

const EventLoop = vaxis.Loop(Event);

const Event = union(enum) {
    key_press: vaxis.Key,
    winsize: vaxis.Winsize,
    data_update,
};

const SharedStat = struct {
    mutex: std.Thread.Mutex = .{},
    data: PingStats,
};

const WorkerContext = struct {
    region: Region,
    stats: *SharedStat,
    shutdown: *std.atomic.Value(bool),
    loop: *EventLoop,
    allocator: std.mem.Allocator,
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
    var warmup_timer = try time.Timer.start();
    var warmup_ready = warmup_ns == 0;

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
    for (regions.REGIONS_LIST, 0..) |region, idx| {
        shared_stats_storage[idx] = .{ .data = PingStats.init(region.name) };
    }

    var worker_threads: [regions.region_count]std.Thread = undefined;
    var worker_count: usize = 0;

    defer {
        shutdown.store(true, .seq_cst);
        var i: usize = 0;
        while (i < worker_count) : (i += 1) {
            worker_threads[i].join();
        }
    }

    for (regions.REGIONS_LIST, 0..) |region, idx| {
        const ctx = WorkerContext{
            .region = region,
            .stats = &shared_stats_storage[idx],
            .shutdown = &shutdown,
            .loop = &loop,
            .allocator = allocator,
        };
        const thread = try std.Thread.spawn(.{}, pingWorker, .{ctx});
        worker_threads[worker_count] = thread;
        worker_count += 1;
    }

    loop.postEvent(.data_update);

    var sorted_index_buffer: [regions.region_count]usize = undefined;

    var running = true;
    while (running) {
        const event = loop.nextEvent();
        var needs_render = false;

        switch (event) {
            .key_press => |key| {
                needs_render = true;
                if (key.matches('q', .{}) or key.matches('c', .{ .ctrl = true })) {
                    running = false;
                }
            },
            .winsize => |ws| {
                try vx.resize(allocator, tty.writer(), ws);
                needs_render = true;
            },
            .data_update => needs_render = true,
        }

        if (!warmup_ready and warmup_timer.read() >= warmup_ns) {
            warmup_ready = true;
            needs_render = true;
        }

        if (needs_render and running and warmup_ready) {
            const sorted_indices = renderer.collectSortedIndices(SharedStat, shared_stats, sorted_index_buffer[0..]);
            try renderer.render(SharedStat, &vx, &tty, shared_stats, sorted_indices);
        }
    }
}

fn pingWorker(ctx: WorkerContext) void {
    var client = std.http.Client{ .allocator = ctx.allocator };
    defer client.deinit();

    const uri = std.Uri.parse(ctx.region.url) catch |err| {
        log.err("invalid region url {s}: {s}", .{ ctx.region.url, @errorName(err) });
        return;
    };

    var redirect_buf: [2048]u8 = undefined;

    while (!ctx.shutdown.load(.acquire)) {
        var retries: u8 = 3;
        var measurement: ?f64 = null;
        while (retries > 0 and measurement == null and !ctx.shutdown.load(.acquire)) : (retries -= 1) {
            measurement = pingOnce(&client, uri, &redirect_buf) catch |err| blk: {
                log.debug("ping failed for {s}: {s}", .{ ctx.region.name, @errorName(err) });
                break :blk null;
            };
            if (measurement == null) {
                sleepWithShutdown(ctx.shutdown, 500 * time.ns_per_ms);
            }
        }

        ctx.stats.mutex.lock();
        ctx.stats.data.addSample(measurement);
        ctx.stats.mutex.unlock();

        if (!ctx.loop.tryPostEvent(.data_update)) {
            ctx.loop.postEvent(.data_update);
        }

        sleepWithShutdown(ctx.shutdown, time.ns_per_s);
    }
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
