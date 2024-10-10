use std::fmt;
use log::warn;
use serde::{Deserialize, Serialize};
use solana_program::pubkey::Pubkey;
use uuid::Uuid;

use crate::config::StrategyConfig;
use crate::detector::Pool;
use crate::trader::current_timestamp;

/// Trade is the main representation of work in the Trader module.
/// Each trade is a set of buy and sell orders that will be executed,
/// it also contains the strategy, status, information about the pool,
/// market data, and timestamps for tracking the trade lifecycle.
///
/// The trade is stored in memory and can be serialized to disk for backup,
/// and restored in case of a crash or restart of the application.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Trade {
    pub id: Uuid,

    pub pool: Pool, // Detected pool for the trade with its metadata
    pub exchange: TradeExchange,
    pub strategy_name: String, // The name of the strategy used for the trade
    pub strategy: StrategyConfig, // The strategy used for the trade
    pub status: TradeStatus, // The status of the trade
    pub wallet: Pubkey, // SOL wallet address used to send txs and receive tokens

    // Amounts in and out for the trade in terms of quote tokens
    pub quote_in_amount: f64,
    pub quote_out_amount: f64,

    // Snapshots of the base prices at the time of the trade
    pub buy_price: f64,
    pub sell_price: f64,

    // Profit percentage and amount
    pub profit_percent: f64,
    pub profit_amount: f64,

    // All related buy and sell orders
    pub buy_order: Option<Order>,
    pub hold_time_order: Option<Order>,
    pub stop_loss_order: Option<Order>,
    pub take_profit_order: Option<Order>,

    // Timestamps for tracking the trade lifecycle
    pub created_at: u64,
    pub updated_at: u64,
    pub completed_at: u64,
}

impl Trade {
    /// Creates a new instance of the Trade struct.
    pub fn new(
        exchange: TradeExchange,
        pool: Pool,
        wallet: Pubkey,
        strategy_name: String,
        strategy: StrategyConfig,
    ) -> Self {
        let timestamp = current_timestamp();
        Trade {
            id: Uuid::new_v4(),
            wallet,
            pool,
            exchange,
            strategy_name,
            strategy,
            buy_price: 0.0,
            sell_price: 0.0,
            status: TradeStatus::Created,
            quote_in_amount: 0.0,
            quote_out_amount: 0.0,
            profit_percent: 0.0,
            profit_amount: 0.0,
            buy_order: None,
            stop_loss_order: None,
            take_profit_order: None,
            hold_time_order: None,
            created_at: timestamp,
            updated_at: 0,
            completed_at: 0,
        }
    }

    pub fn is_completed(&self) -> bool {
        self.status == TradeStatus::Completed
    }

    /// Registers the buy order for the trade.
    /// It doesn't change status, only updates the trade struct.
    /// Status will be changed when the sell order is confirmed.
    pub fn initiate_buy(&mut self, order: Order) -> Result<(), String> {
        if self.buy_order.is_some() {
            warn!("Buy order already exists for the trade");
            return Err("Buy order already exists for the trade".to_string());
        }

        // Add buy order in amount to the trade metadata
        self.quote_in_amount = order.amount_in;
        self.buy_order = Some(order); // Register the buy order for the trade
        self.updated_at = current_timestamp();

        Ok(())
    }

    /// Registers the sell order for the trade.
    /// Does not change the status, only updates the trade struct.
    pub fn initiate_sell(&mut self, order: Order) -> Result<(), String> {
        // Register the sell order based on its kind
        match order.kind {
            OrderKind::HoldTime => self.hold_time_order = Some(order.clone()),
            OrderKind::TakeProfit => self.take_profit_order = Some(order.clone()),
            OrderKind::StopLoss => self.stop_loss_order = Some(order.clone()),
            _ => return Err("Invalid kind of the order".to_string()),
        }

        self.updated_at = current_timestamp();

        Ok(())
    }

    /// Completes the trade after the sell order is confirmed in the blockchain.
    /// It calculates the profit amount, profit percentage, and sell price of
    /// the trade, and updates the status of the sell order.
    pub fn sell(
        &mut self,
        kind: OrderKind,
        in_amount: f64, // In amount is the amount of base tokens
        out_amount: f64, // Out amount is the amount of quote tokens
        tx_id: String,
        signature: String,
        timestamp: u64,
    ) -> Result<(), String> {
        // Skip if the trade is already completed or cancelled
        if self.status == TradeStatus::Completed || self.status == TradeStatus::Cancelled {
            return Err("Trade is already completed or cancelled".to_string());
        }

        // Update the trade status after the sell order is confirmed
        self.quote_out_amount = out_amount;
        self.status = TradeStatus::Completed;

        // Calculate sell price
        self.sell_price = out_amount / in_amount;

        // Mark the trade as completed
        let sell_timestamp = current_timestamp();
        self.completed_at = sell_timestamp;
        self.updated_at = sell_timestamp;

        // Calc the profit amount and percentage for the trade
        self.profit_amount = self.quote_out_amount - self.quote_in_amount;
        self.profit_percent = (self.profit_amount / self.quote_in_amount) * 100.0;

        // Based on the kind get the sell order and update its status
        let sell_order = match kind {
            OrderKind::HoldTime => {
                if let Some(hold_time_order) = self.hold_time_order.as_mut() {
                    hold_time_order
                } else {
                    return Err("Hold time order not found".to_string());
                }
            }
            OrderKind::TakeProfit => {
                if let Some(take_profit_order) = self.take_profit_order.as_mut() {
                    take_profit_order
                } else {
                    return Err("Take profit order not found".to_string());
                }
            }
            OrderKind::StopLoss => {
                if let Some(stop_loss_order) = self.stop_loss_order.as_mut() {
                    stop_loss_order
                } else {
                    return Err("Stop loss order not found".to_string());
                }
            }
            _ => return Err("Invalid kind of the order".to_string()),
        };

        // Assign the blockchain tx details and timestamps
        sell_order.tx_id = Some(tx_id);
        sell_order.signature = Some(signature);
        sell_order.status = OrderStatus::Completed;
        sell_order.confirmed_at = timestamp;

        Ok(())
    }

