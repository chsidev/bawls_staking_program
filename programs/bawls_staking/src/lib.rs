// Finalized BAWLS Staking Program using Anchor 0.31.1
use anchor_lang::prelude::*;
use anchor_spl::token::{self, Token, Transfer, Mint, TokenAccount};
use anchor_spl::associated_token::{AssociatedToken, get_associated_token_address, Create, create};

declare_id!("6WyJk3f8hLswT2FF52Ko8afuLYFpBWb83YDdgaGbTX4W");

pub const CONFIG_SEED: &[u8] = b"config";
pub const POOL_SEED: &[u8] = b"pool";
pub const STATE_SEED: &[u8] = b"state";

#[program]
pub mod bawls_staking {
    use super::*;

    pub fn initialize(ctx: Context<Initialize>, community_wallet: Pubkey) -> Result<()> {
        require_keys_eq!(ctx.accounts.authority.key(), ctx.accounts.payer.key(), StakingError::Unauthorized);
        ctx.accounts.config.version = 1;
        ctx.accounts.config.authority = ctx.accounts.authority.key();
        ctx.accounts.config.paused = false;
        ctx.accounts.config.community_wallet = community_wallet;
        ctx.accounts.config.token_mint = ctx.accounts.token_mint.key();
        ctx.accounts.config.tax_percentage = 5;
        ctx.accounts.config.min_stake_duration = 90 * 86400;
        ctx.accounts.config.bump = ctx.bumps.config;

        ctx.accounts.pool.total_tax_collected = 0;
        ctx.accounts.pool.total_staked = 0;
        ctx.accounts.pool.total_rewards_distributed = 0;
        ctx.accounts.pool.bump = ctx.bumps.pool;

        emit!(InitializedEvent {
            authority: ctx.accounts.authority.key(),
            mint: ctx.accounts.token_mint.key(),
            community_wallet,
            timestamp: Clock::get()?.unix_timestamp,
        });
        Ok(())
    }

    pub fn create_vault(ctx: Context<CreateVault>) -> Result<()> {
        // require_eq!(ctx.accounts.config.version, 1, StakingError::VersionMismatch);
        assert_eq!(ctx.accounts.config.version, 1, "Version mismatch");

        let expected_vault = get_associated_token_address(&ctx.accounts.config.key(), &ctx.accounts.token_mint.key());
        require_keys_eq!(ctx.accounts.vault.key(), expected_vault, StakingError::VaultOwnershipMismatch);

        let signer_seeds: &[&[u8]] = &[CONFIG_SEED, &[ctx.accounts.config.bump]];
        let signer = &[signer_seeds];

        let cpi_accounts = Create {
            payer: ctx.accounts.user.to_account_info(),
            associated_token: ctx.accounts.vault.to_account_info(),
            authority: ctx.accounts.config.to_account_info(),
            mint: ctx.accounts.token_mint.to_account_info(),
            system_program: ctx.accounts.system_program.to_account_info(),
            token_program: ctx.accounts.token_program.to_account_info(),
        };

        let cpi_ctx = CpiContext::new_with_signer(
            ctx.accounts.associated_token_program.to_account_info(),
            cpi_accounts,
            signer,
        );

        // let _ = create(cpi_ctx); 
        if ctx.accounts.vault.to_account_info().owner != &anchor_spl::token::ID {
            create(cpi_ctx)?;
        }
        Ok(())
    }

    pub fn initialize_user_state(ctx: Context<InitializeUserState>) -> Result<()> {
        ctx.accounts.user_state.locked = false;
        ctx.accounts.user_state.amount = 0;
        ctx.accounts.user_state.start_time = 0;
        ctx.accounts.user_state.authority = ctx.accounts.user.key();
        ctx.accounts.user_state.last_tax_snapshot = 0;
        Ok(())
    }

    pub fn stake(ctx: Context<Stake>, amount: u64) -> Result<()> {
        // require_eq!(ctx.accounts.config.version, 1, StakingError::VersionMismatch);
        assert_eq!(ctx.accounts.config.version, 1, "Version mismatch");

        require!(!ctx.accounts.config.paused, StakingError::ContractPaused);
        require!(amount > 0, StakingError::InvalidStakeAmount);

        let cpi_ctx = ctx.accounts.transfer_to_vault_context();
        token::transfer(cpi_ctx, amount)?;

        let clock = Clock::get()?;
        ctx.accounts.user_state.amount += amount;
        ctx.accounts.user_state.start_time = clock.unix_timestamp;
        ctx.accounts.user_state.last_tax_snapshot = ctx.accounts.pool.total_tax_collected;
        ctx.accounts.pool.total_staked += amount;

        emit!(StakeEvent {
            user: ctx.accounts.user.key(),
            amount,
            time: clock.unix_timestamp,
        });

        Ok(())
    }

