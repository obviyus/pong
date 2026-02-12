# pong

![pong-demo](assets/pong-demo.avif)

Pong is a simple CLI program that measures and compares AWS region latencies in a terminal table. This version is written in Rust with [`ratatui`](https://github.com/ratatui/ratatui) and `crossterm`.

## Features

- Concurrent HTTP `HEAD` probes across AWS regions (one worker per region).
- Live TUI table sorted by lowest average latency.
- Rolling `min/avg/max/stddev/p95/p99` stats with fixed-capacity ring buffer percentiles.
- Optional warmup delay before sample collection.

## Build and run

```bash
cargo run
```

With warmup:

```bash
cargo run -- --warmup 5
```
