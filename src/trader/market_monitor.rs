use solana_client::nonblocking::pubsub_client::PubsubClient;
use solana_client::rpc_config::{RpcAccountInfoConfig, RpcProgramAccountsConfig};
use solana_client::rpc_filter::{Memcmp, RpcFilterType};
use solana_account_decoder::{UiAccountData, UiAccountEncoding};
use solana_sdk::pubkey::Pubkey;
use solana_sdk::commitment_config::CommitmentConfig;
use tokio::sync::mpsc::{Receiver, Sender};
use log::{info, error, debug};
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use base64::Engine;
use borsh::BorshDeserialize;
use chrono::Utc;
use dashmap::{DashMap, DashSet};
use futures::StreamExt;
use solana_account_decoder::parse_token::UiTokenAmount;
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_client::rpc_filter::RpcFilterType::DataSize;

use crate::error::handle_attempt;
use crate::raydium::{LiquidityStateV4, MainnetProgramId};
use crate::solana::quote_mint::{USDC_MINT, WSOL_MINT};

// Maximum retry attempts for fetching data from the blockchain
const MAX_FETCH_RETRIES: u32 = 5;
const MAX_SUBSCRIPTION_RETRIES: u32 = 5;
const RECONNECT_BACKOFF: u64 = 1000; // Reconnection backoff in milliseconds

#[derive(Debug)]
pub struct MarketData {
    pub market: Pubkey,
    pub price: f64,
    pub base_reserves: f64,
    pub quote_reserves: f64,
    pub timestamp: std::time::Instant,
}

pub struct MarketStateLayoutV3 {
    pub base_lot_size: u64,
    pub quote_lot_size: u64,
    pub base_vault: Pubkey,
    pub quote_vault: Pubkey,
    pub base_mint: Pubkey,
    pub quote_mint: Pubkey,
    pub own_address: Pubkey,
    pub vault_signer_nonce: u64,
    pub base_deposits_total: u64,
    pub base_fees_accrued: u64,
    pub quote_deposits_total: u64,
    pub quote_fees_accrued: u64,
    pub quote_dust_threshold: u64,
    pub request_queue: Pubkey,
    pub event_queue: Pubkey,
    pub bids: Pubkey,
    pub asks: Pubkey,
    pub fee_rate_bps: u64,
    pub referrer_rebates_accrued: u64,
}

pub struct MarketMonitor {
    market_tx: Sender<MarketData>,
    rpc_client: Arc<RpcClient>,
    pubsub_client: Arc<PubsubClient>,
    watchlist: Arc<DashSet<Pubkey>>,
    watchlist_update_tx: Sender<Pubkey>,
    prices: Arc<DashMap<Pubkey, (f64, std::time::Instant)>>,
}

impl MarketMonitor {
    pub async fn new(
        rpc_client: Arc<RpcClient>,
        rpc_ws_url: &str,
        market_tx: Sender<MarketData>,
    ) -> Self {
        // Initialize the Pubsub client instance
        let pubsub_client = match PubsubClient::new(rpc_ws_url).await {
            Ok(client) => client,
            Err(e) => {
                panic!("Failed to create WebSocket client: {:?}", e);
            }
        };

        let prices = Arc::new(DashMap::new());
        let watchlist = Arc::new(DashSet::new());
        let pubsub_client = Arc::new(pubsub_client);

        let (watchlist_update_tx, watchlist_update_rx) = tokio::sync::mpsc::channel(32);
        let pubsub_client_clone = pubsub_client.clone();
        let watchlist_clone = watchlist.clone();
        let prices_clone = prices.clone();

        // Run the main market monitor task in the background
        let market_data_tx = market_tx.clone();
        let rpc_client_clone = rpc_client.clone();
        tokio::spawn(async move {
            Self::run(
                rpc_client_clone,
                pubsub_client_clone,
                market_data_tx,
                watchlist_update_rx,
                watchlist_clone,
                prices_clone,
            ).await;
        });

        Self {
            prices,
            watchlist,
            market_tx,
            rpc_client,
            pubsub_client,
            watchlist_update_tx,
        }
    }

