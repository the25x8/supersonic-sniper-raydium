use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use chrono::{TimeZone, Utc};
use dashmap::DashMap;
use futures::StreamExt;
use solana_client::nonblocking::rpc_client::RpcClient;
use log::{error, info};
use solana_client::rpc_config::RpcSendTransactionConfig;
use solana_program::pubkey::Pubkey;
use solana_sdk::commitment_config::{CommitmentConfig, CommitmentLevel};
use solana_sdk::signature::Signature;
use solana_sdk::transaction::{VersionedTransaction};
use solana_transaction_status::option_serializer::OptionSerializer;
use solana_transaction_status::UiTransactionEncoding;
use tokio::sync::mpsc::{Sender};
use tokio_util::sync::CancellationToken;
use tokio_util::time::DelayQueue;
use uuid::Uuid;
use crate::config::{BloxrouteConfig, ExecutorConfig, ExecutorType};
use crate::error::handle_attempt;
use crate::executor::order::{Order, OrderDirection};
use crate::executor::backup::Backup;
use crate::executor::{bloxroute, swap};
use crate::solana;
use crate::solana::extract_token_balance_change;
use crate::wallet::Wallet;

// Maximum retry attempts for sending transactions to the blockchain
const MAX_TX_RETRIES: usize = 5;
const MAX_FETCH_RETRIES: u32 = 3;

pub struct Executor {
    // Sender for scheduling orders
    delay_queue_tx: Sender<Order>,
    queue_cancel_tx: Sender<Uuid>,

    // Executed orders dashmap
    pending_orders: Arc<DashMap<Pubkey, Vec<Order>>>,
}

