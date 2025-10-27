# pong

![pong-demo](assets/pong-demo.avif)

Pong is a simple CLI program that I use to check latencies between different AWS regions. It's written in Zig and rendered with the excellent [`libvaxis`](https://github.com/rockorager/libvaxis) TUI toolkit.

## Features

- Zero-copy rendering pipeline with per-frame stack buffers to keep latency stats allocation-free.
- Concurrent HTTP HEAD probes across AWS regions with per-worker threads and shared atomic coordination.
- Rolling percentile/variance tracking without heap churn by using fixed-capacity ring buffers.

## Installation

Download the latest release from the [releases page](https://github.com/obviyus/pong/releases) and add it to your `$PATH`.

```bash
wget https://github.com/obviyus/pong/releases/download/v1.0.9/pong-macos-aarch64
chmod +x pong-macos-aarch64
mv pong-macos-aarch64 /usr/local/bin/pong
```


## Building `pong`

```bash
zig build run
```