    pub fn unstake(ctx: Context<Unstake>) -> Result<()> {
        // require_eq!(ctx.accounts.config.version, 1, StakingError::VersionMismatch);
        assert_eq!(ctx.accounts.config.version, 1, "Version mismatch");

        require!(!ctx.accounts.config.paused, StakingError::ContractPaused);
        require!(!ctx.accounts.user_state.locked, StakingError::AlreadyProcessing);
        ctx.accounts.user_state.locked = true;

        let result = (|| {
            let now = Clock::get()?.unix_timestamp;
            let stake_duration = now - ctx.accounts.user_state.start_time;
            let amount = ctx.accounts.user_state.amount;

            require!(amount > 0, StakingError::NothingToUnstake);

            let tax = if stake_duration >= ctx.accounts.config.min_stake_duration {
                0
            } else {
                (amount as u128 * ctx.accounts.config.tax_percentage as u128 / 100) as u64
            };

            let tax_to_pool = tax * 3 / 5;
            let tax_to_community = tax - tax_to_pool;
            let user_amount = amount - tax;

            require!(ctx.accounts.vault.amount >= amount, StakingError::VaultInsufficientBalance);

            ctx.accounts.pool.total_tax_collected += tax_to_pool;
            ctx.accounts.pool.total_staked -= amount;

            ctx.accounts.user_state.amount = 0;
            ctx.accounts.user_state.start_time = 0;
            ctx.accounts.user_state.last_tax_snapshot = 0;

            let signer_seeds: &[&[u8]] = &[CONFIG_SEED, &[ctx.accounts.config.bump]];
            let signer = &[signer_seeds];

            token::transfer(
                CpiContext::new_with_signer(
                    ctx.accounts.token_program.to_account_info(),
                    Transfer {
                        from: ctx.accounts.vault.to_account_info(),
                        to: ctx.accounts.to.to_account_info(),
                        authority: ctx.accounts.config.to_account_info(),
                    },
                    signer,
                ),
                user_amount,
            )?;

            token::transfer(
                CpiContext::new_with_signer(
                    ctx.accounts.token_program.to_account_info(),
                    Transfer {
                        from: ctx.accounts.vault.to_account_info(),
                        to: ctx.accounts.community_ata.to_account_info(),
                        authority: ctx.accounts.config.to_account_info(),
                    },
                    signer,
                ),
                tax_to_community,
            )?;
            emit!(UnstakeEvent {
                user: ctx.accounts.user.key(),
                unstaked_amount: user_amount,
                tax,
                timestamp: now,
            });

            Ok(())
        })();

        ctx.accounts.user_state.locked = false;
        result
    }

    pub fn claim_rewards(ctx: Context<ClaimRewards>) -> Result<()> {
        // require_eq!(ctx.accounts.config.version, 1, StakingError::VersionMismatch);
        assert_eq!(ctx.accounts.config.version, 1, "Version mismatch");

        require!(!ctx.accounts.config.paused, StakingError::ContractPaused);
        require!(!ctx.accounts.user_state.locked, StakingError::AlreadyProcessing);
        ctx.accounts.user_state.locked = true;

        let result = (|| {
            let user_stake = ctx.accounts.user_state.amount;
            let total_staked = ctx.accounts.pool.total_staked;
            require!(user_stake > 0 && total_staked > 0, StakingError::NothingToClaim);

            let new_rewards = ctx.accounts.pool.total_tax_collected - ctx.accounts.user_state.last_tax_snapshot;
            require!(new_rewards > 0, StakingError::InsufficientFundsInPool);

            // let user_reward = (user_stake as u128 * new_rewards as u128 / total_staked as u128) as u64;
            let user_reward_u128 = (user_stake as u128)
                .checked_mul(new_rewards as u128)
                .and_then(|v| v.checked_div(total_staked as u128))
                .ok_or(StakingError::MathOverflow)?;

            let user_reward = u64::try_from(user_reward_u128)
                .map_err(|_| StakingError::MathOverflow)?;
            require!(user_reward > 0, StakingError::InsufficientFundsInPool);

            let available = ctx.accounts.vault.amount;
            let claimable = user_reward.min(available);

            ctx.accounts.pool.total_rewards_distributed += claimable;
            ctx.accounts.user_state.last_tax_snapshot = ctx.accounts.pool.total_tax_collected;

            let signer_seeds: &[&[u8]] = &[CONFIG_SEED, &[ctx.accounts.config.bump]];
            let signer = &[signer_seeds];

            token::transfer(
                CpiContext::new_with_signer(
                    ctx.accounts.token_program.to_account_info(),
                    Transfer {
                        from: ctx.accounts.vault.to_account_info(),
                        to: ctx.accounts.to.to_account_info(),
                        authority: ctx.accounts.config.to_account_info(),
                    },
                    signer,
                ),
                claimable,
            )?;
            emit!(ClaimRewardsEvent {
                user: ctx.accounts.user.key(),
                reward: claimable,
                timestamp: Clock::get()?.unix_timestamp,
            });

            Ok(())
        })();
    ctx.accounts.user_state.locked = false;
    result        
    }

