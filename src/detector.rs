use std::str::FromStr;
use std::sync::Arc;
use chrono::DateTime;
use dashmap::DashSet;
use log::{debug, error, info};
use mpl_token_metadata::accounts::Metadata;
use serde::{Deserialize, Serialize};
use solana_account_decoder::parse_token::UiTokenAmount;
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_program::program_option::COption;
use solana_program::program_pack::Pack;
use solana_sdk::pubkey::Pubkey;
use spl_token::state::Mint;
use tokio::sync::mpsc;
use tokio::sync::mpsc::Sender;
use tokio_util::sync::CancellationToken;
use crate::config::AppConfig;
use crate::market::serum;
use crate::error::handle_attempt;
use crate::{market, raydium};
use crate::raydium::{detector, MainnetProgramId};
use crate::solana::amount_utils::token_amount_to_float;
use crate::solana::quote_mint::USDC_MINT;

/// The Detector module is responsible for monitoring the Solana blockchain,
/// specifically the Raydium program, to detect new liquidity pools and trading pairs.
/// It uses the Solana RPC client to subscribe to program accounts and filters them
/// based on the watch list provided in the configuration.

/// High-level representation of the liquidity pool keys including the program ID, authority.
/// It also includes the keys for the base and quote vaults, the LP mint, and the market keys.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PoolKeys {
    pub id: Pubkey,
    pub base_mint: Pubkey,
    pub quote_mint: Pubkey,
    pub lp_mint: Pubkey,
    pub version: u8,
    pub program_id: Pubkey,
    pub authority: Pubkey,
    pub base_vault: Pubkey,
    pub quote_vault: Pubkey,
    pub lp_vault: Pubkey,
    pub open_orders: Pubkey,
    pub target_orders: Pubkey,
    pub withdraw_queue: Pubkey,
    pub market_version: u8,
    pub market_program_id: Pubkey,
    pub market_id: Pubkey,
    pub market_authority: Pubkey,
    pub market_base_vault: Pubkey,
    pub market_quote_vault: Pubkey,
    pub market_bids: Pubkey,
    pub market_asks: Pubkey,
    pub market_event_queue: Pubkey,
    pub trade_fee_rate: u64,
}

/// Pool is a struct to represent the detected pool data,
/// it also includes serum market data and other meta info
/// that program needs during the trading process.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pool {
    pub slot: u64,

    // Pool data
    pub keys: PoolKeys, // Pool keys
    pub pool_type: PoolType,
    pub initial_price: f64, // Initial price of the base token in quote token
    pub in_whitelist: bool, // If base or quote token is in the watchlist

    // Base token data
    pub base_decimals: u8,
    pub base_reserves: f64,
    pub base_supply: u64,

    // Base token metadata from MPL Token Metadata program
    pub base_name: String,
    pub base_symbol: String,
    pub base_uri: String,
    pub base_freezable: bool,
    pub base_mint_renounced: bool,
    pub base_meta_mutable: bool,
    pub base_mint_authority: Pubkey,

    pub quote_decimals: u8,
    pub quote_reserves: f64,

    pub open_time: DateTime<chrono::Utc>, // Pool open time
    pub timestamp: DateTime<chrono::Utc>, // Timestamp when the pool was detected
}

/// Enum to represent the type of pool detected.
#[repr(u8)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PoolType {
    RaydiumAmm = 1,
    RaydiumClmm = 2,
}

/// Detected pool is internal structure to hold the detected pool data
/// with meta info such as subscriber type and start time.
#[derive(Debug, Clone)]
pub struct DetectedPool {
    pub data: Pool,
    pub source: SourceType, // Source is a channel where the pool was detected
    pub start_timestamp: DateTime<chrono::Utc>, // Subscription start time
}

#[derive(Debug, Clone, PartialEq)]
#[repr(u8)]
pub enum SourceType {
    PubSub = 1,
    BloxRoute = 2,
}

impl SourceType {
    pub fn to_string(&self) -> String {
        match self {
            SourceType::PubSub => "PubSub".to_string(),
            SourceType::BloxRoute => "BloxRoute".to_string(),
        }
    }
}

const MAX_FETCH_RETRIES: u32 = 3;

