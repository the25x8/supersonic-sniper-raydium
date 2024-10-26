use solana_client::nonblocking::pubsub_client::PubsubClient;
use solana_client::rpc_config::{RpcAccountInfoConfig, RpcProgramAccountsConfig};
use solana_client::rpc_filter::{Memcmp, RpcFilterType};
use solana_account_decoder::{UiAccountData, UiAccountEncoding};
use solana_sdk::pubkey::Pubkey;
use solana_sdk::commitment_config::CommitmentConfig;
use tokio::sync::mpsc::{Receiver, Sender};
use log::{debug, error, info};
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use base64::Engine;
use borsh::BorshDeserialize;
use dashmap::DashMap;
use futures::StreamExt;
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_client::rpc_filter::RpcFilterType::DataSize;
use tokio_util::sync::CancellationToken;
use crate::error::handle_attempt;
use crate::{market, raydium};
use crate::detector::PoolKeys;
use crate::solana::amount_utils::token_amount_to_float;

const PRICE_CACHE_TTL: u64 = 2500; // Price cache time-to-live in milliseconds

// Maximum retry attempts for fetching data from the blockchain
const MAX_SUBSCRIPTION_RETRIES: u32 = 5;
const RECONNECT_BACKOFF: u64 = 1000; // Reconnection backoff in milliseconds

#[derive(Debug)]
pub struct MarketData {
    pub pool: Pubkey,
    pub price: f64,
    pub base_reserves: f64,
    pub quote_reserves: f64,
    pub timestamp: std::time::Instant,
}

pub struct MarketMonitor {
    market_tx: Sender<MarketData>,
    rpc_client: Arc<RpcClient>,
    pubsub_client: Arc<PubsubClient>,
    watchlist: Arc<DashMap<Pubkey, PoolKeys>>,
    watchlist_add_tx: Sender<(Pubkey, PoolKeys)>,
    watchlist_remove_tx: Sender<Pubkey>,
    prices: Arc<DashMap<Pubkey, (f64, std::time::Instant)>>,

    // For cancelling the market monitor task when the bot is stopped
    cancel_token: CancellationToken,
}

impl MarketMonitor {
    pub async fn new(
        rpc_client: Arc<RpcClient>,
        rpc_ws_url: &str,
        market_tx: Sender<MarketData>,
        cancel_token: CancellationToken,
    ) -> Self {
        // Initialize the Pubsub client instance
        let pubsub_client = match PubsubClient::new(rpc_ws_url).await {
            Ok(client) => client,
            Err(e) => {
                cancel_token.cancel(); // Gracefully exit the bot
                panic!("Failed to create WebSocket client: {:?}", e);
            }
        };

        let prices = Arc::new(DashMap::new());
        let watchlist = Arc::new(DashMap::new());
        let pubsub_client = Arc::new(pubsub_client);

        let (watchlist_add_tx, watchlist_add_rx) = tokio::sync::mpsc::channel(20);
        let (watchlist_remove_tx, watchlist_remove_rx) = tokio::sync::mpsc::channel(20);
        let pubsub_client_clone = pubsub_client.clone();
        let prices_clone = prices.clone();

        // Run the watchlist add and remove tasks
        let market_data_tx = market_tx.clone();
        let rpc_client_clone = rpc_client.clone();
        let watchlist_clone = watchlist.clone();
        tokio::spawn(async move {
            Self::watchlist_add_loop(
                watchlist_add_rx,
                watchlist_clone,
                market_data_tx,
                rpc_client_clone,
            ).await;
        });

        let watchlist_clone = watchlist.clone();
        tokio::spawn(async move {
            Self::watchlist_remove_loop(watchlist_remove_rx, watchlist_clone).await;
        });

        // Run the main market monitor task
        let market_data_tx = market_tx.clone();
        let rpc_client_clone = rpc_client.clone();
        let watchlist_clone = watchlist.clone();
        let cancel_token_clone = cancel_token.clone();
        tokio::spawn(async move {
            Self::subscribe_to_reserves_pubsub(
                rpc_client_clone,
                pubsub_client_clone,
                market_data_tx,
                watchlist_clone,
                prices_clone,
                cancel_token_clone,
            ).await;
        });

        Self {
            prices,
            watchlist,
            market_tx,
            rpc_client,
            pubsub_client,
            cancel_token,
            watchlist_add_tx,
            watchlist_remove_tx,
        }
    }