    pub fn set_paused(ctx: Context<SetPaused>, paused: bool) -> Result<()> {
        // require_eq!(ctx.accounts.config.version, 1, StakingError::VersionMismatch);
        assert_eq!(ctx.accounts.config.version, 1, "Version mismatch");

        require_keys_eq!(ctx.accounts.authority.key(), ctx.accounts.config.authority, StakingError::Unauthorized);
        ctx.accounts.config.paused = paused;
        emit!(PausedEvent {
            authority: ctx.accounts.authority.key(),
            paused,
            timestamp: Clock::get()?.unix_timestamp,
        });
        Ok(())
    }
}

#[derive(Accounts)]
pub struct Initialize<'info> {
    #[account(init, seeds = [CONFIG_SEED], bump, payer = payer, space = 8 + 32 + 32 + 1 + 8 + 1 + 1 + 32 + 1)]
    pub config: Account<'info, Config>,
    #[account(init, seeds = [POOL_SEED], bump, payer = payer, space = 8 + 32)]
    pub pool: Account<'info, StakingPool>,
    #[account(mut)]
    pub payer: Signer<'info>,
    #[account(mut)]
    pub token_mint: Account<'info, Mint>,
    pub authority: Signer<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct CreateVault<'info> {
    #[account(mut)]
    pub user: Signer<'info>,
    #[account(mut, seeds = [CONFIG_SEED], bump)]
    pub config: Account<'info, Config>,
    /// CHECK: This is the ATA for the config PDA and token mint. Verified via manual check in instruction.
    #[account(mut)]
    pub vault: UncheckedAccount<'info>,
    #[account(mut)]
    pub token_mint: Account<'info, Mint>,
    pub system_program: Program<'info, System>,
    pub token_program: Program<'info, Token>,
    pub associated_token_program: Program<'info, AssociatedToken>,
}

#[derive(Accounts)]
pub struct InitializeUserState<'info> {
    #[account(init, payer = user, space = 8 + 8 + 8 + 32 + 8 + 1, seeds = [STATE_SEED, user.key().as_ref()], bump)]
    pub user_state: Account<'info, UserState>,
    #[account(mut)]
    pub user: Signer<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct Stake<'info> {
    #[account(mut, has_one = authority)]
    pub user_state: Account<'info, UserState>,
    pub authority: Signer<'info>,
    pub config: Account<'info, Config>,
    #[account(mut)]
    pub pool: Account<'info, StakingPool>,
    #[account(mut)]
    pub user: Signer<'info>,
    #[account(mut, constraint = from.mint == config.token_mint)]
    pub from: Account<'info, TokenAccount>,
    #[account(mut, constraint = vault.mint == config.token_mint)]
    pub vault: Account<'info, TokenAccount>,
    pub token_program: Program<'info, Token>,
}

#[derive(Accounts)]
pub struct Unstake<'info> {
    #[account(mut, has_one = authority)]
    pub user_state: Account<'info, UserState>,
    pub authority: Signer<'info>,
    pub config: Account<'info, Config>,
    #[account(mut)]
    pub pool: Account<'info, StakingPool>,
    #[account(mut)]
    pub user: Signer<'info>,
    #[account(mut, constraint = to.mint == config.token_mint)]
    pub to: Account<'info, TokenAccount>,
    #[account(mut, constraint = vault.mint == config.token_mint)]
    pub vault: Account<'info, TokenAccount>,
    #[account(mut, constraint = community_ata.mint == config.token_mint)]
    pub community_ata: Account<'info, TokenAccount>,
    pub token_program: Program<'info, Token>,
}

