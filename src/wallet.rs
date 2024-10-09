use solana_sdk::signature::{read_keypair, Keypair};
use solana_sdk::signer::Signer;
use solana_sdk::pubkey::Pubkey;
use std::error::Error;
use std::sync::Arc;
use anyhow::format_err;
use crate::config::AppConfig;

/// Struct to hold the wallet information.
/// Contains the Keypair and associated public key.
#[derive(Debug)]
pub struct Wallet {
    pub keypair: Keypair,
    pub pubkey: Pubkey,
}

impl Wallet {
    /// Creates a new Wallet instance from a Keypair.
    ///
    /// # Arguments
    ///
    /// * `keypair` - A Keypair instance representing the wallet's private and public keys.
    pub fn new(keypair: Keypair) -> Self {
        let pubkey = keypair.pubkey();
        Wallet { keypair, pubkey }
    }
}

/// Loads the wallet based on the provided AppConfig.
/// Tries to load from wallet_secret_key first, then from wallet_file.
///
/// # Arguments
///
/// * `app_config` - A reference to the AppConfig containing wallet configuration.
///
/// # Returns
///
/// * `Result<Wallet, Box<dyn Error>>` - Returns a Wallet instance on success or an error on failure.
pub fn load_wallet(secret_key: Option<String>, file_path: Option<String>) -> Result<Wallet, Box<dyn Error>> {
    // Attempt to load wallet from the secret key provided in the configuration
    if let Some(secret) = secret_key {
        let keypair = keypair_from_secret_key_str(&secret)?;
        return Ok(Wallet::new(keypair));
    }

    // If wallet_secret_key is not provided, attempt to load from wallet_file
    if let Some(path) = file_path {
        let keypair = keypair_from_file(&path)?;
        return Ok(Wallet::new(keypair));
    }

    // If neither wallet_secret_key nor wallet_file is provided, return an error
    Err("No wallet information provided. Please specify either wallet_secret_key or wallet_file.".into())
}

/// Creates a Keypair from a secret key string.
///
/// # Arguments
///
/// * `secret_key_str` - A string slice that holds the secret key.
///
/// # Returns
///
/// * `Result<Keypair, Box<dyn Error>>` - Returns a Keypair on success or an error on failure.
fn keypair_from_secret_key_str(secret_key: &str) -> Result<Keypair, Box<dyn Error>> {
    // Parse the secret key string into a vector of u8
    let secret_key_bytes: Vec<u8> = serde_json::from_str(secret_key)?;

    // Create a Keypair from the secret key bytes
    let keypair = Keypair::from_bytes(&secret_key_bytes)?;

    Ok(keypair)
}

/// Creates a Keypair from a wallet file.
///
/// # Arguments
///
/// * `wallet_file` - A string slice that holds the path to the wallet file.
///
/// # Returns
///
/// * `Result<Keypair, Box<dyn Error>>` - Returns a Keypair on success or an error on failure.
fn keypair_from_file(file_path: &str) -> Result<Keypair, Box<dyn Error>> {
    // Read the wallet keypair from the file
    let wallet = solana_sdk::signature::read_keypair_file(file_path)
        .map_err(|_| format_err!("failed to read keypair from {}", file_path))?;
    Ok(wallet)
}