    async fn run(
        rpc_client: Arc<RpcClient>,
        pubsub: Arc<PubsubClient>,
        market_tx: Sender<MarketData>,
        mut watchlist_update_rx: Receiver<Pubkey>,
        watchlist: Arc<DashSet<Pubkey>>,
        prices: Arc<DashMap<Pubkey, (f64, std::time::Instant)>>,
    ) {
        // Clone the Pubsub client instance
        let mut attempts = 0;

        // Task to update the watchlist with new pools.
        // To remove a pool from the watchlist, send the pool
        // pubkey to the watchlist_update_tx channel. If the pool
        // is new, it will be added to the watchlist.
        let watchlist_clone = watchlist.clone();
        tokio::spawn(async move {
            while let Some(pubkey) = watchlist_update_rx.recv().await {
                // Only acquire a write lock if the pool isn't already in the watchlist
                if !watchlist_clone.contains(&pubkey) {
                    watchlist_clone.insert(pubkey);
                    info!("🔍 Pool {} added to watchlist", pubkey);
                    continue;
                }

                // Otherwise, remove the pool from the watchlist
                watchlist_clone.remove(&pubkey);
                info!("🔍 Pool {} removed from watchlist", pubkey);
            }
        });

        loop {
            // Combine the custom filters with common filters
            let filters = vec![
                DataSize(752), // Filter by data size (752 bytes for Raydium LiquidityStateV4)

                // Filter by status field with SwapOnly status
                RpcFilterType::Memcmp(Memcmp::new_base58_encoded(
                    0, // Offset of status field
                    &[6, 0, 0, 0, 0, 0, 0, 0],
                )),

                // Filter by quote mint. Disable because token1/token2 mint can be swapped,
                // determine the base token after parsing the data.
                // RpcFilterType::Memcmp(Memcmp::new(
                //     432,
                //     MemcmpEncodedBytes::Base58(detector::WSOL_MINT.to_string()),
                // )),

                // Filter by market program ID
                // RpcFilterType::Memcmp(Memcmp::new(
                //     560,
                //     MemcmpEncodedBytes::Base58(MainnetProgramId::OpenbookMarket.get_pubkey().to_string()),
                // )),
            ];
            let config = RpcProgramAccountsConfig {
                filters: Some(filters),
                account_config: RpcAccountInfoConfig {
                    encoding: Some(UiAccountEncoding::Base64),
                    commitment: Some(CommitmentConfig::processed()),
                    ..RpcAccountInfoConfig::default()
                },
                with_context: Some(false),
                sort_results: None,
            };

            // Subscribe to the AMM program
            let (mut accounts, _unsubscribe) = match pubsub.program_subscribe(
                &MainnetProgramId::AmmV4.get_pubkey(),
                Some(config),
            ).await {
                Ok(subscription) => subscription,
                Err(e) => {
                    error!("Failed to subscribe to AMM program: {}", e);

                    // Try to reconnect, and return if max retries exceeded
                    if let Err(_) = handle_attempt(
                        &mut attempts,
                        MAX_SUBSCRIPTION_RETRIES,
                        RECONNECT_BACKOFF,
                    ).await {
                        error!("Max subscription retries for Raydium pool stream in Market Monitor exceeded");
                        break;
                    }

                    continue;
                }
            };

            // Reset the connection attempts counter
            attempts = 0;

            // Set the start timestamp when the subscription is successful
            let start_timestamp = Utc::now();

            info!(
                "\n\
                📡 Market Monitor Status\n\
                ▶️ Subscribed to Solana AMM Program\n\
                ───────────────────────────────────────"
            );

            // Process account updates
            while let Some(response) = accounts.next().await {
                // Read public key of amm pool
                let pubkey = match Pubkey::from_str(&response.value.pubkey) {
                    Ok(pk) => pk,
                    Err(e) => {
                        error!("Failed to parse pool pubkey: {}", e);
                        continue;
                    }
                };

                // Handle the account update in a separate task
                let rpc_client = rpc_client.clone();
                let market_tx = market_tx.clone();
                let prices = prices.clone();
                let watchlist = watchlist.clone();
                tokio::spawn(async move {
                    if let Err(e) = Self::handle_account_update(
                        pubkey,
                        &response.value.account.data,
                        rpc_client.clone(),
                        prices.clone(),
                        watchlist.clone(),
                        market_tx.clone(),
                    ).await {
                        error!("Failed to handle account update: {}", e);
                    }
                });
            }

            // Wait before retrying to avoid busy looping
            info!("Resubscribing to Raydium pool stream in Market Monitor...");

            // Try to reconnect after a delay
            if let Err(_) = handle_attempt(
                &mut attempts,
                MAX_SUBSCRIPTION_RETRIES,
                RECONNECT_BACKOFF,
            ).await {
                error!("Max reconnection retries for Raydium pool stream exceeded");
                break;
            }
        }

        // If the subscription task has exited, terminate the bot
        panic!("Market monitor task has exited unexpectedly, terminating...");
    }

