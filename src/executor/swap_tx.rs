use std::str::FromStr;
use std::sync::Arc;
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_program::message::Message;
use solana_program::pubkey::Pubkey;
use solana_program::system_instruction;
use spl_associated_token_account::instruction as ata_instruction;
use solana_sdk::compute_budget::ComputeBudgetInstruction;
use solana_sdk::signature::{Keypair, Signature, Signer};
use solana_sdk::transaction::Transaction;
use crate::executor::order::Order;
use crate::raydium::{build_swap_in_instruction, build_swap_out_instruction};
use crate::solana::build_close_spl_token_account_instruction;

/// Builds a transaction to swap tokens in on the Raydium AMM.
pub async fn build_swap_in_tx(
    rpc_client: Arc<RpcClient>,
    payer: &Keypair,
    order: &Order,
) -> Result<Transaction, Box<dyn std::error::Error>> {
    let payer_pubkey = payer.pubkey();

    // Get the associated token account address for the payer and mint
    let token_account = spl_associated_token_account::get_associated_token_address(
        &payer_pubkey,
        &order.out_mint,
    );

    // Compute Budget Instructions
    let compute_unit_limit_ix = ComputeBudgetInstruction::set_compute_unit_limit(200_000);
    let compute_unit_price_ix = ComputeBudgetInstruction::set_compute_unit_price(10_000);

    // Create Associated Token Account Instruction
    let create_ata_ix = ata_instruction::create_associated_token_account(
        &payer_pubkey,
        &payer_pubkey, // Owner of the token account
        &order.out_mint, // Mint of the token account
        &spl_token::id(),
    );

    // Build swapIn instruction
    let swap_instruction = build_swap_in_instruction(&payer_pubkey, &token_account);

    // Include a bribe transfer instruction
    let bribe_amount = 2_200_000; // 0.0022 SOL in lamports
    let bloxroute_wallet = Pubkey::from_str("HWEoBxYs7ssKuudEjzjmpfJVX7Dvi7wescFsVx2L5yoY")?;
    let bribe_transfer_ix = system_instruction::transfer(
        &payer_pubkey,
        &bloxroute_wallet,
        bribe_amount,
    );

    // Build the transaction
    let instructions = vec![
        compute_unit_limit_ix,
        compute_unit_price_ix,
        create_ata_ix,
        swap_instruction,
        bribe_transfer_ix,
    ];

    // Create a new unsigned transaction
    let message = Message::new(
        &instructions,
        Some(&payer.pubkey()),
    );

    let tx = Transaction::new_unsigned(message);
    Ok(tx)
}

/// Builds a transaction to swap tokens out on the Raydium AMM.
pub async fn build_swap_out_tx(
    rpc_client: Arc<RpcClient>,
    payer: &Keypair,
    order: &Order,
) -> Result<Transaction, Box<dyn std::error::Error>> {
    let payer_pubkey = payer.pubkey();

    // Get the associated token account address for the payer and mint
    let token_account = spl_associated_token_account::get_associated_token_address(
        &payer_pubkey,
        &order.out_mint,
    );

    // Compute Budget Instructions
    let compute_unit_limit_ix = ComputeBudgetInstruction::set_compute_unit_limit(200_000);
    let compute_unit_price_ix = ComputeBudgetInstruction::set_compute_unit_price(10_000); // Adjust as needed

    // Build swapOut instruction
    let swap_out_instruction = build_swap_out_instruction(&payer_pubkey, &token_account);

    // Build close account instruction
    let close_account_ix = match build_close_spl_token_account_instruction(&payer_pubkey, &token_account) {
        Ok(ix) => ix,
        Err(err) => {
            return Err(err)
        }
    };

    // Include a bribe transfer instruction
    let bribe_amount = 2_200_000; // 0.0022 SOL in lamports
    let bloxroute_wallet = Pubkey::from_str("HWEoBxYs7ssKuudEjzjmpfJVX7Dvi7wescFsVx2L5yoY")?;
    let bribe_transfer_ix = system_instruction::transfer(
        &payer_pubkey,
        &bloxroute_wallet,
        bribe_amount,
    );

    // Build the transaction
    let instructions = vec![
        compute_unit_limit_ix,
        compute_unit_price_ix,
        swap_out_instruction,
        close_account_ix,
        bribe_transfer_ix,
    ];

    let recent_blockhash = match rpc_client.get_latest_blockhash().await {
        Ok(hash) => hash,
        Err(err) => {
            return Err(Box::new(err));
        }
    };

    // Sign with payer's keypair
    let transaction = Transaction::new_signed_with_payer(
        &instructions,
        Some(&payer_pubkey),
        &[payer],
        recent_blockhash,
    );

    Ok(transaction)
}
