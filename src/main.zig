const std = @import("std");
const vaxis = @import("vaxis");

const regions = @import("regions.zig");
const stats = @import("stats.zig");

const mem = std.mem;
const sort = std.sort;
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

const RegionSnapshot = struct {
    region: []const u8,
    last: ?f64,
    min: ?f64,
    avg: ?f64,
    max: ?f64,
    stddev: ?f64,
    p95: ?f64,
    p99: ?f64,
};

const column_labels = [_][]const u8{
    "AWS Region",
    "Last",
    "Min",
    "Avg",
    "Max",
    "Stddev",
    "P95",
    "P99",
};

const column_widths = [_]u16{ 28, 9, 9, 9, 9, 9, 9, 9 };
const column_offsets = calcColumnOffsets();

const palette = struct {
    const border = vaxis.Cell.Style{ .fg = .{ .rgb = .{ 80, 120, 160 } } };
    const header = vaxis.Cell.Style{ .fg = .{ .rgb = .{ 120, 200, 255 } }, .bold = true };
    const text = vaxis.Cell.Style{ .fg = .{ .rgb = .{ 220, 220, 220 } } };
    const green = vaxis.Cell.Style{ .fg = .{ .rgb = .{ 120, 200, 140 } } };
    const red = vaxis.Cell.Style{ .fg = .{ .rgb = .{ 230, 120, 120 } } };
    const yellow = vaxis.Cell.Style{ .fg = .{ .rgb = .{ 230, 200, 120 } } };
};

pub fn main() !void {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    defer {
        const leak = gpa.deinit();
        if (leak == .leak) log.err("memory leak detected", .{});
    }
    const allocator = gpa.allocator();

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

    var snapshots_buffer: [regions.region_count]RegionSnapshot = undefined;

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

        if (needs_render and running) {
            const snapshots = collectSnapshots(shared_stats, snapshots_buffer[0..]);
            try render(&vx, &tty, snapshots);
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
            .user_agent = .{ .override = "pong-zig" },
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

fn collectSnapshots(
    shared_stats: []SharedStat,
    out: []RegionSnapshot,
) []RegionSnapshot {
    for (shared_stats, 0..) |*entry, idx| {
        entry.mutex.lock();
        const snapshot = RegionSnapshot{
            .region = entry.data.region,
            .last = entry.data.last(),
            .min = entry.data.min(),
            .avg = entry.data.avg(),
            .max = entry.data.max(),
            .stddev = entry.data.stddev(),
            .p95 = entry.data.p95(),
            .p99 = entry.data.p99(),
        };
        entry.mutex.unlock();
        out[idx] = snapshot;
    }

    const len = shared_stats.len;
    const slice = out[0..len];
    sort.heap(RegionSnapshot, slice, {}, compareByAvg);
    return slice;
}

fn render(
    vx: *vaxis.Vaxis,
    tty: *vaxis.Tty,
    snapshots: []const RegionSnapshot,
) !void {
    var formatted_cache: [regions.region_count][7][16]u8 = undefined;

    const win = vx.window();
    win.clear();

    var table = win.child(.{
        .border = .{
            .where = .all,
            .style = palette.border,
        },
    });
    table.hideCursor();
    table.fill(.{ .default = true });

    if (table.height == 0) {
        try vx.render(tty.writer());
        return;
    }

    for (column_labels, 0..) |label, idx| {
        _ = table.print(&.{
            .{ .text = label, .style = palette.header },
        }, .{
            .row_offset = 0,
            .col_offset = column_offsets[idx],
            .wrap = .none,
        });
    }

    const max_rows: usize = if (table.height > 0)
        @as(usize, table.height) - 1
    else
        0;
    for (snapshots, 0..) |snapshot, i| {
        if (i >= max_rows) break;
        const row: u16 = @intCast(i + 1);

        _ = table.print(&.{
            .{ .text = snapshot.region, .style = palette.text },
        }, .{
            .row_offset = row,
            .col_offset = column_offsets[0],
            .wrap = .none,
        });

        const last_style = styleForLast(snapshot.last, snapshot.avg);
        const row_buffers = &formatted_cache[i];

        const last_text = try formatLatency(snapshot.last, &row_buffers[0]);
        const min_text = try formatLatency(snapshot.min, &row_buffers[1]);
        const avg_text = try formatLatency(snapshot.avg, &row_buffers[2]);
        const max_text = try formatLatency(snapshot.max, &row_buffers[3]);
        const stddev_text = try formatLatency(snapshot.stddev, &row_buffers[4]);
        const p95_text = try formatLatency(snapshot.p95, &row_buffers[5]);
        const p99_text = try formatLatency(snapshot.p99, &row_buffers[6]);

        const values = [_]struct {
            text: []const u8,
            style: vaxis.Cell.Style,
        }{
            .{ .text = last_text, .style = last_style },
            .{ .text = min_text, .style = palette.yellow },
            .{ .text = avg_text, .style = palette.yellow },
            .{ .text = max_text, .style = palette.yellow },
            .{ .text = stddev_text, .style = palette.yellow },
            .{ .text = p95_text, .style = palette.yellow },
            .{ .text = p99_text, .style = palette.yellow },
        };

        for (values, 0..) |cell, col_idx| {
            _ = table.print(&.{
                .{ .text = cell.text, .style = cell.style },
            }, .{
                .row_offset = row,
                .col_offset = column_offsets[col_idx + 1],
                .wrap = .none,
            });
        }
    }

    const status = "Press q or Ctrl+C to quit.";
    if (win.height > 0) {
        _ = win.print(&.{
            .{ .text = status, .style = palette.text },
        }, .{
            .row_offset = win.height - 1,
            .col_offset = 1,
            .wrap = .none,
        });
    }

    try vx.render(tty.writer());
}

fn formatLatency(value: ?f64, buf: *[16]u8) ![]const u8 {
    if (value) |latency| {
        return std.fmt.bufPrint(buf, "{d:>6.2} ms", .{latency});
    }
    return "--";
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

fn styleForLast(last: ?f64, avg: ?f64) vaxis.Cell.Style {
    if (last) |l| {
        if (avg) |a| {
            return if (l > a) palette.red else palette.green;
        }
        return palette.yellow;
    }
    return palette.yellow;
}

fn compareByAvg(_: void, lhs: RegionSnapshot, rhs: RegionSnapshot) bool {
    if (lhs.avg) |la| {
        if (rhs.avg) |ra| {
            if (la == ra) {
                return mem.lessThan(u8, lhs.region, rhs.region);
            }
            return la < ra;
        }
        return false;
    }

    if (rhs.avg != null) {
        return true;
    }

    return mem.lessThan(u8, lhs.region, rhs.region);
}

fn calcColumnOffsets() [column_labels.len]u16 {
    var offsets: [column_labels.len]u16 = undefined;
    var current: u16 = 1;
    var i: usize = 0;
    while (i < column_labels.len) : (i += 1) {
        offsets[i] = current;
        current += column_widths[i] + 2;
    }
    return offsets;
}
