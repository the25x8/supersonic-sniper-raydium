use std::str::FromStr;
use solana_program::instruction::Instruction;
use solana_program::pubkey::Pubkey;
use solana_program::system_instruction;
use spl_associated_token_account::{get_associated_token_address, instruction as ata_instruction};
use solana_program::hash::Hash;
use solana_sdk::compute_budget::ComputeBudgetInstruction;
use solana_sdk::signature::{Keypair, Signer};
use solana_sdk::transaction::{VersionedTransaction};
use crate::config::ExecutorType;
use crate::executor::order::Order;
use crate::raydium::{build_swap_in_instruction};
use crate::solana;

const BLOXROUTE_BRIBE_ADDRESS: &str = "HWEoBxYs7ssKuudEjzjmpfJVX7Dvi7wescFsVx2L5yoY";

/// Builds a transaction to swap tokens in on the Raydium AMM.
pub async fn create_swap_in_tx(
    payer: &Keypair,
    order: &Order,
    recent_blockhash: &Hash,
    compute_unit_limit: u32,
    compute_unit_price: u64,
    bloxroute_enabled: bool,
) -> Result<VersionedTransaction, Box<dyn std::error::Error + Send + Sync>> {
    let payer_pubkey = payer.pubkey();

    // User token source is the associated token account for the quote currency
    let user_token_source = get_associated_token_address(
        &payer_pubkey,
        &order.in_mint,
    );

    // Get the associated token account address for the out mint
    let user_token_destination = get_associated_token_address(
        &payer_pubkey,
        &order.out_mint,
    );

    // Create Associated Token Account instruction
    let create_ata_ix = ata_instruction::create_associated_token_account(
        &payer_pubkey,
        &payer_pubkey, // Owner of the token account
        &order.out_mint, // Mint of the token account
        &spl_token::id(),
    );

    // Build swapIn instruction
    let swap_instruction = build_swap_in_instruction(
        &payer_pubkey, // Current wallet
        &user_token_source,
        &user_token_destination,
        order.amount,
        order.limit_amount,
        &order.pool_keys,
        4,
    );

    // Compute Budget Instructions
    let compute_unit_limit_ix = ComputeBudgetInstruction::set_compute_unit_limit(compute_unit_limit);
    let compute_unit_price_ix = ComputeBudgetInstruction::set_compute_unit_price(compute_unit_price);

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
        instructions.push(add_bloxroute_bribe(
            payer,
            order.executor.clone(),
            order.executor_bribe,
        )?);
    }

    // let raw_account = client.get_account(&address_lookup_table_key)?;
    // let address_lookup_table = AddressLookupTable::deserialize(&raw_account.data)?;
    // let address_lookup_table_account = AddressLookupTableAccount {
    //     key: address_lookup_table_key,
    //     addresses: address_lookup_table.addresses.to_vec(),
    // };

    // Create and sign v0 tx
    let tx = match solana::create_tx(payer, instructions, recent_blockhash) {
        Ok(tx) => tx,
        Err(err) => {
            return Err(err)
        }
    };

    Ok(tx)
}

/// Builds a transaction to swap tokens out on the Raydium AMM.
pub async fn create_swap_out_tx(
    payer: &Keypair,
    order: &Order,
    recent_blockhash: &Hash,
    compute_unit_limit: u32,
    compute_unit_price: u64,
    bloxroute_enabled: bool,
) -> Result<VersionedTransaction, Box<dyn std::error::Error + Send + Sync>> {
    let payer_pubkey = payer.pubkey();

    // User token source is the associated token account for the quote currency
    let user_token_source = get_associated_token_address(
        &payer_pubkey,
        &order.in_mint, // Out mint is the base currency
    );

    // Get the associated token account address for the base mint
    let user_token_destination = get_associated_token_address(
        &payer_pubkey,
        &order.out_mint, // In mint is the quote currency
    );

    // Build swapOut instruction
    let swap_instruction = build_swap_in_instruction(
        &payer_pubkey, // Current wallet
        &user_token_source,
        &user_token_destination,
        order.amount,
        order.limit_amount,
        &order.pool_keys,
        4,
    );

    // Build close account instruction
    let close_account_ix = match solana::build_close_spl_token_account_instruction(
        &payer_pubkey,
        &user_token_source,
    ) {
        Ok(ix) => ix,
        Err(err) => {
            return Err(err)
        }
    };

    // Compute Budget Instructions
    let compute_unit_limit_ix = ComputeBudgetInstruction::set_compute_unit_limit(compute_unit_limit);
    let compute_unit_price_ix = ComputeBudgetInstruction::set_compute_unit_price(compute_unit_price);

    // Build the transaction
    let mut instructions = vec![
        compute_unit_limit_ix,
        compute_unit_price_ix,
        swap_instruction,
        close_account_ix,
    ];

    // Include a bribe transfer instruction if order executor is a bloxroute.
    // Only if bloxroute feature is enabled.
    if bloxroute_enabled {
        instructions.push(add_bloxroute_bribe(
            payer,
            order.executor.clone(),
            order.executor_bribe,
        )?);
    }

    // Create and sign v0 tx
    let tx = match solana::create_tx(payer, instructions, recent_blockhash) {
        Ok(tx) => tx,
        Err(err) => {
            return Err(err)
        }
    };

    Ok(tx)
}

/// Adds a bribe transfer instruction to the transaction if the executor is Bloxroute.
fn add_bloxroute_bribe(
    payer: &Keypair,
    executor: ExecutorType,
    bribe_amount: u64,
) -> Result<Instruction, Box<dyn std::error::Error + Send + Sync>> {
    // Only add bribe transfer instruction if executor is Bloxroute
    if executor != ExecutorType::Bloxroute {
        return Err("Executor is not Bloxroute".into());
    }

    // If bribe amount is not provided or is zero, return early
    if bribe_amount == 0 {
        return Err("Bloxroute bribe amount cannot be zero if executor is Bloxroute".into());
    }

    // Include a bribe transfer instruction if order executor is a bloxroute.
    let bloxroute_wallet = Pubkey::from_str(BLOXROUTE_BRIBE_ADDRESS)?;
    let bribe_transfer_ix = system_instruction::transfer(
        &payer.pubkey(),
        &bloxroute_wallet,
        bribe_amount,
    );

    Ok(bribe_transfer_ix) // Return the bribe transfer instruction
}