    /// Run the watchlist add loop to add pools to the watchlist,
    /// it also fetches the initial price of the pool and emits it to the market_tx channel.
    async fn watchlist_add_loop(
        mut watchlist_add_tx: Receiver<(Pubkey, PoolKeys)>,
        watchlist: Arc<DashMap<Pubkey, PoolKeys>>,
        market_tx: Sender<MarketData>,
        rpc_client: Arc<RpcClient>,
    ) {
        while let Some((pubkey, pool_keys)) = watchlist_add_tx.recv().await {
            // // Add the pool to the watchlist if it doesn't exist
            let exists = watchlist.contains_key(&pubkey);
            if !exists {
                watchlist.insert(pubkey, pool_keys);
                info!("🔍 Pool {} added to watchlist", pubkey);
                // watchlist lock is dropped here
            } else {
                // Pool already exists in the watchlist, skip
                continue;
            }

            // Get liquidity state for the pool
            // let state = match raydium::get_amm_liquidity_state(rpc_client.clone(), &pubkey).await {
            //     Ok(state) => state,
            //     Err(e) => {
            //         error!("Failed to fetch AMM liquidity state: {}", e);
            //         continue;
            //     }
            // };
            // 
            // // Fetch the initial price of the pool and emit to the market_tx channel
            // let (base_reserves, quote_reserves) = match raydium::get_amm_pool_reserves(
            //     rpc_client.clone(),
            //     &state.coin_vault,
            //     &state.pc_vault,
            // ).await {
            //     Ok(reserves) => (reserves[0].clone(), reserves[1].clone()),
            //     Err(e) => {
            //         error!("Failed to fetch initial AMM pool reserves: {}", e);
            //         continue;
            //     }
            // };
            // 
            // // Calculate the initial price of the pool
            // let price = match market::price::convert_reserves_to_price(
            //     state.coin_vault_mint,
            //     state.pc_vault_mint,
            //     &base_reserves,
            //     &quote_reserves,
            // ) {
            //     Ok(price) => price,
            //     Err(e) => {
            //         error!("Failed to calculate initial token price: {}", e);
            //         continue;
            //     }
            // };
            // 
            // // Emit the initial market data to the market_tx channel
            // let base_reserves_amount = token_amount_to_float(&base_reserves.amount, base_reserves.decimals);
            // let quote_reserves_amount = token_amount_to_float(&quote_reserves.amount, quote_reserves.decimals);
            // match market_tx.send(MarketData {
            //     price,
            //     pool: pubkey,
            //     base_reserves: base_reserves_amount,
            //     quote_reserves: quote_reserves_amount,
            //     timestamp: std::time::Instant::now(),
            // }).await {
            //     Ok(_) => (),
            //     Err(e) => {
            //         error!("Failed to send initial market data: {}", e);
            //         continue;
            //     }
            // }
        }
    }

    /// Loop to remove pools from the watchlist when requested.
    async fn watchlist_remove_loop(mut watchlist_remove_rx: Receiver<Pubkey>, watchlist: Arc<DashMap<Pubkey, PoolKeys>>) {
        while let Some(pubkey) = watchlist_remove_rx.recv().await {
            // Remove the pool from the watchlist
            let removed = watchlist.remove(&pubkey);
            if removed.is_some() {
                info!("🔍 Pool {} removed from watchlist", pubkey);
            }
        }
    }

