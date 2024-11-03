pub mod backup;
pub mod trade;
mod strategy;
mod seller;
mod preventer;

use std::collections::HashMap;
use solana_sdk::pubkey::Pubkey;
use std::sync::Arc;
use chrono::TimeZone;
use dashmap::DashMap;
use log::{debug, error, info, warn};
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_program::native_token::sol_to_lamports;
use tokio::sync::mpsc;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use crate::config::{AppConfig, ExecutorType, StrategyConfig};
use crate::detector::{AMMPool};
use crate::solana::quote_mint::USDC_MINT;
use crate::executor::Executor;
use crate::executor::order::{Order, OrderDirection, OrderKind, OrderStatus};
use crate::market;
use crate::market::monitor::{MarketData, MarketMonitor};
use crate::trader::backup::Backup;
use crate::trader::trade::{Trade, TradeExchange};
use crate::wallet::Wallet;

/// The Trader module is responsible for executing token swaps on Raydium with minimal delay.
/// It handles events from the Detector module efficiently and performs post-swap actions
/// such as transferring purchased tokens to specified wallets and implementing advanced
/// selling strategies.

pub struct Trader {
    client: Arc<RpcClient>,
    wallet: Arc<Wallet>,
    config: Arc<AppConfig>,
    backup: Arc<Backup>,
    pool_rx: mpsc::Receiver<AMMPool>,

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
        rpc_client: Arc<RpcClient>,
        wallet: Arc<Wallet>,
        pool_rx: mpsc::Receiver<AMMPool>,
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
        let (market_tx, market_rx) = tokio::sync::broadcast::channel(100);
        let market_monitor = Arc::new(MarketMonitor::new(
            rpc_client.clone(),
            ws_url,
            market_tx,
            &config.bloxroute,
            cancel_token.clone(),
        ).await);

        // The order executor module
        let executor = Self::start_executor(
            rpc_client.clone(),
            wallet.clone(),
            active_trades.clone(),
            market_monitor.clone(),
            backup.clone(),
            &config,
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
            client: rpc_client,
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
        rpc_client: Arc<RpcClient>,
        wallet: Arc<Wallet>,
        active_trades: Arc<DashMap<Pubkey, Vec<Trade>>>,
        market_monitor: Arc<MarketMonitor>,
        backup: Arc<Backup>,
        app_config: &AppConfig,
        cancel_token: CancellationToken,
    ) -> Arc<Executor> {
        // Initialize the executor module and get the arc reference
        let (executed_orders_tx, mut executed_orders_rx) = tokio::sync::mpsc::channel(30);
        let (failed_orders_tx, mut failed_orders_rx) = tokio::sync::mpsc::channel(30);
        let executor = Arc::new(Executor::new(
            rpc_client.clone(),
            wallet.clone(),
            executed_orders_tx,
            failed_orders_tx,
            app_config,
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
                    market_monitor.remove_from_watchlist(&pool_pubkey);
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
        mut market_data_rx: broadcast::Receiver<MarketData>,
    ) {
        // Collect the initial pools to be added to the watchlist
        for pool_trades in active_trades.iter() {
            let trade = &pool_trades.value()[0];
            market_monitor.add_to_watchlist(pool_trades.key(), market::monitor::PoolMeta {
                keys: trade.pool_keys.clone(),
                base_decimals: trade.base_decimals,
                quote_decimals: trade.quote_decimals,
            });
        }

        // Run market monitor in a separate task
        tokio::spawn(async move {
            market_monitor.start().await;
        });

        // Subscribe to market data updates and process them
        let active_trades = active_trades.clone();
        tokio::spawn(async move {
            while let Ok(market_data) = market_data_rx.recv().await {
                // Process the updated market data and try to 
                // sell tokens if some conditions are met.
                seller::try_to_sell(
                    &market_data,
                    executor.clone(),
                    active_trades.clone(),
                ).await;
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
            // Handle buy order completion
            OrderDirection::QuoteIn => trade::update_trade_after_buy(
                trade,
                &order,
                executor,
                trades,
                market_monitor,
            ).await,

            // Handle sell order completion
            OrderDirection::QuoteOut => trade::update_trade_after_sell(
                trade,
                &order,
                trades,
                executor,
                market_monitor,
                backup,
            ).await,
        }
    }

    /// Determines the best strategy to use based on the pool data and the trader configuration.
    /// Creates a new active trade and schedules the buy order if a strategy is chosen.
    async fn process_detected_pool(&self, pool: AMMPool, executor: Arc<Executor>) {
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
            let (strategy_name, strategy) = match strategy::choose_strategy(
                &config.trader.strategies,
                strategy::ChooseStrategyParams {
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
    ) -> Result<Trade, ()> {
        // Confirm that at least one sell condition is set
        if trade.strategy.take_profit == 0.0 &&
            trade.strategy.stop_loss == 0.0 &&
            trade.strategy.hold_time == 0 {
            warn!("Cannot create a buy order without a sell condition");
            return Err(());
        }

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
        let executor_type = trade.strategy.buy_executor.clone();
        let executor_bribe = sol_to_lamports(trade.strategy.buy_bribe.unwrap_or(0.0));

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
            executor_type,
            executor_bribe,
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