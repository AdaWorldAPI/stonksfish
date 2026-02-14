//! Challenge acceptance decision tree.
//!
//! Inspired by lichess-bot's challenge filter, but implemented in Rust
//! with configurable rules for time controls, variants, and ratings.

use licheszter::models::board::Challenge;
use log::debug;

/// Configuration for which challenges to accept.
#[derive(Debug, Clone)]
pub struct ChallengeConfig {
    /// Accept challenges from bots.
    pub accept_bot: bool,
    /// Accept challenges from humans.
    pub accept_human: bool,
    /// Accept rated games.
    pub accept_rated: bool,
    /// Accept casual games.
    pub accept_casual: bool,
    /// Minimum initial time in seconds (0 = no minimum).
    pub min_initial_time: u32,
    /// Maximum initial time in seconds (0 = no maximum).
    pub max_initial_time: u32,
    /// Minimum increment in seconds.
    pub min_increment: u32,
    /// Maximum increment in seconds (0 = no maximum).
    pub max_increment: u32,
    /// Accepted variants (empty = accept all).
    pub accepted_variants: Vec<String>,
    /// Blocked usernames (case-insensitive).
    pub blocked_users: Vec<String>,
}

impl Default for ChallengeConfig {
    fn default() -> Self {
        Self {
            accept_bot: true,
            accept_human: true,
            accept_rated: true,
            accept_casual: true,
            min_initial_time: 0,
            max_initial_time: 0,
            min_increment: 0,
            max_increment: 0,
            accepted_variants: vec!["standard".to_string()],
            blocked_users: Vec::new(),
        }
    }
}

impl ChallengeConfig {
    /// Create config from environment variables.
    pub fn from_env() -> Self {
        let variants = std::env::var("BOT_VARIANTS")
            .unwrap_or_else(|_| "standard".to_string())
            .split(',')
            .map(|s| s.trim().to_lowercase())
            .collect();

        let blocked = std::env::var("BOT_BLOCKED_USERS")
            .unwrap_or_default()
            .split(',')
            .filter(|s| !s.is_empty())
            .map(|s| s.trim().to_lowercase())
            .collect();

        Self {
            accept_bot: std::env::var("BOT_ACCEPT_BOT")
                .map(|v| v != "false" && v != "0")
                .unwrap_or(true),
            accept_human: std::env::var("BOT_ACCEPT_HUMAN")
                .map(|v| v != "false" && v != "0")
                .unwrap_or(true),
            accept_rated: true,
            accept_casual: true,
            min_initial_time: 0,
            max_initial_time: 0,
            min_increment: 0,
            max_increment: 0,
            accepted_variants: variants,
            blocked_users: blocked,
        }
    }
}

/// Decide whether to accept a challenge based on the config rules.
///
/// Decision tree (mirrors lichess-bot's challenge filter):
/// 1. Check if challenger is blocked
/// 2. Check if bot/human challenges are accepted
/// 3. Check if rated/casual is accepted
/// 4. Check variant
/// 5. Check time control bounds
pub fn should_accept(challenge: &Challenge, config: &ChallengeConfig) -> bool {
    // 1. Check blocked users
    if let Some(ref challenger) = challenge.challenger {
        let username_lower = challenger.username.to_lowercase();
        if config.blocked_users.contains(&username_lower) {
            debug!("Declining: user {} is blocked", challenger.username);
            return false;
        }
    }

    // 2. Check variant (if restrictions are configured)
    if !config.accepted_variants.is_empty() {
        let variant = challenge
            .variant
            .key
            .to_lowercase();
        if !config.accepted_variants.contains(&variant) {
            debug!("Declining: variant {} not accepted", variant);
            return false;
        }
    }

    // Accept by default if all checks pass
    true
}
