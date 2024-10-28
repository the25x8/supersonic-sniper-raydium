use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use chrono::{TimeZone, Utc};
use dashmap::DashMap;
use futures::StreamExt;
use jito_sdk_rust::JitoJsonRpcSDK;
use solana_client::nonblocking::rpc_client::RpcClient;
use log::{error, info};
use solana_client::nonblocking::tpu_client::TpuClient;
use solana_client::rpc_client::SerializableTransaction;
use solana_client::rpc_config::RpcSendTransactionConfig;
use solana_client::tpu_client::TpuClientConfig;
use solana_program::hash::Hash;
use solana_program::pubkey::Pubkey;
use solana_quic_client::{QuicConfig, QuicConnectionManager, QuicPool};
use solana_sdk::commitment_config::{CommitmentConfig, CommitmentLevel};
use solana_sdk::signature::Signature;
use solana_sdk::transaction::{VersionedTransaction};
use solana_transaction_status::option_serializer::OptionSerializer;
use solana_transaction_status::UiTransactionEncoding;
use tokio::sync::mpsc::{Sender};
use tokio_util::sync::CancellationToken;
use tokio_util::time::DelayQueue;
use uuid::Uuid;
use crate::config::{AppConfig, ExecutorConfig, ExecutorType, JitoConfig};
use crate::error::handle_attempt;
use crate::executor::order::{Order, OrderDirection};
use crate::executor::backup::Backup;
use crate::executor::{swap};
use crate::solana;
use crate::solana::extract_token_balance_change;
use crate::wallet::Wallet;

// Maximum retry attempts for sending transactions to the blockchain
const MAX_SELL_RETRIES: u32 = 10;
const MAX_FETCH_RETRIES: u32 = 3;

const BLOXROUTE_TIP_ACCOUNT: &str = "HWEoBxYs7ssKuudEjzjmpfJVX7Dvi7wescFsVx2L5yoY";

#[derive(Copy, Clone)]
pub struct TipAccounts {
    jito: Pubkey,
    bloxroute: Pubkey,
}

pub struct Executor {
    // Sender for scheduling orders
    delay_queue_tx: Sender<Order>,
    queue_cancel_tx: Sender<Uuid>,

    // Executed orders dashmap
    pending_orders: Arc<DashMap<Pubkey, Vec<Order>>>,
}

