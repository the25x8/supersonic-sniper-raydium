use solana_program::instruction::{AccountMeta, Instruction};
use solana_program::pubkey::Pubkey;
use crate::raydium::MainnetProgramId;

// Functions for building swap instructions would go here
pub fn build_swap_in_instruction(
    payer_pubkey: &Pubkey,
    token_account: &Pubkey,
) -> Instruction {
    let program_id = MainnetProgramId::AmmV4.get_pubkey();

    // Prepare accounts
    let accounts = vec![
        AccountMeta::new(Pubkey::default(), false),
        AccountMeta::new(Pubkey::default(), false),
        AccountMeta::new_readonly(Pubkey::default(), true), // User Transfer Authority
        // ... (additional accounts required by Raydium's program)
    ];

    // Prepare instruction data
    // This is specific to the Raydium program's instruction format
    let instruction_data = vec![
        9, // Swap instruction ID in Raydium's program
        // ... (additional data)
    ];

    // Build the instruction
    Instruction::new_with_borsh(
        program_id,
        &instruction_data,
        accounts,
    )
}

pub fn build_swap_out_instruction(
    payer_pubkey: &Pubkey,
    token_account: &Pubkey,
    /* other parameters */
) -> Instruction {
    let program_id = MainnetProgramId::AmmV4.get_pubkey();

    // Accounts required for the swap instruction
    let accounts = vec![
        AccountMeta::new_readonly(spl_token::id(), false),
        // Other accounts as per Raydium's specification
    ];

    // Prepare instruction data for swapOut
    let instruction_data = vec![
        10, // SwapOut instruction ID in Raydium's program
        // ... (additional data)
    ];

    // Build the instruction
    Instruction::new_with_borsh(
        program_id,
        &instruction_data,
        accounts,
    )
}