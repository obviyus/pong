mod regions;
mod stats;
mod ui;

use std::error::Error;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::{Duration, Instant};

use crossterm::event::{self, Event as CrosstermEvent, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use crossterm::{ExecutableCommand, execute};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use reqwest::blocking::Client;
use reqwest::redirect;

use crate::regions::{REGIONS_LIST, Region};
use crate::stats::PingStats;

const RETRY_DELAY: Duration = Duration::from_millis(500);
const PING_INTERVAL: Duration = Duration::from_secs(1);
const SPINNER_SLEEP: Duration = Duration::from_millis(150);

#[derive(Clone, Copy)]
pub struct StatsSnapshot {
    pub region: &'static str,
    pub last: Option<f64>,
    pub min: Option<f64>,
    pub avg: Option<f64>,
    pub max: Option<f64>,
    pub stddev: Option<f64>,
    pub p95: Option<f64>,
    pub p99: Option<f64>,
    pub samples: u64,
}

impl StatsSnapshot {
    fn empty(region: &'static str) -> Self {
        Self {
            region,
            last: None,
            min: None,
            avg: None,
            max: None,
            stddev: None,
            p95: None,
            p99: None,
            samples: 0,
        }
    }
}

pub struct SharedStat {
    pub region: &'static str,
    snapshot: RwLock<StatsSnapshot>,
}

impl SharedStat {
    fn new(region: &'static str) -> Self {
        Self {
            region,
            snapshot: RwLock::new(StatsSnapshot::empty(region)),
        }
    }

    fn publish(&self, stats: &mut PingStats) {
        let snapshot = StatsSnapshot {
            region: stats.region,
            last: stats.last(),
            min: stats.min(),
            avg: stats.avg(),
            max: stats.max(),
            stddev: stats.stddev(),
            p95: stats.p95(),
            p99: stats.p99(),
            samples: stats.total_samples(),
        };

        if let Ok(mut guard) = self.snapshot.write() {
            *guard = snapshot;
        }
    }

    fn read(&self) -> StatsSnapshot {
        match self.snapshot.read() {
            Ok(guard) => *guard,
            Err(_) => StatsSnapshot::empty(self.region),
        }
    }
}

struct Cli {
    warmup: Duration,
    help_only: bool,
}

fn main() {
    if let Err(err) = run() {
        eprintln!("{err}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let cli = parse_args()?;
    if cli.help_only {
        return Ok(());
    }

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    let app_result = run_app(&mut terminal, cli.warmup);

    disable_raw_mode()?;
    io::stdout().execute(LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    app_result
}

fn run_app(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    warmup_duration: Duration,
) -> Result<(), Box<dyn Error>> {
    let warmup_total_seconds = warmup_duration.as_secs();
    let warmup_start = Instant::now();
    let mut warmup_ready = warmup_duration.is_zero();

    let shutdown = Arc::new(AtomicBool::new(false));
    let collect_samples = Arc::new(AtomicBool::new(warmup_ready));

    let shared_stats: Vec<Arc<SharedStat>> = REGIONS_LIST
        .iter()
        .map(|region| Arc::new(SharedStat::new(region.name)))
        .collect();

    let (notify_tx, notify_rx) = mpsc::channel::<()>();
    let mut workers = Vec::with_capacity(REGIONS_LIST.len());
    for (region, stat) in REGIONS_LIST.iter().zip(shared_stats.iter()) {
        let worker = spawn_worker(
            *region,
            Arc::clone(stat),
            Arc::clone(&shutdown),
            Arc::clone(&collect_samples),
            notify_tx.clone(),
        );
        workers.push(worker);
    }
    drop(notify_tx);

    let mut running = true;
    let mut needs_render = true;
    while running {
        if !warmup_ready {
            let elapsed = warmup_start.elapsed();
            if elapsed >= warmup_duration {
                warmup_ready = true;
                collect_samples.store(true, Ordering::Release);
                needs_render = true;
            }
        }

        while notify_rx.try_recv().is_ok() {
            needs_render = true;
        }

        let poll_timeout = if warmup_ready {
            Duration::from_millis(100)
        } else {
            SPINNER_SLEEP
        };

        if event::poll(poll_timeout)? {
            match event::read()? {
                CrosstermEvent::Key(key_event) if key_event.kind == KeyEventKind::Press => {
                    if is_quit_key(key_event.code, key_event.modifiers) {
                        running = false;
                    }
                    needs_render = true;
                }
                CrosstermEvent::Resize(_, _) => {
                    needs_render = true;
                }
                _ => {}
            }
        }

        if !running {
            break;
        }

        if warmup_ready {
            if needs_render {
                ui::render(terminal, &shared_stats)?;
                needs_render = false;
            }
        } else {
            let elapsed = warmup_start.elapsed();
            let remaining = warmup_duration.saturating_sub(elapsed);
            ui::render_warmup(terminal, elapsed, remaining, warmup_total_seconds)?;
            needs_render = false;
        }
    }

    shutdown.store(true, Ordering::SeqCst);
    for worker in workers {
        let _ = worker.join();
    }

    Ok(())
}

fn spawn_worker(
    region: Region,
    shared_stat: Arc<SharedStat>,
    shutdown: Arc<AtomicBool>,
    collect_samples: Arc<AtomicBool>,
    notify_tx: Sender<()>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let client = match Client::builder().redirect(redirect::Policy::none()).build() {
            Ok(client) => client,
            Err(_) => return,
        };

        let mut local_stats = PingStats::new(region.name);
        while !shutdown.load(Ordering::Acquire) {
            let measurement = take_measurement(&client, region.url, &shutdown);
            if collect_samples.load(Ordering::Acquire) {
                local_stats.add_sample(measurement);
                shared_stat.publish(&mut local_stats);
                notify(&notify_tx);
            }

            sleep_with_shutdown(&shutdown, PING_INTERVAL);
        }
    })
}

fn notify(tx: &Sender<()>) {
    let _ = tx.send(());
}

fn take_measurement(client: &Client, url: &str, shutdown: &AtomicBool) -> Option<f64> {
    for _ in 0..3 {
        if shutdown.load(Ordering::Acquire) {
            break;
        }

        if let Ok(value) = ping_once(client, url) {
            return Some(value);
        }

        sleep_with_shutdown(shutdown, RETRY_DELAY);
    }
    None
}

fn ping_once(client: &Client, url: &str) -> Result<f64, reqwest::Error> {
    let start = Instant::now();
    let _response = client.head(url).header("user-agent", "pong").send()?;
    Ok(start.elapsed().as_secs_f64() * 1_000.0)
}

fn sleep_with_shutdown(flag: &AtomicBool, total: Duration) {
    let quantum = Duration::from_millis(25);
    let mut remaining = total;
    while remaining > Duration::ZERO && !flag.load(Ordering::Acquire) {
        let step = if remaining < quantum {
            remaining
        } else {
            quantum
        };
        thread::sleep(step);
        remaining = remaining.saturating_sub(step);
    }
}

fn parse_args() -> Result<Cli, Box<dyn Error>> {
    let mut args = std::env::args();
    let program_name = args.next().unwrap_or_else(|| "pong".to_string());

    let mut warmup = Duration::ZERO;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--warmup" => {
                let value = args.next().ok_or("--warmup expects a time in seconds")?;
                let seconds: u64 = value
                    .parse()
                    .map_err(|_| format!("invalid --warmup value '{value}'"))?;
                warmup = Duration::from_secs(seconds);
            }
            "--help" => {
                show_help(&program_name);
                return Ok(Cli {
                    warmup,
                    help_only: true,
                });
            }
            _ => {
                return Err(format!("unrecognized argument '{arg}'").into());
            }
        }
    }

    Ok(Cli {
        warmup,
        help_only: false,
    })
}

fn show_help(program_name: &str) {
    println!(
        "Usage: {program_name} [--warmup <seconds>] [--help]\n\n\
         Options:\n\
           --warmup <seconds>  delay rendering to allow initial pings to settle\n\
           --help              display this help and exit"
    );
}

fn is_quit_key(code: KeyCode, modifiers: KeyModifiers) -> bool {
    matches!(code, KeyCode::Char('q') | KeyCode::Char('Q'))
        || (modifiers.contains(KeyModifiers::CONTROL)
            && matches!(code, KeyCode::Char('c') | KeyCode::Char('C')))
}
