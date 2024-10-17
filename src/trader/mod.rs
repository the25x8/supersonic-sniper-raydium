pub mod backup;
pub mod trade;

use std::collections::HashMap;
use solana_sdk::pubkey::Pubkey;
use std::sync::Arc;
use chrono::TimeZone;
use dashmap::DashMap;
use log::{debug, error, info, warn};
use solana_client::nonblocking::rpc_client::RpcClient;
use tokio::sync::mpsc::Receiver;
use tokio_util::sync::CancellationToken;
use crate::config::{AppConfig, BloxrouteConfig, ExecutorConfig, ExecutorType, StrategyConfig};
use crate::detector::{Pool};
use crate::solana::quote_mint::USDC_MINT;
use crate::executor::Executor;
use crate::executor::order::{Order, OrderDirection, OrderKind, OrderStatus};
use crate::{solana};
use crate::market::monitor::{MarketData, MarketMonitor};
use crate::trader::backup::Backup;
use crate::trader::trade::{Trade, TradeExchange};
use crate::wallet::Wallet;

/// The Trader module is responsible for executing token swaps on Raydium with minimal delay.
/// It handles events from the Detector module efficiently and performs post-swap actions
/// such as transferring purchased tokens to specified wallets and implementing advanced
/// selling strategies.

pub struct ChooseStrategyParams {
    pub pool: Pool,
    pub freezable: bool,
    pub mint_renounced: bool,
    pub meta_mutable: bool,
    pub base_supply: u64,
    pub base_reserves: f64,
    pub quote_reserves: f64,
    pub price: f64,
}

pub struct Trader {
    client: Arc<RpcClient>,
    wallet: Arc<Wallet>,
    config: Arc<AppConfig>,
    backup: Arc<Backup>,
    pool_rx: Receiver<Pool>,

    // Hashmap storing the trades with the pool address as the key
    active_trades: Arc<DashMap<Pubkey, Vec<Trade>>>,
    market_monitor: Arc<MarketMonitor>,
    executor: Arc<Executor>,
    cancel_token: CancellationToken,
}

impl Trader {
    /// Creates a new instance of the Trader module.
    pub async fn new(
        config: Arc<AppConfig>,
        client: Arc<RpcClient>,
        wallet: Arc<Wallet>,
        pool_rx: Receiver<Pool>,
        ws_url: &str,
        cancel_token: CancellationToken,
    ) -> Self {
        // Initialize the active trades hashmap
        let active_trades = Arc::new(DashMap::new());

        // Initialize the backup helper and load the initial active trades from disk
        let backup = Arc::new(Backup::new(
            active_trades.clone(),
            cancel_token.clone(),
        ));
        backup.load_active_trades().await;

        // Auto sync the trades backup
        let backup_clone = backup.clone();
        tokio::spawn(async move {
            backup_clone.trades_auto_sync().await;
        });

        // Initialize the market monitor module
        let (market_tx, market_rx) = tokio::sync::mpsc::channel(100);
        let market_monitor = Arc::new(MarketMonitor::new(
            client.clone(),
            ws_url,
            market_tx,
            cancel_token.clone(),
        ).await);

        // The order executor module
        let executor = Self::start_executor(
            client.clone(),
            wallet.clone(),
            active_trades.clone(),
            market_monitor.clone(),
            backup.clone(),
            &config.executor,
            &config.bloxroute,
            cancel_token.clone(),
        ).await;

        // Start the market data loop in a separate task
        Self::start_market_monitor(
            market_monitor.clone(),
            executor.clone(),
            active_trades.clone(),
            market_rx,
        ).await;

        Self {
            client,
            wallet,
            config,
            pool_rx,
            backup,
            executor,
            cancel_token,
            market_monitor,
            active_trades,
        }
    }

    /// Activates the trader module by starting to listen for new detected pools.
    pub async fn start(&mut self) {
        // Create a semaphore with a limit
        // let semaphore = Arc::new(Semaphore::new(10));
        let cancel_token = self.cancel_token.clone();
        loop {
            tokio::select! {
                // Subscribe to new detected pools and initiate the trading process
                Some(pool) = self.pool_rx.recv() => {
                    self.process_detected_pool(pool, self.executor.clone()).await;
                }

                // Stop the trader if the cancel token is triggered
                _ = cancel_token.cancelled() => {
                    info!("Trader module has been terminated");
                    return;
                }
            }
        }
    }