    /// Add a pool to the watchlist
    pub async fn add_to_watchlist(&self, pubkey: Pubkey) {
        if let Err(e) = self.watchlist_update_tx.send(pubkey).await {
            error!("Failed to send watchlist update: {}", e);
        }
    }

    /// Remove a pool from the watchlist
    pub async fn remove_from_watchlist(&self, pubkey: Pubkey) {
        if let Err(e) = self.watchlist_update_tx.send(pubkey).await {
            error!("Failed to send watchlist update: {}", e);
        }
    }

    /// Handle account deserializes the liquidity state for AMM pools
    /// and emits market data to the market_tx channel.
    async fn handle_account_update(
        pubkey: Pubkey,
        data: &UiAccountData,
        rpc_client: Arc<RpcClient>,
        prices: Arc<DashMap<Pubkey, (f64, std::time::Instant)>>,
        watchlist: Arc<DashSet<Pubkey>>,
        market_tx: Sender<MarketData>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        match data {
            UiAccountData::Binary(data_base64, _encoding) => {
                if let Ok(data) = base64::prelude::BASE64_STANDARD.decode(data_base64) {
                    // Parse the liquidity state data
                    let mut state = match LiquidityStateV4::try_from_slice(&data) {
                        Ok(state) => state,
                        Err(e) => {
                            error!("Failed to parse liquidity state v4 data: {}", e);
                            return Err(e.into());
                        }
                    };

                    // Step 1: Check if the pool is in the watchlist
                    if !watchlist.contains(&pubkey) {
                        return Ok(());
                    }

                    // Step 2: Check the cache for price and timestamp
                    if let Some(entry) = prices.get(&pubkey) {
                        // If the price hasn't changed in the last 5 seconds, skip fetching
                        if entry.value().1.elapsed() < Duration::from_secs(5) {
                            return Ok(());
                        }
                    }

                    // Swap the vaults if the coin vault is USDC or WSOL.
                    // Because the coin vault is the base token and the pc vault is the quote token,
                    // we need to ensure that the base token is not USDC or WSOL.
                    if state.coin_vault_mint.to_string() == USDC_MINT ||
                        state.coin_vault_mint.to_string() == WSOL_MINT {
                        let temp_mint = state.coin_vault_mint;
                        let temp_vault = state.coin_vault;
                        let temp_decimals = state.pc_decimals;

                        // Swap the values
                        state.coin_vault_mint = state.pc_vault_mint;
                        state.coin_vault = state.pc_vault;
                        state.coin_decimals = state.pc_decimals;

                        state.pc_vault_mint = temp_mint;
                        state.pc_vault = temp_vault;
                        state.pc_decimals = temp_decimals;
                    }

                    // Step 3: Fetch the reserves of the AMM pool if cache is stale or empty
                    let (base_reserves, quote_reserves) = match get_amm_pool_reserves(
                        rpc_client,
                        &state.coin_vault,
                        &state.pc_vault,
                    ).await {
                        Ok(reserves) => (reserves[0].clone(), reserves[1].clone()),
                        Err(e) => {
                            error!("Failed to fetch AMM pool reserves: {}", e);
                            return Err(e);
                        }
                    };

                    // Step 4: Calculate the price of the base token in terms of the quote token
                    let price = match convert_reserves_to_price(&base_reserves, &quote_reserves) {
                        Ok(price) => price,
                        Err(e) => {
                            error!("Failed to calculate token price: {}", e);
                            return Err(e);
                        }
                    };

                    debug!(
                        "Market data updated for pool {}\n\
                        Base reserves: {}\n\
                        Quote reserves: {}\n\
                        Price: {}\n",
                        pubkey,
                        base_reserves.ui_amount_string.to_string(),
                        quote_reserves.ui_amount_string.to_string(),
                        price
                    );

                    // Step 5: Emit the market data to the market_tx channel
                    match market_tx.send(MarketData {
                        price, // New price of base token in terms of quote token
                        market: pubkey,
                        base_reserves: f64::from_str(&base_reserves.amount)?,
                        quote_reserves: f64::from_str(&quote_reserves.amount)?,
                        timestamp: std::time::Instant::now(),
                    }).await {
                        Ok(_) => (),
                        Err(e) => {
                            error!("Failed to send market data: {}", e);
                            return Err(e.into());
                        }
                    }

                    // Step 6: Update the cache with the new price and timestamp
                    prices.insert(pubkey, (price, std::time::Instant::now()));
                }

                Ok(())
            }
            _ => {
                error!("Unexpected data format for account {}", pubkey);
                Err("Unexpected data format".into())
            }
        }
    }
}

