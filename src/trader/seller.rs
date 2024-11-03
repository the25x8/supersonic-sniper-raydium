use std::sync::Arc;
use chrono::TimeZone;
use dashmap::DashMap;
use log::{debug, error, info, warn};
use solana_sdk::pubkey::Pubkey;
use crate::config::ExecutorType;
use crate::executor::Executor;
use crate::executor::order::{Order, OrderDirection, OrderKind, OrderStatus};
use crate::market::monitor::{MarketData};
use crate::trader::trade::Trade;

/// Checks if some sell conditions are met and executes the sell order.
pub async fn try_to_sell(
    market_data: &MarketData,
    executor: Arc<Executor>,
    active_trades: Arc<DashMap<Pubkey, Vec<Trade>>>,
) {
    // Get a snapshot of the trades for the pool
    let trades_list = {
        if let Some(trades_ref) = active_trades.get(&market_data.pool) {
            trades_ref.value().clone()
        } else {
            return;
        }
    };

    // Now we can iterate over the trades without holding the lock
    for trade in trades_list {
        // Skip trade if completed
        if trade.is_completed() {
            return;
        }

        // Buy order shouldn't be None and should be completed
        let buy_order = trade.buy_order.clone();
        if buy_order.is_none() || buy_order.as_ref().unwrap().status != OrderStatus::Completed {
            warn!("Buy order not found or not completed for Trade {}", trade.id);
            return;
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
                    Err(err) => {
                        error!(
                        "❌ Failed to execute take profit order for Trade {}: {}",
                        trade.id,
                        err
                    );
                    }
                }

                // Abort if the take profit order was executed
                return;
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
                    Err(err) => {
                        error!(
                        "❌ Failed to execute stop loss order for Trade {}: {}",
                        trade.id,
                        err
                    );
                    }
                }

                // Abort if the stop loss order was executed
                return;
            }
        }

        // Volatility filter check. If there is no volatility in the market,
        // sell the tokens immediately to avoid potential losses.
        if trade.strategy.volatility_filter.enabled {
            debug!(
                "\n🔵 Checking Volatility Filter for Trade {}\n\
                ──────────────────────────────────────────────\n\
                - Buy Price:         {:.10}\n\
                - Current Price:     {:.10}\n\
                - Price Change:      {:.2}%\n\
                - Time Window:       {} ms\n\
                - Strategy:          Volatility Filter\n",
                trade.id,
                trade.buy_price,
                market_data.price,
                trade.strategy.volatility_filter.change_percent,
                trade.strategy.volatility_filter.window
            );

            // Get buy order from the trade
            let buy_order = trade.buy_order.as_ref().unwrap();

            // Get the buy confirmed time
            let buy_confirmed_at = match chrono::Utc.timestamp_opt(buy_order.confirmed_at as i64, 0).single() {
                Some(timestamp) => timestamp,
                None => {
                    error!("Invalid timestamp for trade {}", trade.id);
                    return;
                }
            };

            let elapsed_ms = (chrono::Utc::now() - buy_confirmed_at).num_milliseconds();
            let timeout_ms = trade.strategy.volatility_filter.timeout;
            let window_ms = trade.strategy.volatility_filter.window;

            // If the total elapsed time exceeds the timeout plus window, skip the trade
            if elapsed_ms > (timeout_ms + window_ms) {
                return;
            }

            // Check if the timeout has been reached
            if elapsed_ms >= timeout_ms {
                // Calculate the price change percentage
                let price_change_percent = ((market_data.price - trade.buy_price) / trade.buy_price) * 100.0;

                // If the price change is within the threshold (i.e., low volatility)
                if price_change_percent.abs() < trade.strategy.volatility_filter.change_percent {
                    // Create emergency sell order on the fly
                    let emergency_order = Order::new(
                        OrderDirection::QuoteOut,
                        trade.id,
                        OrderKind::Emergency,
                        &trade.pool_keys,
                        buy_order.balance_change,
                        0,
                        &buy_order.out_mint,
                        &buy_order.in_mint,
                        buy_order.out_decimals,
                        buy_order.in_decimals,
                        // Use RPC executor and sell immediately
                        ExecutorType::RPC,
                        0,
                        0,
                    );

                    // Register the emergency order for the trade
                    if let Some(mut trades_ref) = active_trades.get_mut(&market_data.pool) {
                        if let Some(trade_in_map) = trades_ref.value_mut().iter_mut().find(|t| t.id == trade.id) {
                            if let Err(e) = trade_in_map.initiate_sell(emergency_order.clone()) {
                                error!("Failed to initiate sell for trade {}: {}", trade.id, e);
                                return;
                            }
                        }
                        // trades_ref mutable reference is dropped here
                    }

                    // Execute the order immediately
                    match executor.order(emergency_order).await {
                        Ok(_) => {
                            info!(
                                "\n🔴 Volatility Filter Triggered for Trade {}\n\
                                ──────────────────────────────────────────────\n\
                                - Buy Price:         {:.10}\n\
                                - Current Price:     {:.10}\n\
                                - Price Change:      {:.2}%\n\
                                - Time Window:       {} ms\n\
                                - Strategy:          Volatility Filter\n\
                                \
                                Emergency Sell Order Executed!",
                                trade.id,
                                trade.buy_price,
                                market_data.price,
                                trade.strategy.volatility_filter.change_percent,
                                trade.strategy.volatility_filter.window,
                            );
                        }
                        Err(e) => {
                            error!(
                            "❌ Failed to execute emergency sell order for Trade {}: {}",
                            trade.id, e
                        );
                        }
                    }

                    // Abort if the emergency sell order was executed
                    return;
                }
            }
        }
    }
}