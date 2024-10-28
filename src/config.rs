use std::collections::HashMap;
use config::{Config, ConfigError, Environment, File};
use serde::{Deserialize, Serialize};

/// Struct to hold the application configuration.
/// Uses `serde` for easy deserialization from configuration files and environment variables.
#[derive(Debug, Deserialize, Clone)]
pub struct AppConfig {
    pub rpc: RPCConfig,
    pub bloxroute: BloxrouteConfig,
    pub jito: JitoConfig,
    pub wallet: WalletConfig,
    pub detector: DetectorConfig,
    pub trader: TraderConfig,
    pub executor: ExecutorConfig,
}

#[derive(Debug, Deserialize, Clone)]
pub struct RPCConfig {
    pub url: String, // Solana RPC URL is required. It can be a public or private RPC node.
    pub ws_url: String, // WebSocket URL is required for account updates.
}

#[derive(Debug, Deserialize, Clone)]
pub struct BloxrouteConfig {
    // If enabled, the bot will use Bloxroute to subscribe to account updates
    // and sending transactions if bloxroute is set as the executor.
    pub enabled: bool,

    pub url: Option<String>,
    pub ws_url: Option<String>,
    pub auth_token: Option<String>, // Auth token is required when using Bloxroute.
}

#[derive(Debug, Deserialize, Clone)]
pub struct JitoConfig {
    pub enabled: bool,
    pub url: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct WalletConfig {
    // The bot will use the first account in the wallet to trade.
    // If you want to use a different account, you can specify the
    // account index here.
    pub account_index: Option<u32>,

    // Path to the wallet file. If you are using a wallet file, you can
    // specify the path to the file here.
    pub file_path: Option<String>,

    // Wallet secret key. If you are using a wallet secret key,
    // you can specify the key here.
    pub secret_key: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct DetectorConfig {
    // Watchlist is a list of tokens that the bot will focus on.
    // If the watchlist is empty, the bot will focus on all tokens.
    // Otherwise, the bot will process only tokens from the watchlist.
    pub watchlist: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct TraderConfig {
    pub strategies: HashMap<String, StrategyConfig>, // Map of strategies and their conditions.
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct StrategyConfig {
    pub freezable: bool, // If the token can be frozen set to true.
    pub mint_renounced: bool, // If the owner can mint new tokens set to false.
    pub meta_mutable: Option<bool>, // If the token metadata can be changed set to true.

    pub quote_symbol: Option<String>, // The quote symbol of the trading pair can be USDC or WSOL.
    pub base_symbol: Option<String>, // The base symbol of the trading pair can be SOL or SRM.

    // If the pool detected time is older than the max_delay_since_detect,
    // the bot will not trade. A checking will be performed before trade execution.
    pub max_delay_since_detect: u64, // In milliseconds.

    // Some base conditions can lead to additional data being fetched.
    // This will increase the time it takes to make a trading decision,
    // as the bot needs to fetch the data from different sources.
    pub has_social_networks: Option<bool>,
    pub has_community: Option<bool>,
    pub has_team: Option<bool>,
    pub has_audit: Option<bool>,
    pub has_whitepaper: Option<bool>,
    pub has_partnerships: Option<bool>,
    pub min_audits_passed: Option<u8>,
    pub min_social_networks: Option<u8>,

    // Price conditions. A price in the quote currency.
    pub price_condition: Option<String>, // Can be "gt", "gte", "lt", "lte", "eq", "in".
    pub price: Option<f64>, // Price in the quote currency.
    pub price_range: Option<Vec<f64>>, // List of prices in the quote currency.

    // Reserves conditions in the base and quote currencies.
    pub reserves_base_condition: Option<String>, // Can be "gt", "gte", "lt", "lte", "eq", "in".
    pub reserves_quote_condition: Option<String>,
    pub reserves_base: Option<f64>, // Reserves in the base currency.
    pub reserves_quote: Option<f64>, // Reserves in the quote currency.
    pub reserves_base_range: Option<Vec<f64>>, // List of reserves in the base currency.
    pub reserves_quote_range: Option<Vec<f64>>, // List of reserves in the quote currency.

    // Total supply conditions
    pub total_supply_condition: Option<String>, // Can be "gt", "gte", "lt", "lte", "eq", "in".
    pub total_supply: Option<f64>, // Total supply of the token.
    pub total_supply_range: Option<Vec<f64>>, // List of total supplies.

    // Market data conditions. Enable these conditions to filter tokens
    // based on volume, transactions, makers, buyers, sellers, buys, sells.
    // This data is fetched from the Solana Serum DEX API leads to additional
    // time to make a trading decision and recommended to be disabled.
    pub volume_condition: Option<String>, // Can be "gt", "gte", "lt", "lte", "eq", "in".
    pub volume: Option<f64>, // Volume in the base currency.
    pub volume_range: Option<Vec<f64>>, // List of volumes in the base currency.

    pub buy_volume_condition: Option<String>, // Can be "gt", "gte", "lt", "lte", "eq", "in".
    pub buy_volume: Option<f64>, // Buy volume in the base currency.
    pub buy_volume_range: Option<Vec<f64>>, // List of buy volumes in the base currency.

    pub sell_volume_condition: Option<String>, // Can be "gt", "gte", "lt", "lte", "eq", "in".
    pub sell_volume: Option<f64>, // Sell volume in the base currency.
    pub sell_volume_range: Option<Vec<f64>>, // List of sell volumes in the base currency.

    pub txns_condition: Option<String>, // Can be "gt", "gte", "lt", "lte", "eq", "in".
    pub txns: Option<u64>, // Transactions count.
    pub txns_range: Option<Vec<u64>>, // List of transactions counts.

    pub makers_condition: Option<String>, // Can be "gt", "gte", "lt", "lte", "eq", "in".
    pub makers: Option<u64>, // Makers count.
    pub makers_range: Option<Vec<u64>>, // List of makers counts.

    pub buyers_condition: Option<String>, // Can be "gt", "gte", "lt", "lte", "eq", "in".
    pub buyers: Option<u64>, // Buyers count.
    pub buyers_range: Option<Vec<u64>>, // List of buyers counts.

    pub sellers_condition: Option<String>, // Can be "gt", "gte", "lt", "lte", "eq", "in".
    pub sellers: Option<u64>, // Sellers count.
    pub sellers_range: Option<Vec<u64>>, // List of sellers counts.

    pub buys_condition: Option<String>, // Can be "gt", "gte", "lt", "lte", "eq", "in".
    pub buys: Option<u64>, // Buys count.
    pub buys_range: Option<Vec<u64>>, // List of buys counts.

    pub sells_condition: Option<String>, // Can be "gt", "gte", "lt", "lte", "eq", "in".
    pub sells: Option<u64>, // Sells count.
    pub sells_range: Option<Vec<u64>>, // List of sells counts.

    // Configuration for executing orders.
    pub buy_delay: u64, // Delay in milliseconds before executing the buy order.
    pub buy_slippage: f64, // Maximum slippage allowed for the buy order.
    pub buy_executor: ExecutorType, // Tx executor, can be "rpc" or "bloxroute"

    pub sell_delay: u64,
    pub sell_slippage: f64,
    pub sell_percent: f64, // Percentage of the base currency to sell.

    // Price impact percentage is impact of swap upon the pool's liquidity.
    // If impact is greater than the max_price_impact, the bot will not trade.
    // Zero means any price impact is allowed.
    pub max_price_impact: f64,

    pub max_retries: u8, // Number of retries if the buy order fails.
    pub quote_amount: f64, // Amount of quote currency to spend on the buy order.

    // After holding the token for the hold_time, the bot will
    // execute the sell order automatically by current price.
    pub hold_time: u64, // In milliseconds.

    pub take_profit: f64, // Take profit percentage.
    pub stop_loss: f64, // Stop loss percentage.

    // You can specify the separate executors for take profit and stop loss.
    pub take_profit_executor: ExecutorType,
    pub stop_loss_executor: Option<ExecutorType>,

    // The bribes are used to incentivize the validators to include the transaction
    // in the block. It will be used only if Bloxroute is enabled and auth token is provided.
    // It is recommended to use a 70/30 split between priority fee and jito tip
    pub buy_bribe: Option<f64>, // Amount in SOL
    pub sell_bribe: Option<f64>, // Amount in SOL
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub enum ExecutorType {
    RPC,
    Bloxroute,
    Jito,
}

impl ExecutorType {
    fn as_str(&self) -> &'static str {
        match self {
            ExecutorType::RPC => "rpc",
            ExecutorType::Bloxroute => "bloxroute",
            ExecutorType::Jito => "jito",
        }
    }

    fn from_str(s: &str) -> Option<Self> {
        match s {
            "rpc" => Some(ExecutorType::RPC),
            "bloxroute" => Some(ExecutorType::Bloxroute),
            "jito" => Some(ExecutorType::Jito),
            _ => None
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ExecutorConfig {
    // If enabled, the bot will use the TPU client to send transactions directly to leader.
    pub use_tpu: bool,

    // Bot will build the transaction via bloxroute API if enabled.
    pub build_tx_via_bloxroute: bool,

    // Settings for tx fees and limits.
    pub compute_unit_limit: u32, // The maximum unit limit for the transaction
    pub compute_unit_price: u64, // In lamports
}

/// Loads the application configuration from a specified file and overlays it with environment variables.
///
/// # Arguments
///
/// * `config_file` - A string slice that holds the name of the configuration file.
///
/// # Returns
///
/// * `Result<AppConfig, ConfigError>` - Returns an `AppConfig` instance on success or a `ConfigError` on failure.
pub fn load_config(config_file: &str) -> Result<AppConfig, ConfigError> {
    // Create a new configuration builder
    let builder = Config::builder()
        // Load configuration from the specified file (e.g., config.yml)
        .add_source(File::with_name(config_file))
        // Add in settings from the environment variables (with prefix 'SSS')
        .add_source(Environment::with_prefix("SSS").separator("_"));

    // Build the final configuration and deserialize it into the AppConfig struct
    builder.build()?.try_deserialize::<AppConfig>()
}