    /// Completes the buy order for the trade after it is confirmed in the blockchain.
    /// It calculates the buy price and updates the status of the buy order.
    pub fn buy(
        &mut self,
        in_amount: f64, // In amount is the amount of quote tokens
        out_amount: f64, // Out amount is the amount of base tokens
        tx_id: String,
        signature: String,
        timestamp: u64,
    ) {
        self.buy_price = in_amount / out_amount; // Formula: quote_token_amount / base_token_amount

        // Update the trade status after the buy order is confirmed
        self.status = TradeStatus::Active;
        self.updated_at = current_timestamp();

        // Update the buy order status
        if let Some(buy_order) = self.buy_order.as_mut() {
            buy_order.status = OrderStatus::Completed;
            buy_order.tx_id = Some(tx_id);
            buy_order.signature = Some(signature);
            buy_order.confirmed_at = timestamp;
        }
    }

    /// Archives the trade after it is completed.
    /// All orders except the completed one are cancelled.
    pub fn archive(&mut self) {
        // Get the completed order either take profit, stop loss or hold time
        let completed_order = self.get_completed_sell_order();

        // Handle the case where the completed order is not found
        if completed_order.is_none() {
            warn!("No completed sell order found for the trade");
            return;
        }

        // Get the ID of the completed order
        let completed_order_id = completed_order.unwrap().id;

        // If hold time order exists and it is not the completed order, cancel it
        let mut hold_time_order = self.hold_time_order.clone();
        if hold_time_order.is_some() {
            let hold_time_order = hold_time_order.as_mut().unwrap();
            if hold_time_order.id != completed_order_id {
                hold_time_order.status = OrderStatus::Cancelled;
                hold_time_order.cancelled_at = current_timestamp();
            }
        }

        // If take profit order exists and it is not the completed order, cancel it
        let mut take_profit_order = self.take_profit_order.clone();
        if take_profit_order.is_some() {
            let take_profit_order = take_profit_order.as_mut().unwrap();
            if take_profit_order.id != completed_order_id {
                take_profit_order.status = OrderStatus::Cancelled;
                take_profit_order.cancelled_at = current_timestamp();
            }
        }

        // If stop loss order exists and it is not the completed order, cancel it
        let mut stop_loss_order = self.stop_loss_order.clone();
        if stop_loss_order.is_some() {
            let stop_loss_order = stop_loss_order.as_mut().unwrap();
            if stop_loss_order.id != completed_order_id {
                stop_loss_order.status = OrderStatus::Cancelled;
                stop_loss_order.cancelled_at = current_timestamp();
            }
        }
    }

    /// Returns the completed order for the trade either hold time, take profit or stop loss.
    pub fn get_completed_sell_order(&self) -> Option<&Order> {
        if self.take_profit_order.is_some() && self.take_profit_order.as_ref().unwrap().status == OrderStatus::Completed {
            self.take_profit_order.as_ref()
        } else if self.hold_time_order.is_some() && self.hold_time_order.as_ref().unwrap().status == OrderStatus::Completed {
            self.hold_time_order.as_ref()
        } else if self.stop_loss_order.is_some() && self.stop_loss_order.as_ref().unwrap().status == OrderStatus::Completed {
            self.stop_loss_order.as_ref()
        } else {
            None
        }
    }
}


#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum TradeStatus {
    Created = 1,
    Active = 2,
    Cancelled = 3,
    Completed = 4,
}

impl fmt::Display for TradeStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TradeStatus::Created => write!(f, "Created"),
            TradeStatus::Active => write!(f, "Active"),
            TradeStatus::Cancelled => write!(f, "Cancelled"),
            TradeStatus::Completed => write!(f, "Completed"),
        }
    }
}

/// The orders are the main building blocks of the trade.
/// Each trade consists of a buy order and one or more sell orders.
/// The order provides abstract fields to contain to swap in and out
/// amounts, slippage, delay, and other details.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Order {
    pub id: Uuid,
    pub pool: Pubkey, // The pool address is a main key for the order
    pub trade_id: Uuid, // The trade ID associated with the order
    pub direction: OrderDirection,
    pub status: OrderStatus,
    pub kind: OrderKind,
    pub delay: u64, // Delay in milliseconds before executing the order

    // Swap fields are the same for swapIn and swapOut orders
    pub amount_in: f64,
    pub min_amount_out: f64,

    // Blockchain tx details and timestamps
    pub tx_id: Option<String>,
    pub signature: Option<String>,
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
        pool: Pubkey,
        amount_in: f64,
        min_amount_out: f64,
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
            pool,
            delay,
            kind,
            amount_in,
            trade_id,
            min_amount_out,
            delayed_at,
            tx_id: None,
            signature: None,
            confirmed_at: 0,
            cancelled_at: 0,
            status: OrderStatus::Open,
            created_at: current_timestamp(),
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

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum TradeExchange {
    Raydium = 1,
    Serum = 2,
}
