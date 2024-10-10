mod executor;
mod market_monitor;
pub mod backup;
pub mod models;

use std::collections::{HashMap};
use solana_sdk::pubkey::Pubkey;
use std::str::FromStr;
use std::sync::Arc;
use chrono::TimeZone;
use dashmap::DashMap;
use log::{debug, error, info, warn};
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_program::program_option::COption;
use solana_program::program_pack::Pack;
use mpl_token_metadata::accounts::Metadata;
use solana_account_decoder::parse_token::UiTokenAmount;
use spl_token::state::{Mint};
use tokio::sync::mpsc::{Receiver};
use tokio::sync::{Semaphore};
use tokio_util::sync::CancellationToken;
use crate::config::{StrategyConfig, TraderConfig};
use crate::detector::{Pool, PoolType};
use crate::solana::amount_utils::token_amount_to_float;
use crate::solana::quote_mint::{USDC_MINT, WSOL_MINT};
use crate::trader::executor::Executor;
use crate::trader::market_monitor::{get_amm_pool_reserves, convert_reserves_to_price, MarketData, MarketMonitor};
use crate::trader::backup::{Backup};
use crate::trader::models::{TradeExchange, OrderStatus, Trade, TradeStatus, Order, OrderDirection, OrderKind};
use crate::wallet::Wallet;


/// The Trader module is responsible for executing token swaps on Raydium with minimal delay.
/// It handles events from the Detector module efficiently and performs post-swap actions
/// such as transferring purchased tokens to specified wallets and implementing advanced
/// selling strategies.

// Metaplex Token Metadata Program ID
const METADATA_PROGRAM_ID: &str = "metaqbxxUerdq28cj1RbAWkYQm3ybzjb6a8bt518x1s";

pub struct ChooseStrategyParams {
    pub pool: Pool,
    pub freezable: bool,
    pub mint_renounced: bool,
    pub meta_mutable: bool,
    pub base_metadata: Option<Metadata>,
    pub base_mint: Mint,
    pub base_reserves: UiTokenAmount,
    pub quote_reserves: UiTokenAmount,
    pub price: f64,
}

