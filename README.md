# pong

![pong-demo](assets/pong-demo.avif)

Pong is a simple CLI program that I use to ping different AWS regions. It's written in Rust and uses the `ratatui` library for for TUI.

I wrote this because I purchased a new ethernet cable and wanted to compare latency between WiFi and ethernet.

## Installation

Download the latest release from the [releases page](https://github.com/obviyus/pong/releases) and add it to your `$PATH`.

```bash
wget https://github.com/obviyus/pong/releases/download/v1.0.9/pong-macos-aarch64
chmod +x pong-macos-aarch64
mv pong-macos-aarch64 /usr/local/bin/pong
```


## Building `pong`

```bash
cargo run --release
```