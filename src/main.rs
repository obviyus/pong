mod regions;
mod stats;

use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Layout},
    style::{Color, Style},
    text::Span,
    widgets::{Block, Borders, Cell, Row, Table},
    Terminal,
};
use regions::REGIONS_LIST;
use reqwest::Client;
use stats::PingStats;
use std::{
    collections::HashMap,
    io::stdout,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{
    sync::{mpsc, Mutex},
    task::JoinHandle,
    time::sleep,
};

// AIDEV-NOTE: Inline helper for consistent string formatting
#[inline]
fn format_latency_option(value: Option<f64>) -> String {
    match value {
        Some(v) => format!("{:.2} ms", v),
        None => "--".to_string(),
    }
}

async fn ping_region(client: &Client, url: &str) -> Option<Duration> {
    let start = Instant::now();
    let result = client
        .head(url)
        .timeout(Duration::from_secs(3))
        .send()
        .await;
    match result {
        Ok(_) => Some(start.elapsed()),
        Err(_) => None,
    }
}

async fn fetch_latency_for_region<'a>(
    client: Client,
    region: &'a str,
    url: &'a str,
    tx: mpsc::Sender<(&'a str, Option<Duration>)>,
) {
    loop {
        let mut retries = 3;
        let mut latency;

        loop {
            latency = ping_region(&client, url).await;
            if latency.is_some() || retries == 0 {
                break;
            }
            retries -= 1;
            sleep(Duration::from_millis(500)).await;
        }

        if tx.send((region, latency)).await.is_err() {
            break; // Stop if the channel is closed
        }

        sleep(Duration::from_secs(1)).await;
    }
}

async fn start_fetching_latencies(
    client: Client,
    tx: mpsc::Sender<(&'static str, Option<Duration>)>,
) -> Vec<JoinHandle<()>> {
    REGIONS_LIST
        .iter()
        .map(|(region, url)| {
            let client_clone = client.clone();
            let tx_clone = tx.clone();
            tokio::spawn(fetch_latency_for_region(
                client_clone,
                region,
                url,
                tx_clone,
            ))
        })
        .collect()
}

// AIDEV-NOTE: Pre-allocated buffer for sorting indices to avoid allocations
struct RenderBuffers {
    sorted_indices: Vec<usize>,
}

impl RenderBuffers {
    fn new(capacity: usize) -> Self {
        Self {
            sorted_indices: Vec::with_capacity(capacity),
        }
    }

    fn clear(&mut self) {
        self.sorted_indices.clear();
    }
}

async fn render_ui(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    stats: Arc<Mutex<Vec<PingStats<'_>>>>,
    buffers: &mut RenderBuffers,
) {
    // AIDEV-NOTE: Minimize lock time by cloning only the data we need
    let stats_snapshot: Vec<_> = {
        let stats_guard = stats.lock().await;
        stats_guard
            .iter()
            .enumerate()
            .map(|(i, stat)| {
                (
                    i,
                    stat.region,
                    stat.last(),
                    stat.avg(),
                    stat.min(),
                    stat.max(),
                    stat.stddev(),
                    stat.p95(),
                    stat.p99(),
                )
            })
            .collect()
    };

    terminal
        .draw(|f| {
            let chunks = Layout::default()
                .constraints([Constraint::Percentage(100)].as_ref())
                .split(f.area());

            // AIDEV-NOTE: Sort indices instead of references to avoid allocations
            buffers.clear();
            buffers.sorted_indices.extend(0..stats_snapshot.len());
            buffers.sorted_indices.sort_by(|&a, &b| {
                stats_snapshot[a]
                    .3 // avg field
                    .partial_cmp(&stats_snapshot[b].3)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });

            let rows: Vec<Row> = buffers
                .sorted_indices
                .iter()
                .map(|&idx| {
                    let (_, region, last, avg, min, max, stddev, p95, p99) = &stats_snapshot[idx];

                    let last_style = if let (Some(last_val), Some(avg_val)) = (last, avg) {
                        if last_val > avg_val {
                            Style::default().fg(Color::Red)
                        } else {
                            Style::default().fg(Color::Green)
                        }
                    } else {
                        Style::default().fg(Color::Yellow)
                    };

                    // AIDEV-NOTE: Use helper function for consistent formatting
                    Row::new(vec![
                        Cell::from(Span::styled(*region, Style::default().fg(Color::White))),
                        Cell::from(Span::styled(format_latency_option(*last), last_style)),
                        Cell::from(Span::styled(
                            format_latency_option(*min),
                            Style::default().fg(Color::Yellow),
                        )),
                        Cell::from(Span::styled(
                            format_latency_option(*avg),
                            Style::default().fg(Color::Yellow),
                        )),
                        Cell::from(Span::styled(
                            format_latency_option(*max),
                            Style::default().fg(Color::Yellow),
                        )),
                        Cell::from(Span::styled(
                            format_latency_option(*stddev),
                            Style::default().fg(Color::Yellow),
                        )),
                        Cell::from(Span::styled(
                            format_latency_option(*p95),
                            Style::default().fg(Color::Yellow),
                        )),
                        Cell::from(Span::styled(
                            format_latency_option(*p99),
                            Style::default().fg(Color::Yellow),
                        )),
                    ])
                })
                .collect();

            let widths = [
                Constraint::Percentage(20),
                Constraint::Percentage(10),
                Constraint::Percentage(10),
                Constraint::Percentage(10),
                Constraint::Percentage(10),
                Constraint::Percentage(10),
                Constraint::Percentage(10),
                Constraint::Percentage(10),
            ];

            let table = Table::new(rows, &widths)
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title("Ping Latencies"),
                )
                .header(
                    Row::new(vec![
                        Cell::from("AWS Region"),
                        Cell::from("Last"),
                        Cell::from("Min"),
                        Cell::from("Avg"),
                        Cell::from("Max"),
                        Cell::from("Stddev"),
                        Cell::from("P95"),
                        Cell::from("P99"),
                    ])
                    .style(Style::default().fg(Color::Cyan)),
                );

            f.render_widget(table, chunks[0]);
        })
        .unwrap();
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();

    enable_raw_mode()?;
    let mut stdout = stdout();
    execute!(stdout, EnterAlternateScreen)?;

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let client = Client::new();

    // AIDEV-NOTE: Create region lookup map for O(1) access instead of O(n) search
    let region_to_index: HashMap<&'static str, usize> = REGIONS_LIST
        .iter()
        .enumerate()
        .map(|(i, (region, _))| (*region, i))
        .collect();

    let stats = Arc::new(Mutex::new(
        REGIONS_LIST
            .iter()
            .map(|(region, _)| PingStats::new(region))
            .collect::<Vec<_>>(),
    ));

    let (tx, mut rx) = mpsc::channel(32);

    let handles = start_fetching_latencies(client.clone(), tx).await;

    let (event_tx, mut event_rx) = mpsc::channel(1);
    tokio::spawn(async move {
        loop {
            if event::poll(Duration::from_millis(100)).unwrap() {
                if let Event::Key(key_event) = event::read().unwrap() {
                    event_tx.send(key_event).await.unwrap();
                }
            }
        }
    });

    let mut interval = tokio::time::interval(Duration::from_millis(100));
    let mut exit = false;
    let mut render_buffers = RenderBuffers::new(REGIONS_LIST.len());

    while !exit {
        tokio::select! {
            _ = interval.tick() => {
                render_ui(&mut terminal, Arc::clone(&stats), &mut render_buffers).await;
            }
            Some((region, latency)) = rx.recv() => {
                // AIDEV-NOTE: Use HashMap lookup instead of linear search
                if let Some(&index) = region_to_index.get(region) {
                    let mut stats = stats.lock().await;
                    stats[index].add_latency(latency);
                }
            }
            Some(key_event) = event_rx.recv() => {
                if key_event.code == KeyCode::Char('q') || (key_event.code == KeyCode::Char('c') && key_event.modifiers.contains(crossterm::event::KeyModifiers::CONTROL)) {
                    exit = true;
                }
            }
        }
    }

    for handle in handles {
        handle.abort();
        let _ = handle.await;
    }

    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        crossterm::terminal::LeaveAlternateScreen
    )?;
    terminal.show_cursor()?;

    std::process::exit(0)
}