    /// Creates an executor instance and starts the order processing loop.
    /// The executor listens for executed orders (buy/sell) after they
    /// confirmed in the blockchain and processes them accordingly.
    async fn start_executor(
        client: Arc<RpcClient>,
        wallet: Arc<Wallet>,
        active_trades: Arc<DashMap<Pubkey, Vec<Trade>>>,
        market_monitor: Arc<MarketMonitor>,
        backup: Arc<Backup>,
        executor_config: &ExecutorConfig,
        bloxroute_config: &BloxrouteConfig,
        cancel_token: CancellationToken,
    ) -> Arc<Executor> {
        // Initialize the executor module and get the arc reference
        let (executed_orders_tx, mut executed_orders_rx) = tokio::sync::mpsc::channel(30);
        let (failed_orders_tx, mut failed_orders_rx) = tokio::sync::mpsc::channel(30);
        let executor = Arc::new(Executor::new(
            client.clone(),
            wallet.clone(),
            executed_orders_tx,
            failed_orders_tx,
            executor_config,
            bloxroute_config,
            cancel_token,
        ).await);

        // Create a task to listen for executed orders and process them.
        // Only confirmed orders will be sent to the executor.
        let executor_clone = executor.clone();
        let backup_clone = backup.clone();
        let active_trades_clone = active_trades.clone();
        let market_monitor_clone = market_monitor.clone();
        tokio::spawn(async move {
            while let Some(order) = executed_orders_rx.recv().await {
                let active_trades = active_trades_clone.clone();
                let backup_clone = backup_clone.clone();
                let executor_clone = executor_clone.clone();
                let market_monitor = market_monitor_clone.clone();
                tokio::spawn(async move {
                    Self::process_order(
                        order,
                        executor_clone,
                        active_trades,
                        market_monitor,
                        backup_clone,
                    ).await;
                });
            }
        });

        // Create a task to listen for failed orders and process them.
        tokio::spawn(async move {
            while let Some((order, error)) = failed_orders_rx.recv().await {
                // We need to update the trade status and remove it from the active trades.
                // Track the error and log it for debugging purposes.

                // Get the trade ID from the order
                let trade_id = order.trade_id;
                let pool_pubkey = order.pool_keys.id;

                // Acquire the lock to update the trade related to the failed order
                let mut trade_copy: Option<Trade> = None;
                if let Some(mut trades_ref) = active_trades.get_mut(&pool_pubkey) {
                    if let Some(trade_in_map) = trades_ref
                        .value_mut()
                        .iter_mut()
                        .find(|t| t.id == trade_id)
                    {
                        // Update the trade status
                        trade_in_map.fail(&error);
                        trade_copy = Some(trade_in_map.clone()); // Clone the trade for history

                        // Remove the trade from the trades_ref
                        trades_ref.value_mut().retain(|t| t.id != trade_id);

                        // Check if the trades_ref is empty
                        let is_empty = trades_ref.value().is_empty();
                        drop(trades_ref);

                        // If the trades_ref is empty, remove the pool from the active trades
                        if is_empty {
                            active_trades.remove(&pool_pubkey);
                        }
                    }
                    // trades_ref mutable reference is dropped here
                }

                // If the trade was updated as failed, save it to the history
                if trade_copy.is_some() {
                    market_monitor.remove_from_watchlist(&pool_pubkey).await;
                    backup.save_trade_in_history(trade_copy.unwrap()).await;
                }
            }
        });

        executor
    }

