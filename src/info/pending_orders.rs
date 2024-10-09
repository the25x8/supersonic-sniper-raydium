use std::sync::Arc;
use dashmap::DashMap;
use log::info;
use chrono::{NaiveDateTime};
use solana_sdk::pubkey::Pubkey;
use crate::trader;
use crate::trader::models::{Order, OrderStatus, OrderKind};

pub async fn print_pending_orders() {
    // Load pending orders
    let pending_orders: Arc<DashMap<Pubkey, Order>> = Arc::new(DashMap::new());
    let backup = trader::backup::Backup::new_for_pending_orders(pending_orders.clone());
    backup.load_pending_orders().await;

    if pending_orders.is_empty() {
        info!("🚫 No pending orders at the moment.");
        return;
    }

    // Collect orders into a vector and sort by created_at timestamp
    let mut orders = pending_orders.iter()
        .map(|entry| entry.value().clone())
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
    let created_at = NaiveDateTime::from_timestamp((order.created_at / 1000) as i64, 0)
        .format("%Y-%m-%d %H:%M:%S")
        .to_string();

    let execution_time = if order.delayed_at > 0 {
        NaiveDateTime::from_timestamp((order.delayed_at / 1000) as i64, 0)
            .format("%Y-%m-%d %H:%M:%S")
            .to_string()
    } else {
        "Immediate".to_string()
    };

    format!(
        "{:<36} | {:<44} | {:<10} | {:<12} | {:<12.10} | {:<12.10} | {:<19} | {:<19}",
        order.id,
        order.pool,
        order.direction,
        order.kind,
        order.amount_in,
        order.min_amount_out,
        created_at,
        execution_time,
    )
}
