use base64::Engine;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{Value};
use solana_sdk::transaction::VersionedTransaction;
use std::error::Error;
use std::str::FromStr;
use log::error;
use solana_program::pubkey::Pubkey;
use solana_sdk::signature::Signature;

pub struct JitoClient {
    base_url: String,
    client: Client,
}

impl JitoClient {
    pub fn new(base_url: &str) -> Self {
        Self {
            base_url: base_url.to_string(),
            client: Client::new(),
        }
    }

    pub async fn send_transaction(
        &self,
        transaction: &VersionedTransaction,
        skip_preflight: bool, // Added parameter
    ) -> Result<Signature, Box<dyn Error + Send + Sync>> {
        // Serialize the versioned transaction to base64
        let serialized_tx = bincode::serialize(transaction)?;
        let base64_tx = base64::prelude::BASE64_STANDARD.encode(&serialized_tx);

        // Create the options object
        let options = SendTransactionOptions {
            encoding: "base64".to_string(),
            skip_preflight: skip_preflight,
        };

        // Create the JSON-RPC request
        let request_body = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: 1,
            method: "sendTransaction".to_string(),
            params: vec![
                serde_json::Value::String(base64_tx),
                serde_json::to_value(options)?,
            ],
        };

        // Send the POST request
        let response = self
            .client
            .post(format!("{}/api/v1/transactions", self.base_url))
            .header("Content-Type", "application/json")
            .json(&request_body)
            .send()
            .await?;

        // Check if the response status is success
        if !response.status().is_success() {
            let status = response.status();
            let error_text = response.text().await?;
            return Err(format!(
                "Failed to send transaction. Status: {}, Error: {}",
                status, error_text
            ).into());
        }

        // Parse the JSON-RPC response
        let response_body: serde_json::Value = response.json().await?;
        if let Some(result) = response_body.get("result") {
            // Transaction signature
            let signature = result.as_str().ok_or("Invalid response format")?.to_string();
            Ok(Signature::from_str(&signature)?)
        } else if let Some(error) = response_body.get("error") {
            let error_message = serde_json::to_string(error)?;
            Err(format!("Error response: {}", error_message).into())
        } else {
            Err("Unknown response format".into())
        }
    }

    pub async fn get_tip_accounts(
        &self,
    ) -> Result<Vec<Pubkey>, Box<dyn Error + Send + Sync>> {
        // Create the JSON-RPC request
        let request_body = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: 1,
            method: "getTipAccounts".to_string(),
            params: vec![],
        };

        // Send the POST request
        let response = self
            .client
            .post(format!("{}/api/v1/bundles", self.base_url))
            .header("Content-Type", "application/json")
            .json(&request_body)
            .send()
            .await?;

        // Check if the response status is success
        if !response.status().is_success() {
            let status = response.status();
            let error_text = response.text().await?;
            return Err(format!(
                "Failed to get tip accounts. Status: {}, Error: {}",
                status, error_text
            )
                .into());
        }

        // Parse the JSON-RPC response
        let response_body: serde_json::Value = response.json().await?;
        if let Some(result) = response_body.get("result") {
            let tip_account_strings = serde_json::from_value::<Vec<String>>(result.clone())?;
            let mut tip_accounts = Vec::new();
            for account_str in tip_account_strings {
                let pubkey = account_str.parse::<Pubkey>()?;
                tip_accounts.push(pubkey);
            }
            Ok(tip_accounts)
        } else if let Some(error) = response_body.get("error") {
            let error_message = serde_json::to_string(error)?;
            Err(format!("Error response: {}", error_message).into())
        } else {
            Err("Unknown response format".into())
        }
    }

    // Get a random tip account
    pub async fn get_random_tip_account(
        &self,
    ) -> Result<Pubkey, Box<dyn Error + Send + Sync>> {
        let tip_accounts = self.get_tip_accounts().await?;
        if tip_accounts.is_empty() {
            return Err("No tip accounts available".into());
        }
        use rand::seq::SliceRandom;
        let mut rng = rand::thread_rng();
        let tip_account = tip_accounts.choose(&mut rng).unwrap().clone();
        Ok(tip_account)
    }
}

// Define the Options Struct
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SendTransactionOptions {
    encoding: String,
    skip_preflight: bool,
}

// Define the JSON-RPC request and response structs
#[derive(Serialize, Deserialize)]
struct JsonRpcRequest {
    jsonrpc: String,
    id: u64,
    method: String,
    params: Vec<serde_json::Value>,
}

#[derive(Serialize, Deserialize)]
struct JsonRpcResponse<T> {
    jsonrpc: String,
    result: T,
    id: u64,
}

#[derive(Serialize, Deserialize)]
struct JsonRpcErrorResponse {
    jsonrpc: String,
    error: JsonRpcError,
    id: u64,
}

#[derive(Serialize, Deserialize, Debug)]
struct JsonRpcError {
    code: i64,
    message: String,
    data: Option<Value>,
}