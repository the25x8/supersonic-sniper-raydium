use std::collections::HashMap;
use std::sync::Arc;
use dashmap::DashMap;
use log::info;
use solana_program::pubkey::Pubkey;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use crate::trader;
use crate::trader::models::{OrderKind, Trade};

/// Counts the number of active trades, total spent, and profit,
/// number of profitable, and number of unprofitable trades.
pub async fn print_total_stats() {
    // Load trades history
    let dummy_cancel_token = CancellationToken::new();
    let trades_history: Arc<Mutex<Vec<Trade>>> = Arc::new(Mutex::new(Vec::new()));
    let backup = trader::backup::Backup::new_for_trades(
        Arc::new(DashMap::new()),
        dummy_cancel_token,
    );
    backup.load_trades_history(trades_history.clone()).await;

    let trades_history = trades_history.lock().await;

    // HashMap to store stats per wallet
    let mut wallet_stats: HashMap<Pubkey, WalletStats> = HashMap::new();

    // Collect completed trades and calculate totals per wallet
    for trade in trades_history.iter() {
        let completed_sell_order = trade.get_completed_sell_order();
        if completed_sell_order.is_none() {
            continue;
        }

        let completed_sell_order = completed_sell_order.unwrap();

        // Get the wallet pubkey
        let wallet = trade.wallet;

        // Get or insert the stats struct for this wallet
        let stats = wallet_stats.entry(wallet).or_insert_with(WalletStats::new);

        // Update stats
        stats.total_spent += trade.quote_in_amount;
        stats.total_profit += trade.profit_amount;
        stats.completed_trades.push(trade.clone());

        // Update counts
        match completed_sell_order.kind {
            OrderKind::HoldTime => {
                if trade.profit_percent > 0.0 {
                    stats.profitable_trades += 1;
                } else {
                    stats.unprofitable_trades += 1;
                }
            }
            OrderKind::TakeProfit => {
                stats.profitable_trades += 1;
            }
            OrderKind::StopLoss => {
                stats.unprofitable_trades += 1;
            }
            _ => {}
        }
    }

    // Now, for each wallet, compute the stats and print them
    for (wallet, stats) in wallet_stats.iter() {
        let total_trades = stats.completed_trades.len();
        let total_profit_percent = if stats.total_spent > 0.0 {
            (stats.total_profit / stats.total_spent) * 100.0
        } else {
            0.0
        };

        // Calculate the profitable percentage of the last 10 trades
        let last_10_trades = stats
            .completed_trades
            .iter()
            .rev()
            .take(10)
            .collect::<Vec<_>>();
        let mut last_10_profitable_trades = 0;

        for trade in last_10_trades.iter() {
            if trade.profit_amount > 0.0 {
                last_10_profitable_trades += 1;
            }
        }

        let last_10_profit_percent = if !last_10_trades.is_empty() {
            (last_10_profitable_trades as f64 / last_10_trades.len() as f64) * 100.0
        } else {
            0.0
        };

        // Enhanced log output with emojis
        info!(
            "\n📊 Trade History Statistics for Wallet: {}\n\
            =================================================\n\
            📝 Trades: {}\n\
            💰 Turnover: {:.10} SOL\n\
            💵 Total Profit: {:.10} SOL {}\n\
            📈 Total Profit Percent: {:.2}% {}\n\
            ✅ Successful Trades: {}\n\
            ❌ Unsuccessful Trades: {}\n\
            🔟 Last 10 Trades Profitability: {:.2}% {}\n",
            wallet,
            total_trades,
            stats.total_spent,
            stats.total_profit,
            if stats.total_profit >= 0.0 { "📈" } else { "📉" },
            total_profit_percent,
            if total_profit_percent >= 0.0 { "📈" } else { "📉" },
            stats.profitable_trades,
            stats.unprofitable_trades,
            last_10_profit_percent,
            if last_10_profit_percent >= 50.0 { "👍" } else { "👎" },
        );
    }
}

// Define a struct to hold stats per wallet
struct WalletStats {
    total_spent: f64,
    total_profit: f64,
    profitable_trades: usize,
    unprofitable_trades: usize,
    completed_trades: Vec<Trade>,
}

impl WalletStats {
    fn new() -> Self {
        WalletStats {
            total_spent: 0.0,
            total_profit: 0.0,
            profitable_trades: 0,
            unprofitable_trades: 0,
            completed_trades: Vec::new(),
        }
    }
}