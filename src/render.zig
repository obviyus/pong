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

const column_widths = [_]u16{ 28, 11, 11, 11, 11, 11, 11, 11 };
const column_offsets = calcColumnOffsets();
// AIDEV-NOTE: Columns are hidden right-to-left when terminal is too narrow
// Priority order (left to right): Region, Last, Min, Avg, Max, Stddev, P95, P99
fn calcVisibleColumns(width: u16) usize {
    var total: u16 = 2; // borders
    var visible: usize = 0;
    for (column_widths) |w| {
        const needed = total + w + 2;
        if (needed > width) break;
        total = needed;
        visible += 1;
    }
    return if (visible < 2) 2 else visible; // Always show at least Region + Last
}

fn calcDynamicOffsets(visible_cols: usize, offsets: *[column_labels.len]u16) void {
    var current: u16 = 1;
    var i: usize = 0;
    while (i < visible_cols) : (i += 1) {
        offsets[i] = current;
        current += column_widths[i] + 2;
    }
}

const palette = struct {
    const border = vaxis.Cell.Style{ .fg = .{ .rgb = .{ 80, 120, 160 } } };
    const header = vaxis.Cell.Style{ .fg = .{ .rgb = .{ 120, 200, 255 } }, .bold = true };
    const text = vaxis.Cell.Style{ .fg = .{ .rgb = .{ 220, 220, 220 } } };
    const green = vaxis.Cell.Style{ .fg = .{ .rgb = .{ 120, 200, 140 } } };
    const red = vaxis.Cell.Style{ .fg = .{ .rgb = .{ 230, 120, 120 } } };
    const yellow = vaxis.Cell.Style{ .fg = .{ .rgb = .{ 230, 200, 120 } } };
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
        avg_cache[i] = shared_stats[i].readAvg();
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
    var total_samples: u64 = 0;

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

    if (table.height == 0 or table.width == 0) {
        try vx.render(tty.writer());
        return;
    }

    const visible_cols = calcVisibleColumns(table.width);
    var dynamic_offsets: [column_labels.len]u16 = undefined;
    calcDynamicOffsets(visible_cols, &dynamic_offsets);

    for (column_labels[0..visible_cols], 0..) |label, idx| {
        const label_text = if (idx == 0)
            label
        else
            rightAlignedLabel(&header_cache[idx], label, column_widths[idx]);

        _ = table.print(&.{
            .{ .text = label_text, .style = palette.header },
        }, .{
            .row_offset = 0,
            .col_offset = dynamic_offsets[idx],
            .wrap = .none,
        });
    }

    const max_rows: usize = if (table.height > 0)
        @as(usize, table.height) - 1
    else
        0;
    for (sorted_indices, 0..) |region_idx, i| {
        const row: u16 = @intCast(i + 1);

        const snapshot = shared_stats[region_idx].read();
        total_samples += snapshot.samples;

        if (i >= max_rows) continue;

        _ = table.print(&.{
            .{ .text = snapshot.region, .style = palette.text },
        }, .{
            .row_offset = row,
            .col_offset = dynamic_offsets[0],
            .wrap = .none,
        });

        if (visible_cols < 2) continue;

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

        const data_cols = visible_cols - 1; // exclude region column
        for (values[0..data_cols], 0..) |cell, col_idx| {
            _ = table.print(&.{
                .{ .text = cell.text, .style = cell.style },
            }, .{
                .row_offset = row,
                .col_offset = dynamic_offsets[col_idx + 1],
                .wrap = .none,
            });
        }
    }

    if (win.height > 0) {
        const quit_hint = "Press q or Ctrl+C to quit.";
        _ = win.print(&.{
            .{ .text = quit_hint, .style = palette.text },
        }, .{
            .row_offset = win.height - 1,
            .col_offset = 1,
            .wrap = .none,
        });

        var samples_buf: [64]u8 = undefined;
        const samples_text = try formatSampleCount(&samples_buf, total_samples);
        var status_buf: [80]u8 = undefined;
        const status_text = try std.fmt.bufPrint(&status_buf, "{s} samples", .{samples_text});
        const width = win.width;
        const text_len: u16 = @intCast(status_text.len);
        const offset: u16 = if (width > text_len)
            width - text_len - 1
        else
            0;

        _ = win.print(&.{
            .{ .text = status_text, .style = palette.text },
        }, .{
            .row_offset = win.height - 1,
            .col_offset = offset,
            .wrap = .none,
        });
    }

    try vx.render(tty.writer());
}

