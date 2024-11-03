use std::fmt;
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use solana_sdk::pubkey::Pubkey;
use crate::config::ExecutorType;
use crate::detector::PoolKeys;
use crate::trader::current_timestamp;

/// The orders are the main building blocks of the trade.
/// Each trade consists of a buy order and one or more sell orders.
/// The order provides abstract fields to contain to swap in and out
/// amounts, slippage, delay, and other details.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Order {
    pub id: Uuid,
    pub pool_keys: PoolKeys,
    pub trade_id: Uuid, // The trade ID associated with the order
    pub direction: OrderDirection,
    pub status: OrderStatus,
    pub kind: OrderKind,
    pub delay: u64, // Delay in milliseconds before executing the order

    // Meta information about the tokens
    pub in_mint: Pubkey,
    pub out_mint: Pubkey,
    pub in_decimals: u8,
    pub out_decimals: u8,

    // Swap fields are the same for swapIn and swapOut orders
    pub amount: u64,
    pub limit_amount: u64, // Can be min or max amount depending on the order type

    // Execution details
    pub executor: ExecutorType,
    pub executor_bribe: u64, // Amount paid to the executor for the order

    // Blockchain tx details and timestamps
    pub tx_id: Option<String>,
    pub fee: u64,
    pub compute_units_consumed: u64,

    // Confirmed balance difference on the out account after the order is executed.
    pub balance_change: u64,

    // Timestamps
    pub created_at: u64,
    pub confirmed_at: u64,
    pub cancelled_at: u64,
    pub delayed_at: u64, // Timestamp when the order is delayed
}

impl Order {
    pub fn new(
        direction: OrderDirection,
        trade_id: Uuid,
        kind: OrderKind,
        pool_keys: &PoolKeys,
        amount: u64,
        limit_amount: u64,
        in_mint: &Pubkey,
        out_mint: &Pubkey,
        in_decimals: u8,
        out_decimals: u8,
        executor: ExecutorType,
        bribe: u64,
        delay: u64,
    ) -> Self {
        // Delayed at timestamp is current timestamp + delay if delay is greater than 0.
        let delayed_at = if delay > 0 {
            current_timestamp() + (delay / 1000)
        } else {
            0
        };
        Self {
            id: Uuid::new_v4(),
            direction,
            delay,
            kind,
            in_decimals,
            out_decimals,
            trade_id,
            delayed_at,
            executor,
            amount,
            limit_amount,
            fee: 0,
            tx_id: None,
            confirmed_at: 0,
            cancelled_at: 0,
            in_mint: *in_mint,
            out_mint: *out_mint,
            status: OrderStatus::Open,
            pool_keys: pool_keys.clone(),
            created_at: current_timestamp(),
            executor_bribe: bribe,
            balance_change: 0,
            compute_units_consumed: 0,
        }
    }

    /// Adds the blockchain transaction details to the order.
    pub fn confirm(
        &mut self,
        tx_id: &str,
        balance_change: u64,
        fee: u64,
        units_consumed: u64,
        timestamp: u64,
    ) {
        self.tx_id = Some(tx_id.to_string());
        self.status = OrderStatus::Completed;
        self.balance_change = balance_change; // Balance change after the order is executed
        self.compute_units_consumed = units_consumed;
        self.fee = fee; // Fee paid for the transaction
        self.confirmed_at = timestamp;
    }

    pub fn cancel(&mut self, timestamp: u64) {
        self.status = OrderStatus::Cancelled;
        self.cancelled_at = timestamp;
    }

    /// Updates the order status and does the necessary timestamp updates.
    pub fn update_status(&mut self, status: OrderStatus) {
        self.status = status;
        match status {
            OrderStatus::Completed => self.confirmed_at = current_timestamp(),
            OrderStatus::Cancelled => self.cancelled_at = current_timestamp(),
            _ => {}
        }
    }
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum OrderKind {
    SimpleBuy = 1,
    HoldTime = 2,
    TakeProfit = 3,
    StopLoss = 4,

    /// Special order that can be created on the fly to liquidate position.
    Emergency = 5,
}

impl fmt::Display for OrderKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OrderKind::SimpleBuy => write!(f, "SimpleBuy"),
            OrderKind::HoldTime => write!(f, "HoldTime"),
            OrderKind::TakeProfit => write!(f, "TakeProfit"),
            OrderKind::StopLoss => write!(f, "StopLoss"),
            OrderKind::Emergency => write!(f, "Emergency"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum OrderDirection {
    QuoteIn,
    QuoteOut,
}

impl fmt::Display for OrderDirection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OrderDirection::QuoteIn => write!(f, "Quote In"),
            OrderDirection::QuoteOut => write!(f, "Quote Out"),
        }
    }
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum OrderStatus {
    Open = 1,
    Completed = 2,
    Cancelled = 3,
    Expired = 4,
}

impl fmt::Display for OrderStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OrderStatus::Open => write!(f, "Open"),
            OrderStatus::Completed => write!(f, "Completed"),
            OrderStatus::Cancelled => write!(f, "Cancelled"),
            OrderStatus::Expired => write!(f, "Expired"),
        }
    }
}
