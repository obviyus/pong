mod regions;

use arraydeque::{ArrayDeque, Wrapping};
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
use statrs::statistics::Statistics;
use std::time::{Duration, Instant};
use std::{io::stdout, sync::Arc};
use tokio::{
    sync::{mpsc, Mutex},
    task::JoinHandle,
    time::sleep,
};

struct PingStats<'a> {
    region: &'a str,
    latencies: ArrayDeque<f64, 100, Wrapping>,
}

impl<'a> PingStats<'a> {
    fn new(region: &'a str) -> Self {
        PingStats {
            region,
            latencies: ArrayDeque::new(),
        }
    }

    fn add_latency(&mut self, latency: Option<Duration>) {
        if let Some(lat) = latency {
            self.latencies.push_back(lat.as_secs_f64() * 1000.0);
        }
    }

    fn min(&self) -> Option<f64> {
        self.latencies.iter().copied().reduce(f64::min)
    }

    fn max(&self) -> Option<f64> {
        self.latencies.iter().copied().reduce(f64::max)
    }

    fn avg(&self) -> Option<f64> {
        if self.latencies.is_empty() {
            None
        } else {
            Some(self.latencies.iter().copied().mean())
        }
    }

    fn stddev(&self) -> Option<f64> {
        if self.latencies.len() > 1 {
            Some(self.latencies.iter().copied().std_dev())
        } else {
            None
        }
    }

    fn last(&self) -> Option<f64> {
        self.latencies.iter().last().copied()
    }
}

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

async fn fetch_latency_for_region(
    client: Client,
    region: String,
    url: String,
    tx: mpsc::Sender<(String, Option<Duration>)>,
) {
    loop {
        let mut retries = 3;
        let mut latency;

        loop {
            latency = ping_region(&client, &url).await;
            if latency.is_some() || retries == 0 {
                break;
            }
            retries -= 1;
            sleep(Duration::from_millis(500)).await;
        }

        if tx.send((region.clone(), latency)).await.is_err() {
            break; // Stop if the channel is closed
        }

        sleep(Duration::from_secs(1)).await;
    }
}

async fn start_fetching_latencies(
    client: Client,
    tx: mpsc::Sender<(String, Option<Duration>)>,
) -> Vec<JoinHandle<()>> {
    REGIONS_LIST
        .iter()
        .map(|(region, url)| {
            let client_clone = client.clone();
            let tx_clone = tx.clone();
            let region = region.to_string();
            let url = url.to_string();

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
    let stats = stats.lock().await;

    terminal
        .draw(|f| {
            let chunks = Layout::default()
                .constraints([Constraint::Percentage(100)].as_ref())
                .split(f.area());

            let mut sorted_stats: Vec<_> = stats.iter().collect();
            sorted_stats.sort_by(|a, b| {
                a.avg()
                    .partial_cmp(&b.avg())
                    .unwrap_or(std::cmp::Ordering::Equal)
            });

            let rows: Vec<Row> = sorted_stats
                .iter()
                .map(|stat| {
                    let latency_text = format_latency_option(stat.min());
                    let avg_text = format_latency_option(stat.avg());
                    let max_text = format_latency_option(stat.max());
                    let stddev_text = format_latency_option(stat.stddev());
                    let last_text = format_latency_option(stat.last());

                    Row::new(vec![
                        Cell::from(Span::styled(stat.region, Style::default().fg(Color::Green))),
                        Cell::from(Span::styled(last_text, Style::default().fg(Color::Yellow))),
                        Cell::from(Span::styled(
                            latency_text,
                            Style::default().fg(Color::Yellow),
                        )),
                        Cell::from(Span::styled(avg_text, Style::default().fg(Color::Yellow))),
                        Cell::from(Span::styled(max_text, Style::default().fg(Color::Yellow))),
                        Cell::from(Span::styled(
                            stddev_text,
                            Style::default().fg(Color::Yellow),
                        )),
                    ])
                })
                .collect();

            let widths = [
                Constraint::Percentage(30),
                Constraint::Percentage(14),
                Constraint::Percentage(14),
                Constraint::Percentage(14),
                Constraint::Percentage(14),
                Constraint::Percentage(14),
            ];

            let table = Table::new(rows, &widths)
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title("Ping Latencies"),
                )
                .header(
                    Row::new(vec![
                        Cell::from("Region"),
                        Cell::from("Last"),
                        Cell::from("Min"),
                        Cell::from("Avg"),
                        Cell::from("Max"),
                        Cell::from("Stddev"),
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
            .collect(),
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
    }

    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        crossterm::terminal::LeaveAlternateScreen
    )?;
    terminal.show_cursor()?;

    Ok(())
}
