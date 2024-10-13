use std::str::FromStr;
use std::sync::Arc;
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_program::instruction::Instruction;
use solana_program::pubkey::Pubkey;
use solana_program::system_instruction;
use spl_associated_token_account::instruction as ata_instruction;
use solana_sdk::compute_budget::ComputeBudgetInstruction;
use solana_sdk::signature::{Keypair, Signer};
use solana_sdk::transaction::Transaction;
use crate::config::ExecutorType;
use crate::executor::order::Order;
use crate::raydium::{build_swap_in_instruction, build_swap_out_instruction};
use crate::solana::build_close_spl_token_account_instruction;

/// Builds a transaction to swap tokens in on the Raydium AMM.
pub async fn build_swap_in_tx(
    payer: &Keypair,
    order: &Order,
    bloxroute_enabled: bool,
) -> Result<Transaction, Box<dyn std::error::Error>> {
    let payer_pubkey = payer.pubkey();

    // Get the associated token account address for the out mint
    let user_token_destination = spl_associated_token_account::get_associated_token_address(
        &payer_pubkey,
        &order.out_mint,
    );

    // Create Associated Token Account Instruction
    let create_ata_ix = ata_instruction::create_associated_token_account(
        &payer_pubkey,
        &payer_pubkey, // Owner of the token account
        &order.out_mint, // Mint of the token account
        &spl_token::id(),
    );

    // User token source is the associated token account for the quote currency
    let user_token_source = spl_associated_token_account::get_associated_token_address(
        &payer_pubkey,
        &order.in_mint,
    );

    // Build swapIn instruction
    let scaled_amount_in = spl_token::ui_amount_to_amount(order.amount, order.in_decimals);
    let scaled_min_amount_out = spl_token::ui_amount_to_amount(order.limit_amount, order.out_decimals);
    let swap_instruction = build_swap_in_instruction(
        &payer_pubkey, // Current wallet
        &user_token_source,
        &user_token_destination,
        scaled_amount_in,
        scaled_min_amount_out,
        &order.pool_keys,
        4,
    );


    // Compute Budget Instructions
    let compute_unit_limit_ix = ComputeBudgetInstruction::set_compute_unit_limit(200_000);
    let compute_unit_price_ix = ComputeBudgetInstruction::set_compute_unit_price(10_000);

    // Build the transaction
    let mut instructions = vec![
        compute_unit_limit_ix,
        compute_unit_price_ix,
        create_ata_ix,
        swap_instruction,
    ];

    // Include a bribe transfer instruction if order executor is a bloxroute.
    // Only if bloxroute feature is enabled.
    if bloxroute_enabled {
        if let Err(err) = add_bloxroute_bribe(
            payer,
            &mut instructions,
            order.executor.clone(),
            order.executor_bribe.clone(),
        ) {
            return Err(err);
        }
    }

    // Create tx with payer as the signer
    let tx = Transaction::new_with_payer(&instructions, Some(&payer.pubkey()));
    Ok(tx)
}

/// Builds a transaction to swap tokens out on the Raydium AMM.
pub async fn build_swap_out_tx(
    payer: &Keypair,
    order: &Order,
    bloxroute_enabled: bool,
) -> Result<Transaction, Box<dyn std::error::Error>> {
    let payer_pubkey = payer.pubkey();

    // Get the associated token account address for the base mint
    let user_token_destination = spl_associated_token_account::get_associated_token_address(
        &payer_pubkey,
        &order.in_mint,
    );

    // User token source is the associated token account for the quote currency
    let user_token_source = spl_associated_token_account::get_associated_token_address(
        &payer_pubkey,
        &order.out_mint,
    );

    // Build swapOut instruction
    let scaled_amount_in = spl_token::ui_amount_to_amount(order.amount, order.in_decimals);
    let scaled_min_amount_out = spl_token::ui_amount_to_amount(order.limit_amount, order.out_decimals);
    let swap_out_instruction = build_swap_out_instruction(
        &payer_pubkey, // Current wallet
        &user_token_source,
        &user_token_destination,
        scaled_amount_in,
        scaled_min_amount_out,
        &order.pool_keys,
        4,
    );

    // Build close account instruction
    let close_account_ix = match build_close_spl_token_account_instruction(
        &payer_pubkey,
        &user_token_source,
    ) {
        Ok(ix) => ix,
        Err(err) => {
            return Err(err)
        }
    };

    // Compute Budget Instructions
    let compute_unit_limit_ix = ComputeBudgetInstruction::set_compute_unit_limit(200_000);
    let compute_unit_price_ix = ComputeBudgetInstruction::set_compute_unit_price(10_000); // Adjust as needed

    // Build the transaction
    let mut instructions = vec![
        compute_unit_limit_ix,
        compute_unit_price_ix,
        swap_out_instruction,
        close_account_ix,
    ];
    
    // Include a bribe transfer instruction if order executor is a bloxroute.
    // Only if bloxroute feature is enabled.
    if bloxroute_enabled {
        if let Err(err) = add_bloxroute_bribe(
            payer,
            &mut instructions,
            order.executor.clone(),
            order.executor_bribe.clone(),
        ) {
            return Err(err);
        }
    }

    // Create tx with payer as the signer
    let tx = Transaction::new_with_payer(&instructions, Some(&payer.pubkey()));
    Ok(tx)
}

fn add_bloxroute_bribe(
    payer: &Keypair,
    instructions: &mut Vec<Instruction>,
    executor: ExecutorType,
    bribe_amount: Option<f64>,
) -> Result<(), Box<dyn std::error::Error>> {
    // Only add bribe transfer instruction if executor is Bloxroute
    if executor != ExecutorType::Bloxroute {
        return Err("Executor is not Bloxroute".into());
    }

    // If bribe amount is not provided or is zero, return early
    let bribe_amount = match bribe_amount {
        Some(amount) => amount,
        None => return Err("Bribe amount not provided".into()),
    };

    // Include a bribe transfer instruction if order executor is a bloxroute.
    let bloxroute_wallet = Pubkey::from_str("HWEoBxYs7ssKuudEjzjmpfJVX7Dvi7wescFsVx2L5yoY")?;
    let scaled_bribe_amount = spl_token::ui_amount_to_amount(bribe_amount, 9); // 9 decimals
    let bribe_transfer_ix = system_instruction::transfer(
        &payer.pubkey(),
        &bloxroute_wallet,
        scaled_bribe_amount,
    );

    instructions.push(bribe_transfer_ix); // Add to the instructions
    Ok(())
}