/// Run the detector module to monitor the Solana blockchain for new pools.
pub async fn run(
    config: Arc<AppConfig>,
    client: Arc<RpcClient>,
    tx: Sender<Pool>,
    cancel_token: CancellationToken,
) {
    // Create watchlist to filter pools based on the base token
    let watchlist = Arc::new(DashSet::new());

    // Add watchlist tokens from the config to the set
    config.detector.watchlist.iter().for_each(|token| {
        watchlist.insert(Pubkey::from_str(token).unwrap());
    });

    // Create the internal detected pool channel
    let (internal_tx, mut internal_rx) = mpsc::channel::<DetectedPool>(10);

    // Initialize pool keys cache to avoid duplicate pools
    let processed_pool_cache = Arc::new(DashSet::new());

    // The set of pools that were initialized after the detector started.
    // After initialization, we have only mint addresses but not the pool keys.
    let initialized_pool_cache = Arc::new(DashSet::new());

    // Initialize the Raydium detector
    let pool_tx = internal_tx.clone();
    let raydium_detector = detector::RaydiumDetector::new(
        client.clone(),
        pool_tx.clone(),
        initialized_pool_cache.clone(),
        config.rpc.ws_url.as_str(),
        &config.bloxroute,
        cancel_token.clone(),
    ).await;

    // Start all internal tasks
    tokio::select!(
        // Start the Raydium detector
        _ = raydium_detector.start() => {}

        // Start the pool receiver task
        _ = async {
            while let Some(detected_pool) = internal_rx.recv().await {
                let mut pool = detected_pool.data;
                let start_time = chrono::Utc::now().time();

                // If pool is already in the processed pool cache, skip it.
                if processed_pool_cache.contains(&pool.keys.id) {
                    continue
                } else {
                    processed_pool_cache.insert(pool.keys.id.clone());
                }

                // Adjust quote and base token mints if base mint is WSOL or USDC
                // to prevent fetching base and metadata data for these mints.
                // Get real base mint.
                let base_mint = {
                    // If base mint is WSOL or USDC, the real base mint is quote mint.
                    if pool.keys.base_mint == spl_token::native_mint::id() || pool.keys.base_mint.to_string() == USDC_MINT {
                        pool.keys.quote_mint;
                    }

                    // Otherwise, use the base mint.
                    pool.keys.base_mint
                };

                // If watchlist is set, allow only pools with base or quote token in the watchlist
                if !watchlist.is_empty() {
                    // Skip if the pool is not in the watchlist
                    if !watchlist.contains(&base_mint) {
                        continue
                    }

                    // Mark the pool as in the watchlist
                    pool.in_whitelist = true;
                }

                let is_pubsub = detected_pool.source == SourceType::PubSub;

                // If pool is not in watchlist and source is PubSub, we need to check if the pool key
                // is in the initialized pool cache. That's the way how we can filter out the pools
                // that were initialized after the detector started.
                if is_pubsub && !pool.in_whitelist && !initialized_pool_cache.contains(&pool.keys.id) {
                    continue
                }

                // Process all async operations in the tokio task
                let tx_clone = tx.clone();
                let rpc_client = client.clone();
                tokio::spawn(async move {
                    // Based on the detected pool source, fetch the required data.
                    // In case of PubSub fetch reserves, market data and token metadata.
                    if is_pubsub {
                        // Fetch all the required data concurrently
                        let (
                            pool_reserves_result,
                            market_state_result,
                            token_metadata_result,
                            token_mint_result
                        ) = tokio::join!(
                            raydium::get_amm_pool_reserves(
                                rpc_client.clone(),
                                &pool.keys.base_vault,
                                &pool.keys.quote_vault,
                            ),
                            serum::get_serum_market_state(rpc_client.clone(), &pool.keys.market_id),
                            get_token_metadata(rpc_client.clone(), &base_mint),
                            get_token_mint(rpc_client.clone(), &base_mint)
                        );

                        // Update the pool data with the results
                        update_pool_by_results(
                            &mut pool,
                            Some(pool_reserves_result),
                            Some(market_state_result),
                            Some(token_mint_result),
                            Some(token_metadata_result),
                        );
                    } else {
                        // In case of BloxRoute, we already have the market data and reserves available.
                        // Fetch the only token metadata and mint data and update the pool.
                        let (
                            token_metadata_result,
                            token_mint_result
                        ) = tokio::join!(
                            get_token_metadata(rpc_client.clone(), &base_mint),
                            get_token_mint(rpc_client.clone(), &base_mint)
                        );

                        // Update the pool data with the results
                        update_pool_by_results(
                            &mut pool,
                            None,
                            None,
                            Some(token_mint_result),
                            Some(token_metadata_result),
                        );

                        // Set quote token decimals to WSOL decimals.
                        // TODO Check is quote WSOL or USDC and set decimals accordingly
                        pool.quote_decimals = spl_token::native_mint::DECIMALS;

                        // Calculate price using the correct reserve amounts
                        pool.base_reserves = pool.base_reserves / 10_usize.pow(pool.base_decimals as u32) as f64;
                        pool.quote_reserves = pool.quote_reserves / 10_usize.pow(pool.quote_decimals as u32) as f64;
                        pool.initial_price = pool.quote_reserves / pool.base_reserves;

                        // Calculate the price of base token in terms of quote token
                        pool.initial_price = market::price::adjust_tokens_and_calculate_price(
                            pool.keys.base_mint,
                            pool.keys.quote_mint,
                            pool.base_reserves,
                            pool.quote_reserves,
                        );

                        // All other data is already set by the Raydium detector.
                    }

                    error!("BASE SUPPLY: {}", pool.base_supply);

                    // Print all the information
                    info!(
                        "\n🔵 [POOL DETECTED] New Raydium Pool Identified!\n\
                         ─────────────────────────────────────────────────────\n\
                         🏦 Pool:               {}\n\
                         🌍 Source:             {}\n\
                         📊 Type:               {}\n\
                         ⏳ Open Time:          {}\n\
                         ▶️ Mint A:             {}\n\
                         ▶️ Mint B:             {}\n\
                         ▶️ Reserves A:         {}\n\
                         ▶️ Reserves B:         {}\n\
                         ▶️ Token Price:        {:.10} WSOL\n\
                         ▶️ Symbol:             {}\n\
                         ▶️ Supply:             {}\n\
                         ▶️ Decimals:           {}\n\
                         ▶️ Owner:              {}\n\
                         ▶️ Can Owner Mint?:    {}\n\
                         ▶️ Can Owner Freeze?:  {}\n\
                         ▶️ Metadata Mutable?:  {}\n\
                         ⚡️ Processed in:       {} ms\n",
                        pool.keys.id.to_string(),
                        detected_pool.source.to_string(),
                        match pool.pool_type {
                            PoolType::RaydiumAmm => "Raydium AMM",
                            PoolType::RaydiumClmm => "Raydium CLMM",
                        },
                        pool.open_time.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                        pool.keys.base_mint.to_string(),
                        pool.keys.quote_mint.to_string(),
                        pool.base_reserves.to_string(),
                        pool.quote_reserves.to_string(),
                        pool.initial_price,
                        pool.base_symbol,
                        pool.base_supply,
                        pool.base_decimals,
                        pool.base_mint_authority,
                        if pool.base_mint_renounced { "No" } else { "Yes" },
                        if pool.base_freezable { "Yes" } else { "No" },
                        if pool.base_meta_mutable { "Yes" } else { "No" },
                        (chrono::Utc::now().time() - start_time).num_milliseconds()
                    );

                    // Send the detected pool data to the bot pool channel
                    if let Err(e) = tx_clone.send(pool).await {
                        error!("Failed to send detected pool to bot pool channel: {}", e.to_string());
                    }
                });
            }
        } => {}

        else => return,
    );
}

