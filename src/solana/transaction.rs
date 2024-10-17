use std::str::FromStr;
use log::error;
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_program::hash::Hash;
use solana_program::instruction::Instruction;
use solana_program::message::{v0, VersionedMessage};
use solana_sdk::commitment_config::CommitmentConfig;
use solana_sdk::signature::{Keypair, Signature, Signer};
use solana_sdk::transaction::VersionedTransaction;
use solana_transaction_status::{UiTransactionEncoding, UiTransactionStatusMeta};
use crate::error::handle_attempt;

pub fn create_tx(
    payer: &Keypair,
    instructions: Vec<Instruction>,
    recent_blockhash: &Hash,
) -> Result<VersionedTransaction, Box<dyn std::error::Error + Send + Sync>> {
    let message = match v0::Message::try_compile(
        &payer.pubkey(),
        &instructions,
        &[],
        *recent_blockhash,
    ) {
        Ok(message) => message,
        Err(err) => {
            error!("Error compiling v0 message: {:?}", err);
            return Err(err.into());
        }
    };

    // Create tx with payer as the signer
    let versioned_message = VersionedMessage::V0(message);
    match VersionedTransaction::try_new(versioned_message, &[payer]) {
        Ok(tx) => Ok(tx),
        Err(err) => {
            error!("Error creating versioned transaction: {:?}", err);
            Err(err.into())
        }
    }
}

pub async fn get_transaction_metadata(
    rpc_client: &RpcClient,
    tx_signature: &str,
) -> Result<UiTransactionStatusMeta, Box<dyn std::error::Error + Send + Sync>> {
    let mut attempts = 0;
    const MAX_RETRIES: u32 = 3;

    loop {
        let signature = Signature::from_str(tx_signature)?;
        let confirmed_tx = rpc_client.get_transaction_with_config(
            &signature,
            solana_client::rpc_config::RpcTransactionConfig {
                encoding: Some(UiTransactionEncoding::JsonParsed),
                commitment: Some(CommitmentConfig::confirmed()),
                max_supported_transaction_version: Some(0),
            },
        ).await;

        match confirmed_tx {
            Ok(transaction_details) => {
                match transaction_details.transaction.meta {
                    Some(meta) => {
                        // Handle transaction error if present
                        if let Some(error) = meta.err {
                            return match error {
                                solana_sdk::transaction::TransactionError::InstructionError(_, _) => {
                                    error!("Transaction failed with instruction error: {:?}", error);
                                    Err(Box::new(error))
                                }
                                solana_sdk::transaction::TransactionError::InsufficientFundsForFee => {
                                    error!("Insufficient funds for fee: {:?}", error);
                                    Err(Box::new(error))
                                }
                                _ => {
                                    error!("Transaction failed with error: {:?}", error);
                                    Err(Box::new(error))
                                }
                            };
                        }

                        // Return transaction metadata
                        return Ok(meta);
                    }
                    None => {
                        // Increment retry attempt counter and handle retry logic
                        match handle_attempt(&mut attempts, MAX_RETRIES, 300).await {
                            Ok(_) => continue,
                            Err(_) => return Err("Transaction details not found".into()),
                        }
                    }
                }
            }
            Err(err) => {
                // Increment retry attempt counter and handle retry logic
                match handle_attempt(&mut attempts, MAX_RETRIES, 300).await {
                    Ok(_) => continue,
                    Err(_) => return Err(err.into()),
                }
            }
        }
    }
}