    /// Starts the market monitor loop that listens for market data such as price updates.
    /// It processes the trades, checks the take profit and stop loss conditions, and executes the orders.
    async fn start_market_monitor(
        market_monitor: Arc<MarketMonitor>,
        executor: Arc<Executor>,
        active_trades: Arc<DashMap<Pubkey, Vec<Trade>>>,
        mut market_data_rx: Receiver<MarketData>,
    ) {
        // Collect the initial pools to be added to the watchlist
        for pool_trades in active_trades.iter() {
            let pool_keys = &pool_trades.value()[0].pool_keys;
            market_monitor.add_to_watchlist(pool_trades.key(), pool_keys).await;
        }

        // Subscribe to market data updates and process them
        let active_trades = active_trades.clone();
        tokio::spawn(async move {
            while let Some(market_data) = market_data_rx.recv().await {
                // Process the trades for the current market
                if let Some(trades_list) = active_trades.get(&market_data.pool) {
                    for trade in trades_list.iter() {
                        // Skip trade if completed
                        if trade.is_completed() {
                            continue;
                        }

                        // Buy order shouldn't be None and should be completed
                        let buy_order = trade.buy_order.clone();
                        if buy_order.is_none() || buy_order.as_ref().unwrap().status != OrderStatus::Completed {
                            warn!("Buy order not found or not completed for Trade {}", trade.id);
                            continue;
                        }

                        // Check the take profit order condition
                        let take_profit_order = trade.take_profit_order.clone();
                        if let Some(take_profit_order) = take_profit_order {
                            // Calculate the price change and take profit
                            let take_profit_price = trade.buy_price + (trade.buy_price * (trade.strategy.take_profit / 100.0));
                            let take_profit_percent = (market_data.price - trade.buy_price) / trade.buy_price * 100.0;

                            debug!(
                                "\n🟢 Checking Take Profit Condition for Trade {}\n\
                                 ──────────────────────────────────────────────\n\
                                 - Price Change:      {:+.10}%\n\
                                 - Buy Price:         {:.10}\n\
                                 - Current Price:     {:.10}\n\
                                 - Take Profit Price: {:.10} (at +{:.2}%)\n\
                                 - Strategy:          Take Profit at +{:.2}%\n",
                                trade.id,
                                take_profit_percent,        // Change in price as a percentage
                                trade.buy_price,            // Original buy price
                                market_data.price,          // Current market price
                                take_profit_price,          // Calculated take profit price
                                trade.strategy.take_profit,
                                trade.strategy.take_profit  // Strategy setting for take profit
                            );

                            // If the price has increased to or beyond the take profit threshold, execute the order
                            if take_profit_percent >= trade.strategy.take_profit {
                                match executor.order(take_profit_order).await {
                                    Ok(_) => {
                                        info!(
                                            "\n🟢 ✅ Take Profit Condition Met for Trade {}\n\
                                             ──────────────────────────────────────────────\n\
                                             - Price Change:      {:+.10}%\n\
                                             - Buy Price:         {:.10}\n\
                                             - Current Price:     {:.10}\n\
                                             - Take Profit Price: {:.10} (at +{:.2}%)\n\
                                             - Strategy:          Take Profit at +{:.2}%\n\
                                             \
                                             🚀 Take Profit Order Executed!\n",
                                            trade.id,
                                            take_profit_percent,        // Change in price as a percentage
                                            trade.buy_price,            // Original buy price
                                            market_data.price,          // Current market price
                                            take_profit_price,          // Calculated take profit price
                                            trade.strategy.take_profit,
                                            trade.strategy.take_profit  // Strategy setting for take profit
                                        );
                                    }
                                    Err(e) => {
                                        error!(
                                            "❌ Failed to execute take profit order for Trade {}: {}",
                                            trade.id, e
                                        );
                                    }
                                }

                                // Skip the trade if the take profit order was executed
                                continue;
                            }
                        }

                        // Check the stop loss condition
                        let stop_loss_order = trade.stop_loss_order.clone();
                        if let Some(stop_loss_order) = stop_loss_order {
                            // Calculate the price change and stop loss
                            let stop_loss_price = trade.buy_price - (trade.buy_price * (trade.strategy.stop_loss / 100.0));
                            let stop_loss_percent = (trade.buy_price - market_data.price) / trade.buy_price * 100.0;

                            debug!(
                                "\n🔴 Checking Stop Loss Condition for Trade {}\n\
                                 ──────────────────────────────────────────────\n\
                                 - Price Change:      {:+.10}%\n\
                                 - Buy Price:         {:.10}\n\
                                 - Current Price:     {:.10}\n\
                                 - Stop Loss Price:   {:.10} (at -{:.2}%)\n\
                                 - Strategy:          Stop Loss at -{:.2}%\n",
                                trade.id,
                                stop_loss_percent,          // Change in price as a percentage
                                trade.buy_price,            // Original buy price
                                market_data.price,          // Current market price
                                stop_loss_price,            // Calculated stop loss price
                                trade.strategy.stop_loss,
                                trade.strategy.stop_loss    // Strategy setting for stop loss
                            );

                            // If the price has dropped to or beyond the stop loss threshold, execute the order
                            if stop_loss_percent >= trade.strategy.stop_loss {
                                match executor.order(stop_loss_order).await {
                                    Ok(_) => {
                                        info!(
                                            "\n❌ Stop Loss Condition Met for Trade {}\n\
                                             ──────────────────────────────────────────────\n\
                                             - Price Change:      {:+.10}%\n\
                                             - Buy Price:         {:.10}\n\
                                             - Current Price:     {:.10}\n\
                                             - Stop Loss Price:   {:.10} (at -{:.2}%)\n\
                                             - Strategy:          Stop Loss at -{:.2}%\n\
                                             \
                                             Stop Loss Order Executed!",
                                            trade.id,
                                            stop_loss_percent,          // Change in price as a percentage
                                            trade.buy_price,            // Original buy price
                                            market_data.price,          // Current market price
                                            stop_loss_price,            // Calculated stop loss price
                                            trade.strategy.stop_loss,
                                            trade.strategy.stop_loss    // Strategy setting for stop loss
                                        );
                                    }
                                    Err(e) => {
                                        error!(
                                            "❌ Failed to execute stop loss order for Trade {}: {}",
                                            trade.id, e
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
            }
        });
    }

    /// Processes the completed order and updates the trade status accordingly.
    /// It implements the hook logic and executes the post-swap actions like
    /// scheduling take profit, stop loss, and hold time orders.
    /// If it is a sell order, it also cancels uncompleted orders if the trade is completed.
    /// For buy orders, it confirms the buy for the trade and schedules the sell orders.
    pub async fn process_order(
        order: Order,
        executor: Arc<Executor>,
        trades: Arc<DashMap<Pubkey, Vec<Trade>>>,
        market_monitor: Arc<MarketMonitor>,
        backup: Arc<Backup>,
    ) {
        // Check is the order is completed
        if order.status != OrderStatus::Completed {
            error!("Received order is not completed");
            return;
        }
        if order.tx_id.is_none() {
            error!("Transaction ID is missing for the order");
            return;
        }

        let trade_id = order.trade_id;
        let pool_pubkey = order.pool_keys.id;
        let trade_option = {
            // Scope for the mutable reference
            if let Some(trades_ref) = trades.get(&pool_pubkey) {
                // Clone the trades vector
                let trades_vec = trades_ref.value().clone();
                // Find the index of the trade in the Vec
                trades_vec.iter().find(|t| t.id == trade_id).cloned()
                // trades_ref ref is dropped here
            } else {
                // No trades for this pool
                None
            }
        };

        // If the trade was not found, return early
        let trade = match trade_option {
            Some(data) => data,
            None => return,
        };

        // Perform asynchronous operations without holding any locks

        // Handle the order based on the direction
        match order.direction {
            // In case of buy order create sell orders and schedule them.
            OrderDirection::QuoteIn => {
                // The min amount out is the amount of tokens that should be received after selling.
                // Zero if the sell slippage is not set.
                let min_amount_out = {
                    if trade.strategy.sell_slippage > 0.0 {
                        let balance_change = spl_token::amount_to_ui_amount(
                            order.balance_change,
                            order.in_decimals,
                        );
                        balance_change - (balance_change * (trade.strategy.sell_slippage / 100.0))
                    } else {
                        0.0
                    }
                };

                // Re-acquire the lock to update the trade with buy details
                if let Some(mut trades_ref) = trades.get_mut(&pool_pubkey) {
                    if let Some(trade_in_map) = trades_ref.value_mut().iter_mut().find(|t| t.id == trade.id) {
                        trade_in_map.buy(&order); // Pass the confirmed order to the trade
                    }
                    // trades_ref mutable reference is dropped here
                }

                // Register the new orders for the trade (before executing)
                if let Some(mut trades_ref) = trades.get_mut(&pool_pubkey) {
                    if let Some(trade_in_map) = trades_ref.value_mut().iter_mut().find(|t| t.id == trade.id) {
                        // Define stop-loss executor for the order
                        let stop_loss_executor = {
                            if trade.strategy.stop_loss_executor.is_some() {
                                trade.strategy.stop_loss_executor.unwrap()
                            } else {
                                ExecutorType::RPC
                            }
                        };

                        // Define mint and decimals of the sell order,
                        // based on the buy order's in and out mints.
                        let sell_in_mint = order.out_mint; // Buy order's out mint is the in mint for the sell order
                        let sell_out_mint = order.in_mint; // Buy order's in mint is the out mint for the sell order
                        let sell_in_decimals = order.out_decimals;
                        let sell_out_decimals = order.in_decimals;

                        // Convert the min amount to the raw amount
                        let min_amount_out = spl_token::ui_amount_to_amount(min_amount_out, sell_out_decimals);
                        let bloxroute_bribe = spl_token::ui_amount_to_amount(
                            trade.strategy.sell_bribe.unwrap_or(0.0),
                            sell_out_decimals,
                        );

                        // Determine the possible sell orders based on the strategy.
                        let possible_sell_orders = vec![
                            (
                                trade.strategy.hold_time > 0,
                                Order::new(
                                    OrderDirection::QuoteOut,
                                    trade.id,
                                    OrderKind::HoldTime,
                                    &trade.pool_keys,
                                    order.balance_change, // amount of received tokens
                                    min_amount_out,
                                    &sell_in_mint,
                                    &sell_out_mint,
                                    sell_in_decimals,
                                    sell_out_decimals,
                                    ExecutorType::RPC, // Hold time order is always executed by RPC
                                    0, // RPC doesn't require a bribe
                                    trade.strategy.hold_time,
                                ),
                            ),
                            (
                                trade.strategy.take_profit > 0.0,
                                Order::new(
                                    OrderDirection::QuoteOut,
                                    trade.id,
                                    OrderKind::TakeProfit,
                                    &trade.pool_keys,
                                    order.balance_change, // amount of received tokens
                                    min_amount_out,
                                    &sell_in_mint,
                                    &sell_out_mint,
                                    sell_in_decimals,
                                    sell_out_decimals,
                                    trade.strategy.take_profit_executor,
                                    bloxroute_bribe,
                                    trade.strategy.sell_delay,
                                ),
                            ),
                            (
                                trade.strategy.stop_loss > 0.0,
                                Order::new(
                                    OrderDirection::QuoteOut,
                                    trade.id,
                                    OrderKind::StopLoss,
                                    &trade.pool_keys,
                                    order.balance_change, // amount of received tokens
                                    min_amount_out,
                                    &sell_in_mint,
                                    &sell_out_mint,
                                    sell_in_decimals,
                                    sell_out_decimals,
                                    stop_loss_executor,
                                    bloxroute_bribe,
                                    trade.strategy.sell_delay,
                                ),
                            )
                        ];

                        // Execute the orders where the condition is true
                        for (condition, order) in possible_sell_orders {
                            if condition {
                                let kind = order.kind;
                                match trade_in_map.initiate_sell(order) {
                                    Ok(_) => (),
                                    Err(e) => {
                                        error!("Failed to schedule {} order: {}", kind, e);
                                    }
                                }
                            }
                        }

                        // Copy hold time order for execution
                        let hold_order = trade_in_map.clone().hold_time_order;
                        drop(trades_ref); // Explicitly drop the mutable reference here

                        // If hold time is set, execute the hold order immediately
                        if hold_order.is_some() {
                            // Hold order is the first order in the list
                            if let Err(e) = executor.order(hold_order.unwrap()).await {
                                error!("Failed to schedule sell order: {}", e);
                                return;
                            }
                        }
                    }
                }

                // If you take profit or stop loss is set, add the pool to the watchlist
                if trade.strategy.take_profit > 0.0 || trade.strategy.stop_loss > 0.0 {
                    market_monitor.add_to_watchlist(&pool_pubkey, &trade.pool_keys).await;
                }
            }

            // Process sell order
            OrderDirection::QuoteOut => {
                // Re-acquire the lock to update the trade
                if let Some(mut trades_ref) = trades.get_mut(&pool_pubkey) {
                    if let Some(trade_in_map) = trades_ref.value_mut().iter_mut().find(|t| t.id == trade.id) {
                        // Pass the confirmed sell order to the trade
                        if let Err(e) = trade_in_map.sell(&order) {
                            error!("Failed to confirm sell order: {}", e);
                            return;
                        }

                        // Return error if order isn't completed
                        if !trade_in_map.is_completed() {
                            error!("Sell order isn't completed for trade {}", order.trade_id);
                        }

                        // If the trade is completed, move it to the history and remove from active trades
                        trade_in_map.archive();

                        // Make a copy of the trade for history
                        let trade_copy = trade_in_map.clone();
                        let buy_order = trade_in_map.buy_order.as_ref().unwrap();
                        let completed_sell_order = trade_in_map.get_completed_sell_order().unwrap();
                        let completed_in = completed_sell_order.confirmed_at - buy_order.created_at;

                        info!(
                            "\n🟢 Trade Completed Successfully!\n\
                             ───────────────────────────────\n\
                             👉 Trade ID:        {}\n\
                             👉 Result:          {}\n\
                             📊 Pool:            {}\n\
                             💰 Buy Price:       {:.10}\n\
                             💰 Sell Price:      {:.10}\n\
                             💰 Profit:          {:.10} (SOL)\n\
                             💰 Profit %:        {:.2}%\n\
                             🕒 Hold Time:       {} ms\n\
                             🤔 Strategy:        {}\n\
                             🕒 Buy Time:        {}\n\
                             🕒 Sell Time:       {}\n\
                             🚀 Completed in:    {} sec.\n",
                            trade_in_map.id,
                            completed_sell_order.kind,
                            trade_in_map.pool_keys.id.to_string(),
                            trade_in_map.buy_price,
                            trade_in_map.sell_price,
                            trade_in_map.profit_amount,
                            trade_in_map.profit_percent,
                            trade_in_map.strategy.hold_time,
                            trade_in_map.strategy_name,
                            chrono::Utc.timestamp_opt(buy_order.confirmed_at as i64, 0).unwrap().to_rfc3339(),
                            chrono::Utc.timestamp_opt(completed_sell_order.confirmed_at as i64, 0).unwrap().to_rfc3339(),
                            completed_in,
                        );

                        // Remove the trade from the trades_ref
                        trades_ref.value_mut().retain(|t| t.id != trade_id);

                        // Check if the trades_ref is empty
                        let is_empty = trades_ref.value().is_empty();
                        drop(trades_ref); // // Explicitly drop the mutable reference here

                        // All async operations here after lock release

                        // If it was the last trade for the pool, clean active trades
                        // and remove pool from the watchlist.
                        if is_empty {
                            trades.remove(&pool_pubkey);
                            market_monitor.remove_from_watchlist(&pool_pubkey).await;
                        }

                        // If it was not a hold time order, call the executor
                        // to cancel the uncompleted hold time order
                        if order.kind != OrderKind::HoldTime {
                            if let Some(order) = trade_copy.hold_time_order.as_ref() {
                                match executor.cancel_order(order.id).await {
                                    Ok(_) => (),
                                    Err(e) => {
                                        error!("Failed to cancel hold time order: {}", e);
                                    }
                                }
                            }
                        }

                        // Save trade to history
                        backup.save_trade_in_history(trade_copy).await;
                    }
                }
            }
        }
    }

    /// Determines the best strategy to use based on the pool data and the trader configuration.
    /// Creates a new active trade and schedules the buy order if a strategy is chosen.
    async fn process_detected_pool(&self, pool: Pool, executor: Arc<Executor>) {
        let start_time = chrono::Utc::now().time();
        let wallet = self.wallet.clone(); // Clone the wallet to use in tasks
        let config = self.config.clone(); // Clone the config to use in tasks
        let executor = executor.clone(); // Clone the executor to use in tasks
        let active_trades = self.active_trades.clone(); // Clone the active trades hashmap

        // Check if the pool is already being traded. It can happen when
        // the active trade for this pool was recovered from the backup file.
        // if active_trades.contains_key(&keys.id) {
        //     return;
        // }

        // Process the detected pool
        tokio::spawn(async move {
            // Choose the best strategy based on the pool data and the trader configuration
            let (strategy_name, strategy) = match Self::choose_strategy(
                &config.trader.strategies,
                ChooseStrategyParams {
                    pool: pool.clone(),
                    base_reserves: pool.base_reserves,
                    quote_reserves: pool.quote_reserves,
                    freezable: pool.token_freezable,
                    mint_renounced: pool.token_mint_renounced,
                    meta_mutable: pool.token_meta_mutable,
                    base_supply: pool.token_supply,
                    price: pool.initial_price,
                },
            ) {
                Ok((strategy, config)) => (strategy, config),
                Err(_) => {
                    // No strategy chosen, skip the pool
                    warn!("No strategy chosen for the pool");
                    return;
                }
            };

            // Create a new trade instance and its orders
            let trade = Trade::new(
                TradeExchange::Raydium,
                &pool.keys,
                &wallet.pubkey,
                strategy_name,
                strategy,
                pool.base_decimals,
                pool.quote_decimals,
            );

            // Execute the buy order for the trade and schedule the sell orders based on the strategy
            let trade = match Self::create_buy_order(
                trade,
                executor.clone(),
                pool.initial_price,
                config.bloxroute.enabled,
            ).await {
                Ok(trade) => trade,
                Err(_) => {
                    warn!("Failed to create trade orders for the pool");
                    return;
                }
            };

            // Add the new trade to the active trades hashmap
            if let Some(mut existing_trades) = active_trades.get_mut(&pool.keys.id) {
                existing_trades.push(trade.clone());
            } else {
                // Create a new vector and insert the trade
                active_trades.insert(pool.keys.id, vec![trade.clone()]);
            }

            debug!(
                "\n🟢 Trade Created Successfully for Pool: {}\n\
                 ─────────────────────────────────────────────────────\n\
                 ▶️ Strategy:        {}\n\
                 ▶️ Buy Amount:      {:.10}\n\
                 ▶️ Buy Slippage:    {:.10}%\n\
                 ▶️ Buy Delay:       {} ms\n\
                 ▶️ Take Profit:     {:.10}%\n\
                 ▶️ Stop Loss:       {:.10}%\n\
                 ▶️ Hold Time:       {} ms\n\
                 ⚡️ Processed in:    {} ms\n",
                pool.keys.id.to_string(),
                trade.strategy_name,
                trade.strategy.quote_amount,
                trade.strategy.buy_slippage,
                trade.strategy.buy_delay,
                trade.strategy.take_profit,
                trade.strategy.stop_loss,
                trade.strategy.hold_time,
                (chrono::Utc::now().time() - start_time).num_milliseconds()
            );
        });
    }

    /// This helper function creates a buy order for the trade and executes it.
    /// It returns the trade with the buy order registered.
    async fn create_buy_order(
        mut trade: Trade,
        executor: Arc<Executor>,
        current_price: f64,
        bloxroute_enabled: bool,
    ) -> Result<Trade, ()> {
        // Confirm that at least one sell condition is set
        if trade.strategy.take_profit == 0.0 &&
            trade.strategy.stop_loss == 0.0 &&
            trade.strategy.hold_time == 0 {
            warn!("Cannot create a buy order without a sell condition");
            return Err(());
        }

        // Define should we use BloxRoute for the buy order
        let use_bloxroute = {
            // Disable if it is not enabled in the config
            if !bloxroute_enabled {
                false;
            }

            // Set to true if the buy executor is BloxRoute
            if trade.strategy.buy_executor == ExecutorType::Bloxroute {
                true;
            }

            false
        };

        // Prepare the buy order
        let amount_in = trade.strategy.quote_amount;

        // Calculate the min amount out based on the buy slippage.
        // If the slippage is not set, the min amount out is zero.
        let min_amount_out = {
            if trade.strategy.buy_slippage > 0.0 {
                let approx_amount_out = amount_in / current_price;
                approx_amount_out - (approx_amount_out * (trade.strategy.buy_slippage / 100.0))
            } else {
                0.0
            }
        };

        // Determine real quote and base tokens. If the base mint is wrapped SOL or USDC reverse the mints.
        let (
            in_mint,
            out_mint,
            in_decimals,
            out_decimals,
        ) = adjust_swap_tokens(&trade);

        let raw_amount_in = spl_token::ui_amount_to_amount(amount_in, in_decimals);
        let raw_min_amount_out = spl_token::ui_amount_to_amount(min_amount_out, out_decimals);
        let bloxroute_bribe = spl_token::ui_amount_to_amount(
            trade.strategy.buy_bribe.unwrap_or(0.0),
            in_decimals,
        );

        let buy_order = Order::new(
            OrderDirection::QuoteIn,
            trade.id,
            OrderKind::SimpleBuy,
            &trade.pool_keys,
            raw_amount_in,
            raw_min_amount_out,
            &in_mint,
            &out_mint,
            in_decimals,
            out_decimals,
            // Determine the executor type for the buy order
            if use_bloxroute {
                ExecutorType::Bloxroute
            } else {
                ExecutorType::RPC
            },
            bloxroute_bribe,
            trade.strategy.buy_delay,
        );

        // Register the buy order for the trade
        match trade.initiate_buy(buy_order.clone()) {
            Ok(_) => (),
            Err(e) => {
                error!(
                    "Failed to register buy order for the pool: {}.\n Error: {}",
                    trade.pool_keys.id.to_string(), e
                );
                return Err(());
            }
        };

        // Immediately execute the buy order
        match executor.order(buy_order.clone()).await {
            Ok(order) => order,
            Err(e) => {
                error!(
                    "Failed to execute buy order for the pool: {}.\n Error: {}",
                    trade.pool_keys.id.to_string(), e
                );
                return Err(());
            }
        };

        Ok(trade)
    }

    /// Chooses the best strategy based on the pool data and the trader configuration.
    /// The function returns the configuration of the chosen strategy.
    fn choose_strategy(strategies: &HashMap<String, StrategyConfig>, params: ChooseStrategyParams) -> Result<(String, StrategyConfig), ()> {
        // Check that strategies are defined in the config
        if strategies.is_empty() {
            error!("No strategies defined in the configuration");
            return Err(());
        }

        // Iterate over the strategies in the config and check if the conditions are met
        for (name, strategy) in strategies.iter() {
            // Check should the token be freezable
            if strategy.freezable != params.freezable {
                error!("Freezable condition not met");
                continue;
            }

            // Can the mint authority mint new tokens
            if strategy.mint_renounced != params.mint_renounced {
                error!("Mint renounced condition not met");
                continue;
            }

            // Can the metadata be changed
            if strategy.meta_mutable.is_some() && strategy.meta_mutable.unwrap() != params.meta_mutable {
                error!("Metadata mutable condition not met");
                continue;
            }

            // Check quote symbol conditions
            if let Some(quote_symbol) = strategy.quote_symbol.clone() {
                // Check if the quote mint or base mint is wrapped SOL or USDC
                if (quote_symbol == "WSOL" &&
                    params.pool.keys.quote_mint != spl_token::native_mint::id() &&
                    params.pool.keys.base_mint != spl_token::native_mint::id()) ||
                    (quote_symbol == "USDC" &&
                        params.pool.keys.quote_mint.to_string() != USDC_MINT &&
                        params.pool.keys.base_mint.to_string() != USDC_MINT) {
                    error!("Quote symbol condition not met");
                    continue;
                }
            }

            // Check if the pool detected time is within the max delay
            if params.pool.open_time.timestamp() > 0 {
                let elapsed = chrono::Utc::now() - params.pool.timestamp;
                if elapsed.num_milliseconds() > strategy.max_delay_since_detect as i64 {
                    error!("Max delay since open condition not met. {}, {}, {}",
                        params.pool.open_time.timestamp_millis(),
                        elapsed.num_milliseconds(),
                        strategy.max_delay_since_detect,
                    );
                    continue;
                }
            }

            // TODO Check additional data conditions, like website, social networks, audits, etc.

            // Check price conditions
            if Trader::is_condition_met(
                strategy.price_condition.clone(),
                params.price,
                strategy.price,
                strategy.price_range.clone(),
            ) {
                error!("Price condition not met");
                continue;
            }

            // Check reserves conditions
            if Trader::is_condition_met(
                strategy.reserves_quote_condition.clone(),
                params.quote_reserves,
                Some(strategy.reserves_quote.unwrap_or(0.0)),
                strategy.reserves_quote_range.clone(),
            ) {
                error!("Quote reserves condition not met");
                continue;
            }

            if Trader::is_condition_met(
                strategy.reserves_base_condition.clone(),
                params.base_reserves,
                Some(strategy.reserves_base.unwrap_or(0.0)),
                strategy.reserves_base_range.clone(),
            ) {
                error!("Base reserves condition not met");
                continue;
            }

            // Check total supply conditions
            if Trader::is_condition_met(
                strategy.total_supply_condition.clone(),
                params.base_supply as f64,
                Some(strategy.total_supply.unwrap_or(0.0)),
                strategy.total_supply_range.clone(),
            ) {
                error!("Base supply condition not met");
                continue;
            }

            // TODO Fetch market data, bids, asks, etc.

            // All conditions are met, return the strategy
            return Ok((name.to_string(), strategy.clone()));
        }

        // If no strategy is chosen, return an error
        Err(())
    }

    /// Checks if a condition is met based on the target value and the value to compare.
    pub fn is_condition_met(condition: Option<String>, value: f64, target: Option<f64>, target_range: Option<Vec<f64>>) -> bool {
        if let Some(target_value) = target {
            if condition.is_none() {
                return false;
            }

            let is_met = match condition.unwrap().as_str() {
                "gt" => value <= target_value,
                "gte" => value < target_value,
                "lt" => value >= target_value,
                "lte" => value > target_value,
                "eq" => value != target_value,

                // In operator for ranges. Value must be in the range.
                "in" => {
                    if let Some(range) = target_range {
                        return if range.len() == 2 {
                            value < range[0] || value > range[1]
                        } else {
                            true
                        };
                    } else {
                        true
                    }
                }
                _ => false,
            };

            return is_met;
        }

        // If no target value is provided, skip the condition check and return true
        true
    }
}

/// Adjusts the quote and base tokens used in the swap based on the mint types.
/// If the base mint is wrapped SOL or USDC, the tokens are swapped to
/// ensure that quote token is always the main currency for trading.
fn adjust_swap_tokens(trade: &Trade) -> (Pubkey, Pubkey, u8, u8) {
    let keys = &trade.pool_keys;

    // If the base mint is wrapped SOL or USDC we need to swap base and quote tokens
    let (in_mint, out_mint) = {
        if keys.base_mint == spl_token::native_mint::id() || keys.base_mint.to_string() == USDC_MINT {
            (keys.base_mint, keys.quote_mint)
        } else {
            (keys.quote_mint, keys.base_mint)
        }
    };

    let (in_decimals, out_decimals) = {
        if keys.base_mint == spl_token::native_mint::id() || keys.base_mint.to_string() == USDC_MINT {
            (trade.base_decimals, trade.quote_decimals)
        } else {
            (trade.quote_decimals, trade.base_decimals)
        }
    };

    (in_mint, out_mint, in_decimals, out_decimals)
}

pub fn current_timestamp() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}