async fn get_token_mint(client: Arc<RpcClient>, token_mint: &Pubkey) -> Result<Mint, Box<dyn std::error::Error + Send + Sync>> {
    let mut attempts = 0;

    error!("GETTING TOKEN MINT FOR: {}", token_mint);

    loop {
        // Get the account data for the token vault mint
        let token_mint_account = match client.get_account(token_mint).await {
            Ok(account) => account,
            Err(e) => {
                error!("Failed to get token mint account: {}", e);

                // Increment retry attempt counter and handle retry logic
                match handle_attempt(&mut attempts, MAX_FETCH_RETRIES, 300).await {
                    Ok(_) => continue,
                    Err(_) => return Err(Box::new(e)),
                }
            }
        };

        // Unpack the mint data
        return match Mint::unpack_from_slice(&token_mint_account.data) {
            Ok(mint) => Ok(mint),
            Err(e) => {
                error!("Failed to unpack mint data: {}", e);
                Err(Box::new(e))
            }
        };
    }
}

async fn get_token_metadata(client: Arc<RpcClient>, mint_address: &Pubkey) -> Result<Option<Metadata>, Box<dyn std::error::Error + Send + Sync>> {
    let mut attempts = 0;

    loop {
        // Derive the PDA (Program Derived Address) for the token metadata
        let mpl_program_id = Pubkey::from_str(&mpl_token_metadata::programs::MPL_TOKEN_METADATA_ID.to_string()).unwrap();
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

                // Increment retry attempt counter and handle retry logic
                match handle_attempt(&mut attempts, MAX_FETCH_RETRIES, 300).await {
                    Ok(_) => continue,
                    Err(_) => return Err(Box::new(e)),
                }
            }
        };

        // Deserialize the data into the Metadata struct
        return match Metadata::from_bytes(&account_data) {
            Ok(metadata) => Ok(Some(metadata)),
            Err(e) => {
                error!("Failed to deserialize metadata account data: {}", e);
                Ok(None)
            }
        };
    }
}

