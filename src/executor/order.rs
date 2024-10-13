use std::fmt;
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use solana_program::pubkey::Pubkey;
use crate::config::ExecutorType;
use crate::detector::Pool;
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
    pub amount: f64,
    pub limit_amount: f64, // Can be min or max amount depending on the order type

    // Execution details
    pub executor: ExecutorType,
    pub executor_bribe: Option<f64>, // Amount paid to the executor for the order

    // Blockchain tx details and timestamps
    pub tx_id: Option<String>,
    pub feed_lamports: Option<u64>,
    pub confirmed_at: u64,
    pub cancelled_at: u64,
    pub created_at: u64,
    pub delayed_at: u64, // Timestamp when the order is delayed
}

impl Order {
    pub fn new(
        direction: OrderDirection,
        trade_id: Uuid,
        kind: OrderKind,
        pool_keys: &PoolKeys,
        amount_in: f64,
        min_amount_out: f64,
        in_mint: &Pubkey,
        out_mint: &Pubkey,
        in_decimals: u8,
        out_decimals: u8,
        executor: ExecutorType,
        bribe: Option<f64>,
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
            amount: amount_in,
            limit_amount: min_amount_out,
            tx_id: None,
            confirmed_at: 0,
            cancelled_at: 0,
            in_mint: *in_mint,
            out_mint: *out_mint,
            status: OrderStatus::Open,
            pool_keys: pool_keys.clone(),
            created_at: current_timestamp(),
            executor_bribe: bribe,
            feed_lamports: None,
        }
    }

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
}

impl fmt::Display for OrderKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OrderKind::SimpleBuy => write!(f, "SimpleBuy"),
            OrderKind::HoldTime => write!(f, "HoldTime"),
            OrderKind::TakeProfit => write!(f, "TakeProfit"),
            OrderKind::StopLoss => write!(f, "StopLoss"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum OrderDirection {
    BaseIn,
    BaseOut,
}

impl OrderDirection {
    pub fn value(&self) -> String {
        match *self {
            OrderDirection::BaseIn => "base_in".to_string(),
            OrderDirection::BaseOut => "base_out".to_string(),
        }
    }
}

impl fmt::Display for OrderDirection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OrderDirection::BaseIn => write!(f, "BaseIn"),
            OrderDirection::BaseOut => write!(f, "BaseOut"),
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
