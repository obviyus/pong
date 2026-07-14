# pong

![pong-demo](assets/pong-demo.avif)

Which AWS region is actually closest to you? `pong` pings all 33 of them at once and shows a live, self-sorting latency table right in your terminal.

## Features

- Concurrent HTTP `HEAD` probes across 33 AWS regions — one worker thread per region
- Live TUI table, always sorted by lowest average latency
- Rolling `min / avg / max / stddev / p95 / p99` via fixed-capacity ring-buffer percentiles
- Optional warmup so the numbers settle before you read them

## Build and run

```bash
cargo run                 # start pinging
cargo run -- --warmup 5   # let pings settle for 5s first
cargo run -- --help
```

Quit with `q` or `Ctrl-C`. Prebuilt binaries (Linux x86_64 / aarch64, macOS aarch64) ship on each release.

## How it works

Each region gets a thread that fires a `HEAD` at `https://dynamodb.<region>.amazonaws.com/ping` every second — DynamoDB's health endpoint makes a conveniently ubiquitous latency target. Results flow back over an `mpsc` channel into the ring buffers behind the table. Pure `std` threads, no async runtime. Written in Rust with [`ratatui`](https://github.com/ratatui/ratatui) + `crossterm`.