#[derive(Accounts)]
pub struct ClaimRewards<'info> {
    #[account(mut, has_one = authority)]
    pub user_state: Account<'info, UserState>,
    pub authority: Signer<'info>,
    pub config: Account<'info, Config>,
    #[account(mut)]
    pub pool: Account<'info, StakingPool>,
    #[account(mut)]
    pub user: Signer<'info>,
    #[account(mut, constraint = to.mint == config.token_mint)]
    pub to: Account<'info, TokenAccount>,
    pub token_program: Program<'info, Token>,
    #[account(mut, constraint = vault.owner == config.key() && vault.mint == config.token_mint)]
    pub vault: Account<'info, TokenAccount>,
}

#[derive(Accounts)]
pub struct SetPaused<'info> {
    #[account(mut, has_one = authority)]
    pub config: Account<'info, Config>,
    pub authority: Signer<'info>,
}

#[derive(Accounts)]
pub struct SetConfig<'info> {
    #[account(mut, has_one = authority)]
    pub config: Account<'info, Config>,
    pub authority: Signer<'info>,
}


#[account]
pub struct Config {
    pub community_wallet: Pubkey,
    pub token_mint: Pubkey,
    pub tax_percentage: u8,
    pub min_stake_duration: i64,
    pub bump: u8,
    pub paused: bool,
    pub authority: Pubkey,
    pub version: u8, 
}

#[account]
pub struct UserState {
    pub amount: u64,
    pub start_time: i64,
    pub authority: Pubkey,
    pub last_tax_snapshot: u64,
    pub locked: bool, 
}

#[account]
pub struct StakingPool {
    pub total_tax_collected: u64,
    pub total_rewards_distributed: u64,
    pub total_staked: u64,
    pub bump: u8,
}

impl<'info> Stake<'info> {
    pub fn transfer_to_vault_context(&self) -> CpiContext<'_, '_, '_, 'info, Transfer<'info>> {
        let cpi_accounts = Transfer {
            from: self.from.to_account_info(),
            to: self.vault.to_account_info(),
            authority: self.user.to_account_info(),
        };
        CpiContext::new(self.token_program.to_account_info(), cpi_accounts)
    }
}

#[event]
pub struct StakeEvent {
    pub user: Pubkey,
    pub amount: u64,
    pub time: i64,
}

#[event]
pub struct UnstakeEvent {
    pub user: Pubkey,
    pub unstaked_amount: u64,
    pub tax: u64,
    pub timestamp: i64,
}

#[event]
pub struct ClaimRewardsEvent {
    pub user: Pubkey,
    pub reward: u64,
    pub timestamp: i64,
}

#[event]
pub struct ConfigUpdatedEvent {
    pub authority: Pubkey,
    pub new_community_wallet: Pubkey,
    pub new_tax_percentage: u8,
    pub new_min_stake_duration: i64,
}

#[event]
pub struct PausedEvent {
    pub authority: Pubkey,
    pub paused: bool,
    pub timestamp: i64,
}

#[event]
pub struct InitializedEvent {
    pub authority: Pubkey,
    pub mint: Pubkey,
    pub community_wallet: Pubkey,
    pub timestamp: i64,
}

#[error_code]
pub enum StakingError {
    #[msg("Nothing to unstake.")]
    NothingToUnstake,
    #[msg("Nothing to claim.")]
    NothingToClaim,
    #[msg("Insufficient funds in pool.")]
    InsufficientFundsInPool,
    #[msg("Invalid stake amount.")]
    InvalidStakeAmount,
    #[msg("Vault has insufficient funds.")]
    VaultInsufficientBalance,
    #[msg("Vault ATA is not owned by config PDA.")]
    VaultOwnershipMismatch,
    #[msg("Action already in progress. Try again.")]
    AlreadyProcessing,
    #[msg("The contract is currently paused.")]
    ContractPaused,
    #[msg("Unauthorized access.")]
    Unauthorized,
    #[msg("Incompatible config version.")]
    VersionMismatch,
    #[msg("Math overflow or conversion failed.")]
    MathOverflow,
}