impl Executor {
    pub async fn new(
        rpc_client: Arc<RpcClient>,
        wallet: Arc<Wallet>,
        executed_orders_tx: Sender<Order>,
        failed_orders_tx: Sender<(Order, String)>,
        app_config: &AppConfig,
        cancel_token: CancellationToken,
    ) -> Self {
        let (delay_queue_tx, mut delay_queue_rx) = tokio::sync::mpsc::channel::<Order>(20);
        let (queue_cancel_tx, mut queue_cancel_rx) = tokio::sync::mpsc::channel::<Uuid>(10);

        // Initialize the backup helper and load the pending orders from disk
        let pending_orders: Arc<DashMap<Pubkey, Vec<Order>>> = Arc::new(DashMap::new());
        let backup = Arc::new(Backup::new(
            pending_orders.clone(),
            cancel_token.clone(),
        ));
        backup.load_pending_orders().await;

        // Start auto-saving the pending orders to disk every 5 seconds
        let backup_clone = backup.clone();
        tokio::spawn(async move {
            backup_clone.pending_auto_sync().await;
        });

        // Initialize TPU client
        let tpu_client = match TpuClient::new(
            "supersonic_sniper_bot",
            rpc_client.clone(),
            app_config.rpc.ws_url.as_str(),
            TpuClientConfig {
                fanout_slots: 20
            },
        ).await {
            Ok(tpu) => tpu,
            Err(e) => {
                panic!("Failed to initialize Solana TPU client: {}", e);
            }
        };

        let tpu_client = Arc::new(tpu_client);

        // Initialize Jito sdk and fetch a random tip account
        let jito_sdk = Arc::new(JitoJsonRpcSDK::new(app_config.jito.url.as_str(), None));
        let jito_tip_account = match jito_sdk.get_random_tip_account().await {
            Ok(account) => {
                match Pubkey::from_str(account.as_str()) {
                    Ok(pubkey) => pubkey,
                    Err(e) => {
                        panic!("Failed to parse random Jito tip account: {}", e);
                    }
                }
            }
            Err(e) => {
                panic!("Failed to fetch random Jito tip account: {}", e);
            }
        };

        // Create tip accounts struct
        let tip_accounts = TipAccounts {
            jito: jito_tip_account,
            bloxroute: Pubkey::from_str(BLOXROUTE_TIP_ACCOUNT).unwrap(),
        };

        // Start the executor task
        let executed_orders_tx_clone = executed_orders_tx.clone();
        let pending_orders_clone = pending_orders.clone();
        let app_config_clone = app_config.clone();
        tokio::spawn(async move {
            let mut delay_queue = DelayQueue::new();
            let mut order_keys = HashMap::new();

            // Restore the pending orders from the backup
            for orders in pending_orders_clone.iter() {
                for order in orders.value() {
                    let delay = get_delay_duration(order.delay, order.delayed_at);
                    let order_key = delay_queue.insert(order.clone(), delay);

                    // Save the order key to the order_keys map
                    order_keys.insert(order.id, order_key);
                }
            }

            let rpc_client_clone = rpc_client.clone();
            let wallet_clone = wallet.clone();

            loop {
                tokio::select! {
                    // Receive new orders to be added to the delay queue
                    Some(order) = delay_queue_rx.recv() => {
                        let id = order.id;
                        let delay = get_delay_duration(order.delay, order.delayed_at);

                        // Save the order key to the order_keys map
                        let order_key = delay_queue.insert(order, delay);
                        order_keys.insert(id, order_key);
                    }

                    // Receive orders to be removed from the delay queue
                    Some(order_id) = queue_cancel_rx.recv() => {
                        // Find the order key in the order_keys map
                        // and remove it from the delay queue.
                        if let Some(order_key) = order_keys.remove(&order_id) {
                            delay_queue.try_remove(&order_key);
                        }
                    }

                    // Process expired items from the delay queue
                    Some(expired) = delay_queue.next() => {
                        let order = expired.into_inner();

                        // Check is requested executor is enabled, return error if it's not.
                        if order.executor == ExecutorType::Bloxroute && !app_config_clone.bloxroute.enabled {
                            error!("Bloxroute executor is required but not enabled");
                            return;
                        } else if order.executor == ExecutorType::Jito && !app_config_clone.jito.enabled {
                            error!("Jito executor is required but not enabled");
                            return;
                        }

                        // Clone all the required variables for the executor task
                        let app_config_clone = app_config_clone.clone();
                        let executed_orders_tx_clone = executed_orders_tx_clone.clone();
                        let failed_orders_tx_clone = failed_orders_tx.clone();
                        let pending_orders_clone = pending_orders_clone.clone();
                        let rpc_client_clone = rpc_client_clone.clone();
                        let tpu_client_clone = tpu_client.clone();
                        let jito_client_clone = jito_sdk.clone();
                        let wallet_clone = wallet_clone.clone();
                        tokio::spawn(async move {
                            match execute_order(
                                order.clone(),
                                wallet_clone,
                                rpc_client_clone,
                                tpu_client_clone,
                                jito_client_clone,
                                &app_config_clone.executor,
                                &tip_accounts,
                            ).await {
                                // Emit the executed order to the executed_orders_tx channel
                                Ok(order) => match executed_orders_tx_clone.send(order.clone()).await {
                                    Ok(_) => (),
                                    Err(e) => {
                                        info!("Failed to send executed order: {}", e);
                                    }
                                },

                                // Handle failed orders
                                Err(e) => {
                                    error!("Failed to execute the order: {}", e);

                                    // Emit the failed order to the failed_orders_tx channel
                                    match failed_orders_tx_clone.send((order.clone(), e.to_string())).await {
                                        Ok(_) => (),
                                        Err(e) => {
                                            error!("Failed to notify about failed order: {}", e);
                                        }
                                    }
                                },
                            }

                            // Remove the order from the pending orders hashmap
                            // after it has been executed or failed.
                            if let Some(mut orders_ref) = pending_orders_clone.get_mut(&order.pool_keys.id) {
                                // Retain all orders except the one that was executed
                                orders_ref.retain(|o| o.trade_id != order.trade_id);

                                // After modifying the entry, check if the entry is empty
                                let pool_is_empty = orders_ref.is_empty();
                                drop(orders_ref); // Drop the mutable reference before removing the entry

                                if pool_is_empty {
                                    pending_orders_clone.remove(&order.pool_keys.id);
                                }
                            }
                        });
                    }

                    // Handle cancellation
                    _ = cancel_token.cancelled() => {
                        info!("Executor task terminated");
                        break;
                    }
                }
            }
        });

        Self {
            pending_orders,
            delay_queue_tx,
            queue_cancel_tx,
        }
    }

