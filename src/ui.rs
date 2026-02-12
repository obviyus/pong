use std::cmp::Ordering;
use std::io;
use std::sync::Arc;
use std::time::Duration;

use ratatui::layout::{Alignment, Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table};
use ratatui::{Frame, Terminal};

use crate::{SharedStat, StatsSnapshot};

const COLUMN_LABELS: [&str; 8] = [
    "AWS Region",
    "Last",
    "Min",
    "Avg",
    "Max",
    "Stddev",
    "P95",
    "P99",
];
const COLUMN_WIDTHS: [u16; 8] = [28, 11, 11, 11, 11, 11, 11, 11];

const BORDER_STYLE: Style = Style::new().fg(Color::Rgb(80, 120, 160));
const HEADER_STYLE: Style = Style::new()
    .fg(Color::Rgb(120, 200, 255))
    .add_modifier(Modifier::BOLD);
const TEXT_STYLE: Style = Style::new().fg(Color::Rgb(220, 220, 220));
const GREEN_STYLE: Style = Style::new().fg(Color::Rgb(120, 200, 140));
const RED_STYLE: Style = Style::new().fg(Color::Rgb(230, 120, 120));
const YELLOW_STYLE: Style = Style::new().fg(Color::Rgb(230, 200, 120));

pub fn render<B: ratatui::backend::Backend<Error = io::Error>>(
    terminal: &mut Terminal<B>,
    shared_stats: &[Arc<SharedStat>],
) -> io::Result<()> {
    let mut snapshots: Vec<StatsSnapshot> = shared_stats.iter().map(|s| s.read()).collect();
    snapshots.sort_by(compare_snapshot);
    let total_samples: u64 = snapshots.iter().map(|s| s.samples).sum();

    terminal
        .draw(|frame| draw_table(frame, &snapshots, total_samples))
        .map(|_| ())
}

pub fn render_warmup<B: ratatui::backend::Backend<Error = io::Error>>(
    terminal: &mut Terminal<B>,
    elapsed: Duration,
    remaining: Duration,
    total_seconds: u64,
) -> io::Result<()> {
    terminal
        .draw(|frame| draw_warmup(frame, elapsed, remaining, total_seconds))
        .map(|_| ())
}

fn draw_table(frame: &mut Frame, snapshots: &[StatsSnapshot], total_samples: u64) {
    let area = frame.area();
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(area);

    let table_area = layout[0];
    let footer_area = layout[1];
    let table_width = table_area.width.saturating_sub(2);
    let visible_cols = calc_visible_columns(table_width);

    let header_cells = (0..visible_cols).map(|idx| {
        let text = if idx == 0 {
            COLUMN_LABELS[idx].to_string()
        } else {
            format!(
                "{:>width$}",
                COLUMN_LABELS[idx],
                width = COLUMN_WIDTHS[idx] as usize
            )
        };
        Cell::from(text).style(HEADER_STYLE)
    });
    let header = Row::new(header_cells);

    let rows = snapshots
        .iter()
        .map(|snapshot| row_for_snapshot(snapshot, visible_cols));
    let widths: Vec<Constraint> = COLUMN_WIDTHS[..visible_cols]
        .iter()
        .map(|w| Constraint::Length(*w))
        .collect();

    let table = Table::new(rows, widths)
        .header(header)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(BORDER_STYLE),
        )
        .column_spacing(2);
    frame.render_widget(table, table_area);

    let hint = "Press q or Ctrl+C to quit.";
    let status = format!("{} samples", format_sample_count(total_samples));
    let status_width = status.len() as u16 + 1;
    let footer = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(0), Constraint::Length(status_width)])
        .split(footer_area);

    let hint_widget = Paragraph::new(hint).style(TEXT_STYLE);
    frame.render_widget(hint_widget, footer[0]);

    if footer[1].width > 0 {
        let status_widget = Paragraph::new(status)
            .style(TEXT_STYLE)
            .alignment(Alignment::Right);
        frame.render_widget(status_widget, footer[1]);
    }
}

