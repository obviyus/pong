const std = @import("std");
const vaxis = @import("vaxis");
const regions = @import("regions.zig");

const mem = std.mem;

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

fn SortContext(comptime SharedStatType: type) type {
    return struct {
        avg_cache: []const ?f64,
        shared_stats: []const SharedStatType,
    };
}

pub fn collectSortedIndices(
    comptime SharedStatType: type,
    shared_stats: []SharedStatType,
    out: []usize,
) []const usize {
    const len = shared_stats.len;
    var avg_cache: [regions.region_count]?f64 = undefined;

    var i: usize = 0;
    while (i < len) : (i += 1) {
        out[i] = i;
        shared_stats[i].mutex.lock();
        avg_cache[i] = shared_stats[i].data.avg();
        shared_stats[i].mutex.unlock();
    }

    const slice = out[0..len];
    const ctx = SortContext(SharedStatType){
        .avg_cache = avg_cache[0..len],
        .shared_stats = shared_stats,
    };
    const less = CompareIndexByAvg(SharedStatType).less;
    std.sort.heap(usize, slice, ctx, less);
    return slice;
}

pub fn render(
    comptime SharedStatType: type,
    vx: *vaxis.Vaxis,
    tty: *vaxis.Tty,
    shared_stats: []SharedStatType,
    sorted_indices: []const usize,
) !void {
    var formatted_cache: [regions.region_count][7][16]u8 = undefined;
    var header_cache: [column_labels.len][32]u8 = undefined;

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
        const label_text = if (idx == 0)
            label
        else
            rightAlignedLabel(&header_cache[idx], label, column_widths[idx]);

        _ = table.print(&.{
            .{ .text = label_text, .style = palette.header },
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
    for (sorted_indices, 0..) |region_idx, i| {
        if (i >= max_rows) break;
        const row: u16 = @intCast(i + 1);

        const snapshot = snapshotSharedStat(SharedStatType, &shared_stats[region_idx]);

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

fn CompareIndexByAvg(comptime SharedStatType: type) type {
    return struct {
        fn less(ctx: SortContext(SharedStatType), lhs: usize, rhs: usize) bool {
            const lhs_avg = ctx.avg_cache[lhs];
            const rhs_avg = ctx.avg_cache[rhs];

            if (lhs_avg) |la| {
                if (rhs_avg) |ra| {
                    if (la == ra) {
                        const lhs_region = ctx.shared_stats[lhs].data.region;
                        const rhs_region = ctx.shared_stats[rhs].data.region;
                        return mem.lessThan(u8, lhs_region, rhs_region);
                    }
                    return la < ra;
                }
                return false;
            }

            if (rhs_avg != null) {
                return true;
            }

            const lhs_region = ctx.shared_stats[lhs].data.region;
            const rhs_region = ctx.shared_stats[rhs].data.region;
            return mem.lessThan(u8, lhs_region, rhs_region);
        }
    };
}

fn snapshotSharedStat(comptime SharedStatType: type, entry: *SharedStatType) RegionSnapshot {
    entry.mutex.lock();
    defer entry.mutex.unlock();

    return RegionSnapshot{
        .region = entry.data.region,
        .last = entry.data.last(),
        .min = entry.data.min(),
        .avg = entry.data.avg(),
        .max = entry.data.max(),
        .stddev = entry.data.stddev(),
        .p95 = entry.data.p95(),
        .p99 = entry.data.p99(),
    };
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

fn formatLatency(value: ?f64, buf: *[16]u8) ![]const u8 {
    if (value) |latency| {
        return std.fmt.bufPrint(buf, "{d:>6.2} ms", .{latency});
    }
    return "--";
}

fn rightAlignedLabel(buf: *[32]u8, label: []const u8, width: u16) []const u8 {
    const target_width: usize = @min(buf.len, @as(usize, width));

    if (label.len >= target_width) {
        const copy_len = @min(label.len, buf.len);
        mem.copyForwards(u8, buf[0..copy_len], label[0..copy_len]);
        return buf[0..copy_len];
    }

    const padding = target_width - label.len;
    var pad_idx: usize = 0;
    while (pad_idx < padding) : (pad_idx += 1) {
        buf[pad_idx] = ' ';
    }
    mem.copyForwards(u8, buf[padding .. padding + label.len], label);
    return buf[0..target_width];
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