fn update_pool_by_results(
    pool: &mut Pool,
    pool_reserves_result: Option<Result<Vec<UiTokenAmount>, Box<dyn std::error::Error + Send + Sync>>>,
    market_state_result: Option<Result<serum::MarketStateV3, Box<dyn std::error::Error + Send + Sync>>>,
    token_mint_result: Option<Result<Mint, Box<dyn std::error::Error + Send + Sync>>>,
    token_metadata_result: Option<Result<Option<Metadata>, Box<dyn std::error::Error + Send + Sync>>>,
) {
    // Assign the pool reserves if some
    if let Some(pool_reserves_result) = pool_reserves_result {
        let (base_reserves, quote_reserves) = match pool_reserves_result {
            Ok(balances) => (balances[0].clone(), balances[1].clone()),
            Err(e) => {
                error!("Failed to get pool liquidity: {}", e);
                return;
            }
        };

        // Assign the reserves to the pool
        pool.base_reserves = token_amount_to_float(&base_reserves.amount, pool.base_decimals);
        pool.quote_reserves = token_amount_to_float(&quote_reserves.amount, pool.quote_decimals);

        // Calculate the price of base token in terms of quote token
        pool.initial_price = match market::price::convert_reserves_to_price(
            pool.keys.base_mint,
            pool.keys.quote_mint,
            &base_reserves,
            &quote_reserves,
        ) {
            Ok(price) => price,
            Err(e) => {
                error!("Failed to calculate token price: {}", e);
                return;
            }
        };
    }

    // Collect the market state
    if let Some(market_state_result) = market_state_result {
        let market_state = match market_state_result {
            Ok(state) => state,
            Err(e) => {
                error!("Failed to get serum market state: {}", e);
                return;
            }
        };

        // Update the pool keys with the serum market data.
        // Market id should be set by the Raydium detector.
        pool.keys.market_version = 3;
        pool.keys.market_program_id = MainnetProgramId::SerumMarket.get_pubkey();
        pool.keys.market_authority = market_state.own_address;
        pool.keys.market_base_vault = market_state.coin_vault;
        pool.keys.market_quote_vault = market_state.pc_vault;
        pool.keys.market_bids = market_state.bids;
        pool.keys.market_asks = market_state.asks;
        pool.keys.market_event_queue = market_state.event_queue;
        pool.keys.trade_fee_rate = market_state.fee_rate_bps;
    }

    // Collect the token base mint
    if let Some(token_mint_result) = token_mint_result {
        let base_mint = match token_mint_result {
            Ok(mint) => mint,
            Err(e) => {
                error!("Failed to get token mint account: {}", e);
                return;
            }
        };

        // Set decimals and supply for the base token
        pool.base_decimals = base_mint.decimals;
        pool.base_supply = base_mint.supply;
        pool.base_freezable = base_mint.freeze_authority != COption::None;
        pool.base_mint_renounced = base_mint.mint_authority == COption::None;
        pool.base_mint_authority = base_mint.mint_authority.unwrap_or_default();
    }

    // Collect the token metadata
    if let Some(token_metadata_result) = token_metadata_result {
        let token_metadata = match token_metadata_result {
            Ok(metadata) => metadata,
            Err(e) => {
                error!("Failed to get token metadata: {}", e);
                return;
            }
        };

        // Set some metadata fields if available
        if let Some(metadata) = token_metadata {
            // Base token metadata
            pool.base_name = metadata.name;
            pool.base_symbol = metadata.symbol;
            pool.base_uri = metadata.uri;
            pool.base_meta_mutable = metadata.is_mutable;
        } else {
            pool.base_name = "N/A".to_string();
            pool.base_symbol = "N/A".to_string();
            pool.base_uri = "N/A".to_string();
            pool.base_meta_mutable = false;
        }
    }
}
