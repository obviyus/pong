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
    io::stdout,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{
    sync::{mpsc, Mutex},
    task::JoinHandle,
    time::sleep,
};

fn format_latency_option(value: Option<f64>) -> String {
    value
        .map(|v| format!("{:.2} ms", v))
        .unwrap_or("--".to_string())
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

async fn render_ui(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    stats: Arc<Mutex<Vec<PingStats<'_>>>>,
) {
    // Lock stats only once and collect references
    let stats_guard = stats.lock().await;

    terminal
        .draw(|f| {
            let chunks = Layout::default()
                .constraints([Constraint::Percentage(100)].as_ref())
                .split(f.area());

            // Create a vector of references for sorting
            let mut sorted_stats: Vec<&PingStats> = stats_guard.iter().collect();
            sorted_stats.sort_by(|a, b| {
                a.avg()
                    .partial_cmp(&b.avg())
                    .unwrap_or(std::cmp::Ordering::Equal)
            });

            let rows: Vec<Row> = sorted_stats
                .iter()
                .map(|stat| {
                    let last_text = format_latency_option(stat.last());
                    let avg_text = format_latency_option(stat.avg());
                    let min_text = format_latency_option(stat.min());
                    let max_text = format_latency_option(stat.max());
                    let stddev_text = format_latency_option(stat.stddev());
                    let p95_text = format_latency_option(stat.p95());
                    let p99_text = format_latency_option(stat.p99());

                    let last_value = stat.last();
                    let avg_value = stat.avg();

                    let last_style = if let (Some(last), Some(avg)) = (last_value, avg_value) {
                        if last > avg {
                            Style::default().fg(Color::Red) // Worse performance
                        } else {
                            Style::default().fg(Color::Green) // Better performance
                        }
                    } else {
                        Style::default().fg(Color::Yellow)
                    };

                    Row::new(vec![
                        Cell::from(Span::styled(stat.region, Style::default().fg(Color::White))),
                        Cell::from(Span::styled(last_text, last_style)),
                        Cell::from(Span::styled(min_text, Style::default().fg(Color::Yellow))),
                        Cell::from(Span::styled(avg_text, Style::default().fg(Color::Yellow))),
                        Cell::from(Span::styled(max_text, Style::default().fg(Color::Yellow))),
                        Cell::from(Span::styled(
                            stddev_text,
                            Style::default().fg(Color::Yellow),
                        )),
                        Cell::from(Span::styled(p95_text, Style::default().fg(Color::Yellow))),
                        Cell::from(Span::styled(p99_text, Style::default().fg(Color::Yellow))),
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

    while !exit {
        tokio::select! {
            _ = interval.tick() => {
                render_ui(&mut terminal, Arc::clone(&stats)).await;
            }
            Some((region, latency)) = rx.recv() => {
                let mut stats = stats.lock().await;
                if let Some(stat) = stats.iter_mut().find(|stat| stat.region == region) {
                    stat.add_latency(latency);
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
