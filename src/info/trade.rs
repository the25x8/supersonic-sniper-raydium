use std::sync::Arc;
use std::str::FromStr;
use dashmap::DashMap;
use tokio::sync::Mutex;
use uuid::Uuid;
use log::info;
use chrono::{NaiveDateTime, TimeZone};
use solana_sdk::pubkey::Pubkey;
use tokio_util::sync::CancellationToken;
use crate::executor::order::{Order, OrderKind, OrderStatus};
use crate::trader::trade::{Trade, TradeStatus};
use crate::trader::backup::Backup;

pub async fn print_trade_info(query: &str) {
    // Load trades (both active and history)
    let dummy_cancel_token = CancellationToken::new();
    let trades_history: Arc<Mutex<Vec<Trade>>> = Arc::new(Mutex::new(Vec::new()));
    let active_trades: Arc<DashMap<Pubkey, Vec<Trade>>> = Arc::new(DashMap::new());
    let backup = Backup::new(active_trades.clone(), dummy_cancel_token);
    backup.load_trades_history(trades_history.clone()).await;
    backup.load_active_trades().await;

    // Lock trades for reading
    let trades_history = trades_history.lock().await;

    // Collect all trades into a vector
    let mut all_trades = trades_history.clone();
    for entry in active_trades.iter() {
        all_trades.push(entry.value()[0].clone());
    }

    // Search for trades matching the query
    let mut matching_trades = Vec::new();

    // Try to parse query as Uuid (trade id)
    if let Ok(trade_id) = Uuid::parse_str(query) {
        // Search for trade with this id
        if let Some(trade) = all_trades.iter().find(|t| t.id == trade_id) {
            matching_trades.push(trade.clone());
        }
    } else if let Ok(pubkey) = Pubkey::from_str(query) {
        // Search for trades where pool id or base mint matches
        for trade in all_trades.iter() {
            if trade.pool_keys.id == pubkey || trade.pool_keys.base_mint == pubkey {
                matching_trades.push(trade.clone());
            }
        }
    } else {
        // Invalid query, print error
        info!("❌ Invalid query: {}", query);
        return;
    }

    if matching_trades.is_empty() {
        info!("🚫 No trades found matching query: {}", query);
        return;
    }

    // For each matching trade, print all fields and associated orders
    for trade in matching_trades {
        info!(
            "\n\
            📄 Trade Information {}\n\
            🆔 Trade ID:         {}\n\
            📈 Status:           {:?}\n\
            🏦 Pool Address:     {}\n\
            🔑 Base Mint:        {}\n\
            💰 Quote Mint:       {}\n\
            📝 Strategy:         {}\n\
            👛 Wallet:           {}\n\
            📅 Created At:       {}\n\
            📅 Updated At:       {}\n\
            📅 Completed At:     {}\n\
            💸 Quote In Amount:  {:.9}\n\
            💵 Quote Out Amount: {:.9}\n\
            💲 Buy Price:        {:.9}\n\
            💲 Sell Price:       {:.9}\n\
            📊 Profit Percent:   {:.2}%\n\
            💹 Profit Amount:    {:.9}\n",
            "=".repeat(80),
            trade.id,
            trade.status,
            trade.pool_keys.id,
            trade.pool_keys.base_mint,
            trade.pool_keys.quote_mint,
            trade.strategy_name,
            trade.wallet,
            format_timestamp(trade.created_at),
            format_timestamp(trade.updated_at),
            format_timestamp(trade.completed_at),
            trade.quote_in_amount,
            trade.quote_out_amount,
            trade.buy_price,
            trade.sell_price,
            trade.profit_percent,
            trade.profit_amount,
        );

        // Collect all orders associated with this trade
        let mut orders = Vec::new();
        if let Some(ref buy_order) = trade.buy_order {
            orders.push(buy_order.clone());
        }
        if let Some(ref hold_time_order) = trade.hold_time_order {
            orders.push(hold_time_order.clone());
        }
        if let Some(ref take_profit_order) = trade.take_profit_order {
            orders.push(take_profit_order.clone());
        }
        if let Some(ref stop_loss_order) = trade.stop_loss_order {
            orders.push(stop_loss_order.clone());
        }

        // Print orders in a table
        if orders.is_empty() {
            info!("🚫 No orders associated with this trade.");
        } else {
            info!("\n📝 Orders {}", "-".repeat(140));

            let header = get_orders_header();
            let separator = "-".repeat(header.len());

            let mut table = String::new();
            table.push_str(&header);
            table.push('\n');
            table.push_str(&separator);

            for order in orders {
                let row = format_order_row(&order);
                table.push('\n');
                table.push_str(&row);
            }

            // Log the table
            info!("\n{}", table);
        }

        info!("{}", "=".repeat(80));
    }
}

// Helper functions
fn format_timestamp(timestamp: u64) -> String {
    if timestamp == 0 {
        "N/A".to_string()
    } else {
        chrono::Utc.timestamp_opt(timestamp as i64, 0)
            .single()
            .unwrap()
            .to_rfc3339()
    }
}

fn get_orders_header() -> String {
    format!(
        "{:<36} | {:<10} | {:<12} | {:<12} | {:<14} | {:<14} | {:<19} | {:<19}",
        "Order ID", "Direction", "Kind", "Amount In", "Limit Amount", "Status", "Created At", "Executed At"
    )
}

fn format_order_row(order: &Order) -> String {
    let created_at = format_timestamp(order.created_at);
    let executed_at = if order.confirmed_at > 0 {
        format_timestamp(order.confirmed_at)
    } else {
        "Pending".to_string()
    };

    format!(
        "{:<36} | {:<10} | {:<12} | {:<12.9} | {:<14.9} | {:<14} | {:<19} | {:<19}",
        order.id,
        order.direction,
        order.kind,
        order.amount,
        order.limit_amount,
        order.status,
        created_at,
        executed_at,
    )
}
