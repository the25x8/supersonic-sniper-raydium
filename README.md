# Supersonic Sniper Bot

> :warning: This was an experiment for fun and purely for educational purposes. Ultimately, without robust analytics, it still operates at a loss like any other bot. Furthermore, **meme coins are essentially a scam**.

**Supersonic Sniper** is a ~~high performance~~ (not enough) trading bot designed to execute rapid buy and sell operations on the Raydium decentralized exchange (DEX) within the Solana blockchain network. Leveraging ultra-fast detection and execution capabilities, the bot aims to capitalize on new token listings and market movements with minimal latency.

 > ℹ️ This bot performs quickly enough, but if you want to guarantee entry into the first block with a success rate above 90%, you'll need to develop your own Geyser plugin, purchase a dedicated node, and configure it properly. You will also need a node for sending transactions. Synchronizing a Solana node on your own hardware can be a hassle - or you might have to pay a lot for a ready-made solution from a provider. Additionally, the bot must send multiple transactions to improve its chances: one directly to the Raydium program, and another as CPI via a proxy smart contract. Please note that this client does not work with the Geyser plugin. Based on my understanding of Rust, developing such a plugin could take months. Also, consider monitoring transactions with migrations from pumpfun. I hope you find this information useful!

## Features

### Core Functionality
- Command-line interface (CLI) for easy configuration and control.
- Real-time monitoring of Raydium events, including new liquidity pools and available swaps.
- Customizable watch lists for specific trading pairs or tokens.
- Rapid swap execution with customizable strategies, including percentage-based and time-based selling.

## Getting Started

### Prerequisites

- Rust (stable version 2021 or later)
- Solana CLI installed and configured
- Solana wallets set up

### Installation

```bash
git clone https://github.com/the25x8/supersonic-sniper-bot
cd supersonic-sniper-bot
cargo build --release
```

#### Required files and directories
- `config/`: Contains configuration files for the bot.
- `data/active_trades.json`: List of trades currently active.
- `data/trade_history.json`: History of completed trades.
- `data/pending_orders.json`: Current open orders.

## Configuration

The bot can be configured via a `.yml` file or environment variables. If using environment variables, they should be prefixed with `SSS_`. For example:
```bash
SSS_RPC_URL=https://api.mainnet-beta.solana.com \
SSS_WALLET_SECRET_KEY=your_private_key_here
```

### Usage
Start the bot with default settings:
```bash
cargo run -- --config config/default.yml
```

This `README.md` provides a high-level overview of the bot and how to get started. As we add features, we'll keep updating this documentation.

### Build

To build the project for your current platform, run:
```bash
cargo build --release
```

If you want to further analyze the binary size, you can use cargo bloat:
```bash
cargo install cargo-bloat
cargo bloat --release --crates
```

Once you've built the release version, you can inspect the size of the binary:
```bash
ls -lh target/release/supersonic-sniper-bot
```

This will show a breakdown of which crates are contributing the most to the size of the binary.

 ##### Lint-fix
```bash
cargo clippy --fix
```

#### Cross-Compile for Windows from macOS
First, install the appropriate target:

For GNU ABI (easier for cross-compilation from non-Windows platforms):
```bash
rustup target add x86_64-pc-windows-gnu
cargo build --release --target x86_64-pc-windows-gnu
```

> If you're cross-compiling for MSVC (Microsoft Visual Studio ABI), you'll need to use x86_64-pc-windows-msvc, but this typically requires additional tooling like wine or a Windows system for linking.

#### Compile for Linux from macOS
To cross-compile for Linux from macOS, use the x86_64-unknown-linux-gnu target:
```bash
rustup target add x86_64-unknown-linux-gnu
cargo build --release --target x86_64-unknown-linux-gnu
```

For fully statically linked binaries on Linux (useful when deploying to minimal systems), you can target musl:
```bash
rustup target add x86_64-unknown-linux-musl
cargo build --release --target x86_64-unknown-linux-musl
```

Install the mingw-w64 toolchain for cross-compiling to Windows from macOS.
```bash
brew install mingw-w64
```

After installation, you can build for Windows using:
```bash
CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=x86_64-w64-mingw32-gcc cargo build --release --target x86_64-pc-windows-gnu
```

### Benchmarking Production Build
After creating your optimized build, you can benchmark its performance using tools like hyperfine:
```bash
cargo install hyperfine
hyperfine target/release/supersonic-sniper-bot -- --config config/default.yml
```

This will give you insights into the runtime performance of your optimized binary.