pub struct Trader {
    client: Arc<RpcClient>,
    wallet: Arc<Wallet>,
    config: Arc<TraderConfig>,
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
        config: Arc<TraderConfig>,
        client: Arc<RpcClient>,
        wallet: Arc<Wallet>,
        pool_rx: Receiver<Pool>,
        ws_url: &str,
        cancel_token: CancellationToken,
    ) -> Self {
        // Initialize the active trades hashmap
        let active_trades = Arc::new(DashMap::new());

        // Initialize the backup helper and load the initial active trades from disk
        let backup = Arc::new(Backup::new_for_trades(
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
        let (market_tx, market_rx) = tokio::sync::mpsc::channel(10);
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
        let semaphore = Arc::new(Semaphore::new(10));
        let cancel_token = self.cancel_token.clone();
        tokio::select! {
            // Subscribe to new detected pools and initiate the trading process
            _ = async {
                while let Some(pool) = self.pool_rx.recv().await {
                    self.process_detected_pool(pool, self.executor.clone(), semaphore.clone()).await;
                }
            } => {}

            // Stop the trader if the cancel token is triggered
            _ = cancel_token.cancelled() => {
                info!("Trader module has been terminated");
                return;
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
        cancel_token: CancellationToken,
    ) -> Arc<Executor> {
        // Initialize the executor module and get the arc reference
        let (executed_orders_tx, mut executed_orders_rx) = tokio::sync::mpsc::channel(10);
        let executor = Arc::new(Executor::new(
            client.clone(),
            wallet.clone(),
            executed_orders_tx,
            cancel_token,
        ).await);

        // Create a task to listen for executed orders and process them.
        // Only confirmed orders will be sent to the executor.
        let executor_clone = executor.clone();
        tokio::spawn(async move {
            while let Some(order) = executed_orders_rx.recv().await {
                let active_trades = active_trades.clone();
                let backup = backup.clone();
                let rpc_client = client.clone();
                let executor_clone = executor_clone.clone();
                let market_monitor = market_monitor.clone();
                tokio::spawn(async move {
                    Self::process_order(
                        order,
                        executor_clone.clone(),
                        active_trades.clone(),
                        market_monitor.clone(),
                        rpc_client.clone(),
                        backup.clone(),
                    ).await;
                });
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
        // Collect the pools (keys) to be added to the watchlist
        let pools: Vec<Pubkey> = {
            active_trades.clone().iter().map(|t| t.key().clone()).collect()
        };

        // Add each pool to the market monitor watchlist
        for pool in pools {
            market_monitor.add_to_watchlist(pool).await;
        }

        // Subscribe to market data updates and process them
        let active_trades = active_trades.clone();
        tokio::spawn(async move {
            while let Some(market_data) = market_data_rx.recv().await {
                // Process the trades for the current market
                if let Some(trades_list) = active_trades.get(&market_data.market) {
                    for trade in trades_list.iter() {
                        debug!(
                            "\n\
                            🔔 Market Data Received for Trade!\n\
                            ────────────────────────────────\n\
                            📊 Trade ID:        {}\n\
                            💰 Current Price:   {:.10}\n\
                            ────────────────────────────────\n",
                            trade.id,
                            market_data.price,
                        );

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

                                // Skip the trade if the stop loss order was executed
                                continue;
                            }
                        }
                    }
                }
            }
        });
    }

    /// Processes the executed order and updates the trade status accordingly.
    /// It implements the hook logic and executes the post-swap actions like
    /// scheduling take profit, stop loss, and hold time orders.
    /// If it is a sell order, it also cancels uncompleted orders if the trade is completed.
    /// For buy orders, it confirms the buy for the trade and schedules the sell orders.
    pub async fn process_order(
        order: Order,
        executor: Arc<Executor>,
        trades: Arc<DashMap<Pubkey, Vec<Trade>>>,
        market_monitor: Arc<MarketMonitor>,
        rpc_client: Arc<RpcClient>,
        backup: Arc<Backup>,
    ) {
        let trade_id = order.trade_id;
        let pool_pubkey = order.pool;
        let trade_option = {
            // Scope for the mutable reference
            if let Some(trades_ref) = trades.get(&pool_pubkey) {
                // Clone the trades vector
                let trades_vec = trades_ref.value().clone();
                // Find the index of the trade in the Vec
                if let Some(trade) = trades_vec.iter().find(|t| t.id == trade_id) {
                    Some(trade.clone())
                } else {
                    // Trade not found
                    None
                }
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

        // 1. Get current price
        let current_price = match get_amm_pool_reserves(
            rpc_client.clone(),
            &trade.pool.base_vault,
            &trade.pool.quote_vault,
        ).await {
            Ok(reserves) => match convert_reserves_to_price(&reserves[0], &reserves[1]) {
                Ok(price) => price,
                Err(e) => {
                    error!("Failed to get current price: {}", e);
                    return;
                }
            },
            Err(e) => {
                error!("Failed to get current price: {}", e);
                return;
            }
        };

        // 2. Estimate the amount out based on the current price and amount in
        // TODO - This is a simple estimation, we need to improve this by
        //  fetching the actual amount from spl-token accounts of the wallet.
        let estimated_amount_out = match order.direction {
            OrderDirection::BaseIn => order.amount_in / current_price,
            OrderDirection::BaseOut => order.amount_in * current_price,
        };

        // 3. Handle the order based on the direction
        match order.direction {
            // Process buy order
            OrderDirection::BaseIn => {
                // Re-acquire the lock to update the trade with buy details
                if let Some(mut trades_ref) = trades.get_mut(&pool_pubkey) {
                    if let Some(trade_in_map) = trades_ref.value_mut().iter_mut().find(|t| t.id == trade.id) {
                        trade_in_map.buy(
                            order.amount_in,
                            estimated_amount_out,
                            "dummy_tx_id".to_string(),
                            "dummy_signature".to_string(),
                            current_timestamp(),
                        );
                    }
                    // trades_ref mutable reference is dropped here
                }

                let mut next_orders = vec![];

                // Schedule sell by hold time order if the strategy has hold time
                if trade.strategy.hold_time > 0 {
                    next_orders.push(Order::new(
                        OrderDirection::BaseOut,
                        trade.id,
                        OrderKind::HoldTime,
                        pool_pubkey,
                        estimated_amount_out,
                        estimated_amount_out - (estimated_amount_out * (trade.strategy.sell_slippage / 100.0)),
                        trade.strategy.hold_time,
                    ));
                }

                // Schedule take profit and stop loss orders if the strategy has them
                if trade.strategy.take_profit > 0.0 {
                    next_orders.push(Order::new(
                        OrderDirection::BaseOut,
                        trade.id,
                        OrderKind::TakeProfit,
                        pool_pubkey,
                        estimated_amount_out,
                        estimated_amount_out - (estimated_amount_out * (trade.strategy.sell_slippage / 100.0)),
                        trade.strategy.sell_delay,
                    ));
                }

                if trade.strategy.stop_loss > 0.0 {
                    next_orders.push(Order::new(
                        OrderDirection::BaseOut,
                        trade.id,
                        OrderKind::StopLoss,
                        pool_pubkey,
                        estimated_amount_out,
                        estimated_amount_out - (estimated_amount_out * (trade.strategy.sell_slippage / 100.0)),
                        trade.strategy.sell_delay,
                    ));
                }

                // Register the new orders for the trade and execute them
                for order in next_orders.clone() {
                    if let Some(mut trades_ref) = trades.get_mut(&pool_pubkey) {
                        if let Some(trade_in_map) = trades_ref.value_mut().iter_mut().find(|t| t.id == trade.id) {
                            if let Err(e) = trade_in_map.initiate_sell(order.clone()) {
                                error!("Failed to register take profit order: {}", e);
                                return;
                            }
                        }
                        // trades_ref mutable reference is dropped here
                    }
                }

                // Execute only hold time orders immediately, stop loss and take profit
                // will be executed by the market monitor based on the price.
                if trade.strategy.hold_time > 0 && next_orders.len() > 0 {
                    // Hold order is the first order in the list
                    if let Err(e) = executor.order(next_orders[0].clone()).await {
                        error!("Failed to schedule sell order: {}", e);
                        return;
                    }
                }

                // If you take profit or stop loss is set, add the pool to the watchlist
                if trade.strategy.take_profit > 0.0 || trade.strategy.stop_loss > 0.0 {
                    market_monitor.add_to_watchlist(pool_pubkey).await;
                }
            }

            // Process sell order
            OrderDirection::BaseOut => {
                let mut is_completed = false;
                let mut trade_copy: Option<Trade> = None;

                // Re-acquire the lock to update the trade
                if let Some(mut trades_ref) = trades.get_mut(&pool_pubkey) {
                    if let Some(trade_in_map) = trades_ref.value_mut().iter_mut().find(|t| t.id == trade.id) {
                        // Confirm the sell order
                        if let Err(e) = trade_in_map.sell(
                            order.kind,
                            order.amount_in,
                            estimated_amount_out,
                            "dummy_tx_id".to_string(),
                            "dummy_signature".to_string(),
                            current_timestamp(),
                        ) {
                            error!("Failed to confirm sell order: {}", e);
                            return;
                        }

                        // If the trade is completed, move it to the history and remove from active trades
                        if trade_in_map.is_completed() {
                            trade_in_map.archive();

                            // Set these values to perform async operations after dropping the lock
                            is_completed = true;
                            trade_copy = Some(trade_in_map.clone());

                            let buy_order = trade_in_map.buy_order.as_ref().unwrap();
                            let completed_sell_order = trade_in_map.get_completed_sell_order().unwrap();
                            let completed_in = completed_sell_order.confirmed_at - buy_order.created_at;
                            info!(
                                "\n🟢 Trade Completed Successfully!\n\
                                ───────────────────────────────\n\
                                👉 Trade ID:        {}\n\
                                👉 Result:          {}\n\
                                📊 Pool:            {}\n\
                                💰 Buy Amount:      {:.10}\n\
                                💰 Sell Amount:     {:.10}\n\
                                💰 Profit:          {:.10} (SOL)\n\
                                💰 Profit %:        {:.2}%\n\
                                🕒 Hold Time:       {} ms\n\
                                🤔 Strategy:        {}\n\
                                🕒 Buy Time:        {}\n\
                                🕒 Sell Time:       {}\n\
                                🚀 Completed in:    {} sec.\n",
                                trade_in_map.id,
                                completed_sell_order.kind,
                                trade_in_map.pool.pool_address,
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
                        }

                        // Check if the trades_ref is empty
                        let is_empty = trades_ref.value().is_empty();
                        drop(trades_ref); // // Explicitly drop the mutable reference here

                        if is_empty {
                            trades.remove(&pool_pubkey);
                        }
                    }
                }

                // Async operations after dropping the lock
                if is_completed && trade_copy.is_some() {
                    market_monitor.remove_from_watchlist(pool_pubkey).await;
                    backup.save_trade_in_history(trade_copy.unwrap()).await;
                }
            }
        }
    }

    /// Determines the best strategy to use based on the pool data and the trader configuration.
    /// Creates a new active trade and schedules the buy order if a strategy is chosen.
    async fn process_detected_pool(&self, pool: Pool, executor: Arc<Executor>, semaphore: Arc<Semaphore>) {
        let start_time = chrono::Utc::now().time();
        let pool = pool.clone(); // Clone the pool to use in tasks
        let wallet = self.wallet.clone(); // Clone the wallet to use in tasks
        let config = self.config.clone(); // Clone the config to use in tasks
        let executor = executor.clone(); // Clone the executor to use in tasks
        let active_trades = self.active_trades.clone(); // Clone the active trades hashmap
        let semaphore = semaphore.clone();
        let client = self.client.clone();

        // Check if the pool is already being traded. It can happen when
        // the active trade for this pool was recovered from the backup file.
        if active_trades.contains_key(&pool.pubkey) {
            return;
        }

        // Process the detected pool
        tokio::spawn(async move {
            let _permit = semaphore.acquire().await; // Acquire permit before processing

            debug!(
                "\nProcessing new pool: {}\n\
                Open time: {}\n",
                pool.pool_address,
                pool.open_time.to_rfc3339(),
            );

            // Fetch all the required data concurrently
            let (
                pool_reserves_result,
                token_metadata_result,
                token_mint_result
            ) = tokio::join!(
                get_amm_pool_reserves(client.clone(), &pool.base_vault, &pool.quote_vault),
                Trader::get_token_metadata(client.clone(), &pool.base_mint),
                Trader::get_token_mint(client.clone(), &pool.base_mint)
            );

            // Get liquidity of the pool
            let (base_reserves, quote_reserves) = match pool_reserves_result {
                Ok(balances) => (balances[0].clone(), balances[1].clone()),
                Err(e) => {
                    error!("Failed to get pool liquidity: {}", e);
                    return;
                }
            };

            // Get the account data for the token vault mint
            let base_mint = match token_mint_result {
                Ok(mint) => mint,
                Err(e) => {
                    error!("Failed to get token mint account: {}", e);
                    return;
                }
            };

            // Get the token metadata
            let token_metadata = match token_metadata_result {
                Ok(metadata) => metadata,
                Err(e) => {
                    error!("Failed to get token metadata: {}", e);
                    return;
                }
            };

            // Calculate the price of token1 in terms of token2
            let price = match market_monitor::convert_reserves_to_price(&base_reserves, &quote_reserves) {
                Ok(price) => price,
                Err(e) => {
                    error!("Failed to calculate token price: {}", e);
                    return;
                }
            };

            let freezable = base_mint.freeze_authority != COption::None;
            let mint_renounced = base_mint.mint_authority == COption::None;

            // Clone the metadata for printing
            let cloned_metadata = token_metadata.clone();
            let meta_mutable = if !cloned_metadata.is_none() {
                cloned_metadata.unwrap().is_mutable.clone()
            } else {
                false
            };

            // Choose the best strategy based on the pool data and the trader configuration
            let (strategy_name, strategy) = match Self::choose_strategy(
                config.strategies.clone(),
                ChooseStrategyParams {
                    freezable,
                    mint_renounced,
                    meta_mutable,
                    pool: pool.clone(),
                    base_mint: base_mint.clone(),
                    base_metadata: token_metadata.clone(),
                    base_reserves: base_reserves.clone(),
                    quote_reserves: quote_reserves.clone(),
                    price: price.clone(),
                },
            ) {
                Ok((strategy, config)) => (strategy, config),
                Err(_) => {
                    // No strategy chosen, skip the pool
                    warn!("No strategy chosen for the pool");
                    return;
                }
            };

            // Print all the information
            debug!(
                "\n🔵 New {} Pool Processed Successfully!\n\
                 ─────────────────────────────────────────────────────\n\
                 ▶️ Pair:               {}\n\
                 ▶️ Mint:               {}\n\
                 ▶️ Opened:             {}\n\
                 ▶️ Pooled Tokens:      {}\n\
                 ▶️ Pooled Base:        {}\n\
                 ▶️ Token Price:        {:.10}\n\
                 ▶️ Symbol:             {}\n\
                 ▶️ Supply:             {}\n\
                 ▶️ Decimals:           {}\n\
                 ▶️ Owner:              {}\n\
                 ▶️ Can Owner Mint?:    {}\n\
                 ▶️ Can Owner Freeze?:  {}\n\
                 ▶️ Metadata Mutable?:  {}\n\
                 ▶️ Strategy:           {:?}\n\
                 ⚡️ Processed in:       {} ms\n",
                match pool.pool_type {
                    PoolType::RaydiumAmm => "Raydium AMM",
                    PoolType::RaydiumClmm => "Raydium CLMM",
                },
                pool.pool_address,
                pool.base_mint,
                pool.open_time.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                base_reserves.ui_amount_string,
                quote_reserves.ui_amount_string,
                price,
                token_metadata.map(|m| m.symbol).unwrap_or("N/A".to_string()),
                base_mint.supply,
                base_mint.decimals,
                base_mint.mint_authority.unwrap_or_default().to_string(),
                if mint_renounced { "No" } else { "Yes" },
                if freezable { "Yes" } else { "No" },
                if meta_mutable { "Yes" } else { "No" },
                strategy_name,
                (chrono::Utc::now().time() - start_time).num_milliseconds()
            );

            let start_time = chrono::Utc::now().time();

            // Create a new trade instance and its orders
            let pool_clone = pool.clone();
            let trade = Trade::new(
                TradeExchange::Raydium,
                pool_clone,
                wallet.pubkey,
                strategy_name,
                strategy,
            );

            // Execute the buy order for the trade and schedule the sell orders based on the strategy
            let trade = match Self::create_buy_order(trade, executor.clone(), price).await {
                Ok(trade) => trade,
                Err(_) => {
                    warn!("Failed to create trade orders for the pool");
                    return;
                }
            };

            // Add the new trade to the active trades hashmap
            if let Some(mut existing_trades) = active_trades.get_mut(&pool.pubkey) {
                existing_trades.push(trade.clone());
            } else {
                // Create a new vector and insert the trade
                active_trades.insert(pool.pubkey, vec![trade.clone()]);
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
                trade.pool.pubkey.to_string(),
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
        let approx_amount_out = amount_in / current_price;
        let min_amount_out = approx_amount_out - (approx_amount_out * (trade.strategy.buy_slippage / 100.0));

        let buy_order = Order::new(
            OrderDirection::BaseIn,
            trade.id,
            OrderKind::SimpleBuy,
            trade.pool.pubkey,
            amount_in,
            min_amount_out,
            trade.strategy.buy_delay,
        );

        // Register the buy order for the trade
        match trade.initiate_buy(buy_order.clone()) {
            Ok(_) => (),
            Err(e) => {
                error!(
                    "Failed to register buy order for the pool: {}.\n Error: {}",
                    trade.pool.pubkey.to_string(), e
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
                    trade.pool.pubkey.to_string(), e
                );
                return Err(());
            }
        };

        Ok(trade)
    }

    /// Chooses the best strategy based on the pool data and the trader configuration.
    /// The function returns the configuration of the chosen strategy.
    fn choose_strategy(strategies: HashMap<String, StrategyConfig>, params: ChooseStrategyParams) -> Result<(String, StrategyConfig), ()> {
        // Check that strategies are defined in the config
        if strategies.is_empty() {
            error!("No strategies defined in the configuration");
            return Err(());
        }

        // Iterate over the strategies in the config and check if the conditions are met
        for (name, strategy) in strategies.iter() {
            // Check should the token be freezable
            if strategy.freezable != params.freezable {
                debug!("Freezable condition not met");
                continue;
            }

            // Can the mint authority mint new tokens
            if strategy.mint_renounced != params.mint_renounced {
                debug!("Mint renounced condition not met");
                continue;
            }

            // Can the metadata be changed
            if !strategy.meta_mutable.is_none() && strategy.meta_mutable.unwrap() != params.meta_mutable {
                debug!("Metadata mutable condition not met");
                continue;
            }

            // Check quote symbol conditions
            if let Some(quote_symbol) = strategy.quote_symbol.clone() {
                let quote_mint = params.pool.quote_mint.to_string();
                // Check is the quote mint is wrapped SOL or USDC
                if (quote_symbol == "WSOL" && quote_mint != WSOL_MINT) ||
                    (quote_symbol == "USDC" && quote_mint != USDC_MINT) {
                    debug!("Quote symbol condition not met");
                    continue;
                }
            }

            // Check if the pool detected time is within the max delay
            if params.pool.open_time.timestamp() > 0 {
                let elapsed = chrono::Utc::now() - params.pool.timestamp;
                if elapsed.num_milliseconds() > strategy.max_delay_since_detect as i64 {
                    debug!("Max delay since open condition not met. {}, {}, {}",
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
                debug!("Price condition not met");
                continue;
            }

            // Check reserves conditions
            let scaled_quote_reserves = token_amount_to_float(
                &params.quote_reserves.amount,
                params.pool.quote_decimals.unwrap_or(9),
            );
            if Trader::is_condition_met(
                strategy.reserves_quote_condition.clone(),
                scaled_quote_reserves,
                Some(strategy.reserves_quote.unwrap_or(0.0)),
                strategy.reserves_quote_range.clone(),
            ) {
                debug!("Quote reserves condition not met");
                continue;
            }

            let scaled_base_reserves = token_amount_to_float(
                &params.base_reserves.amount,
                params.pool.base_decimals.unwrap_or(9),
            );
            if Trader::is_condition_met(
                strategy.reserves_base_condition.clone(),
                scaled_base_reserves,
                Some(strategy.reserves_base.unwrap_or(0.0)),
                strategy.reserves_base_range.clone(),
            ) {
                debug!("Base reserves condition not met");
                continue;
            }

            // Check total supply conditions
            if Trader::is_condition_met(
                strategy.total_supply_condition.clone(),
                params.base_mint.supply as f64,
                Some(strategy.total_supply.unwrap_or(0.0)),
                strategy.total_supply_range.clone(),
            ) {
                debug!("Total supply condition not met");
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

    async fn get_token_mint(client: Arc<RpcClient>, token_mint: &Pubkey) -> Result<Mint, Box<dyn std::error::Error + Send + Sync>> {
        // Get the account data for the token vault mint
        let token_mint_account = match client.get_account(token_mint).await {
            Ok(account) => account,
            Err(e) => {
                error!("Failed to get token mint account: {}", e);
                return Err(Box::new(e));
            }
        };

        // Unpack the mint data
        let mint = match Mint::unpack_from_slice(&token_mint_account.data) {
            Ok(mint) => mint,
            Err(e) => {
                error!("Failed to unpack mint data: {}", e);
                return Err(Box::new(e));
            }
        };
        Ok(mint)
    }

    async fn get_token_metadata(client: Arc<RpcClient>, mint_address: &Pubkey) -> Result<Option<Metadata>, Box<dyn std::error::Error + Send + Sync>> {
        // Derive the PDA (Program Derived Address) for the token metadata
        let mpl_program_id = Pubkey::from_str(METADATA_PROGRAM_ID).unwrap();
        let (metadata_pda, _bump_seed) = Pubkey::find_program_address(
            &[
                b"metadata",
                &mpl_program_id.to_bytes(),
                mint_address.as_ref(),
            ],
            &mpl_program_id,
        );

        // Fetch the metadata account
        let account_data = match client.get_account_data(&metadata_pda).await {
            Ok(data) => data,
            Err(e) => {
                error!("Failed to get metadata account data: {}", e);
                return Ok(None);
            }
        };

        // Deserialize the data into the Metadata struct
        let metadata = match Metadata::from_bytes(&account_data) {
            Ok(metadata) => metadata,
            Err(e) => {
                error!("Failed to deserialize metadata account data: {}", e);
                return Ok(None);
            }
        };

        Ok(Some(metadata))
    }
}

fn current_timestamp() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}