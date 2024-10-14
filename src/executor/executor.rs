use std::sync::Arc;
use std::time::Duration;
use chrono::{TimeZone, Utc};
use dashmap::DashMap;
use futures::StreamExt;
use solana_client::nonblocking::rpc_client::RpcClient;
use log::{error, info, warn};
use solana_client::rpc_config::RpcSendTransactionConfig;
use solana_program::pubkey::Pubkey;
use solana_sdk::commitment_config::{CommitmentConfig, CommitmentLevel};
use solana_sdk::signature::Signature;
use solana_transaction_status::UiTransactionEncoding;
use tokio::sync::mpsc::{Sender};
use tokio_util::sync::CancellationToken;
use tokio_util::time::DelayQueue;
use crate::config::{BloxrouteConfig, ExecutorType};
use crate::executor::order::{Order, OrderDirection, OrderKind, OrderStatus};
use crate::executor::backup::Backup;
use crate::executor::{bloxroute, swap};
use crate::wallet::Wallet;

// Maximum retry attempts for sending transactions to the blockchain
const MAX_TX_RETRIES: usize = 5;

pub struct Executor {
    // Sender for scheduling orders
    delay_queue_tx: Sender<Order>,

    // Executed orders dashmap
    pending_orders: Arc<DashMap<Pubkey, Vec<Order>>>,
}