    /// Add an order to the executor for processing. The order will be scheduled
    /// for execution based on the specified delay. Order will be added to the
    /// pending_orders cache to prevent duplicates.
    pub async fn order(&self, order: Order) -> Result<(), Box<dyn std::error::Error>> {
        // Add the order to the pending_orders cache
        {
            // Acquire a mutable reference to the vector of orders for the pool
            let mut orders = self.pending_orders
                .entry(order.pool_keys.id)
                .or_insert_with(Vec::new);

            // If an order with the same kind already exists for the pool, skip.
            // Sometimes the same order may be triggered multiple times until
            // trade is completed, so we need to check for duplicates.
            if orders.iter().any(|o| o.kind == order.kind) {
                return Ok(());
            }

            // Add the order to the vector
            orders.push(order.clone());
        }

        info!(
            "\n\
            📊 Order {}\n\
            Pool: {}\n\
            Trade: {}\n\
            Side: {:?}\n\
            Amount in: {}\n\
            Created at: {}\n",
            if order.delay > 0 {
                "scheduled with delay"
            } else {
                "will be executed immediately"
            },
            order.pool_keys.id.to_string(),
            order.trade_id,
            order.direction,
            order.amount,
            Utc.timestamp_opt(order.created_at as i64, 0).unwrap().to_rfc3339(),
        );

        // Send the order to the delay queue for execution.
        match self.delay_queue_tx.send(order).await {
            Ok(_) => (),
            Err(e) => {
                info!("Failed to send order to delay queue: {}", e);
                return Err(Box::new(e));
            }
        }

        Ok(())
    }

    /// Cancel an order from the executor by its ID. It will flush the order from
    /// the pending orders cache and remove it from the delay queue.
    /// This method is used to cancel orders that are no longer needed.
    pub async fn cancel_order(&self, order_id: Uuid) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        // Send the order ID to the queue_remove_tx channel
        match self.queue_cancel_tx.send(order_id).await {
            Ok(_) => (),
            Err(e) => {
                info!("Failed to send order ID to cancel queue: {}", e);
                return Err(Box::new(e));
            }
        }

        Ok(())
    }
}