impl Executor {
    pub async fn new(
        client: Arc<RpcClient>,
        wallet: Arc<Wallet>,
        executed_orders_tx: Sender<Order>,
        failed_orders_tx: Sender<(Order, String)>,
        executor_config: &ExecutorConfig,
        bloxroute_config: &BloxrouteConfig,
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

        // Start the executor task
        let executed_orders_tx_clone = executed_orders_tx.clone();
        let pending_orders_clone = pending_orders.clone();
        let bloxroute_config_clone = bloxroute_config.clone();
        let executor_config_clone = executor_config.clone();
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

            let rpc_client_clone = client.clone();
            let wallet_clone = wallet.clone();

            loop {
                tokio::select! {
                    // Receive new orders to be added to the delay queue
                    Some(order) = delay_queue_rx.recv() => {
                        let id = order.id.clone();
                        let delay = get_delay_duration(order.delay, order.delayed_at);
                        let order_key = delay_queue.insert(order, delay);

                        // Save the order key to the order_keys map
                        order_keys.insert(id, order_key);
                    }

                    // Receive orders to be removed from the delay queue
                    Some(order_id) = queue_cancel_rx.recv() => {
                        // Find the order key in the order_keys map
                        // and remove it from the delay queue.
                        if let Some(order_key) = order_keys.remove(&order_id) {
                            delay_queue.remove(&order_key);
                        }
                    }

                    // Process expired items from the delay queue
                    Some(expired) = delay_queue.next() => {
                        let order = expired.into_inner();
                        let bloxroute_config_clone = bloxroute_config_clone.clone();
                        let executor_config_clone = executor_config_clone.clone();
                        let executed_orders_tx_clone = executed_orders_tx_clone.clone();
                        let failed_orders_tx_clone = failed_orders_tx.clone();
                        let pending_orders_clone = pending_orders_clone.clone();
                        let rpc_client_clone = rpc_client_clone.clone();
                        let wallet_clone = wallet_clone.clone();
                        tokio::spawn(async move {
                            match execute_order(
                                order.clone(),
                                wallet_clone,
                                rpc_client_clone,
                                &executor_config_clone,
                                &bloxroute_config_clone,
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
    executor_config: &ExecutorConfig,
    bloxroute_config: &BloxrouteConfig,
) -> Result<Order, Box<dyn std::error::Error + Send + Sync>> {
    // Check if the executor is Bloxroute and if it is enabled
    let bloxroute_enabled = bloxroute_config.enabled && order.executor == ExecutorType::Bloxroute;
    let start_time = Utc::now().time();

    // Get the latest blockhash
    let recent_blockhash = match rpc_client
        .get_latest_blockhash_with_commitment(CommitmentConfig::confirmed())
        .await {
        Ok((blockhash, _)) => blockhash,
        Err(e) => {
            error!("Failed to get latest blockhash: {}", e);
            return Err(Box::new(e));
        }
    };

    // Build the unsigned transaction for the order
    let tx = {
        // If enabled and create_swap_tx is true, create the swap tx via Bloxroute API
        if bloxroute_enabled && bloxroute_config.create_swap_tx {
            match bloxroute::create_swap_tx(&[]).await {
                Ok(tx) => tx,
                Err(e) => {
                    error!("Failed to create swap tx via Bloxroute: {}", e);
                    return Err(e);
                }
            };
        }

        // Otherwise, build the tx locally. It will include the bribe
        // transfer instruction if the executor is Bloxroute.
        match order.direction {
            OrderDirection::QuoteIn => {
                match swap::create_swap_in_tx(
                    &wallet.keypair,
                    &order,
                    &recent_blockhash,
                    executor_config.compute_unit_limit,
                    executor_config.compute_unit_price,
                    bloxroute_enabled,
                ).await {
                    Ok(tx) => tx,
                    Err(e) => {
                        error!("Failed to build swap in transaction for order: {}", e);
                        return Err(e);
                    }
                }
            }
            OrderDirection::QuoteOut => {
                match swap::create_swap_out_tx(
                    &wallet.keypair,
                    &order,
                    &recent_blockhash,
                    executor_config.compute_unit_limit,
                    executor_config.compute_unit_price,
                    bloxroute_enabled,
                ).await {
                    Ok(tx) => tx,
                    Err(e) => {
                        error!("Failed to build swap out transaction for order: {}", e);
                        return Err(e);
                    }
                }
            }
        }
    };

    // Broadcast the transaction to the blockchain and get the signature
    let signature = if bloxroute_enabled {
        // Send signed tx to blockchain via Bloxroute API and wait for confirmation
        // match bloxroute::send_tx(&tx).await {
        //     Ok(signature) => signature,
        //     Err(e) => {
        //         error!("Failed to send transaction via Bloxroute: {}", e);
        //         return Err(Box::new(e));
        //     }
        // }

        // Simulate tx confirmation and receipt time (~2000ms / 5 blocks)
        tokio::time::sleep(Duration::from_millis(2000)).await;
        tx.signatures[0]
    } else {
        // Send and confirm the transaction via RPC client
        match send_and_confirm(rpc_client.clone(), &tx).await {
            Ok(signature) => signature,
            Err(e) => {
                error!("Failed to send and confirm transaction: {}", e);
                return Err(e);
            }
        }

        // // Simulate tx confirmation and receipt time (~15000ms / 37 blocks)
        // tokio::time::sleep(Duration::from_millis(15000)).await;
        // tx.signatures[0]
    };

    let signature_string = &signature.to_string();

    // Get the transaction metadata by signature
    let tx_meta = match solana::get_transaction_metadata(
        &rpc_client,
        signature_string,
    ).await {
        Ok(meta) => meta,
        Err(e) => {
            error!("Failed to fetch confirmed transaction: {}", e);
            return Err(e);
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
        signature_string,
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

async fn send_and_confirm(
    rpc_client: Arc<RpcClient>,
    tx: &VersionedTransaction,
) -> Result<Signature, Box<dyn std::error::Error + Send + Sync>> {
    // Wait for the transaction to be confirmed
    let mut attempts = 0;

    loop {
        // Get the latest blockhash again to ensure it's up-to-date
        let recent_blockhash = match rpc_client
            .get_latest_blockhash_with_commitment(CommitmentConfig::confirmed())
            .await {
            Ok((blockhash, _)) => blockhash,
            Err(e) => {
                error!("Failed to get latest blockhash: {}", e);

                // Increment retry attempt counter and handle retry logic
                match handle_attempt(&mut attempts, MAX_FETCH_RETRIES, 300).await {
                    Ok(_) => continue,
                    Err(_) => {
                        // TODO Handle failed transaction confirmation gracefully
                        return Err(Box::new(e))
                    }
                }
            }
        };

        // Send tx via RPC client and wait for confirmation
        let signature = match rpc_client.send_transaction_with_config(
            tx,
            RpcSendTransactionConfig {
                encoding: Some(UiTransactionEncoding::Base64),
                skip_preflight: true, // Skip preflight checks for faster execution
                max_retries: Some(MAX_TX_RETRIES),
                preflight_commitment: Some(CommitmentLevel::Confirmed),
                min_context_slot: None,
            },
        ).await {
            Ok(signature) => signature,
            Err(e) => {
                error!("Failed to send transaction: {}", e);

                // Increment retry attempt counter and handle retry logic
                match handle_attempt(&mut attempts, MAX_TX_RETRIES as u32, 300).await {
                    Ok(_) => continue,
                    Err(_) => {
                        // TODO Handle failed transaction confirmation gracefully
                        return Err(Box::new(e))
                    }
                }
            }
        };

        // Wait for the transaction to be confirmed
        match rpc_client.confirm_transaction_with_spinner(
            &signature,
            &recent_blockhash,
            CommitmentConfig::confirmed(),
        ).await {
            Ok(_) => return Ok(signature), // Return the signature if confirmed
            Err(e) => {
                error!("Failed to confirm transaction: {}", e);

                // Increment retry attempt counter and handle retry logic
                match handle_attempt(&mut attempts, MAX_FETCH_RETRIES, 300).await {
                    Ok(_) => continue,
                    Err(_) => {
                        // TODO Handle failed transaction confirmation gracefully
                        return Err(Box::new(e))
                    }
                }
            }
        };
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