impl Executor {
    pub async fn new(
        client: Arc<RpcClient>,
        wallet: Arc<Wallet>,
        executed_orders_tx: Sender<Order>,
        bloxroute_config: &BloxrouteConfig,
        cancel_token: CancellationToken,
    ) -> Self {
        let (delay_queue_tx, mut delay_queue_rx) = tokio::sync::mpsc::channel::<Order>(10);

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
        tokio::spawn(async move {
            let mut delay_queue = DelayQueue::new();

            // Restore the pending orders from the backup
            for orders in pending_orders_clone.iter() {
                for order in orders.value() {
                    let delay = get_delay_duration(order.delay, order.delayed_at);
                    delay_queue.insert(order.clone(), delay);
                }
            }

            let rpc_client_clone = client.clone();
            let wallet_clone = wallet.clone();

            loop {
                tokio::select! {
                    // Receive new orders to be added to the delay queue
                    Some(order) = delay_queue_rx.recv() => {
                        let delay = get_delay_duration(order.delay, order.delayed_at);
                        delay_queue.insert(order, delay);
                    }

                    // Process expired items from the delay queue
                    Some(expired) = delay_queue.next() => {
                        let order = expired.into_inner();
                        let bloxroute_config_clone = bloxroute_config_clone.clone();
                        let executed_orders_tx_clone = executed_orders_tx_clone.clone();
                        let pending_orders_clone = pending_orders_clone.clone();
                        let rpc_client_clone = rpc_client_clone.clone();
                        let wallet_clone = wallet_clone.clone();
                        tokio::spawn(async move {
                            match execute_order(
                                order,
                                wallet_clone,
                                rpc_client_clone,
                                executed_orders_tx_clone,
                                pending_orders_clone,
                                bloxroute_config_clone,
                            ).await {
                                Ok(_) => (),
                                Err(e) => warn!("Failed to execute expired order: {}", e),
                            }
                        });
                    }

                    // Handle cancellation
                    _ = cancel_token.cancelled() => {
                        info!("Executor task cancelled.");
                        break;
                    }
                }
            }
        });

        Self {
            pending_orders,
            delay_queue_tx,
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
    executed_orders_tx: Sender<Order>,
    pending_orders: Arc<DashMap<Pubkey, Vec<Order>>>,
    bloxroute_config: BloxrouteConfig,
) -> Result<Order, Box<dyn std::error::Error>> {
    // Check if the executor is Bloxroute and if it is enabled
    let bloxroute_enabled = bloxroute_config.enabled && order.executor == ExecutorType::Bloxroute;

    let start_time = Utc::now().time();

    // Build the unsigned transaction for the order
    let mut tx = {
        // If enabled and create_swap_tx is true, create the swap tx via Bloxroute API
        if bloxroute_enabled && bloxroute_config.create_swap_tx {
            match bloxroute::create_swap_tx(&vec![]).await {
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
            OrderDirection::BaseIn => {
                match swap::build_swap_in_tx(&wallet.keypair, &order, bloxroute_enabled).await {
                    Ok(tx) => tx,
                    Err(e) => {
                        error!("Failed to build swap in transaction for order: {}", e);
                        return Err(e);
                    }
                }
            }
            OrderDirection::BaseOut => {
                match swap::build_swap_out_tx(&wallet.keypair, &order, bloxroute_enabled).await {
                    Ok(tx) => tx,
                    Err(e) => {
                        error!("Failed to build swap out transaction for order: {}", e);
                        return Err(e);
                    }
                }
            }
        }
    };

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

    // Sign the transaction with the wallet keypair
    if let Err(e) = tx.try_sign(&[&wallet.keypair], recent_blockhash) {
        error!("Transaction sign failed with error {e:?}");
        return Err(Box::new(e));
    }

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

        // Simulate tx confirmation and receipt time (~800ms / 2 blocks)
        tokio::time::sleep(Duration::from_millis(800)).await;
        tx.signatures[0]
    } else {
        // Send tx via RPC client and wait for confirmation
        // let signature = match rpc_client.send_transaction_with_config(
        //     &tx,
        //     RpcSendTransactionConfig {
        //         encoding: Some(UiTransactionEncoding::Base64), // Use default encoding
        //         skip_preflight: false, // Skip preflight checks for faster execution
        //         max_retries: Some(MAX_TX_RETRIES), // Maximum retry attempts
        //         preflight_commitment: Some(CommitmentLevel::Confirmed), // Use confirmed commitment
        //         min_context_slot: None,
        //     },
        // ).await {
        //     Ok(signature) => signature,
        //     Err(e) => {
        //         error!("Failed to send transaction: {}", e);
        //         return Err(Box::new(e));
        //     }
        // };

        // Wait for the transaction to be confirmed
        // match rpc_client.confirm_transaction_with_spinner(
        //     &signature,
        //     &recent_blockhash,
        //     CommitmentConfig::confirmed(),
        // ).await {
        //     Ok(_) => signature,
        //     Err(e) => {
        //         error!("Failed to confirm transaction: {}", e);
        //         return Err(Box::new(e));
        //     }
        // }

        // // Simulate tx confirmation and receipt time (~2000ms / 5 blocks)
        tokio::time::sleep(Duration::from_millis(2000)).await;
        tx.signatures[0]
    };

    // Update order status
    order.tx_id = Some(signature.to_string());
    order.status = OrderStatus::Completed;

    // Use current timestamp for simplicity
    order.confirmed_at = current_timestamp();

    info!(
        "\n✅ Order Executed!\n\
         ─────────────────────────────────────────────────────\n\
         ▶️ Trade ID:        {}\n\
         ▶️ Pool:            {}\n\
         ▶️ Tx:              {}\n\
         ▶️ Status:          {}\n\
         ▶️ Side:            {:?}\n\
         ▶️ Amount In:       {:.10}\n\
         ▶️ Confirmed At:    {}\n\
         ▶️ Processed In:    {}ms\n",
        order.trade_id,
        order.pool_keys.id.to_string(),
        signature.to_string(),
        order.status,
        order.direction,
        order.amount,
        Utc.timestamp_opt(order.confirmed_at as i64, 0).unwrap().to_rfc3339(),
        (Utc::now().time() - start_time).num_milliseconds()
    );

    // Emit the executed order to the executed_orders_tx channel
    match executed_orders_tx.send(order.clone()).await {
        Ok(_) => (),
        Err(e) => {
            info!("Failed to send executed order: {}", e);
            return Err(Box::new(e));
        }
    }

    // Remove the order from the pending orders hashmap
    if let Some(mut orders_ref) = pending_orders.get_mut(&order.pool_keys.id) {
        // Retain all orders except the one that was executed
        orders_ref.retain(|o| o.trade_id != order.trade_id);

        // After modifying the entry, check if the entry is empty
        let pool_is_empty = orders_ref.is_empty();
        drop(orders_ref); // Drop the mutable reference before removing the entry

        if pool_is_empty {
            pending_orders.remove(&order.pool_keys.id);
        }
    }

    Ok(order)
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
