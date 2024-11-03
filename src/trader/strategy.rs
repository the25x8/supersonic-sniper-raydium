use std::collections::HashMap;
use log::warn;
use crate::config::StrategyConfig;
use crate::detector::AMMPool;
use crate::solana::quote_mint::USDC_MINT;

pub struct ChooseStrategyParams {
    pub pool: AMMPool,
    pub freezable: bool,
    pub mint_renounced: bool,
    pub meta_mutable: bool,
    pub base_supply: u64,
    pub base_reserves: f64,
    pub quote_reserves: f64,
    pub price: f64,
}

/// Chooses the best strategy based on the pool data and the trader configuration.
/// The function returns the configuration of the chosen strategy.
pub fn choose_strategy(strategies: &HashMap<String, StrategyConfig>, params: ChooseStrategyParams) -> Result<(String, StrategyConfig), ()> {
    // Check that strategies are defined in the config
    if strategies.is_empty() {
        warn!("No strategies defined in the configuration");
        return Err(());
    }

    // Iterate over the strategies in the config and check if the conditions are met
    for (name, strategy) in strategies.iter() {
        // Check should the token be freezable
        if strategy.freezable != params.freezable {
            warn!("Freezable condition not met");
            continue;
        }

        // Can the mint authority mint new tokens
        if strategy.mint_renounced != params.mint_renounced {
            warn!("Mint renounced condition not met");
            continue;
        }

        // Can the metadata be changed
        if strategy.meta_mutable.is_some() && strategy.meta_mutable.unwrap() != params.meta_mutable {
            warn!("Metadata mutable condition not met");
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
                warn!("Quote symbol condition not met");
                continue;
            }
        }

        // Check if the pool detected time is within the max delay
        if params.pool.open_time.timestamp() > 0 {
            let elapsed = chrono::Utc::now() - params.pool.timestamp;
            if elapsed.num_milliseconds() > strategy.max_delay_since_detect as i64 {
                warn!("Max delay since open condition not met. {}, {}, {}",
                        params.pool.open_time.timestamp_millis(),
                        elapsed.num_milliseconds(),
                        strategy.max_delay_since_detect,
                    );
                continue;
            }
        }

        // TODO Check additional data conditions, like website, social networks, audits, etc.

        // Check price conditions
        if is_condition_met(
            strategy.price_condition.clone(),
            params.price,
            strategy.price,
            strategy.price_range.clone(),
        ) {
            warn!("Price condition not met");
            continue;
        }

        // Check reserves conditions
        if is_condition_met(
            strategy.reserves_quote_condition.clone(),
            params.quote_reserves,
            Some(strategy.reserves_quote.unwrap_or(0.0)),
            strategy.reserves_quote_range.clone(),
        ) {
            warn!("Quote reserves condition not met");
            continue;
        }

        if is_condition_met(
            strategy.reserves_base_condition.clone(),
            params.base_reserves,
            Some(strategy.reserves_base.unwrap_or(0.0)),
            strategy.reserves_base_range.clone(),
        ) {
            warn!("Base reserves condition not met");
            continue;
        }

        // Check total supply conditions
        if is_condition_met(
            strategy.total_supply_condition.clone(),
            params.base_supply as f64,
            Some(strategy.total_supply.unwrap_or(0.0)),
            strategy.total_supply_range.clone(),
        ) {
            warn!("Base supply condition not met");
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
fn is_condition_met(condition: Option<String>, value: f64, target: Option<f64>, target_range: Option<Vec<f64>>) -> bool {
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