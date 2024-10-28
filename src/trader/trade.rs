use std::fmt;
use log::warn;
use serde::{Deserialize, Deserializer, Serialize};
use solana_program::native_token::lamports_to_sol;
use solana_program::pubkey::Pubkey;
use uuid::Uuid;

use crate::config::StrategyConfig;
use crate::detector::PoolKeys;
use crate::executor::order::{Order, OrderKind, OrderStatus};
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

    // Pool keys contain the all the keys required to execute the trade
    pub pool_keys: PoolKeys,
    pub exchange: TradeExchange,
    pub strategy_name: String, // The name of the strategy used for the trade
    pub strategy: StrategyConfig, // The strategy used for the trade
    pub status: TradeStatus, // The status of the trade
    pub wallet: Pubkey, // SOL wallet address used to send txs and receive tokens
    pub error: Option<String>, // Error message if the trade failed

    pub base_decimals: u8,
    pub quote_decimals: u8,

    // Amounts in and out for the trade in terms of quote tokens
    pub quote_in_amount: f64,
    pub quote_out_amount: f64,

    // Snapshots of the base prices at the time of the trade
    #[serde(deserialize_with = "deserialize_f64_null_as_nan")]
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

    pub total_fee: f64, // Total fee paid for the trade in lamports
    pub total_compute_units: u64, // Total compute units consumed for the trade
    pub total_bribe: f64, // Total bribe paid to the executors for the trade

    // Timestamps for tracking the trade lifecycle
    pub created_at: u64,
    pub updated_at: u64,
    pub completed_at: u64,
}