/// Execute an order by building and submitting a transaction to the blockchain.
/// It will create a swap transaction based on the order direction and executor,
/// and submit it to the blockchain. If the executor is Bloxroute, it will use
/// the Bloxroute API to create and send the transaction. Otherwise, it will use
/// the RPC client to send the transaction to the blockchain.
/// The order will be marked as completed and removed from the pending orders cache.
async fn execute_order(
    mut order: Order,
    wallet: Arc<Wallet>,
    rpc_client: Arc<RpcClient>,
    tpu_client: Arc<TpuClient<QuicPool, QuicConnectionManager, QuicConfig>>,
    jito_client: Arc<JitoJsonRpcSDK>,
    executor_config: &ExecutorConfig,
    tip_accounts: &TipAccounts,
) -> Result<Order, Box<dyn std::error::Error + Send + Sync>> {
    // Check if the executor is Bloxroute and if it is enabled
    let start_time = Utc::now().time();

    // We try only once when buy, and retry N times when sell
    let max_attempts = if order.direction == OrderDirection::QuoteIn { 1 } else { MAX_SELL_RETRIES };
    let mut send_tx_attempts = 0;

    // Get tip account based on the executor type
    let tip_account = match order.executor {
        ExecutorType::Bloxroute => Some(&tip_accounts.bloxroute),
        ExecutorType::Jito => Some(&tip_accounts.jito),
        _ => None,
    };

    // Try to build, send and confirm the transaction until it's successful
    let signature_string = loop {
        // Get the latest blockhash
        let recent_blockhash = match rpc_client
            .get_latest_blockhash_with_commitment(CommitmentConfig::confirmed())
            .await {
            Ok((blockhash, _)) => blockhash,
            Err(e) => {
                error!("Failed to get latest blockhash: {}", e);
                continue;
            }
        };

        // Build transaction via bloxroute API if configured
        if executor_config.build_tx_via_bloxroute {
            // match bloxroute::create_swap_tx(&[]).await {
            //     Ok(tx) => tx,
            //     Err(e) => {
            //         error!("Failed to create swap tx via Bloxroute: {}", e);
            //         match handle_attempt(&mut send_tx_attempts, max_attempts, 300).await {
            //             Ok(_) => continue,
            //             Err(_) => return Err("Unable to create swap transaction".into()),
            //         }
            //     }
            // };
        }

        // Otherwise, create tx message manually based on the order direction
        let message = match order.direction {
            OrderDirection::QuoteIn => {
                match swap::create_amm_swap_in(
                    &wallet.keypair,
                    &order,
                    &recent_blockhash,
                    executor_config.compute_unit_limit,
                    executor_config.compute_unit_price,
                    &order.executor,
                    tip_account,
                    order.executor_bribe,
                ).await {
                    Ok(msg) => msg,
                    Err(e) => {
                        error!("Failed to build swap in message: {}", e);
                        continue;
                    }
                }
            }
            OrderDirection::QuoteOut => {
                match swap::create_amm_swap_out(
                    &wallet.keypair,
                    &order,
                    &recent_blockhash,
                    executor_config.compute_unit_limit,
                    executor_config.compute_unit_price,
                    &order.executor,
                    tip_account,
                    order.executor_bribe,
                ).await {
                    Ok(msg) => msg,
                    Err(e) => {
                        error!("Failed to build swap out message: {}", e);
                        continue;
                    }
                }
            }
        };

        // Sign the message and create the transaction
        let transaction = match VersionedTransaction::try_new(
            message,
            &[&wallet.keypair],
        ) {
            Ok(tx) => tx,
            Err(err) => {
                error!("Error signing versioned transaction: {:?}", err);
                continue;
            }
        };

        // If executor is not TPU, send the transaction via RPC or API provider
        if !executor_config.use_tpu {
            // If bloxroute is enabled, send the transaction via bloxroute API
            let signature = if order.executor == ExecutorType::Bloxroute {
                // Send signed tx to blockchain via Bloxroute API and wait for confirmation
                // match bloxroute::send_tx(&tx).await {
                //     Ok(signature) => signature,
                //     Err(e) => {
                //         error!("Failed to send transaction via Bloxroute: {}", e);
                //         return Err(Box::new(e));
                //     }
                // }

                // Mock tx confirmation and receipt time (~2000ms / 5 blocks)
                tokio::time::sleep(Duration::from_millis(2000)).await;
                transaction.signatures[0]
            } else {
                match broadcast_and_confirm_transaction_via_rpc(
                    rpc_client.clone(),
                    &transaction,
                    &recent_blockhash,
                ).await {
                    Ok(signature) => signature,
                    Err(e) => {
                        error!("Failed to send and confirm transaction via rpc: {}", e);
                        match handle_attempt(&mut send_tx_attempts, max_attempts, 100).await {
                            Ok(_) => continue,
                            Err(_) => return Err(e),
                        }
                    }
                }
            };

            // Convert the signature to a string and break the loop
            break signature.to_string();
        }

        // Otherwise, send transaction via TPU client and wait for confirmation
        match broadcast_and_confirm_transaction_via_tpu(
            tpu_client.clone(),
            &transaction,
            &recent_blockhash,
        ).await {
            Ok(signature) => {
                // Convert the signature to a string and break the loop
                break signature.to_string();
            }
            Err(e) => {
                error!("Failed to send and confirm transaction via TPU: {}", e);
                match handle_attempt(&mut send_tx_attempts, max_attempts, 100).await {
                    Ok(_) => continue,
                    Err(_) => return Err(e),
                }
            }
        }
    };

    // Get the transaction metadata by signature
    let tx_meta = match solana::get_transaction_metadata(
        &rpc_client,
        &signature_string,
    ).await {
        Ok(meta) => meta,
        Err(e) => {
            error!("Failed to fetch confirmed transaction: {}", e);
            return Err("Transaction details not found".into());
        }
    };

    // Get compute units consumed
    let compute_units_consumed = match tx_meta.compute_units_consumed {
        OptionSerializer::Some(units) => units,
        _ => 0,
    };

    // Compute out token address and extract the real amount out from the tx metadata
    let received_amount = match extract_token_balance_change(
        &tx_meta,
        &wallet.pubkey,
        &order.out_mint,
    ) {
        Some((_, _, amount_change)) => amount_change,
        None => {
            error!("Destination token balance change not found in transaction metadata");
            return Err("Destination token balance change not found".into());
        }
    };

    // Confirm the order with the transaction signature
    order.confirm(
        &signature_string,
        received_amount,
        tx_meta.fee,
        compute_units_consumed,
        current_timestamp(),
    );

    info!(
        "\n✅ Order Executed!\n\
        ─────────────────────────────────────────────────────\n\
        ▶️ Trade ID:        {}\n\
        ▶️ Pool:            {}\n\
        ▶️ Tx:              {}\n\
        ▶️ Status:          {}\n\
        ▶️ Side:            {:?}\n\
        ▶️ Amount In:       {:.9}\n\
        ▶️ Amount Out:      {:.9}\n\
        ▶️ Fee:             {:.9}\n\
        ▶️ Compute Units:   {}\n\
        ▶️ Confirmed At:    {}\n\
        ▶️ Processed In:    {}ms\n\
        ▶️ Explorer:        https://solscan.io/tx/{}\n",
        order.trade_id,
        order.pool_keys.id.to_string(),
        signature_string,
        order.status,
        order.direction,
        order.amount,
        received_amount,
        order.fee,
        compute_units_consumed,
        Utc.timestamp_opt(order.confirmed_at as i64, 0).unwrap().to_rfc3339(),
        (Utc::now().time() - start_time).num_milliseconds(),
        signature_string,
    );

    Ok(order)
}

