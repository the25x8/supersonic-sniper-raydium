mod config;
mod wallet;
mod trader;
mod raydium;
mod error;
mod info;
mod solana;
mod executor;
mod market;
mod detector;

use std::sync::Arc;
use std::time::Duration;
use clap::{Arg, ArgAction, Command};
use log::{error, info};
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::commitment_config::CommitmentConfig;
use tokio::signal;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

const VERSION: &str = "0.2";

#[tokio::main]
async fn main() {
    // Initialize the logger
    env_logger::init();
    print_logo();

    // Set panic hook to exit the process after printing the panic error
    // let default_panic = std::panic::take_hook();
    // std::panic::set_hook(Box::new(move |info| {
    //     default_panic(info);
    //     std::process::exit(1);
    // }));

    // Create a new CancellationToken
    let shutdown_token = CancellationToken::new();

    // Spawn the application task using the tracker and clone the receiver for each task
    let shutdown_token_clone = shutdown_token.clone();
    let main_task = tokio::spawn(run_app(shutdown_token_clone));

    // Listen for external Ctrl+C signal or internal shutdown request
    tokio::select! {
        _ = signal::ctrl_c() => {
            info!("Ctrl+C signal received. Shutting down the bot...");
        }
        _ = shutdown_token.cancelled() => {}
    }

    // Send shutdown signal to all tasks using the token
    shutdown_token.cancel();

    // Wait for the main task to complete
    main_task.await.unwrap();

    // Graceful shutdown of the bot
    info!("All tasks closed! Bot has been shut down.");
}

/// Run the bot program based on the provided CLI arguments.
async fn run_app(shutdown_token: CancellationToken) {
    // Define CLI using Clap
    let matches = Command::new("Supersonic Sniper Bot")
        .version(VERSION)
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
    let (pool_tx, pool_rx) = mpsc::channel::<detector::AMMPool>(10);

    // Initialize solana RPC client with commitment
    let rpc_client_timeout = Duration::from_secs(10);
    let confirm_tx_initial_timeout = Duration::from_secs(20);
    let rpc_client = RpcClient::new_with_timeouts_and_commitment(
        app_config_arc.rpc.url.to_string().clone(),
        rpc_client_timeout,
        CommitmentConfig::processed(),
        confirm_tx_initial_timeout,
    );

    // Wrap the RpcClient in an Arc for shared ownership
    let rpc_client_arc = Arc::new(rpc_client);

    // Create task tracker for graceful shutdown of all background tasks.
    let tracker = TaskTracker::new();

    // Initialize the detector module as a future task
    let detector_future = detector::run(
        app_config_arc.clone(),
        rpc_client_arc.clone(),
        pool_tx,
        shutdown_token.clone(),
    );

    match matches.subcommand() {
        // Run the bot with detector and trader modules
        Some(("run", _)) => {
            info!(
                "\n\
                🚀 Supersonic Sniper Bot is initializing...\n\
                ▶️ Version: v{}\n\
                ───────────────────────────────────────\n",
                VERSION
            );

            // Initialize the trader module
            let app_config_clone = app_config_arc.clone();
            let rpc_ws_url = app_config_arc.rpc.ws_url.to_string().clone();
            let mut trader = trader::Trader::new(
                app_config_clone,
                rpc_client_arc.clone(),
                wallet,
                pool_rx,
                rpc_ws_url.as_str(),
                shutdown_token.clone(),
            ).await;

            // Start detector and trader tasks
            tracker.spawn(detector_future);
            tracker.spawn(async move {
                trader.start().await;
            });
        }

        // Run the detector module only
        Some(("detector", _)) => {
            info!(
                "\n\
                👁️ Detector Module is running...\n\
                ▶️ Version: v0.1\n\
                ───────────────────────────────────────\n"
            );

            tracker.spawn(detector_future);
        }

        // Print information about trades, orders, history, and stats
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

            shutdown_token.cancel(); // Close the app after info is printed
        }

        _ => unreachable!(), // If all subcommands are defined above, anything else is unreachable
    }

    // Close the tracker after tasks start
    tracker.close();

    // Wait for all tasks to complete
    tracker.wait().await;
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

Super Sonic Sniper Bot - The High-Speed Trading Bot for Raydium AMM
────────────────────────────────────────────────────────────────────────
"#
    );
}