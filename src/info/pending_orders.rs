use std::sync::Arc;
use dashmap::DashMap;
use log::info;
use chrono::{NaiveDateTime, TimeZone};
use solana_sdk::pubkey::Pubkey;
use tokio_util::sync::CancellationToken;
use crate::executor::order::{Order, OrderKind};
use crate::executor::Backup;

pub async fn print_pending_orders() {
    let dummy_cancel_token = CancellationToken::new();

    // Load pending orders
    let pending_orders: Arc<DashMap<Pubkey, Vec<Order>>> = Arc::new(DashMap::new());
    let backup = Backup::new(
        pending_orders.clone(),
        dummy_cancel_token,
    );
    backup.load_pending_orders().await;

    if pending_orders.is_empty() {
        info!("🚫 No pending orders at the moment.");
        return;
    }

    // Collect orders into a vector and sort by created_at timestamp
    let mut orders = pending_orders.iter()
        .map(|entry| entry.value().clone())
        .flatten()
        .collect::<Vec<_>>();

    // Sort orders by created_at timestamp
    orders.sort_by(|a, b| b.created_at.cmp(&a.created_at));

    // Enhanced output with formatting
    info!("\n📋 Pending Orders\n{}", "=".repeat(140));

    let header = get_pending_orders_header();
    let separator = "-".repeat(header.len());

    let mut table = String::new();
    table.push_str(&header);
    table.push('\n');
    table.push_str(&separator);

    for order in orders {
        let row = format_pending_order_row(&order);
        table.push('\n');
        table.push_str(&row);
    }

    // Log the table
    info!("\n{}", table);
}

fn get_pending_orders_header() -> String {
    format!(
        "{:<36} | {:<44} | {:<10} | {:<12} | {:<12} | {:<12} | {:<19} | {:<19}",
        "Order ID", "Pool", "Direction", "Kind", "Amount In", "Min Amount Out", "Created At", "Execution Time"
    )
}

fn format_pending_order_row(order: &Order) -> String {
    let created_at = chrono::Utc.timestamp_opt(order.created_at as i64, 0)
        .single()
        .unwrap()
        .to_rfc3339();

    let execution_time = if order.delayed_at > 0 {
        chrono::Utc.timestamp_opt(order.delayed_at as i64, 0)
            .single()
            .unwrap()
            .to_rfc3339()
    } else {
        "Immediate".to_string()
    };

    format!(
        "{:<36} | {:<44} | {:<10} | {:<12} | {:<12.9} | {:<12.9} | {:<19} | {:<19}",
        order.id,
        order.pool_keys.id.to_string(),
        order.direction,
        order.kind,
        order.amount,
        order.limit_amount,
        created_at,
        execution_time,
    )
}