/// This function will broadcast the transaction to the blockchain and wait for
/// confirmation. It will use the RPC client.
async fn broadcast_and_confirm_transaction_via_rpc(
    rpc_client: Arc<RpcClient>,
    transaction: &VersionedTransaction,
    recent_blockhash: &Hash,
) -> Result<Signature, Box<dyn std::error::Error + Send + Sync>> {
    // Send the transaction to the blockchain
    let signature = match rpc_client.send_transaction_with_config(
        transaction,
        RpcSendTransactionConfig {
            encoding: Some(UiTransactionEncoding::Base64),
            skip_preflight: true, // Skip preflight checks for faster execution
            max_retries: Some(0), // Do not retry on failure here
            preflight_commitment: Some(CommitmentLevel::Confirmed),
            min_context_slot: None,
        },
    ).await {
        Ok(signature) => signature,
        Err(e) => {
            error!("Failed to send transaction: {}", e);
            return Err(Box::new(e));
        }
    };

    // Wait for the transaction to be confirmed.
    match rpc_client.confirm_transaction_with_spinner(
        &signature,
        recent_blockhash,
        CommitmentConfig::confirmed(),
    ).await {
        Ok(_) => Ok(signature),
        Err(e) => {
            error!("Failed to confirm transaction sent via RPC: {}", e);
            Err(Box::new(e))
        }
    }
}

/// This function will broadcast the transaction to the blockchain and wait for
/// confirmation. It will use the TPU client.
pub async fn broadcast_and_confirm_transaction_via_tpu(
    tpu_client: Arc<TpuClient<QuicPool, QuicConnectionManager, QuicConfig>>,
    transaction: &VersionedTransaction,
    recent_blockhash: &Hash,
) -> Result<Signature, Box<dyn std::error::Error + Send + Sync>> {
    // Serialize the versioned transaction to bytes
    let serialized_tx = match bincode::serialize(&transaction) {
        Ok(tx) => tx,
        Err(e) => {
            error!("Failed to serialize versioned transaction: {}", e);
            return Err(Box::new(e));
        }
    };

    // Send the transaction to the leader via TPU
    let result = tpu_client.send_wire_transaction(serialized_tx).await;
    if !result {
        // If the transaction failed to send via TPU, return an error
        return Err("Failed to send transaction via TPU".into());
    }

    // If transaction was sent successfully, wait for confirmation via RPC
    let signature = transaction.get_signature();
    match tpu_client.rpc_client().confirm_transaction_with_spinner(
        signature,
        recent_blockhash,
        CommitmentConfig::confirmed(),
    ).await {
        Ok(_) => Ok(*signature),
        Err(e) => {
            error!("Failed to confirm transaction sent via TPU: {}", e);
            Err(Box::new(e))
        }
    }
}


fn get_delay_duration(delay: u64, delayed_at: u64) -> Duration {
    let now = Utc::now().timestamp() as u64;
    let delay_ms = if delayed_at > now {
        (delayed_at - now) * 1000
    } else {
        delay * 1000
    };

    Duration::from_millis(delay_ms)
}

fn current_timestamp() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