impl Trade {
    /// Creates a new instance of the Trade struct.
    pub fn new(
        exchange: TradeExchange,
        pool_keys: &PoolKeys,
        wallet: &Pubkey,
        strategy_name: String,
        strategy: StrategyConfig,
        base_decimals: u8,
        quote_decimals: u8,
    ) -> Self {
        let timestamp = current_timestamp();
        Trade {
            id: Uuid::new_v4(),
            exchange,
            strategy_name,
            strategy,
            base_decimals,
            quote_decimals,
            buy_price: 0.0,
            sell_price: 0.0,
            wallet: *wallet,
            pool_keys: pool_keys.clone(),
            status: TradeStatus::Created,
            quote_in_amount: 0.0,
            quote_out_amount: 0.0,
            profit_percent: 0.0,
            profit_amount: 0.0,
            buy_order: None,
            stop_loss_order: None,
            take_profit_order: None,
            hold_time_order: None,
            error: None,
            total_fee: 0.0,
            total_compute_units: 0,
            total_bribe: 0.0,
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
        let buy_amount = spl_token::amount_to_ui_amount(order.amount, order.in_decimals);
        self.quote_in_amount = buy_amount;

        // Add the order bribe to the total bribe for the trade
        self.total_bribe += lamports_to_sol(order.executor_bribe);
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
    pub fn sell(&mut self, order: &Order) -> Result<(), String> {
        // Skip if the trade is already completed or cancelled
        if self.status == TradeStatus::Completed || self.status == TradeStatus::Cancelled {
            return Err("Trade is already completed or cancelled".to_string());
        }

        // Divide the in amount by the out amount to get the sell price
        let in_amount = spl_token::amount_to_ui_amount(order.amount, order.in_decimals);
        let out_amount = spl_token::amount_to_ui_amount(order.balance_change, order.out_decimals);

        self.sell_price = out_amount / in_amount; // Formula: quote_token / base_token
        self.quote_out_amount = out_amount;
        self.status = TradeStatus::Completed;

        // Update the total fee and compute units for the trade
        self.total_fee += lamports_to_sol(order.fee);
        self.total_compute_units += order.compute_units_consumed;

        // Mark the trade as completed
        let sell_timestamp = current_timestamp();
        self.completed_at = sell_timestamp;
        self.updated_at = sell_timestamp;

        // Calc the profit amount and percentage for the trade
        self.profit_amount = self.quote_out_amount - self.quote_in_amount;
        self.profit_percent = (self.profit_amount / self.quote_in_amount) * 100.0;

        // Replace the sell order with the confirmed order
        match order.kind {
            OrderKind::HoldTime => self.hold_time_order = Some(order.clone()),
            OrderKind::TakeProfit => self.take_profit_order = Some(order.clone()),
            OrderKind::StopLoss => self.stop_loss_order = Some(order.clone()),
            _ => return Err("Invalid kind of the order".to_string()),
        }

        Ok(())
    }

    /// Completes the buy order for the trade after it is confirmed in the blockchain.
    /// It calculates the buy price and updates the status of the buy order.
    pub fn buy(&mut self, order: &Order) {
        // Formula: quote_token / base_token
        let in_amount = spl_token::amount_to_ui_amount(order.amount, order.in_decimals);
        let out_amount = spl_token::amount_to_ui_amount(order.balance_change, order.out_decimals);
        self.buy_price = in_amount / out_amount;

        // Update the trade status after the buy order is confirmed
        self.status = TradeStatus::Active;
        self.updated_at = current_timestamp();

        // Update the total fee and compute units for the trade
        self.total_fee += lamports_to_sol(order.fee);
        self.total_compute_units += order.compute_units_consumed;

        // Replace the buy order with the confirmed order
        self.buy_order = Some(order.clone());
    }

    /// Marks the trade as failed and cancels all orders.
    /// It is used when the trade cannot be completed due to some error.
    pub fn fail(&mut self, error: &str) {
        // Update the trade status
        self.status = TradeStatus::Error;
        self.error = Some(error.to_string());
        self.updated_at = current_timestamp();

        // Cancel buy order only if it is not completed
        if let Some(order) = self.buy_order.as_mut() {
            if order.status != OrderStatus::Completed {
                order.status = OrderStatus::Cancelled;
                order.cancelled_at = current_timestamp();
            }
        }

        // Cancel hold time order only if it is not completed
        if let Some(order) = self.hold_time_order.as_mut() {
            if order.status != OrderStatus::Completed {
                order.status = OrderStatus::Cancelled;
                order.cancelled_at = current_timestamp();
            }
        }

        // Cancel take profit order only if it is not completed
        if let Some(order) = self.take_profit_order.as_mut() {
            if order.status != OrderStatus::Completed {
                order.status = OrderStatus::Cancelled;
                order.cancelled_at = current_timestamp();
            }
        }

        // Cancel stop loss order only if it is not completed
        if let Some(order) = self.stop_loss_order.as_mut() {
            if order.status != OrderStatus::Completed {
                order.status = OrderStatus::Cancelled;
                order.cancelled_at = current_timestamp();
            }
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
        if let Some(hold_time_order) = self.hold_time_order.as_mut() {
            if hold_time_order.id != completed_order_id {
                hold_time_order.status = OrderStatus::Cancelled;
                hold_time_order.cancelled_at = current_timestamp();
            }
        }

        // If take profit order exists and it is not the completed order, cancel it
        if let Some(take_profit_order) = self.take_profit_order.as_mut() {
            if take_profit_order.id != completed_order_id {
                take_profit_order.status = OrderStatus::Cancelled;
                take_profit_order.cancelled_at = current_timestamp();
            }
        }

        // If stop loss order exists and it is not the completed order, cancel it
        if let Some(stop_loss_order) = self.stop_loss_order.as_mut() {
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
    Error = 5,
}

impl fmt::Display for TradeStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TradeStatus::Created => write!(f, "Created"),
            TradeStatus::Active => write!(f, "Active"),
            TradeStatus::Cancelled => write!(f, "Cancelled"),
            TradeStatus::Completed => write!(f, "Completed"),
            TradeStatus::Error => write!(f, "Error"),
        }
    }
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum TradeExchange {
    Raydium = 1,
    Serum = 2,
}

/// A helper to deserialize `f64`, treating JSON null as f64::NAN.
/// See https://github.com/serde-rs/json/issues/202
fn deserialize_f64_null_as_nan<'de, D: Deserializer<'de>>(des: D) -> Result<f64, D::Error> {
    let optional = Option::<f64>::deserialize(des)?;
    Ok(optional.unwrap_or(f64::NAN))
}