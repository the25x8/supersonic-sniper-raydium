mod config;
mod wallet;
mod trader;
mod detector;
mod raydium;
mod error;
mod info;
mod solana;

use detector::Pool;

use std::sync::Arc;
use clap::{Arg, ArgAction, Command};
use log::{error, info};
use solana_client::nonblocking::rpc_client::{RpcClient};
use solana_sdk::commitment_config::CommitmentConfig;
use tokio::sync::mpsc;

#[tokio::main]
async fn main() {
    // Initialize the logger
    env_logger::init();
    print_logo();

    // Set panic hook to exit the process after printing the panic error
    let default_panic = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        default_panic(info);
        std::process::exit(1);
    }));

    // Define CLI using Clap
    let matches = Command::new("Supersonic Sniper Bot")
        .version("0.1.0")
        .author("25x8 <25x8@tuta.io>")
        .about("A high-performance sniper bot for Raydium AMM on Solana")
        .arg(
            Arg::new("config")
                .short('c')
                .long("config")
                .value_name("FILE")
                .help("Sets a custom config file")
                .action(ArgAction::Set),
        )
        .arg(
            Arg::new("wallet_secret_key")
                .short('w')
                .long("wallet-secret-key")
                .value_name("SECRET_KEY")
                .help("Sets the wallet secret key directly")
                .action(ArgAction::Set),
        )
        .subcommand_required(true)
        .subcommand(
            Command::new("run")
                .about("Runs the bot with detector and trader modules"),
        )
        .subcommand(
            Command::new("detector")
                .about("Runs only the detector module"),
        )
        .subcommand(
            Command::new("info")
                .about("Prints information about trades, orders, history, and stats")
                .arg_required_else_help(true)
                .arg(
                    Arg::new("trade")
                        .short('i')
                        .long("trade")
                        .help("Show detailed information about a trade. Provide the trade ID, pool or base token address")
                        .value_name("ID")
                        .action(ArgAction::Append),
                )
                .arg(
                    Arg::new("active")
                        .short('a')
                        .long("active")
                        .help("Show the table with active trades")
                        .action(ArgAction::SetTrue),
                )
                .arg(
                    Arg::new("pending")
                        .short('p')
                        .long("pending")
                        .help("Show list of pending orders")
                        .action(ArgAction::SetTrue),
                )
                .arg(
                    Arg::new("history")
                        .short('t')
                        .long("history")
                        .help("Show the table with trade history")
                        .action(ArgAction::SetTrue),
                )
                .arg(
                    Arg::new("stats")
                        .short('s')
                        .long("stats")
                        .help("Print total stats based on trade history")
                        .action(ArgAction::SetTrue),
                ),
        )
        .get_matches();

    // Load configuration
    let config_name = &"config/config.yml".to_string();
    let config_file = matches.get_one::<String>("config").unwrap_or(config_name);
    let mut app_config = match config::load_config(config_file) {
        Ok(cfg) => cfg,
        Err(e) => {
            error!("Failed to load configuration: {}", e.to_string());
            return;
        }
    };
    // Override wallet secret key if provided via command-line argument
    if let Some(cli_wallet_secret_key) = matches.get_one::<String>("wallet_secret_key") {
        app_config.wallet.secret_key = Some(cli_wallet_secret_key.clone());
    }

    // Wrap the AppConfig in an Arc for shared ownership
    let app_config_arc = Arc::new(app_config);

    // Load the wallet
    let wallet = match wallet::load_wallet(
        app_config_arc.wallet.secret_key.clone(),
        app_config_arc.wallet.file_path.clone(),
    ) {
        Ok(w) => Arc::new(w),
        Err(e) => {
            error!("Failed to load wallet: {}", e);
            return;
        }
    };

    // Create the detected pool channel
    let (pool_tx, pool_rx) = mpsc::channel::<Pool>(10);

    // Initialize solana RPC client with commitment
    let rpc_client = RpcClient::new_with_commitment(
        app_config_arc.rpc.url.to_string().clone(),
        CommitmentConfig::processed(), // for faster fetching
    );

    // Wrap the RpcClient in an Arc for shared ownership
    let rpc_client_arc = Arc::new(rpc_client);

    // Initialize the detector module (core logic)
    let detector = detector::Detector::new(
        app_config_arc.clone(),
        rpc_client_arc.clone(),
        pool_tx,
    );

    // Determine which subcommand was used
    match matches.subcommand() {
        Some(("run", _)) => {
            info!(
                "\n\
                🚀 Supersonic Sniper Bot is initializing...\n\
                ▶️ Version: v0.1\n\
                ▶️ Powered by Solana & Rust\n\
                ───────────────────────────────────────"
            );

            // Initialize the trader module
            let trader_config_arc = Arc::new(app_config_arc.trader.clone());
            let rpc_ws_url = app_config_arc.rpc.ws_url.to_string().clone();
            let mut trader = trader::Trader::new(
                trader_config_arc,
                rpc_client_arc.clone(),
                wallet,
                pool_rx,
                rpc_ws_url.as_str(),
            ).await;

            // Start detector and trader concurrently
            tokio::join!(
                detector.start(),
                trader.start(),
            );
        }
        Some(("detector", _)) => {
            info!("Starting Super Sonic sniper bot in detector mode...");
            detector.start().await;
        }
        Some(("info", sub_m)) => {
            if sub_m.get_flag("active") {
                info::active_trades::print_active_trades().await;
            } else if sub_m.get_flag("pending") {
                info::pending_orders::print_pending_orders().await;
            } else if sub_m.get_flag("history") {
                info::trade_history::print_trades_history().await;
            } else if sub_m.get_flag("stats") {
                info::statistics::print_total_stats().await;
            } else if let Some(query) = sub_m.get_one::<String>("trade") {
                info::trade::print_trade_info(query).await;
            }
        }
        _ => unreachable!(), // If all subcommands are defined above, anything else is unreachable
    }
}

fn print_logo() {
    println!(
        r#"
________                               ________            _____       
__  ___/___  ______________________    __  ___/_______________(_)______
_____ \_  / / /__  __ \  _ \_  ___/    _____ \_  __ \_  __ \_  /_  ___/
____/ // /_/ /__  /_/ /  __/  /        ____/ // /_/ /  / / /  / / /__  
/____/ \__,_/ _  .___/\___//_/         /____/ \____//_/ /_//_/  \___/  
              /_/                                                      

Super Sonic Sniper Bot v0.1 - The High-Speed Trading Bot for Raydium AMM
────────────────────────────────────────────────────────────────────────
"#
    );
}