fn draw_warmup(frame: &mut Frame, elapsed: Duration, remaining: Duration, total_seconds: u64) {
    let area = frame.area();
    let spinner_frames = ["-", "\\", "|", "/"];
    let spinner_index = ((elapsed.as_millis() / 150) as usize) % spinner_frames.len();
    let spinner = spinner_frames[spinner_index];

    let message = format!("{spinner} Warming up...");
    let remaining_seconds = if remaining.is_zero() {
        0
    } else {
        (remaining.as_millis().div_ceil(1_000)) as u64
    };
    let countdown_width = digit_count(total_seconds);
    let countdown = format!(
        "{remaining_seconds:>width$}s remaining",
        width = countdown_width
    );

    let centered = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Fill(1),
            Constraint::Length(2),
            Constraint::Fill(1),
        ])
        .split(area)[1];
    let lines = vec![
        Line::from(Span::styled(message, HEADER_STYLE)),
        Line::from(Span::styled(countdown, TEXT_STYLE)),
    ];
    frame.render_widget(Paragraph::new(lines).alignment(Alignment::Center), centered);
}

fn row_for_snapshot(snapshot: &StatsSnapshot, visible_cols: usize) -> Row<'static> {
    let mut cells = Vec::with_capacity(visible_cols);
    cells.push(Cell::from(snapshot.region.to_string()).style(TEXT_STYLE));

    if visible_cols > 1 {
        cells.push(
            Cell::from(format_latency(snapshot.last))
                .style(style_for_last(snapshot.last, snapshot.avg)),
        );
    }
    if visible_cols > 2 {
        cells.push(Cell::from(format_latency(snapshot.min)).style(YELLOW_STYLE));
    }
    if visible_cols > 3 {
        cells.push(Cell::from(format_latency(snapshot.avg)).style(YELLOW_STYLE));
    }
    if visible_cols > 4 {
        cells.push(Cell::from(format_latency(snapshot.max)).style(YELLOW_STYLE));
    }
    if visible_cols > 5 {
        cells.push(Cell::from(format_latency(snapshot.stddev)).style(YELLOW_STYLE));
    }
    if visible_cols > 6 {
        cells.push(Cell::from(format_latency(snapshot.p95)).style(YELLOW_STYLE));
    }
    if visible_cols > 7 {
        cells.push(Cell::from(format_latency(snapshot.p99)).style(YELLOW_STYLE));
    }

    Row::new(cells)
}

fn compare_snapshot(lhs: &StatsSnapshot, rhs: &StatsSnapshot) -> Ordering {
    match (lhs.avg, rhs.avg) {
        (Some(la), Some(ra)) => la
            .partial_cmp(&ra)
            .unwrap_or(Ordering::Equal)
            .then_with(|| lhs.region.cmp(rhs.region)),
        (None, Some(_)) => Ordering::Less,
        (Some(_), None) => Ordering::Greater,
        (None, None) => lhs.region.cmp(rhs.region),
    }
}

// AIDEV-NOTE: Columns are hidden right-to-left when terminal is narrow.
fn calc_visible_columns(width: u16) -> usize {
    let mut total: u16 = 2;
    let mut visible: usize = 0;
    for col_width in COLUMN_WIDTHS {
        let needed = total.saturating_add(col_width).saturating_add(2);
        if needed > width {
            break;
        }
        total = needed;
        visible += 1;
    }
    visible.clamp(2, COLUMN_LABELS.len())
}

fn style_for_last(last: Option<f64>, avg: Option<f64>) -> Style {
    match (last, avg) {
        (Some(l), Some(a)) if l > a => RED_STYLE,
        (Some(_), Some(_)) => GREEN_STYLE,
        _ => YELLOW_STYLE,
    }
}

fn format_latency(value: Option<f64>) -> String {
    match value {
        Some(latency) => format!("{latency:>9.2}ms"),
        None => "       --".to_string(),
    }
}

fn format_sample_count(value: u64) -> String {
    const THRESHOLDS: &[(u64, &str)] = &[
        (1_000_000_000_000, "T"),
        (1_000_000_000, "B"),
        (1_000_000, "M"),
        (1_000, "K"),
    ];

    for (threshold, suffix) in THRESHOLDS {
        if value >= *threshold {
            let scaled = value as f64 / *threshold as f64;
            return format!("{scaled:.1}{suffix}");
        }
    }

    value.to_string()
}

fn digit_count(mut value: u64) -> usize {
    let mut count = 1;
    while value >= 10 {
        value /= 10;
        count += 1;
    }
    count
}