    /// The task subscribes to the AMM program, listens for liquidity updates,
    /// and emits updated market data to the market_tx channel.
    async fn subscribe_to_reserves_pubsub(
        rpc_client: Arc<RpcClient>,
        pubsub: Arc<PubsubClient>,
        market_tx: Sender<MarketData>,
        watchlist: Arc<DashMap<Pubkey, PoolKeys>>,
        prices: Arc<DashMap<Pubkey, (f64, std::time::Instant)>>,
        cancel_token: CancellationToken,
    ) {
        // Clone the Pubsub client instance
        let mut attempts = 0;

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
            let (mut accounts, program_unsubscribe) =
                match pubsub.program_subscribe(
                    &raydium::MainnetProgramId::AmmV4.get_pubkey(),
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

            tokio::select! {
                // Handle the program subscription stream
                _ = async {
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
                } => {}

                // Unsubscribe from the program subscription and exit the task if cancelled
                _ = cancel_token.cancelled() => {
                    info!("Unsubscribing from Raydium pool stream in Market Monitor...");
                    program_unsubscribe().await;
                    break;
                }

                else => break,
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

        // Cancel the token
        info!("Market monitor has been terminated");
        cancel_token.cancel();
    }

    /// Add a pool to the watchlist
    pub async fn add_to_watchlist(&self, pubkey: &Pubkey, pool_keys: &PoolKeys) {
        if let Err(e) = self.watchlist_add_tx.send((*pubkey, pool_keys.clone())).await {
            error!("Failed to add pool to watchlist: {}", e);
        }
    }

    /// Remove a pool from the watchlist
    pub async fn remove_from_watchlist(&self, pubkey: &Pubkey) {
        if let Err(e) = self.watchlist_remove_tx.send(*pubkey).await {
            error!("Failed to remove pool from watchlist: {}", e);
        }
    }

    /// Handle account deserializes the liquidity state for AMM pools
    /// and emits market data to the market_tx channel.
    async fn handle_account_update(
        pubkey: Pubkey,
        data: &UiAccountData,
        rpc_client: Arc<RpcClient>,
        prices: Arc<DashMap<Pubkey, (f64, std::time::Instant)>>,
        watchlist: Arc<DashMap<Pubkey, PoolKeys>>,
        market_tx: Sender<MarketData>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        match data {
            UiAccountData::Binary(data_base64, _encoding) => {
                if let Ok(data) = base64::prelude::BASE64_STANDARD.decode(data_base64) {
                    // Parse the liquidity state data
                    let state = match raydium::LiquidityStateV4::try_from_slice(&data) {
                        Ok(state) => state,
                        Err(e) => {
                            error!("Failed to parse liquidity state v4 data: {}", e);
                            return Err(e.into());
                        }
                    };

                    // Check if the pool is in the watchlist
                    if !watchlist.contains_key(&pubkey) {
                        return Ok(());
                    }

                    // If price is cached, skip fetching
                    if let Some(entry) = prices.get(&pubkey) {
                        // If the price hasn't changed in the last 5 seconds, skip fetching
                        if entry.value().1.elapsed() < Duration::from_millis(PRICE_CACHE_TTL) {
                            return Ok(());
                        }
                    }

                    // Fetch the reserves of the AMM pool if cache is stale or empty
                    let (base_reserves, quote_reserves) =
                        match raydium::get_amm_pool_reserves(
                            rpc_client.clone(),
                            &state.coin_vault,
                            &state.pc_vault,
                        ).await {
                            Ok(reserves) => (reserves[0].clone(), reserves[1].clone()),
                            Err(e) => {
                                error!("Failed to fetch AMM pool reserves: {}", e);
                                return Err(e);
                            }
                        };

                    // Calculate the price of the base token in terms of the quote token
                    let price = match market::price::convert_reserves_to_price(
                        state.coin_vault_mint,
                        state.pc_vault_mint,
                        &base_reserves,
                        &quote_reserves,
                    ) {
                        Ok(price) => price,
                        Err(e) => {
                            error!("Failed to calculate token price: {}", e);
                            return Err(e);
                        }
                    };

                    // Liquidity rug pull check
                    let liquidity_check = match raydium::check_liquidity(
                        &state.lp_mint,
                        state.lp_amount as f64,
                        rpc_client,
                    ).await {
                        Ok(liquidity_check) => liquidity_check,
                        Err(e) => {
                            error!("Failed to check liquidity: {}", e);
                            return Err(e);
                        }
                    };

                    debug!(
                        "Market data updated for pool {}\n\
                        Base reserves: {}\n\
                        Quote reserves: {}\n\
                        Price: {}\n\
                        Liquidity check: {:?}\n",
                        pubkey,
                        base_reserves.ui_amount_string.to_string(),
                        quote_reserves.ui_amount_string.to_string(),
                        price,
                        liquidity_check
                    );

                    // Send market data to the market_tx channel
                    match market_tx.send(MarketData {
                        price, // New price of base token in terms of quote token
                        pool: pubkey,
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