use std::sync::Arc;
use chrono::NaiveDateTime;
use dashmap::DashMap;
use log::info;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use crate::trader;
use crate::trader::trade::Trade;

/// Prints the active trades to the console, trades sorted by created_at timestamp in descending order.
pub async fn print_trades_history() {
    let dummy_cancel_token = CancellationToken::new();
    let trades_history: Arc<Mutex<Vec<Trade>>> = Arc::new(Mutex::new(Vec::new()));
    let backup = trader::backup::Backup::new(
        Arc::new(DashMap::new()),
        dummy_cancel_token,
    );
    backup.load_trades_history(trades_history.clone()).await;

    // Print trade history
    let mut trades_history = trades_history.lock().await;
    if trades_history.is_empty() {
        info!("🚫 No trade history at the moment.");
        return;
    }

    // Sort trades by created_at timestamp in descending order
    trades_history.sort_by(|a, b| b.created_at.cmp(&a.created_at));

    // Print the trade history
    log_trade_history(trades_history.to_vec());
}

fn get_trade_history_header() -> String {
    format!(
        "{:<46} | {:<10} | {:<12} | {:<12} | {:<12} | {:<12} | {:<14} | {:<19}",
        "Pool", "Status", "Amount", "Buy Price", "Sell Price", "Profit (%)", "Profit Amount", "Created At"
    )
}

fn format_trade_row(trade: &Trade) -> String {
    let created_at = NaiveDateTime::from_timestamp(trade.created_at as i64, 0)
        .format("%Y-%m-%d %H:%M:%S")
        .to_string();

    format!(
        "{:<46} | {:<10} | {:<12.9} | {:<12.9} | {:<12.9} | {:<12.2} | {:<14.9} | {:<19}",
        trade.pool_keys.id.to_string(),
        trade.get_completed_sell_order().unwrap().kind,
        trade.quote_in_amount,
        trade.buy_price,
        trade.sell_price,
        trade.profit_percent,
        trade.profit_amount,
        created_at,
    )
}

fn log_trade_history(trades: Vec<Trade>) {
    let header = get_trade_history_header();
    let separator = "-".repeat(header.len());

    let mut table = String::new();
    table.push_str(&header);
    table.push('\n');
    table.push_str(&separator);

    for trade in trades {
        let row = format_trade_row(&trade);
        table.push('\n');
        table.push_str(&row);
    }

    // Log the table
    info!("\n{}", table);
}