pub async fn get_amm_pool_reserves(
    client: Arc<RpcClient>,
    base_vault: &Pubkey,
    quote_vault: &Pubkey,
) -> Result<Vec<UiTokenAmount>, Box<dyn std::error::Error + Send + Sync>> {
    let mut attempts = 0;

    loop {
        // Fetch both base and quote vault balances concurrently
        let base_vault_future = client.get_token_account_balance(base_vault);
        let quote_vault_future = client.get_token_account_balance(quote_vault);

        match tokio::join!(base_vault_future, quote_vault_future) {
            (Ok(base_vault_balance), Ok(quote_vault_balance)) => {
                return Ok(vec![base_vault_balance, quote_vault_balance]);
            }
            (Err(e), _) | (_, Err(e)) => {
                error!("An error occurred while fetching AMM pool reserves: {}", e);

                // Increment retry attempt counter and handle retry logic
                match handle_attempt(
                    &mut attempts,
                    MAX_FETCH_RETRIES,
                    100,
                ).await {
                    Ok(_) => continue,
                    Err(_) => return Err(Box::new(e)),
                }
            }
        }
    }
}

pub fn convert_reserves_to_price(
    base_reserves: &UiTokenAmount,
    quote_reserves: &UiTokenAmount,
) -> Result<f64, Box<dyn std::error::Error + Send + Sync>> {
    // Convert base and quote reserves into f64, adjusting for the token decimals
    let base_reserve: f64 = base_reserves.amount.parse::<f64>()? / 10f64.powi(base_reserves.decimals as i32);
    let quote_reserve: f64 = quote_reserves.amount.parse::<f64>()? / 10f64.powi(quote_reserves.decimals as i32);

    // Calculate price using the correct reserve amounts
    Ok(quote_reserve / base_reserve)
}
