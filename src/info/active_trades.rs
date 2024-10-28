use std::sync::Arc;
use chrono::{TimeZone, Utc};
use dashmap::DashMap;
use log::info;
use solana_program::pubkey::Pubkey;
use tokio_util::sync::CancellationToken;
use crate::trader;
use crate::trader::trade::Trade;

pub async fn print_active_trades() {
    let dummy_cancel_token = CancellationToken::new();

    // Load active trades
    let active_trades: Arc<DashMap<Pubkey, Vec<Trade>>> = Arc::new(DashMap::new());
    let backup = trader::backup::Backup::new(
        active_trades.clone(),
        dummy_cancel_token,
    );
    backup.load_active_trades().await;

    if active_trades.is_empty() {
        info!("🚫 No active trades at the moment.");
        return;
    }

    let mut trades = active_trades.iter()
        .map(|entry| entry.value().clone())
        .collect::<Vec<_>>();

    // Sort trades by created_at timestamp in descending order
    trades.sort_by(|a, b| b[0].created_at.cmp(&a[0].created_at));

    // Enhanced output with formatting
    info!("\n🔥 Active Trades\n{}", "=".repeat(100));

    let header = get_active_trades_header();
    let separator = "-".repeat(header.len());

    let mut table = String::new();
    table.push_str(&header);
    table.push('\n');
    table.push_str(&separator);

    for trade in trades {
        let row = format_active_trade_row(&trade[0]);
        table.push('\n');
        table.push_str(&row);
    }

    // Log the table
    info!("\n{}", table);
}

fn get_active_trades_header() -> String {
    format!(
        "{:<44} | {:<12} | {:<15} | {:<12} | {:<12} | {:<14} | {:<19}",
        "Pool", "Amount", "Tokens", "Buy Price", "Profit (%)", "Profit Amount", "Created At"
    )
}

fn format_active_trade_row(trade: &Trade) -> String {
    let created_at = Utc
        .timestamp_opt(trade.created_at as i64, 0)
        .unwrap()
        .to_rfc3339();

    let amount = format!("{:.9}", trade.quote_in_amount);

    let tokens = if trade.buy_price > 0.0 {
        let tokens_bought = trade.quote_in_amount / trade.buy_price;
        format!("{:.9}", tokens_bought)
    } else {
        "N/A".to_string()
    };

    let buy_price = if trade.buy_price > 0.0 {
        format!("{:.9}", trade.buy_price)
    } else {
        "N/A".to_string()
    };

    let profit_percent = if trade.profit_percent != 0.0 {
        format!("{:.2}", trade.profit_percent)
    } else {
        "N/A".to_string()
    };

    let profit_amount = if trade.profit_amount != 0.0 {
        format!("{:.9}", trade.profit_amount)
    } else {
        "N/A".to_string()
    };

    format!(
        "{:<44} | {:<12} | {:<15} | {:<12} | {:<12} | {:<14} | {:<19}",
        trade.pool_keys.id.to_string(),
        amount,
        tokens,
        buy_price,
        profit_percent,
        profit_amount,
        created_at,
    )
}