pub fn renderWarmup(
    vx: *vaxis.Vaxis,
    tty: *vaxis.Tty,
    elapsed_ns: u64,
    remaining_ns: u64,
    total_seconds: u64,
) !void {
    const spinner_frames = [_][]const u8{ "-", "\\", "|", "/" };
    const spinner_period_ns: u64 = 150 * std.time.ns_per_ms;
    const frame_index: usize = if (spinner_period_ns == 0)
        0
    else
        @intCast((elapsed_ns / spinner_period_ns) % spinner_frames.len);
    const spinner = spinner_frames[frame_index];

    var message_buf: [64]u8 = undefined;
    const message = try std.fmt.bufPrint(&message_buf, "{s} {s}", .{ spinner, "Warming up..." });

    const remaining_seconds = if (remaining_ns == 0)
        0
    else
        (remaining_ns + std.time.ns_per_s - 1) / std.time.ns_per_s;

    const countdown_width = digitCount(total_seconds);

    var remaining_buf: [16]u8 = undefined;
    const remaining_raw = try std.fmt.bufPrint(&remaining_buf, "{d}", .{remaining_seconds});

    var padded_number: [32]u8 = undefined;
    const pad_len = if (countdown_width > remaining_raw.len)
        countdown_width - remaining_raw.len
    else
        0;
    if (pad_len > 0) {
        @memset(padded_number[0..pad_len], ' ');
    }
    std.mem.copyForwards(u8, padded_number[pad_len .. pad_len + remaining_raw.len], remaining_raw);
    const padded_slice = padded_number[0 .. pad_len + remaining_raw.len];

    var countdown_buf: [64]u8 = undefined;
    const countdown = try std.fmt.bufPrint(&countdown_buf, "{s}s remaining", .{padded_slice});

    const win = vx.window();
    win.clear();
    win.hideCursor();
    win.fill(.{ .default = true });

    if (win.height == 0 or win.width == 0) {
        try vx.render(tty.writer());
        return;
    }

    const row_center: u16 = @intCast(win.height / 2);
    const col_message: u16 = @intCast(if (win.width > message.len) (win.width - message.len) / 2 else 0);
    const col_countdown: u16 = @intCast(if (win.width > countdown.len) (win.width - countdown.len) / 2 else 0);

    _ = win.print(&.{
        .{ .text = message, .style = palette.header },
    }, .{
        .row_offset = row_center,
        .col_offset = col_message,
        .wrap = .none,
    });

    if (row_center + 1 < win.height) {
        _ = win.print(&.{
            .{ .text = countdown, .style = palette.text },
        }, .{
            .row_offset = row_center + 1,
            .col_offset = col_countdown,
            .wrap = .none,
        });
    }

    try vx.render(tty.writer());
}

fn digitCount(value: u64) usize {
    var n = value;
    var count: usize = 1;
    while (n >= 10) {
        n /= 10;
        count += 1;
    }
    return count;
}

fn CompareIndexByAvg(comptime SharedStatType: type) type {
    return struct {
        fn less(ctx: SortContext(SharedStatType), lhs: usize, rhs: usize) bool {
            const lhs_avg = ctx.avg_cache[lhs];
            const rhs_avg = ctx.avg_cache[rhs];

            if (lhs_avg) |la| {
                if (rhs_avg) |ra| {
                    if (la == ra) {
                        const lhs_region = ctx.shared_stats[lhs].region;
                        const rhs_region = ctx.shared_stats[rhs].region;
                        return mem.lessThan(u8, lhs_region, rhs_region);
                    }
                    return la < ra;
                }
                return false;
            }

            if (rhs_avg != null) {
                return true;
            }

            const lhs_region = ctx.shared_stats[lhs].region;
            const rhs_region = ctx.shared_stats[rhs].region;
            return mem.lessThan(u8, lhs_region, rhs_region);
        }
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
        // AIDEV-NOTE: Fixed width of 9 chars total (6 digits + dot + 2 decimal) ensures "ms" always aligns
        return std.fmt.bufPrint(buf, "{d:>9.2}ms", .{latency});
    }
    return "       --";
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

fn formatSampleCount(buf: *[64]u8, value: u64) ![]const u8 {
    const thresholds = [_]struct {
        value: u64,
        suffix: []const u8,
    }{
        .{ .value = 1_000_000_000_000, .suffix = "T" },
        .{ .value = 1_000_000_000, .suffix = "B" },
        .{ .value = 1_000_000, .suffix = "M" },
        .{ .value = 1_000, .suffix = "K" },
    };

    var i: usize = 0;
    while (i < thresholds.len) : (i += 1) {
        const threshold = thresholds[i];
        if (value >= threshold.value) {
            const scaled = @as(f64, @floatFromInt(value)) / @as(f64, @floatFromInt(threshold.value));
            return std.fmt.bufPrint(buf, "{d:.1}{s}", .{ scaled, threshold.suffix });
        }
    }

    return std.fmt.bufPrint(buf, "{d}", .{value});
}
