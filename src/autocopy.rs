use crate::{monitor::WatchedEmployee, polymarket::UserTrade};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    env, fmt, fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const WEATHERHK_WALLET: &str = "0x488c725253fc21c7a9ca812030dc2f6343f98c1c";
const DEFAULT_STATE_PATH: &str = "logs/weatherhk_autocopy_state.json";
const DEFAULT_GLOBAL_STATE_PATH: &str = "logs/autocopy_global_state.json";
const STATE_LOG_LIMIT: usize = 500;
const PROCESSED_TRADE_LIMIT: usize = 1_000;
const FAILURE_COOLDOWN_LIMIT: usize = 200;
const SOURCE_FLOW_LIMIT: usize = 500;
const SOURCE_OUTCOME_METADATA_LIMIT: usize = 1_000;
const SOURCE_POSITION_LEDGER_LIMIT: usize = 1_000;
const SOURCE_EVENT_BASKET_LIMIT: usize = 1_000;
const AUTO_COPY_STATE_SCHEMA_VERSION: u8 = 7;
const COPY_FRACTION: f64 = 0.50;
const DEFAULT_COPY_TARGET_CAP_USD: f64 = 50.0;
const SMALL_BUY_FULL_COPY_THRESHOLD_USD: f64 = 15.0;
const SOURCE_BUY_BOOTSTRAP_WINDOW_SECONDS: u64 = 180;
const SOURCE_OUTCOME_METADATA_RETENTION_SECONDS: u64 = 129_600;
const SOURCE_SELL_COVERAGE_TTL_SECONDS: u64 = 300;
const MIN_STARTER_COPY_USD: f64 = 1.0;
const MIN_CLOB_ORDER_SIZE_SHARES: f64 = 5.0;
const TARGET_RECONCILE_MIN_NOTIONAL_USD: f64 = 1.0;
const TARGET_RECONCILE_RELATIVE_TOLERANCE: f64 = 0.10;
const LOW_PRICE_BUY_THRESHOLD: f64 = 0.15;
const MID_PRICE_BUY_THRESHOLD: f64 = 0.30;
const LOW_PRICE_MAX_CHASE_PCT: f64 = 1.00;
const MID_PRICE_MAX_CHASE_PCT: f64 = 0.50;
const LOCK_PROFIT_SOURCE_PRICE: f64 = 0.99;
const LOCK_PROFIT_MIN_SELL_PRICE: f64 = 0.998;
const SMALL_SELL_PASSIVE_FRACTION_THRESHOLD: f64 = 0.20;
const SMALL_SELL_PASSIVE_DISCOUNT_PCT: f64 = 0.15;
const MAX_PENDING_SYNCS_PER_TICK: usize = 2;
const MIN_TRACKED_ACTUAL_BALANCE_SHARES: f64 = 0.01;
const SOURCE_RECONCILE_ACTION_COOLDOWN_SECONDS: u64 = 21_600;
const SOURCE_POSITION_RECONCILE_GRACE_SECONDS: u64 = 60;
const SOURCE_POSITION_ABSENCE_CONFIRM_SECONDS: u64 = 180;
const SOURCE_POSITION_ABSENCE_CONFIRM_MIN_COUNT: u32 = 2;
const SOURCE_POSITION_ABSENCE_RETENTION_SECONDS: u64 = 3_600;
const TRANSIENT_EXIT_RETRY_SECONDS: u64 = 10;
const SELL_ACTION_FAILURE_PREFIX: &str = "action:SELL:";
const SOURCE_POSITION_REPRICE_NOTICE_PREFIX: &str = "notice:source-position-reprice:";
const SOURCE_POSITION_REPRICE_NOTICE_COOLDOWN_SECONDS: u64 = 1_800;
const GLOBAL_AUTO_COPY_STATE_SCHEMA_VERSION: u8 = 1;
const GLOBAL_STATE_LOCK_RETRY_MS: u64 = 10;
const GLOBAL_STATE_LOCK_RETRIES: usize = 25;
const GLOBAL_SOURCE_STALE_SECONDS: u64 = 180;
const GLOBAL_BUY_RESERVATION_TTL_SECONDS: u64 = 120;
const GLOBAL_LOCAL_REFRESH_SECONDS: u64 = 30;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutoCopyStrategyConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub shadow_event_baskets: bool,
    #[serde(default = "default_normal_event_budget_usd")]
    pub normal_event_budget_usd: f64,
    #[serde(default = "default_strong_event_budget_usd")]
    pub strong_event_budget_usd: f64,
    #[serde(default = "default_max_event_budget_usd")]
    pub max_event_budget_usd: f64,
    #[serde(default = "default_strong_event_source_notional_usd")]
    pub strong_event_source_notional_usd: f64,
    #[serde(default = "default_strong_event_min_outcomes")]
    pub strong_event_min_outcomes: usize,
    #[serde(default = "default_min_basket_outcomes_for_rebalance")]
    pub min_basket_outcomes_for_rebalance: usize,
    #[serde(default = "default_hot_path_max_single_buy_usd")]
    pub hot_path_max_single_buy_usd: f64,
    #[serde(default = "default_low_price_leg_threshold")]
    pub low_price_leg_threshold: f64,
    #[serde(default = "default_low_price_min_leg_usd")]
    pub low_price_min_leg_usd: f64,
    #[serde(default = "default_source_reconcile_buy_enabled")]
    pub source_reconcile_buy_enabled: bool,
    #[serde(default = "default_source_reconcile_sell_enabled")]
    pub source_reconcile_sell_enabled: bool,
    #[serde(default = "default_price_buckets")]
    pub price_buckets: Vec<PriceBucketConfig>,
    #[serde(default = "default_notional_multipliers")]
    pub source_notional_multipliers: Vec<NotionalMultiplierConfig>,
}

impl Default for AutoCopyStrategyConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            shadow_event_baskets: false,
            normal_event_budget_usd: default_normal_event_budget_usd(),
            strong_event_budget_usd: default_strong_event_budget_usd(),
            max_event_budget_usd: default_max_event_budget_usd(),
            strong_event_source_notional_usd: default_strong_event_source_notional_usd(),
            strong_event_min_outcomes: default_strong_event_min_outcomes(),
            min_basket_outcomes_for_rebalance: default_min_basket_outcomes_for_rebalance(),
            hot_path_max_single_buy_usd: default_hot_path_max_single_buy_usd(),
            low_price_leg_threshold: default_low_price_leg_threshold(),
            low_price_min_leg_usd: default_low_price_min_leg_usd(),
            source_reconcile_buy_enabled: default_source_reconcile_buy_enabled(),
            source_reconcile_sell_enabled: default_source_reconcile_sell_enabled(),
            price_buckets: default_price_buckets(),
            source_notional_multipliers: default_notional_multipliers(),
        }
    }
}

impl AutoCopyStrategyConfig {
    fn price_bucket(&self, source_price: f64) -> Option<&PriceBucketConfig> {
        self.price_buckets.iter().find(|bucket| {
            source_price >= bucket.min_price
                && (source_price < bucket.max_price
                    || (bucket.max_price >= 1.0 && source_price <= bucket.max_price))
        })
    }

    fn notional_multiplier(&self, source_notional_usd: f64) -> Option<&NotionalMultiplierConfig> {
        self.source_notional_multipliers.iter().find(|multiplier| {
            source_notional_usd >= multiplier.min_notional_usd
                && multiplier
                    .max_notional_usd
                    .map(|max| source_notional_usd < max)
                    .unwrap_or(true)
        })
    }

    fn event_budget(&self, event_buy_notional_usd: f64, outcome_count: usize) -> f64 {
        let base = if event_buy_notional_usd >= self.strong_event_source_notional_usd
            || outcome_count >= self.strong_event_min_outcomes
        {
            self.strong_event_budget_usd
        } else {
            self.normal_event_budget_usd
        };
        base.min(self.max_event_budget_usd).max(0.0)
    }

    fn validate(&self) -> Result<(), String> {
        if self.normal_event_budget_usd <= 0.0 {
            return Err("strategy normal_event_budget_usd must be > 0".to_owned());
        }
        if self.strong_event_budget_usd <= 0.0 {
            return Err("strategy strong_event_budget_usd must be > 0".to_owned());
        }
        if self.max_event_budget_usd <= 0.0 {
            return Err("strategy max_event_budget_usd must be > 0".to_owned());
        }
        if self.hot_path_max_single_buy_usd <= 0.0 {
            return Err("strategy hot_path_max_single_buy_usd must be > 0".to_owned());
        }
        if !(0.0..=1.0).contains(&self.low_price_leg_threshold) {
            return Err("strategy low_price_leg_threshold must be between 0 and 1".to_owned());
        }
        if self.low_price_min_leg_usd < 0.0 {
            return Err("strategy low_price_min_leg_usd must be >= 0".to_owned());
        }
        if self.price_buckets.is_empty() {
            return Err("strategy price_buckets must not be empty".to_owned());
        }
        for bucket in &self.price_buckets {
            bucket.validate()?;
        }
        if self.source_notional_multipliers.is_empty() {
            return Err("strategy source_notional_multipliers must not be empty".to_owned());
        }
        for multiplier in &self.source_notional_multipliers {
            multiplier.validate()?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PriceBucketConfig {
    pub min_price: f64,
    pub max_price: f64,
    pub base_buy_usd: f64,
    pub label: String,
}

impl PriceBucketConfig {
    fn validate(&self) -> Result<(), String> {
        if !(0.0..=1.0).contains(&self.min_price)
            || !(0.0..=1.0).contains(&self.max_price)
            || self.min_price >= self.max_price
        {
            return Err(format!(
                "invalid price bucket {}: min/max must be within 0..1 and min < max",
                self.label
            ));
        }
        if self.base_buy_usd < 0.0 {
            return Err(format!(
                "invalid price bucket {}: base_buy_usd must be >= 0",
                self.label
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotionalMultiplierConfig {
    pub min_notional_usd: f64,
    #[serde(default)]
    pub max_notional_usd: Option<f64>,
    pub multiplier: f64,
    pub label: String,
}

impl NotionalMultiplierConfig {
    fn validate(&self) -> Result<(), String> {
        if self.min_notional_usd < 0.0 {
            return Err(format!(
                "invalid notional multiplier {}: min_notional_usd must be >= 0",
                self.label
            ));
        }
        if self
            .max_notional_usd
            .is_some_and(|max| max <= self.min_notional_usd)
        {
            return Err(format!(
                "invalid notional multiplier {}: max_notional_usd must be > min_notional_usd",
                self.label
            ));
        }
        if self.multiplier < 0.0 {
            return Err(format!(
                "invalid notional multiplier {}: multiplier must be >= 0",
                self.label
            ));
        }
        Ok(())
    }
}

fn load_strategy_config_from_env(path: Option<&Path>) -> AutoCopyStrategyConfig {
    let Some(path) = path else {
        return AutoCopyStrategyConfig::default();
    };
    load_strategy_config_for_path(path)
}

pub fn load_strategy_config_for_path(path: &Path) -> AutoCopyStrategyConfig {
    match fs::read(path) {
        Ok(bytes) => match serde_json::from_slice::<AutoCopyStrategyConfig>(&bytes) {
            Ok(config) => config,
            Err(error) => {
                eprintln!(
                    "failed to parse auto-copy strategy config {}: {error}; using disabled default strategy",
                    path.display()
                );
                AutoCopyStrategyConfig::default()
            }
        },
        Err(error) => {
            eprintln!(
                "failed to read auto-copy strategy config {}: {error}; using disabled default strategy",
                path.display()
            );
            AutoCopyStrategyConfig::default()
        }
    }
}

fn default_normal_event_budget_usd() -> f64 {
    15.0
}

fn default_strong_event_budget_usd() -> f64 {
    30.0
}

fn default_max_event_budget_usd() -> f64 {
    45.0
}

fn default_strong_event_source_notional_usd() -> f64 {
    80.0
}

fn default_strong_event_min_outcomes() -> usize {
    3
}

fn default_min_basket_outcomes_for_rebalance() -> usize {
    2
}

fn default_hot_path_max_single_buy_usd() -> f64 {
    6.0
}

fn default_low_price_leg_threshold() -> f64 {
    0.30
}

fn default_low_price_min_leg_usd() -> f64 {
    1.0
}

fn default_source_reconcile_buy_enabled() -> bool {
    false
}

fn default_source_reconcile_sell_enabled() -> bool {
    false
}

fn default_price_buckets() -> Vec<PriceBucketConfig> {
    vec![
        PriceBucketConfig {
            min_price: 0.0,
            max_price: 0.10,
            base_buy_usd: 1.0,
            label: "0-10c low-upside-leg".to_owned(),
        },
        PriceBucketConfig {
            min_price: 0.10,
            max_price: 0.30,
            base_buy_usd: 1.5,
            label: "10-30c opportunity-leg".to_owned(),
        },
        PriceBucketConfig {
            min_price: 0.30,
            max_price: 0.60,
            base_buy_usd: 3.0,
            label: "30-60c main-judgement".to_owned(),
        },
        PriceBucketConfig {
            min_price: 0.60,
            max_price: 0.85,
            base_buy_usd: 2.0,
            label: "60-85c lower-edge".to_owned(),
        },
        PriceBucketConfig {
            min_price: 0.85,
            max_price: 0.95,
            base_buy_usd: 1.0,
            label: "85-95c high-price".to_owned(),
        },
        PriceBucketConfig {
            min_price: 0.95,
            max_price: 0.98,
            base_buy_usd: 0.0,
            label: "95-98c near-no-edge".to_owned(),
        },
        PriceBucketConfig {
            min_price: 0.98,
            max_price: 1.0,
            base_buy_usd: 0.0,
            label: ">98c no-edge".to_owned(),
        },
    ]
}

fn default_notional_multipliers() -> Vec<NotionalMultiplierConfig> {
    vec![
        NotionalMultiplierConfig {
            min_notional_usd: 0.0,
            max_notional_usd: Some(5.0),
            multiplier: 1.0,
            label: "<5U".to_owned(),
        },
        NotionalMultiplierConfig {
            min_notional_usd: 5.0,
            max_notional_usd: Some(20.0),
            multiplier: 1.5,
            label: "5-20U".to_owned(),
        },
        NotionalMultiplierConfig {
            min_notional_usd: 20.0,
            max_notional_usd: Some(80.0),
            multiplier: 2.0,
            label: "20-80U".to_owned(),
        },
        NotionalMultiplierConfig {
            min_notional_usd: 80.0,
            max_notional_usd: None,
            multiplier: 3.0,
            label: "80U+".to_owned(),
        },
    ]
}

#[derive(Debug, Clone)]
pub struct AutoCopyConfig {
    pub enabled: bool,
    pub mode: AutoCopyMode,
    pub source_wallet: String,
    pub source_name: String,
    pub domain: String,
    pub specialty_keywords: Vec<String>,
    pub blocked_position_keys: Vec<String>,
    pub state_path: PathBuf,
    pub global_state_path: PathBuf,
    pub strategy_config_path: Option<PathBuf>,
    pub strategy: AutoCopyStrategyConfig,
    pub executor_command: Option<String>,
    pub max_single_copy_usd: f64,
    pub copy_target_cap_usd: f64,
    pub max_market_exposure_usd: f64,
    pub max_daily_spend_usd: f64,
    pub max_daily_loss_usd: f64,
    pub max_chase_pct: f64,
    pub passive_offset_pct: f64,
    pub max_chase_delta: f64,
    pub passive_offset: f64,
    pub buy_take_enabled: bool,
    pub small_buy_full_copy_enabled: bool,
    pub min_buy_source_notional_usd: f64,
    pub skip_buy_price_at_or_above: f64,
    pub skip_buy_price_at_or_below: f64,
    pub reconcile_skip_buy_price_at_or_above: f64,
    pub reconcile_max_source_drawdown_pct: f64,
    pub high_price_exposure_threshold: f64,
    pub high_price_exposure_cap_usd: f64,
    pub high_price_max_chase_pct: f64,
    pub min_sell_sync_notional_usd: f64,
    pub passive_order_ttl_seconds: u64,
    pub startup_backfill_seconds: u64,
    pub pending_sync_seconds: u64,
    pub failed_action_cooldown_seconds: u64,
    pub source_flow_window_seconds: u64,
    pub post_sell_buy_guard_seconds: u64,
    pub source_pressure_cooldown_seconds: u64,
    pub source_pressure_min_sell_count: usize,
    pub source_pressure_min_sell_notional_usd: f64,
    pub source_pressure_max_avg_sell_gap_seconds: u64,
    pub source_reentry_alert_buy_usd: f64,
    pub small_sell_passive_fraction_threshold: f64,
    pub small_sell_passive_discount_pct: f64,
}

impl AutoCopyConfig {
    pub fn weatherhk_default() -> Self {
        let strategy_config_path = env_string("WEATHERHK_STRATEGY_CONFIG_PATH").map(PathBuf::from);
        let strategy = load_strategy_config_from_env(strategy_config_path.as_deref());
        Self {
            enabled: env_bool("WEATHERHK_AUTO_COPY_ENABLED", false),
            mode: env_string("WEATHERHK_AUTO_COPY_MODE")
                .as_deref()
                .map(AutoCopyMode::parse)
                .transpose()
                .unwrap_or_else(|error| {
                    eprintln!("{error}; falling back to dry-run");
                    Some(AutoCopyMode::DryRun)
                })
                .unwrap_or(AutoCopyMode::DryRun),
            source_wallet: env_string("WEATHERHK_SOURCE_WALLET")
                .unwrap_or_else(|| WEATHERHK_WALLET.to_owned()),
            source_name: env_string("WEATHERHK_SOURCE_NAME")
                .unwrap_or_else(|| "WeatherHK".to_owned()),
            domain: env_string("WEATHERHK_DOMAIN").unwrap_or_else(|| "WEATHER".to_owned()),
            specialty_keywords: env_keywords(
                "WEATHERHK_SPECIALTY_KEYWORDS",
                "weather,temperature,precipitation,hurricane,typhoon,cyclone,storm,rain,snow,wind",
            ),
            blocked_position_keys: env_keywords("WEATHERHK_BLOCKED_POSITION_KEYS", ""),
            state_path: PathBuf::from(
                env_string("WEATHERHK_STATE_PATH").unwrap_or_else(|| DEFAULT_STATE_PATH.to_owned()),
            ),
            global_state_path: PathBuf::from(
                env_string("WEATHERHK_GLOBAL_STATE_PATH")
                    .unwrap_or_else(|| DEFAULT_GLOBAL_STATE_PATH.to_owned()),
            ),
            strategy_config_path,
            strategy,
            executor_command: env_string("WEATHERHK_AUTO_COPY_EXEC"),
            max_single_copy_usd: env_f64("WEATHERHK_MAX_SINGLE_COPY_USD", 100_000.0),
            copy_target_cap_usd: env_f64(
                "WEATHERHK_COPY_TARGET_CAP_USD",
                DEFAULT_COPY_TARGET_CAP_USD,
            ),
            max_market_exposure_usd: env_f64("WEATHERHK_MAX_MARKET_EXPOSURE_USD", 100_000.0),
            max_daily_spend_usd: env_f64("WEATHERHK_MAX_DAILY_SPEND_USD", 100_000.0),
            max_daily_loss_usd: env_f64("WEATHERHK_MAX_DAILY_LOSS_USD", 100_000.0),
            max_chase_pct: env_f64("WEATHERHK_MAX_CHASE_PCT", 0.15),
            passive_offset_pct: env_f64("WEATHERHK_PASSIVE_OFFSET_PCT", 0.05),
            max_chase_delta: env_f64("WEATHERHK_MAX_CHASE_DELTA", 0.03),
            passive_offset: env_f64("WEATHERHK_PASSIVE_OFFSET", 0.02),
            buy_take_enabled: env_bool("WEATHERHK_BUY_TAKE_ENABLED", false),
            small_buy_full_copy_enabled: env_bool("WEATHERHK_SMALL_BUY_FULL_COPY_ENABLED", true),
            min_buy_source_notional_usd: env_f64("WEATHERHK_MIN_BUY_SOURCE_NOTIONAL_USD", 0.0),
            skip_buy_price_at_or_above: env_f64("WEATHERHK_SKIP_BUY_PRICE_AT_OR_ABOVE", 0.98),
            skip_buy_price_at_or_below: env_f64("WEATHERHK_SKIP_BUY_PRICE_AT_OR_BELOW", 0.0),
            reconcile_skip_buy_price_at_or_above: env_f64(
                "WEATHERHK_RECONCILE_SKIP_BUY_PRICE_AT_OR_ABOVE",
                0.95,
            ),
            reconcile_max_source_drawdown_pct: env_f64(
                "WEATHERHK_RECONCILE_MAX_SOURCE_DRAWDOWN_PCT",
                0.95,
            ),
            high_price_exposure_threshold: env_f64("WEATHERHK_HIGH_PRICE_EXPOSURE_THRESHOLD", 0.90),
            high_price_exposure_cap_usd: env_f64("WEATHERHK_HIGH_PRICE_EXPOSURE_CAP_USD", 50.0),
            high_price_max_chase_pct: env_f64("WEATHERHK_HIGH_PRICE_MAX_CHASE_PCT", 0.0),
            min_sell_sync_notional_usd: env_f64("WEATHERHK_MIN_SELL_SYNC_NOTIONAL_USD", 0.0),
            passive_order_ttl_seconds: env_u64("WEATHERHK_PASSIVE_TTL_SECONDS", 0),
            startup_backfill_seconds: env_u64("WEATHERHK_STARTUP_BACKFILL_SECONDS", 1_800),
            pending_sync_seconds: env_u64("WEATHERHK_PENDING_SYNC_SECONDS", 30),
            failed_action_cooldown_seconds: env_u64(
                "WEATHERHK_FAILED_ACTION_COOLDOWN_SECONDS",
                900,
            ),
            source_flow_window_seconds: env_u64("WEATHERHK_SOURCE_FLOW_WINDOW_SECONDS", 120),
            post_sell_buy_guard_seconds: env_u64("WEATHERHK_POST_SELL_BUY_GUARD_SECONDS", 120),
            source_pressure_cooldown_seconds: env_u64(
                "WEATHERHK_SOURCE_PRESSURE_COOLDOWN_SECONDS",
                300,
            ),
            source_pressure_min_sell_count: env_usize(
                "WEATHERHK_SOURCE_PRESSURE_MIN_SELL_COUNT",
                3,
            ),
            source_pressure_min_sell_notional_usd: env_f64(
                "WEATHERHK_SOURCE_PRESSURE_MIN_SELL_NOTIONAL_USD",
                3.0,
            ),
            source_pressure_max_avg_sell_gap_seconds: env_u64(
                "WEATHERHK_SOURCE_PRESSURE_MAX_AVG_SELL_GAP_SECONDS",
                30,
            ),
            source_reentry_alert_buy_usd: env_f64("WEATHERHK_SOURCE_REENTRY_ALERT_BUY_USD", 30.0),
            small_sell_passive_fraction_threshold: env_f64(
                "WEATHERHK_SMALL_SELL_PASSIVE_FRACTION_THRESHOLD",
                SMALL_SELL_PASSIVE_FRACTION_THRESHOLD,
            ),
            small_sell_passive_discount_pct: env_f64(
                "WEATHERHK_SMALL_SELL_PASSIVE_DISCOUNT_PCT",
                SMALL_SELL_PASSIVE_DISCOUNT_PCT,
            ),
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if !self.enabled {
            return Ok(());
        }

        if self.mode == AutoCopyMode::LiveExternal
            && self
                .executor_command
                .as_deref()
                .map(str::trim)
                .filter(|command| !command.is_empty())
                .is_none()
        {
            return Err(
                "WeatherHK live-external mode requires --weatherhk-auto-copy-exec".to_owned(),
            );
        }

        if self.max_single_copy_usd <= 0.0 {
            return Err("WeatherHK max single copy must be > 0".to_owned());
        }
        if self.copy_target_cap_usd <= 0.0 {
            return Err("WeatherHK copy target cap must be > 0".to_owned());
        }
        if self.max_market_exposure_usd <= 0.0 {
            return Err("WeatherHK market exposure cap must be > 0".to_owned());
        }
        if self.max_daily_spend_usd <= 0.0 {
            return Err("WeatherHK daily spend cap must be > 0".to_owned());
        }
        if !(0.0..=1.0).contains(&self.max_chase_pct) {
            return Err("WeatherHK max chase pct must be between 0 and 1".to_owned());
        }
        if !(0.0..=1.0).contains(&self.passive_offset_pct) {
            return Err("WeatherHK passive offset pct must be between 0 and 1".to_owned());
        }
        if !(0.0..=0.25).contains(&self.max_chase_delta) {
            return Err("WeatherHK max chase delta must be between 0 and 0.25".to_owned());
        }
        if !(0.0..=0.25).contains(&self.passive_offset) {
            return Err("WeatherHK passive offset must be between 0 and 0.25".to_owned());
        }
        if self.min_buy_source_notional_usd < 0.0 {
            return Err("WeatherHK min buy source notional must be >= 0".to_owned());
        }
        if !(0.01..=1.0).contains(&self.skip_buy_price_at_or_above) {
            return Err("WeatherHK skip buy price must be between 0.01 and 1.0".to_owned());
        }
        if !(0.0..=1.0).contains(&self.skip_buy_price_at_or_below) {
            return Err(
                "WeatherHK low-price skip buy threshold must be between 0 and 1.0".to_owned(),
            );
        }
        if !(0.01..=1.0).contains(&self.reconcile_skip_buy_price_at_or_above) {
            return Err(
                "WeatherHK reconcile skip buy price must be between 0.01 and 1.0".to_owned(),
            );
        }
        if !(0.0..=1.0).contains(&self.reconcile_max_source_drawdown_pct) {
            return Err(
                "WeatherHK reconcile max source drawdown must be between 0 and 1".to_owned(),
            );
        }
        if !(0.0..=1.0).contains(&self.high_price_exposure_threshold) {
            return Err(
                "WeatherHK high-price exposure threshold must be between 0 and 1.0".to_owned(),
            );
        }
        if self.high_price_exposure_cap_usd <= 0.0 {
            return Err("WeatherHK high-price exposure cap must be > 0".to_owned());
        }
        if !(0.0..=1.0).contains(&self.high_price_max_chase_pct) {
            return Err("WeatherHK high-price max chase pct must be between 0 and 1".to_owned());
        }
        if self.min_sell_sync_notional_usd < 0.0 {
            return Err("WeatherHK min sell sync notional must be >= 0".to_owned());
        }
        if self.source_pressure_min_sell_count == 0 {
            return Err("WeatherHK source pressure sell count must be > 0".to_owned());
        }
        if self.source_pressure_min_sell_notional_usd < 0.0 {
            return Err("WeatherHK source pressure sell notional must be >= 0".to_owned());
        }
        if self.source_reentry_alert_buy_usd < 0.0 {
            return Err("WeatherHK source reentry alert buy amount must be >= 0".to_owned());
        }
        if !(0.0..=1.0).contains(&self.small_sell_passive_fraction_threshold) {
            return Err(
                "WeatherHK small-sell passive fraction threshold must be between 0 and 1"
                    .to_owned(),
            );
        }
        if !(0.0..=0.95).contains(&self.small_sell_passive_discount_pct) {
            return Err(
                "WeatherHK small-sell passive discount pct must be between 0 and 0.95".to_owned(),
            );
        }
        self.strategy.validate()?;

        Ok(())
    }
}

impl Default for AutoCopyConfig {
    fn default() -> Self {
        Self::weatherhk_default()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AutoCopyMode {
    DryRun,
    LiveExternal,
}

impl AutoCopyMode {
    pub fn parse(value: &str) -> Result<Self, String> {
        match value.trim().to_lowercase().as_str() {
            "dry-run" | "dryrun" | "dry" => Ok(Self::DryRun),
            "live-external" | "external" | "live" => Ok(Self::LiveExternal),
            _ => Err(format!(
                "invalid WeatherHK auto-copy mode: {value}; use dry-run or live-external"
            )),
        }
    }

    pub fn label_for_display(self) -> &'static str {
        self.label()
    }

    fn label(self) -> &'static str {
        match self {
            Self::DryRun => "dry-run",
            Self::LiveExternal => "live-external",
        }
    }
}

#[derive(Debug, Clone)]
struct GlobalOutcomeMetadata {
    position_key: String,
    market_title: Option<String>,
    outcome: Option<String>,
    asset: String,
    condition_id: String,
}

impl GlobalOutcomeMetadata {
    fn from_trade(trade: &UserTrade) -> Self {
        Self {
            position_key: position_key(trade),
            market_title: trade.title.clone(),
            outcome: trade.outcome.clone(),
            asset: trade.asset.clone(),
            condition_id: trade.condition_id.clone(),
        }
    }

    fn from_source_metadata(metadata: &SourceOutcomeMetadata) -> Self {
        Self {
            position_key: metadata.position_key.clone(),
            market_title: metadata.market_title.clone(),
            outcome: metadata.outcome.clone(),
            asset: metadata.asset.clone(),
            condition_id: metadata.condition_id.clone(),
        }
    }

    fn from_position(position: &CopyPosition) -> Self {
        Self {
            position_key: position.position_key.clone(),
            market_title: position.market_title.clone(),
            outcome: position.outcome.clone(),
            asset: position.asset.clone(),
            condition_id: position.condition_id.clone(),
        }
    }

    fn from_order(order: &PendingCopyOrder) -> Self {
        Self {
            position_key: order.position_key.clone(),
            market_title: order.market_title.clone(),
            outcome: order.outcome.clone(),
            asset: order.asset.clone(),
            condition_id: order.condition_id.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct GlobalPendingBuyOrder {
    local_order_id: String,
    external_order_id: Option<String>,
    copy_amount_usd: f64,
    remaining_amount_usd: f64,
    limit_price: f64,
    created_at_secs: u64,
    expires_at_secs: u64,
    updated_at_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct GlobalBuyReservation {
    amount_usd: f64,
    reserved_at_secs: u64,
    expires_at_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct GlobalSourceOutcomeState {
    source_name: String,
    source_wallet: String,
    position_key: String,
    market_title: Option<String>,
    outcome: Option<String>,
    asset: String,
    condition_id: String,
    target_usd: f64,
    filled_position_usd: f64,
    pending_buy_usd: f64,
    pending_buy_orders: Vec<GlobalPendingBuyOrder>,
    reservation: Option<GlobalBuyReservation>,
    updated_at_secs: u64,
}

impl GlobalSourceOutcomeState {
    fn is_active(&self, now: u64) -> bool {
        now.saturating_sub(self.updated_at_secs) <= GLOBAL_SOURCE_STALE_SECONDS
    }

    fn active_reservation_usd(&self, now: u64) -> f64 {
        self.reservation
            .as_ref()
            .filter(|reservation| reservation.amount_usd > 0.0 && reservation.expires_at_secs > now)
            .map(|reservation| reservation.amount_usd)
            .unwrap_or(0.0)
    }

    fn active_pending_orders(&self, now: u64) -> impl Iterator<Item = &GlobalPendingBuyOrder> {
        self.pending_buy_orders.iter().filter(move |order| {
            order.remaining_amount_usd > 0.000_001
                && (order.expires_at_secs == 0 || order.expires_at_secs > now)
        })
    }

    fn committed_usd(&self, now: u64) -> f64 {
        self.filled_position_usd.max(0.0)
            + self.pending_buy_usd.max(0.0)
            + self.active_reservation_usd(now)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct GlobalAutoCopyState {
    #[serde(default)]
    schema_version: u8,
    #[serde(default)]
    source_outcomes: Vec<GlobalSourceOutcomeState>,
}

impl Default for GlobalAutoCopyState {
    fn default() -> Self {
        Self {
            schema_version: GLOBAL_AUTO_COPY_STATE_SCHEMA_VERSION,
            source_outcomes: Vec::new(),
        }
    }
}

impl GlobalAutoCopyState {
    fn load(path: &Path) -> Result<Self, AutoCopyError> {
        if !path.exists() {
            return Ok(Self::default());
        }

        let bytes = fs::read(path).map_err(AutoCopyError::Io)?;
        let mut state: Self = serde_json::from_slice(&bytes).map_err(AutoCopyError::Json)?;
        state.migrate();
        Ok(state)
    }

    fn migrate(&mut self) {
        if self.schema_version < GLOBAL_AUTO_COPY_STATE_SCHEMA_VERSION {
            self.schema_version = GLOBAL_AUTO_COPY_STATE_SCHEMA_VERSION;
        }
    }

    fn save(&self, path: &Path) -> Result<(), AutoCopyError> {
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent).map_err(AutoCopyError::Io)?;
        }

        let json = serde_json::to_vec_pretty(self).map_err(AutoCopyError::Json)?;
        fs::write(path, json).map_err(AutoCopyError::Io)
    }

    fn prune_expired_reservations(&mut self, now: u64) {
        for outcome in &mut self.source_outcomes {
            if outcome
                .reservation
                .as_ref()
                .is_some_and(|reservation| reservation.expires_at_secs <= now)
            {
                outcome.reservation = None;
            }
        }
    }

    fn upsert_source_outcome(&mut self, snapshot: GlobalSourceOutcomeState) {
        if let Some(existing) = self.source_outcomes.iter_mut().find(|outcome| {
            outcome.source_name == snapshot.source_name
                && outcome.position_key == snapshot.position_key
        }) {
            *existing = snapshot;
            return;
        }

        self.source_outcomes.push(snapshot);
    }

    fn source_target_usd(&self, source_name: &str, position_key: &str) -> Option<f64> {
        self.source_outcomes
            .iter()
            .find(|outcome| {
                outcome.source_name == source_name && outcome.position_key == position_key
            })
            .map(|outcome| outcome.target_usd)
    }

    fn global_target_usd(&self, position_key: &str, cap_usd: f64, now: u64) -> f64 {
        self.source_outcomes
            .iter()
            .filter(|outcome| outcome.position_key == position_key && outcome.is_active(now))
            .map(|outcome| outcome.target_usd.max(0.0))
            .sum::<f64>()
            .min(cap_usd)
    }

    fn global_committed_usd(&self, position_key: &str, now: u64) -> f64 {
        self.source_outcomes
            .iter()
            .filter(|outcome| outcome.position_key == position_key && outcome.is_active(now))
            .map(|outcome| outcome.committed_usd(now))
            .sum::<f64>()
    }

    fn active_pending_buy_orders(
        &self,
        position_key: &str,
        now: u64,
    ) -> Vec<(&GlobalSourceOutcomeState, &GlobalPendingBuyOrder)> {
        self.source_outcomes
            .iter()
            .filter(|outcome| outcome.position_key == position_key && outcome.is_active(now))
            .flat_map(|outcome| {
                outcome
                    .active_pending_orders(now)
                    .map(move |order| (outcome, order))
            })
            .collect()
    }
}

#[derive(Debug, Clone)]
struct GlobalBuyPlan {
    source_target_usd: f64,
    source_committed_usd: f64,
    global_target_usd: f64,
    global_committed_usd: f64,
    global_gap_usd: f64,
    requested_amount_usd: f64,
    reserved_amount_usd: f64,
    active_pending_count: usize,
    active_pending_source: Option<String>,
}

impl GlobalBuyPlan {
    fn blocked_by_pending(&self) -> bool {
        self.requested_amount_usd > 0.000_001 && self.active_pending_count > 0
    }
}

#[derive(Debug, Clone)]
struct GlobalSourceSupport {
    source_name: String,
    target_usd: f64,
}

#[derive(Debug, Clone)]
struct GlobalSellPlan {
    source_target_before_usd: f64,
    source_target_after_usd: f64,
    global_target_before_usd: f64,
    global_target_after_usd: f64,
    global_committed_usd: f64,
    excess_usd: f64,
    sell_fraction: f64,
    clear_all: bool,
    supporting_sources: Vec<GlobalSourceSupport>,
}

impl GlobalSellPlan {
    fn should_sell(&self) -> bool {
        self.excess_usd > 0.000_001 && self.sell_fraction > 0.000_001
    }

    fn supporting_sources_label(&self) -> String {
        if self.supporting_sources.is_empty() {
            return "没有其他员工继续贡献目标".to_owned();
        }

        let sources = self
            .supporting_sources
            .iter()
            .map(|support| format!("{} {:.2}U", support.source_name, support.target_usd))
            .collect::<Vec<_>>()
            .join(", ");
        format!("其他员工仍支持: {sources}")
    }

    fn coordination_reason(&self) -> String {
        format!(
            "全局卖出协调: 本员工目标 {:.4}U -> {:.4}U；多员工全局目标 {:.4}U -> {:.4}U，当前全局已成交+挂单+预留 {:.4}U，超额 {:.4}U，按真实钱包余额比例卖出 {:.2}%；{}。",
            self.source_target_before_usd,
            self.source_target_after_usd,
            self.global_target_before_usd,
            self.global_target_after_usd,
            self.global_committed_usd,
            self.excess_usd,
            self.sell_fraction * 100.0,
            self.supporting_sources_label()
        )
    }
}

struct GlobalStateLock {
    path: PathBuf,
}

impl GlobalStateLock {
    fn acquire(state_path: &Path) -> Result<Self, AutoCopyError> {
        let mut lock_path = state_path.to_path_buf();
        let extension = lock_path
            .extension()
            .and_then(|extension| extension.to_str())
            .map(|extension| format!("{extension}.lock"))
            .unwrap_or_else(|| "lock".to_owned());
        lock_path.set_extension(extension);

        if let Some(parent) = lock_path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent).map_err(AutoCopyError::Io)?;
        }

        for attempt in 0..=GLOBAL_STATE_LOCK_RETRIES {
            match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&lock_path)
            {
                Ok(mut file) => {
                    let _ = writeln!(file, "{}", now_secs());
                    return Ok(Self { path: lock_path });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    if attempt == GLOBAL_STATE_LOCK_RETRIES {
                        return Err(AutoCopyError::Io(error));
                    }
                    std::thread::sleep(Duration::from_millis(GLOBAL_STATE_LOCK_RETRY_MS));
                }
                Err(error) => return Err(AutoCopyError::Io(error)),
            }
        }

        unreachable!("lock acquisition loop always returns")
    }
}

impl Drop for GlobalStateLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

#[derive(Debug)]
pub struct AutoCopyEngine {
    config: AutoCopyConfig,
    state: AutoCopyState,
}

impl AutoCopyEngine {
    pub fn new(config: AutoCopyConfig) -> Result<Self, AutoCopyError> {
        let mut state = AutoCopyState::load(&config.state_path)?;
        state.reset_day_if_needed(now_secs());

        Ok(Self { config, state })
    }

    pub fn config(&self) -> &AutoCopyConfig {
        &self.config
    }

    pub fn should_backfill_startup_trade(
        &self,
        employee: &WatchedEmployee,
        trade: &UserTrade,
        now_secs: u64,
    ) -> bool {
        if self.config.startup_backfill_seconds == 0 {
            return false;
        }

        if !self.should_handle(employee, trade) {
            return false;
        }

        let Some(timestamp) = trade.timestamp else {
            return false;
        };

        now_secs.saturating_sub(timestamp) <= self.config.startup_backfill_seconds
    }

    pub fn handle_tick(&mut self) -> Vec<AutoCopyReport> {
        let now = now_secs();
        self.state.reset_day_if_needed(now);

        let mut reports = Vec::new();
        let cleared_terminal_positions = self.clear_terminal_exit_failures();
        let cleared_stale_exit_retries = self.clear_stale_exit_retries();
        if let Err(error) = self.refresh_global_local_activity(now) {
            reports.push(AutoCopyReport::system(format!(
                "[{} 全局跟单状态刷新失败]\n原因: {error}",
                self.config.source_name
            )));
        }
        reports.extend(self.sync_pending_orders(now, MAX_PENDING_SYNCS_PER_TICK).0);
        reports.extend(self.cancel_expired_orders(now, usize::MAX).0);
        reports.extend(self.retry_transient_exit_failures(now, usize::MAX).0);
        if cleared_terminal_positions || cleared_stale_exit_retries {
            self.persist(&mut reports);
        } else {
            self.persist_if_needed(&mut reports);
        }
        reports
    }

    pub fn handle_maintenance_step(&mut self) -> Vec<AutoCopyReport> {
        let now = now_secs();
        self.state.reset_day_if_needed(now);

        let mut reports = Vec::new();
        let cleared_terminal_positions = self.clear_terminal_exit_failures();
        let cleared_stale_exit_retries = self.clear_stale_exit_retries();
        if let Err(error) = self.refresh_global_local_activity(now) {
            reports.push(AutoCopyReport::system(format!(
                "[{} 全局跟单状态刷新失败]\n原因: {error}",
                self.config.source_name
            )));
        }

        let (step_reports, attempted) = self.retry_transient_exit_failures(now, 1);
        reports.extend(step_reports);
        let mut attempted_external_action = attempted;

        if !attempted_external_action {
            let (step_reports, attempted) = self.cancel_expired_orders(now, 1);
            reports.extend(step_reports);
            attempted_external_action = attempted;
        }

        if !attempted_external_action {
            let (step_reports, _) = self.sync_pending_orders(now, 1);
            reports.extend(step_reports);
        }

        if cleared_terminal_positions || cleared_stale_exit_retries {
            self.persist(&mut reports);
        } else {
            self.persist_if_needed(&mut reports);
        }
        reports
    }

    pub fn needs_source_position_reconcile(&self) -> bool {
        self.has_pending_buy_orders()
            || self
                .state
                .positions
                .iter()
                .any(|position| position.size_shares > 0.0)
            || !self.state.source_outcomes.is_empty()
    }

    fn has_pending_buy_orders(&self) -> bool {
        self.state
            .pending_orders
            .iter()
            .any(|order| order.side == "BUY")
    }

    pub fn reconcile_absent_from_source_positions(
        &mut self,
        source_positions: &HashMap<String, ObservedSourcePosition>,
    ) -> Vec<AutoCopyReport> {
        let now = now_secs();
        let orders = self
            .state
            .pending_orders
            .iter()
            .filter(|order| {
                should_cancel_pending_buy_absent_from_source_position(order, source_positions, now)
            })
            .cloned()
            .collect::<Vec<_>>();
        let positions = self
            .state
            .positions
            .iter()
            .filter(|position| {
                position.size_shares > 0.0
                    && !position.asset.trim().is_empty()
                    && !source_positions.contains_key(&position.asset)
            })
            .cloned()
            .collect::<Vec<_>>();

        let mut reports = Vec::new();
        for order in orders {
            let reason = format!(
                "{} 当前已不持有该 outcome，取消未成交跟单买单",
                self.config.source_name
            );
            for report in self.cancel_pending_order(&order, &reason, now) {
                self.push_report(report, &mut reports, now);
            }
        }

        for position in positions {
            match self.absent_source_position_action(&position, now) {
                SourcePositionAbsenceAction::Sell => {
                    for report in self.sell_position_absent_from_source(&position, now) {
                        self.push_report(report, &mut reports, now);
                    }
                }
                SourcePositionAbsenceAction::Wait(report) => {
                    self.push_report(report, &mut reports, now);
                }
                SourcePositionAbsenceAction::Ignore => {}
            }
        }

        self.state
            .replace_source_position_snapshots(source_positions, now);

        self.persist_if_needed(&mut reports);
        reports
    }

    pub fn reconcile_absent_from_source_positions_step(
        &mut self,
        source_positions: &HashMap<String, ObservedSourcePosition>,
    ) -> Vec<AutoCopyReport> {
        let now = now_secs();
        let order = self
            .state
            .pending_orders
            .iter()
            .find(|order| {
                should_cancel_pending_buy_absent_from_source_position(order, source_positions, now)
            })
            .cloned();
        let positions = self
            .state
            .positions
            .iter()
            .filter(|position| {
                position.size_shares > 0.0
                    && !position.asset.trim().is_empty()
                    && !source_positions.contains_key(&position.asset)
                    && !self.state.failure_in_cooldown(
                        &action_failure_cooldown_key("SELL", &position.position_key),
                        now,
                        SOURCE_RECONCILE_ACTION_COOLDOWN_SECONDS,
                    )
            })
            .cloned()
            .collect::<Vec<_>>();
        let mut position_action = None;
        if order.is_none() {
            for position in positions {
                match self.absent_source_position_action(&position, now) {
                    SourcePositionAbsenceAction::Sell => {
                        position_action = Some((position, None));
                        break;
                    }
                    SourcePositionAbsenceAction::Wait(report) => {
                        position_action = Some((position, Some(report)));
                        break;
                    }
                    SourcePositionAbsenceAction::Ignore => {}
                }
            }
        }
        let ineligible_order = if order.is_none() && position_action.is_none() {
            self.pending_buy_ineligible_for_reconcile(source_positions)
        } else {
            None
        };
        let stale_price_notice =
            if order.is_none() && position_action.is_none() && ineligible_order.is_none() {
                self.pending_buy_source_position_reprice_notice(source_positions, now)
            } else {
                None
            };
        let over_target_order = if order.is_none()
            && position_action.is_none()
            && ineligible_order.is_none()
            && stale_price_notice.is_none()
        {
            self.pending_buy_over_target(source_positions, now)
        } else {
            None
        };
        let under_target = if order.is_none()
            && position_action.is_none()
            && ineligible_order.is_none()
            && stale_price_notice.is_none()
            && over_target_order.is_none()
        {
            self.under_target_source_position(source_positions, now)
        } else {
            None
        };

        let mut reports = Vec::new();
        if let Some(order) = order {
            let reason = format!(
                "{} 当前已不持有该 outcome，取消未成交跟单买单",
                self.config.source_name
            );
            for report in self.cancel_pending_order(&order, &reason, now) {
                self.push_report(report, &mut reports, now);
            }
        } else if let Some((position, wait_report)) = position_action {
            if let Some(report) = wait_report {
                self.push_report(report, &mut reports, now);
            } else {
                for report in self.sell_position_absent_from_source(&position, now) {
                    self.push_report(report, &mut reports, now);
                }
            }
        } else if let Some((order, reason)) = ineligible_order {
            for report in self.cancel_pending_order(&order, &reason, now) {
                self.push_report(report, &mut reports, now);
            }
        } else if let Some(report) = stale_price_notice {
            self.push_report(report, &mut reports, now);
        } else if let Some(order) = over_target_order {
            let reason = format!(
                "{} 当前持仓目标已低于我方已承诺仓位，取消多余未成交买单",
                self.config.source_name
            );
            for report in self.cancel_pending_order(&order, &reason, now) {
                self.push_report(report, &mut reports, now);
            }
        } else if let Some(reconcile) = under_target {
            let report = self.buy_position_under_target(&reconcile, now);
            self.push_report(report, &mut reports, now);
        }

        self.state
            .replace_source_position_snapshots(source_positions, now);
        self.persist_if_needed(&mut reports);
        reports
    }

    fn absent_source_position_action(
        &mut self,
        position: &CopyPosition,
        now: u64,
    ) -> SourcePositionAbsenceAction {
        let previous_snapshot = self
            .state
            .source_position_snapshot(&position.asset)
            .cloned();
        let absence =
            self.state
                .record_source_position_absence(position, previous_snapshot.as_ref(), now);
        let last_buy_at = self
            .state
            .source_outcome_by_asset(&position.asset)
            .and_then(|outcome| outcome.last_buy_at_secs);
        let protected_from = absence
            .first_missing_at_secs
            .max(last_buy_at.unwrap_or_default());
        let elapsed = now.saturating_sub(protected_from);
        let had_prior_source_position = absence
            .last_seen_source_size_shares
            .is_some_and(|size| size > 0.0);

        if had_prior_source_position
            && absence.missing_count >= SOURCE_POSITION_ABSENCE_CONFIRM_MIN_COUNT
            && elapsed >= SOURCE_POSITION_ABSENCE_CONFIRM_SECONDS
        {
            return SourcePositionAbsenceAction::Sell;
        }

        if !had_prior_source_position && absence.missing_count > 1 {
            return SourcePositionAbsenceAction::Ignore;
        }

        SourcePositionAbsenceAction::Wait(self.report_for_source_position_absence_wait(
            position,
            &absence,
            last_buy_at,
            now,
        ))
    }

    fn report_for_source_position_absence_wait(
        &self,
        position: &CopyPosition,
        absence: &SourcePositionAbsence,
        last_buy_at: Option<u64>,
        now: u64,
    ) -> AutoCopyReport {
        let protected_from = absence
            .first_missing_at_secs
            .max(last_buy_at.unwrap_or_default());
        let elapsed = now.saturating_sub(protected_from);
        let remaining = SOURCE_POSITION_ABSENCE_CONFIRM_SECONDS.saturating_sub(elapsed);
        let had_prior_source_position = absence
            .last_seen_source_size_shares
            .is_some_and(|size| size > 0.0);
        let source_name = self.config.source_name.as_str();
        let reason = if had_prior_source_position {
            format!(
                "/positions 本轮未出现该 outcome，但缺失只是弱信号；此前曾见源仓位 {:.4} 份，本次为第 {} 次缺失，距离保护锚点 {} 秒，还需至少 {} 秒且连续缺失满 {} 次才允许源仓对账卖出。若 {} 出现明确 SELL 成交，仍会立即按 SELL 跟随。",
                absence.last_seen_source_size_shares.unwrap_or_default(),
                absence.missing_count,
                elapsed,
                remaining,
                SOURCE_POSITION_ABSENCE_CONFIRM_MIN_COUNT,
                source_name
            )
        } else {
            format!(
                "/positions 本轮未出现该 outcome，但我们此前从未在 {} 的 /positions 中正向确认持有该 outcome；该缺失不能证明员工已清仓，本次只记录观察，不卖出。我方后续只根据明确 SELL 或真实源仓位先出现后消失来减仓。",
                source_name
            )
        };
        let text = format!(
            "[{} 源仓位对账观察 / {}]\n\
状态: skipped\n\
市场: {}\n\
方向: {}\n\
源仓位: /positions 本轮未出现该 outcome\n\
我方: 暂不卖出 {:.4} 份\n\
原因: {}",
            source_name,
            self.config.mode.label(),
            position.market_title.as_deref().unwrap_or("-"),
            position.outcome.as_deref().unwrap_or("-"),
            position.size_shares,
            reason
        );

        AutoCopyReport {
            action: "SKIP:源仓位缺失待确认".to_owned(),
            status: "skipped".to_owned(),
            reason,
            source_trade_key: "-".to_owned(),
            market_title: position.market_title.clone(),
            outcome: position.outcome.clone(),
            position_key: position.position_key.clone(),
            source_price: None,
            source_notional_usd: 0.0,
            copy_amount_usd: 0.0,
            copy_price: None,
            order_id: None,
            realized_pnl_usd: None,
            market_exposure_after_usd: self.state.market_exposure_usd(&position.position_key),
            daily_spend_after_usd: self.state.daily_spend_usd + self.state.daily_reserved_buy_usd(),
            created_at_secs: now,
            text,
        }
    }

    fn pending_buy_ineligible_for_reconcile(
        &self,
        source_positions: &HashMap<String, ObservedSourcePosition>,
    ) -> Option<(PendingCopyOrder, String)> {
        self.state
            .pending_orders
            .iter()
            .filter(|order| order.side == "BUY")
            .find_map(|order| {
                let source_position = source_positions.get(&order.asset)?;
                let reason = if self.is_position_blocked(&order.position_key) {
                    Some("该 outcome 已加入停止跟单名单".to_owned())
                } else {
                    self.source_position_reconcile_skip_reason(
                        source_position,
                        order.market_title.as_deref(),
                    )
                }?;
                Some((order.clone(), format!("{reason}；取消现有对账补买挂单。")))
            })
    }

    fn pending_buy_source_position_reprice_notice(
        &mut self,
        source_positions: &HashMap<String, ObservedSourcePosition>,
        now: u64,
    ) -> Option<AutoCopyReport> {
        let candidate = self
            .state
            .pending_orders
            .iter()
            .filter(|order| order.side == "BUY")
            .find_map(|order| {
                let source_position = source_positions.get(&order.asset)?;
                if self
                    .source_position_reconcile_skip_reason(
                        source_position,
                        order.market_title.as_deref(),
                    )
                    .is_some()
                    || self.is_position_blocked(&order.position_key)
                {
                    return None;
                }
                let avg_price = source_position
                    .avg_price
                    .filter(|price| *price > 0.0 && *price <= 1.0)?;
                let metadata = self.state.source_outcome_by_asset(&order.asset);
                let entry_price = metadata
                    .and_then(|metadata| {
                        let buy_at = metadata.last_buy_at_secs?;
                        if metadata
                            .last_sell_at_secs
                            .is_some_and(|sell_at| buy_at <= sell_at)
                        {
                            return None;
                        }
                        metadata.last_buy_price
                    })
                    .filter(|price| *price > 0.0 && *price <= 1.0)
                    .unwrap_or(avg_price);
                let direct_limit =
                    price_with_pct_upside(entry_price, self.effective_buy_chase_pct(entry_price));
                let desired_price = price_with_capped_upside(
                    entry_price,
                    self.config.passive_offset_pct,
                    self.config.passive_offset,
                )
                .min(direct_limit);
                let strategy_limit = order.requested_limit_price.unwrap_or(order.limit_price);
                (desired_price > strategy_limit + 0.01).then_some((
                    order.clone(),
                    entry_price,
                    desired_price,
                    strategy_limit,
                ))
            })?;

        let (order, entry_price, desired_price, strategy_limit) = candidate;
        let notice_key = format!(
            "{SOURCE_POSITION_REPRICE_NOTICE_PREFIX}{}",
            order.position_key
        );
        if self.state.failure_in_cooldown(
            &notice_key,
            now,
            SOURCE_POSITION_REPRICE_NOTICE_COOLDOWN_SECONDS,
        ) {
            return None;
        }

        let reason = format!(
            "源仓位对账看到 {} 参考价 {:.2}c，对应新挂单基准 {:.2}c，高于现有挂单策略价 {:.2}c；阶段 3 不再因为 /positions 或对账均价抬价撤单/重挂，保留原挂单，等待下一笔明确 BUY 再决定是否重挂。",
            self.config.source_name,
            entry_price * 100.0,
            desired_price * 100.0,
            strategy_limit * 100.0
        );
        self.state.record_failure(notice_key, reason.clone(), now);

        let text = format!(
            "[{} 对账不抬价重挂 / {}]\n状态: skipped\n市场: {}\n方向: {}\n原挂单: {:.2}U @ {:.2}c\n对账参考: {:.2}c -> 新基准 {:.2}c\n原因: {}",
            self.config.source_name,
            self.config.mode.label(),
            order.market_title.as_deref().unwrap_or("-"),
            order.outcome.as_deref().unwrap_or("-"),
            order.copy_amount_usd,
            order.limit_price * 100.0,
            entry_price * 100.0,
            desired_price * 100.0,
            reason
        );

        Some(AutoCopyReport {
            action: "SKIP:对账不抬价重挂".to_owned(),
            status: "skipped".to_owned(),
            reason,
            source_trade_key: order.source_trade_key.clone(),
            market_title: order.market_title.clone(),
            outcome: order.outcome.clone(),
            position_key: order.position_key.clone(),
            source_price: None,
            source_notional_usd: 0.0,
            copy_amount_usd: 0.0,
            copy_price: Some(order.limit_price),
            order_id: order.external_order_id.clone(),
            realized_pnl_usd: None,
            market_exposure_after_usd: self.state.market_exposure_usd(&order.position_key),
            daily_spend_after_usd: self.state.daily_spend_usd + self.state.daily_reserved_buy_usd(),
            created_at_secs: now_secs(),
            text,
        })
    }

    fn source_position_reconcile_skip_reason(
        &self,
        source_position: &ObservedSourcePosition,
        fallback_title: Option<&str>,
    ) -> Option<String> {
        if let Some(source_cost) = source_position
            .avg_price
            .filter(|price| *price > 0.0 && *price <= 1.0)
        {
            if should_skip_near_zero_buy(source_cost, self.config.skip_buy_price_at_or_below) {
                return Some(format!(
                    "{} 持仓均价 {:.2}c 达到低价尾部跳过线 {:.2}c",
                    self.config.source_name,
                    source_cost * 100.0,
                    self.config.skip_buy_price_at_or_below * 100.0
                ));
            }
            if source_cost >= self.config.reconcile_skip_buy_price_at_or_above {
                return Some(format!(
                    "{} 持仓均价 {:.2}c 达到对账补买高价低收益跳过线 {:.2}c",
                    self.config.source_name,
                    source_cost * 100.0,
                    self.config.reconcile_skip_buy_price_at_or_above * 100.0
                ));
            }
            if let Some(current_price) = source_position
                .current_price
                .filter(|price| *price > 0.0 && *price <= 1.0)
            {
                let drawdown_pct = (1.0 - current_price / source_cost).max(0.0);
                if drawdown_pct > self.config.reconcile_max_source_drawdown_pct + 0.000_000_001 {
                    return Some(format!(
                        "该 outcome 当前价 {:.2}c，较 {} 持仓均价 {:.2}c 已下跌 {:.2}%，超过 {:.0}% 停止补买线",
                        current_price * 100.0,
                        self.config.source_name,
                        source_cost * 100.0,
                        drawdown_pct * 100.0,
                        self.config.reconcile_max_source_drawdown_pct * 100.0
                    ));
                }
            }
        }

        let specialty_haystack = format!(
            "{} {} {} {}",
            source_position.market_title.as_deref().unwrap_or(""),
            source_position.slug.as_deref().unwrap_or(""),
            source_position.event_slug.as_deref().unwrap_or(""),
            fallback_title.unwrap_or("")
        );
        if !matches_specialty_keywords(&self.config.specialty_keywords, &specialty_haystack) {
            return Some(format!(
                "该仓位不匹配 {} 的天气专长关键词",
                self.config.source_name
            ));
        }

        None
    }

    fn pending_buy_over_target(
        &self,
        source_positions: &HashMap<String, ObservedSourcePosition>,
        now: u64,
    ) -> Option<PendingCopyOrder> {
        self.state
            .pending_orders
            .iter()
            .filter(|order| order.side == "BUY")
            .filter(|order| {
                !should_cancel_pending_buy_absent_from_source_position(order, source_positions, now)
            })
            .find(|order| {
                let Some(source_position) = source_positions.get(&order.asset) else {
                    return false;
                };
                let source_notional = source_position
                    .avg_price
                    .map(|avg_price| source_position.size_shares * avg_price)
                    .unwrap_or(0.0);
                let copy_fraction = self.copy_fraction_for_source_notional(source_notional);
                let target_amount = capped_copy_target(
                    source_notional * copy_fraction,
                    self.config.copy_target_cap_usd,
                );
                let committed_amount = self.state.market_exposure_usd(&order.position_key);
                committed_amount > target_amount + target_reconcile_amount_tolerance(target_amount)
            })
            .cloned()
    }

    fn under_target_source_position(
        &self,
        source_positions: &HashMap<String, ObservedSourcePosition>,
        now: u64,
    ) -> Option<TargetReconcileBuy> {
        source_positions
            .iter()
            .filter_map(|(asset, source_position)| {
                let source_avg_cost = source_position
                    .avg_price
                    .filter(|price| *price > 0.0 && *price <= 1.0)?;
                if self
                    .source_position_reconcile_skip_reason(source_position, None)
                    .is_some()
                {
                    return None;
                }
                let metadata = self
                    .state
                    .source_outcome_by_asset(asset)
                    .cloned()
                    .or_else(|| source_position.to_metadata(asset, now))?;
                if self.is_position_blocked(&metadata.position_key)
                    || self.state.failure_in_cooldown(
                        &action_failure_cooldown_key("BUY", &metadata.position_key),
                        now,
                        self.config.failed_action_cooldown_seconds,
                    )
                {
                    return None;
                }
                if metadata
                    .last_sell_at_secs
                    .is_some_and(|sell_at| metadata.last_buy_at_secs.unwrap_or(0) <= sell_at)
                {
                    return None;
                }
                let explicit_buy_entry_price = metadata
                    .last_buy_price
                    .filter(|price| *price > 0.0 && *price <= 1.0);
                let source_entry_price = explicit_buy_entry_price.unwrap_or(source_avg_cost);
                let normal_direct_limit_price = if explicit_buy_entry_price.is_some() {
                    price_with_pct_upside(
                        source_entry_price,
                        self.effective_buy_chase_pct(source_entry_price),
                    )
                } else {
                    source_entry_price
                };
                let normal_limit_price = if explicit_buy_entry_price.is_some() {
                    price_with_capped_upside(
                        source_entry_price,
                        self.config.passive_offset_pct,
                        self.config.passive_offset,
                    )
                    .min(normal_direct_limit_price)
                } else {
                    source_entry_price
                };
                let direct_limit_price = high_price_guarded_direct_limit(
                    source_entry_price,
                    normal_direct_limit_price,
                    self.config.high_price_exposure_threshold,
                    self.config.high_price_max_chase_pct,
                );
                let limit_price = high_price_guarded_passive_limit(
                    source_entry_price,
                    normal_limit_price,
                    direct_limit_price,
                    self.config.high_price_exposure_threshold,
                );
                let source_notional = source_position.size_shares * source_avg_cost;
                let copy_fraction = self.copy_fraction_for_source_notional(source_notional);
                let proportional_target = source_notional * copy_fraction;
                let high_price_reference = source_entry_price
                    .max(source_avg_cost)
                    .max(normal_limit_price);
                let target_amount = high_price_copy_target(
                    proportional_target,
                    high_price_reference,
                    self.config.high_price_exposure_threshold,
                    self.config.high_price_exposure_cap_usd,
                );
                let target_amount =
                    capped_copy_target(target_amount, self.config.copy_target_cap_usd);
                let committed_amount = self.state.market_exposure_usd(&metadata.position_key);
                let missing_amount = (target_amount - committed_amount).max(0.0);
                if missing_amount <= target_reconcile_amount_tolerance(target_amount) {
                    return None;
                }
                if missing_amount < TARGET_RECONCILE_MIN_NOTIONAL_USD {
                    return None;
                }
                let target_size = shares_for_amount(target_amount, limit_price);
                let committed = self.state.committed_buy_shares(&metadata.position_key);
                let missing = shares_for_amount(missing_amount, limit_price);
                Some(TargetReconcileBuy {
                    metadata,
                    source_size_shares: source_position.size_shares,
                    target_amount_usd: target_amount,
                    committed_amount_usd: committed_amount,
                    target_size_shares: target_size,
                    committed_size_shares: committed,
                    missing_size_shares: missing,
                    source_avg_cost,
                    source_entry_price,
                    high_price_reference,
                    copy_fraction,
                    proportional_target_amount_usd: proportional_target,
                    limit_price,
                    copy_amount_usd: missing_amount,
                })
            })
            .max_by(|left, right| {
                left.metadata
                    .last_buy_at_secs
                    .unwrap_or(0)
                    .cmp(&right.metadata.last_buy_at_secs.unwrap_or(0))
                    .then_with(|| left.copy_amount_usd.total_cmp(&right.copy_amount_usd))
            })
    }

    fn buy_position_under_target(
        &mut self,
        reconcile: &TargetReconcileBuy,
        now: u64,
    ) -> AutoCopyReport {
        if self.config.strategy.enabled && !self.config.strategy.source_reconcile_buy_enabled {
            return AutoCopyReport {
                action: "SKIP:源仓位对账只校准".to_owned(),
                status: "skipped".to_owned(),
                reason: format!(
                    "源仓位对账发现 {} 持有该 outcome 且我方低于旧目标，但事件篮子策略启用后 /positions 只作为校准和补漏线索，不直接补买；等待明确 /activity BUY 或事件篮子补仓逻辑处理。",
                    self.config.source_name
                ),
                source_trade_key: "-".to_owned(),
                market_title: reconcile.metadata.market_title.clone(),
                outcome: reconcile.metadata.outcome.clone(),
                position_key: reconcile.metadata.position_key.clone(),
                source_price: Some(reconcile.source_entry_price),
                source_notional_usd: reconcile.source_size_shares * reconcile.source_avg_cost,
                copy_amount_usd: 0.0,
                copy_price: None,
                order_id: None,
                realized_pnl_usd: None,
                market_exposure_after_usd: self
                    .state
                    .market_exposure_usd(&reconcile.metadata.position_key),
                daily_spend_after_usd: self.state.daily_spend_usd
                    + self.state.daily_reserved_buy_usd(),
                created_at_secs: now_secs(),
                text: format!(
                    "[{} 源仓位对账只校准 / {}]\n状态: skipped\n市场: {}\n方向: {}\n原因: /positions 不直接补买，等待 /activity 或事件篮子补仓。",
                    self.config.source_name,
                    self.config.mode.label(),
                    reconcile.metadata.market_title.as_deref().unwrap_or("-"),
                    reconcile.metadata.outcome.as_deref().unwrap_or("-")
                ),
            };
        }
        let mut trade = reconcile.metadata.synthetic_trade(
            "BUY",
            reconcile.source_entry_price,
            reconcile.source_size_shares,
            now,
        );
        trade.proxy_wallet = self.config.source_wallet.clone();
        trade.name = Some(self.config.source_name.clone());
        let direct_limit_price = reconcile.limit_price;
        let passive_limit_price = reconcile.limit_price;
        let local_requested_copy_amount = reconcile
            .copy_amount_usd
            .min(self.config.max_single_copy_usd)
            .min(
                self.config.max_market_exposure_usd
                    - self
                        .state
                        .market_exposure_usd(&reconcile.metadata.position_key),
            )
            .min(
                self.config.max_daily_spend_usd
                    - self.state.daily_spend_usd
                    - self.state.daily_reserved_buy_usd(),
            );
        let metadata = GlobalOutcomeMetadata::from_source_metadata(&reconcile.metadata);
        let min_copy_amount =
            TARGET_RECONCILE_MIN_NOTIONAL_USD.max(MIN_CLOB_ORDER_SIZE_SHARES * passive_limit_price);
        let global_plan = self.reserve_global_buy(
            &metadata,
            reconcile.target_amount_usd,
            local_requested_copy_amount,
            min_copy_amount,
            now,
        );
        let (copy_amount, global_coordination_note, coordination_failed) = match global_plan {
            Ok(plan) => {
                let note = if plan.blocked_by_pending() {
                    format!(
                        "；全局协调: 同 outcome 已有 {} 笔 BUY 挂单{}，阶段 1 不叠加第二笔挂单",
                        plan.active_pending_count,
                        plan.active_pending_source
                            .as_deref()
                            .map(|source| format!("（来自 {source}）"))
                            .unwrap_or_default()
                    )
                } else {
                    format!(
                        "；全局协调: 多员工目标 {:.4}U / 上限 {:.2}U，已占用 {:.4}U，剩余 {:.4}U，本次预留 {:.4}U",
                        plan.global_target_usd,
                        self.config.copy_target_cap_usd,
                        plan.global_committed_usd,
                        plan.global_gap_usd,
                        plan.reserved_amount_usd
                    )
                };
                (plan.reserved_amount_usd, note, false)
            }
            Err(error) => (
                0.0,
                format!("；全局协调状态不可用，为避免多员工重复买入，本次不补挂；原因: {error}"),
                true,
            ),
        };
        let sizing_reason = format!(
            "源仓位对账补挂：{} 当前持有 {:.4}份，持仓均价 {:.2}c，最近补仓/买入价 {:.2}c；按 {:.0}% 金额原始目标 {:.4}U，风控后目标 {:.4}U / {:.4}份{}，本员工已成交+挂单承诺 {:.4}U / {:.4}份，缺口 {:.4}U / {:.4}份；按最近买入价小幅溢价至 {:.2}c 挂 post-only，不按当前市价/FOK 追买，本地最多挂 {:.4}U{}",
            self.config.source_name,
            reconcile.source_size_shares,
            reconcile.source_avg_cost * 100.0,
            reconcile.source_entry_price * 100.0,
            reconcile.copy_fraction * 100.0,
            reconcile.proportional_target_amount_usd,
            reconcile.target_amount_usd,
            reconcile.target_size_shares,
            copy_target_cap_note(
                reconcile.proportional_target_amount_usd,
                reconcile.target_amount_usd,
                reconcile.high_price_reference,
                self.config.high_price_exposure_threshold,
                self.config.high_price_exposure_cap_usd,
                self.config.copy_target_cap_usd,
            ),
            reconcile.committed_amount_usd,
            reconcile.committed_size_shares,
            reconcile.copy_amount_usd,
            reconcile.missing_size_shares,
            reconcile.limit_price * 100.0,
            local_requested_copy_amount,
            global_coordination_note
        );
        let execution = if coordination_failed {
            ExecutionResult::skipped("global auto-copy coordinator unavailable")
        } else if copy_amount >= min_copy_amount {
            let request = AutoCopyExecutionRequest::buy(
                self.config.mode,
                self.config.source_name.clone(),
                &trade,
                copy_amount,
                direct_limit_price,
                passive_limit_price,
                false,
                self.config.passive_order_ttl_seconds,
            );
            self.execute_request(&request)
        } else {
            ExecutionResult::skipped(
                "target reconcile buy gap is below global cap, available budget, or exchange minimum",
            )
        };
        let failure_key = action_failure_cooldown_key("BUY", &reconcile.metadata.position_key);
        if matches!(
            execution.status,
            ExecutionStatus::Failed | ExecutionStatus::Skipped
        ) {
            self.state.record_failure(
                failure_key,
                execution
                    .message
                    .clone()
                    .unwrap_or_else(|| "target reconcile buy failed".to_owned()),
                now,
            );
        } else {
            self.state.clear_failure(&failure_key);
        }
        let mut report = self.report_from_execution(
            "BUY",
            &trade,
            reconcile.source_size_shares * reconcile.source_avg_cost,
            copy_amount.max(0.0),
            direct_limit_price,
            Some(passive_limit_price),
            Some(false),
            &execution,
            Some(&sizing_reason),
        );
        self.apply_buy_execution(
            &reconcile.metadata.position_key,
            &trade,
            copy_amount.max(0.0),
            passive_limit_price,
            now,
            &execution,
        );
        if !coordination_failed {
            let _ =
                self.sync_global_source_exposure(&metadata, Some(reconcile.target_amount_usd), now);
        }
        report.market_exposure_after_usd = self
            .state
            .market_exposure_usd(&reconcile.metadata.position_key);
        report.daily_spend_after_usd =
            self.state.daily_spend_usd + self.state.daily_reserved_buy_usd();
        report
    }

    pub fn handle_trade(
        &mut self,
        employee: &WatchedEmployee,
        trade: &UserTrade,
    ) -> Vec<AutoCopyReport> {
        self.handle_trade_with_source_positions(employee, trade, None)
    }

    pub fn handle_trade_with_source_positions(
        &mut self,
        employee: &WatchedEmployee,
        trade: &UserTrade,
        source_positions: Option<&HashMap<String, ObservedSourcePosition>>,
    ) -> Vec<AutoCopyReport> {
        let now = now_secs();
        self.state.reset_day_if_needed(now);

        if !self.should_handle(employee, trade) {
            return Vec::new();
        }

        let mut reports = Vec::new();

        let source_key = source_trade_key(trade);
        if self.state.has_processed_source_trade(&source_key) {
            return reports;
        }
        self.state
            .remember_processed_source_trade(source_key.clone());
        let event_time = trade.timestamp.unwrap_or(now);
        self.state
            .prune_source_memory(event_time, self.source_memory_retention_seconds());
        self.state.record_source_outcome(trade, event_time);
        self.state
            .record_source_flow(trade, source_key.clone(), now);
        let source_ledger_before = self.state.source_position_ledger(&trade.asset).cloned();
        self.state
            .record_source_position_ledger_trade(trade, event_time);
        if self.config.strategy.enabled || self.config.strategy.shadow_event_baskets {
            self.state
                .record_source_event_basket_trade(trade, event_time);
        }

        let side = trade.side.to_uppercase();
        let trade_reports = if side == "BUY" {
            self.handle_buy(trade, now, event_time)
        } else if side == "SELL" {
            self.handle_sell(trade, now, source_positions, source_ledger_before.as_ref())
        } else {
            vec![self.skip_report(
                "未知方向",
                trade,
                format!(
                    "不支持的 {} 交易方向: {}",
                    self.config.source_name, trade.side
                ),
            )]
        };

        for report in trade_reports {
            self.push_report(report, &mut reports, now);
        }

        if let Some(source_positions) = source_positions {
            self.state
                .replace_source_position_snapshots(source_positions, now);
        }

        self.persist(&mut reports);
        reports
    }

    fn source_memory_retention_seconds(&self) -> u64 {
        self.config
            .source_flow_window_seconds
            .max(SOURCE_BUY_BOOTSTRAP_WINDOW_SECONDS)
            .max(self.config.post_sell_buy_guard_seconds)
            .max(self.config.source_pressure_cooldown_seconds)
            .max(SOURCE_OUTCOME_METADATA_RETENTION_SECONDS)
    }

    fn effective_buy_chase_pct(&self, source_price: f64) -> f64 {
        if source_price > self.config.high_price_exposure_threshold {
            self.config.high_price_max_chase_pct
        } else if source_price < LOW_PRICE_BUY_THRESHOLD {
            LOW_PRICE_MAX_CHASE_PCT
        } else if source_price < MID_PRICE_BUY_THRESHOLD {
            MID_PRICE_MAX_CHASE_PCT
        } else {
            self.config.max_chase_pct
        }
    }

    fn copy_fraction_for_source_notional(&self, source_notional: f64) -> f64 {
        copy_fraction_for_source_notional_with_policy(
            source_notional,
            self.config.small_buy_full_copy_enabled,
        )
    }

    fn buy_sizing_decision(
        &self,
        trade: &UserTrade,
        source_price: f64,
        source_buy_target: &SourceBuyTarget,
        high_price_reference: f64,
    ) -> BuySizingDecision {
        if !self.config.strategy.enabled {
            let copy_fraction =
                self.copy_fraction_for_source_notional(source_buy_target.source_buy_notional_usd);
            let proportional_target = source_buy_target.source_buy_notional_usd * copy_fraction;
            let target_copy_amount = high_price_copy_target(
                proportional_target,
                high_price_reference,
                self.config.high_price_exposure_threshold,
                self.config.high_price_exposure_cap_usd,
            );
            let target_copy_amount =
                capped_copy_target(target_copy_amount, self.config.copy_target_cap_usd);
            let target_cap_note = copy_target_cap_note(
                proportional_target,
                target_copy_amount,
                high_price_reference,
                self.config.high_price_exposure_threshold,
                self.config.high_price_exposure_cap_usd,
                self.config.copy_target_cap_usd,
            );
            return BuySizingDecision {
                target_copy_amount_usd: target_copy_amount,
                target_cap_note,
                target_description: format!(
                    "按 {:.0}% 金额原始目标 {:.4}U；按 {:.0}% 金额目标 {:.4}U",
                    copy_fraction * 100.0,
                    proportional_target,
                    copy_fraction * 100.0,
                    proportional_target
                ),
                event_budget_usd: None,
                event_committed_usd: None,
                event_remaining_usd: None,
            };
        }

        let Some(bucket) = self.config.strategy.price_bucket(source_price) else {
            return BuySizingDecision {
                target_copy_amount_usd: 0.0,
                target_cap_note: "；策略价格档：未匹配价格档，本次不跟".to_owned(),
                target_description: "策略目标 0.0000U".to_owned(),
                event_budget_usd: None,
                event_committed_usd: None,
                event_remaining_usd: None,
            };
        };
        let multiplier = self
            .config
            .strategy
            .notional_multiplier(source_buy_target.source_buy_notional_usd)
            .cloned()
            .unwrap_or(NotionalMultiplierConfig {
                min_notional_usd: 0.0,
                max_notional_usd: None,
                multiplier: 1.0,
                label: "default".to_owned(),
            });
        let hot_path_target = (bucket.base_buy_usd * multiplier.multiplier)
            .min(self.config.strategy.hot_path_max_single_buy_usd)
            .max(0.0);
        let mut raw_target = hot_path_target;
        let mut strategy_parts = vec![format!(
            "价格档 {} 基础 {:.2}U × 员工同 outcome 累计 {} 倍数 {:.2} = {:.2}U，热路径单 outcome 初始上限 {:.2}U",
            bucket.label,
            bucket.base_buy_usd,
            multiplier.label,
            multiplier.multiplier,
            bucket.base_buy_usd * multiplier.multiplier,
            self.config.strategy.hot_path_max_single_buy_usd
        )];

        let mut event_budget_usd = None;
        let mut event_committed_usd = None;
        let mut event_remaining_usd = None;
        if let Some(event_slug) = event_slug_for_trade(trade) {
            if let Some(basket) = self.state.source_event_basket(&event_slug) {
                let outcome_count = basket.buy_outcome_count();
                let budget = self
                    .config
                    .strategy
                    .event_budget(basket.buy_notional_usd, outcome_count);
                let committed = self.state.event_committed_usd(&event_slug);
                let remaining = (budget - committed).max(0.0);
                event_budget_usd = Some(budget);
                event_committed_usd = Some(committed);
                event_remaining_usd = Some(remaining);
                if outcome_count >= self.config.strategy.min_basket_outcomes_for_rebalance {
                    let net_total = basket
                        .outcomes
                        .iter()
                        .map(SourceEventBasketOutcome::net_buy_notional_usd)
                        .sum::<f64>()
                        .max(basket.buy_notional_usd.max(0.0));
                    if net_total > 0.0 {
                        if let Some(outcome) = basket.outcome(&source_buy_target.position_key) {
                            let weight =
                                (outcome.net_buy_notional_usd() / net_total).clamp(0.0, 1.0);
                            let mut basket_target = budget * weight;
                            if source_price <= self.config.strategy.low_price_leg_threshold
                                && outcome.buy_notional_usd > 0.0
                                && basket_target < self.config.strategy.low_price_min_leg_usd
                            {
                                basket_target = self.config.strategy.low_price_min_leg_usd;
                            }
                            raw_target = basket_target;
                            strategy_parts.push(format!(
                                "事件篮子 {}：{} 个买入选项，事件预算 {:.2}U，当前 outcome 权重 {:.2}%，篮子目标 {:.2}U，事件已承诺 {:.2}U/剩余 {:.2}U",
                                event_slug,
                                outcome_count,
                                budget,
                                weight * 100.0,
                                basket_target,
                                committed,
                                remaining
                            ));
                        }
                    }
                } else {
                    strategy_parts.push(format!(
                        "事件篮子 {}：当前仅 {} 个买入选项，未达到 {} 个 outcome 的篮子重算门槛，先按热路径小买；事件预算 {:.2}U，已承诺 {:.2}U/剩余 {:.2}U",
                        event_slug,
                        outcome_count,
                        self.config.strategy.min_basket_outcomes_for_rebalance,
                        budget,
                        committed,
                        remaining
                    ));
                }
            }
        }

        let target_after_high_price = high_price_copy_target(
            raw_target,
            high_price_reference,
            self.config.high_price_exposure_threshold,
            self.config.high_price_exposure_cap_usd,
        );
        let target_copy_amount =
            capped_copy_target(target_after_high_price, self.config.copy_target_cap_usd);
        let mut target_cap_note = copy_target_cap_note(
            raw_target,
            target_copy_amount,
            high_price_reference,
            self.config.high_price_exposure_threshold,
            self.config.high_price_exposure_cap_usd,
            self.config.copy_target_cap_usd,
        );
        if !strategy_parts.is_empty() {
            target_cap_note.push_str("；");
            target_cap_note.push_str(&strategy_parts.join("；"));
        }
        BuySizingDecision {
            target_copy_amount_usd: target_copy_amount,
            target_cap_note,
            target_description: format!("策略目标原始 {:.4}U", raw_target),
            event_budget_usd,
            event_committed_usd,
            event_remaining_usd,
        }
    }

    fn should_use_passive_small_sell(
        &self,
        source_decision: &SourceSellDecision,
        global_sell_plan: &GlobalSellPlan,
    ) -> bool {
        self.config.small_sell_passive_fraction_threshold > 0.0
            && !source_decision.clear_all
            && !global_sell_plan.clear_all
            && source_decision.sell_fraction
                <= self.config.small_sell_passive_fraction_threshold + 0.000_001
    }

    fn passive_small_sell_limit_price(&self, source_price: f64, lock_profit: bool) -> f64 {
        let discounted =
            price_with_pct_downside(source_price, self.config.small_sell_passive_discount_pct);
        if lock_profit {
            discounted.max(LOCK_PROFIT_MIN_SELL_PRICE)
        } else {
            discounted
        }
    }

    fn fallback_source_target_before_sell(
        &self,
        source_price: f64,
        source_notional: f64,
        source_decision: &SourceSellDecision,
    ) -> Option<f64> {
        if source_price <= 0.0 || source_decision.sell_fraction <= 0.0 {
            return None;
        }

        let estimated_source_notional_before =
            (source_notional / source_decision.sell_fraction).max(source_notional);
        let copy_fraction =
            self.copy_fraction_for_source_notional(estimated_source_notional_before);
        let proportional_target = estimated_source_notional_before * copy_fraction;
        let target = high_price_copy_target(
            proportional_target,
            source_price,
            self.config.high_price_exposure_threshold,
            self.config.high_price_exposure_cap_usd,
        );
        Some(capped_copy_target(target, self.config.copy_target_cap_usd))
    }

    fn with_global_state<T>(
        &self,
        update: impl FnOnce(&mut GlobalAutoCopyState) -> T,
    ) -> Result<T, AutoCopyError> {
        let _lock = GlobalStateLock::acquire(&self.config.global_state_path)?;
        let mut global_state = GlobalAutoCopyState::load(&self.config.global_state_path)?;
        let result = update(&mut global_state);
        global_state.save(&self.config.global_state_path)?;
        Ok(result)
    }

    fn global_snapshot_for_source(
        &self,
        metadata: &GlobalOutcomeMetadata,
        target_usd: f64,
        reservation: Option<GlobalBuyReservation>,
        now: u64,
    ) -> GlobalSourceOutcomeState {
        let filled_position_usd = self
            .state
            .position(&metadata.position_key)
            .map(|position| position.cost_usd.max(0.0))
            .unwrap_or(0.0);
        let pending_buy_orders = self
            .state
            .pending_orders
            .iter()
            .filter(|order| order.side == "BUY" && order.position_key == metadata.position_key)
            .map(|order| {
                let remaining_amount = (order.copy_amount_usd - order.filled_amount_usd).max(0.0);
                GlobalPendingBuyOrder {
                    local_order_id: order.local_order_id.clone(),
                    external_order_id: order.external_order_id.clone(),
                    copy_amount_usd: order.copy_amount_usd,
                    remaining_amount_usd: remaining_amount,
                    limit_price: order.limit_price,
                    created_at_secs: order.created_at_secs,
                    expires_at_secs: order.expires_at_secs,
                    updated_at_secs: order.last_sync_at_secs.max(now),
                }
            })
            .collect::<Vec<_>>();
        let pending_buy_usd = pending_buy_orders
            .iter()
            .map(|order| order.remaining_amount_usd.max(0.0))
            .sum();

        GlobalSourceOutcomeState {
            source_name: self.config.source_name.clone(),
            source_wallet: self.config.source_wallet.clone(),
            position_key: metadata.position_key.clone(),
            market_title: metadata.market_title.clone(),
            outcome: metadata.outcome.clone(),
            asset: metadata.asset.clone(),
            condition_id: metadata.condition_id.clone(),
            target_usd: target_usd.max(0.0),
            filled_position_usd,
            pending_buy_usd,
            pending_buy_orders,
            reservation,
            updated_at_secs: now,
        }
    }

    fn reserve_global_buy(
        &self,
        metadata: &GlobalOutcomeMetadata,
        source_target_usd: f64,
        requested_amount_usd: f64,
        min_copy_amount_usd: f64,
        now: u64,
    ) -> Result<GlobalBuyPlan, AutoCopyError> {
        self.with_global_state(|global_state| {
            global_state.prune_expired_reservations(now);
            let snapshot = self.global_snapshot_for_source(metadata, source_target_usd, None, now);
            let source_committed_usd = snapshot.committed_usd(now);
            global_state.upsert_source_outcome(snapshot);

            let (active_pending_count, active_pending_source) = {
                let active_pending =
                    global_state.active_pending_buy_orders(&metadata.position_key, now);
                (
                    active_pending.len(),
                    active_pending
                        .first()
                        .map(|(source, _)| source.source_name.clone()),
                )
            };
            let global_target_usd = global_state.global_target_usd(
                &metadata.position_key,
                self.config.copy_target_cap_usd,
                now,
            );
            let global_committed_usd =
                global_state.global_committed_usd(&metadata.position_key, now);
            let global_gap_usd = (global_target_usd - global_committed_usd).max(0.0);
            let mut reserved_amount_usd = 0.0;

            if requested_amount_usd > 0.000_001
                && active_pending_count == 0
                && global_gap_usd + 0.000_001 >= min_copy_amount_usd
            {
                let candidate = requested_amount_usd.min(global_gap_usd);
                if candidate + 0.000_001 >= min_copy_amount_usd {
                    reserved_amount_usd = candidate;
                    let reservation = GlobalBuyReservation {
                        amount_usd: reserved_amount_usd,
                        reserved_at_secs: now,
                        expires_at_secs: now + GLOBAL_BUY_RESERVATION_TTL_SECONDS,
                    };
                    let snapshot = self.global_snapshot_for_source(
                        metadata,
                        source_target_usd,
                        Some(reservation),
                        now,
                    );
                    global_state.upsert_source_outcome(snapshot);
                }
            }

            GlobalBuyPlan {
                source_target_usd: source_target_usd.max(0.0),
                source_committed_usd,
                global_target_usd,
                global_committed_usd,
                global_gap_usd,
                requested_amount_usd: requested_amount_usd.max(0.0),
                reserved_amount_usd,
                active_pending_count,
                active_pending_source,
            }
        })
    }

    fn plan_global_sell_with_target(
        &self,
        metadata: &GlobalOutcomeMetadata,
        fallback_source_target_before_usd: Option<f64>,
        target_after: impl FnOnce(f64) -> f64,
        now: u64,
    ) -> Result<GlobalSellPlan, AutoCopyError> {
        self.with_global_state(|global_state| {
            global_state.prune_expired_reservations(now);
            let source_target_before_usd = global_state
                .source_target_usd(&self.config.source_name, &metadata.position_key)
                .unwrap_or_else(|| {
                    let local_exposure = self.state.market_exposure_usd(&metadata.position_key);
                    let fallback = if local_exposure > 0.000_001 {
                        local_exposure
                    } else {
                        fallback_source_target_before_usd.unwrap_or(0.0)
                    };
                    fallback.min(self.config.copy_target_cap_usd)
                });
            let current_snapshot =
                self.global_snapshot_for_source(metadata, source_target_before_usd, None, now);
            global_state.upsert_source_outcome(current_snapshot);
            let global_target_before_usd = global_state.global_target_usd(
                &metadata.position_key,
                self.config.copy_target_cap_usd,
                now,
            );

            let source_target_after_usd = target_after(source_target_before_usd)
                .max(0.0)
                .min(self.config.copy_target_cap_usd);
            let updated_snapshot =
                self.global_snapshot_for_source(metadata, source_target_after_usd, None, now);
            global_state.upsert_source_outcome(updated_snapshot);

            let global_target_after_usd = global_state.global_target_usd(
                &metadata.position_key,
                self.config.copy_target_cap_usd,
                now,
            );
            let recorded_global_committed_usd =
                global_state.global_committed_usd(&metadata.position_key, now);
            let global_committed_usd = recorded_global_committed_usd.max(global_target_before_usd);
            let excess_usd = (global_committed_usd - global_target_after_usd).max(0.0);
            let sell_fraction = if global_committed_usd > 0.0 {
                (excess_usd / global_committed_usd).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let clear_all = sell_fraction >= 0.999_999 && global_target_after_usd <= 0.000_001;
            let supporting_sources = global_state
                .source_outcomes
                .iter()
                .filter(|outcome| {
                    outcome.position_key == metadata.position_key
                        && outcome.is_active(now)
                        && outcome.source_name != self.config.source_name
                        && outcome.target_usd > 0.000_001
                })
                .map(|outcome| GlobalSourceSupport {
                    source_name: outcome.source_name.clone(),
                    target_usd: outcome.target_usd,
                })
                .collect::<Vec<_>>();

            GlobalSellPlan {
                source_target_before_usd,
                source_target_after_usd,
                global_target_before_usd,
                global_target_after_usd,
                global_committed_usd,
                excess_usd,
                sell_fraction,
                clear_all,
                supporting_sources,
            }
        })
    }

    fn plan_global_sell_to_target(
        &self,
        metadata: &GlobalOutcomeMetadata,
        source_target_after_usd: f64,
        now: u64,
    ) -> Result<GlobalSellPlan, AutoCopyError> {
        self.plan_global_sell_with_target(metadata, None, |_| source_target_after_usd, now)
    }

    fn plan_global_sell_for_source_decision(
        &self,
        metadata: &GlobalOutcomeMetadata,
        source_decision: &SourceSellDecision,
        fallback_source_target_before_usd: Option<f64>,
        now: u64,
    ) -> Result<GlobalSellPlan, AutoCopyError> {
        self.plan_global_sell_with_target(
            metadata,
            fallback_source_target_before_usd,
            |source_target_before_usd| {
                if source_decision.clear_all {
                    0.0
                } else {
                    source_target_before_usd * (1.0 - source_decision.sell_fraction).clamp(0.0, 1.0)
                }
            },
            now,
        )
    }

    fn sync_global_source_exposure(
        &self,
        metadata: &GlobalOutcomeMetadata,
        target_usd: Option<f64>,
        now: u64,
    ) -> Result<(), AutoCopyError> {
        self.with_global_state(|global_state| {
            global_state.prune_expired_reservations(now);
            let target_usd = target_usd
                .or_else(|| {
                    global_state.source_target_usd(&self.config.source_name, &metadata.position_key)
                })
                .unwrap_or_else(|| {
                    self.state
                        .market_exposure_usd(&metadata.position_key)
                        .min(self.config.copy_target_cap_usd)
                });
            let snapshot = self.global_snapshot_for_source(metadata, target_usd, None, now);
            global_state.upsert_source_outcome(snapshot);
        })
    }

    fn global_metadata_for_key(&self, key: &str) -> Option<GlobalOutcomeMetadata> {
        self.state
            .position(key)
            .map(GlobalOutcomeMetadata::from_position)
            .or_else(|| {
                self.state
                    .pending_orders
                    .iter()
                    .find(|order| order.position_key == key)
                    .map(GlobalOutcomeMetadata::from_order)
            })
            .or_else(|| {
                self.state
                    .source_outcomes
                    .iter()
                    .find(|outcome| outcome.position_key == key)
                    .map(GlobalOutcomeMetadata::from_source_metadata)
            })
    }

    fn sync_global_source_exposure_for_key(
        &self,
        key: &str,
        target_usd: Option<f64>,
        now: u64,
    ) -> Result<(), AutoCopyError> {
        let Some(metadata) = self.global_metadata_for_key(key) else {
            return Ok(());
        };
        self.sync_global_source_exposure(&metadata, target_usd, now)
    }

    fn refresh_global_local_activity(&self, now: u64) -> Result<(), AutoCopyError> {
        let mut metadata_by_key = HashMap::<String, GlobalOutcomeMetadata>::new();
        for position in &self.state.positions {
            if position.size_shares > 0.0 || position.cost_usd > 0.0 {
                metadata_by_key.insert(
                    position.position_key.clone(),
                    GlobalOutcomeMetadata::from_position(position),
                );
            }
        }
        for order in &self.state.pending_orders {
            if order.side == "BUY" {
                metadata_by_key
                    .entry(order.position_key.clone())
                    .or_insert_with(|| GlobalOutcomeMetadata::from_order(order));
            }
        }

        if metadata_by_key.is_empty() {
            return Ok(());
        }

        self.with_global_state(|global_state| {
            global_state.prune_expired_reservations(now);
            let needs_refresh = metadata_by_key.values().any(|metadata| {
                global_state
                    .source_outcomes
                    .iter()
                    .find(|outcome| {
                        outcome.source_name == self.config.source_name
                            && outcome.position_key == metadata.position_key
                    })
                    .map(|outcome| {
                        now.saturating_sub(outcome.updated_at_secs) >= GLOBAL_LOCAL_REFRESH_SECONDS
                    })
                    .unwrap_or(true)
            });
            if !needs_refresh {
                return;
            }

            for metadata in metadata_by_key.values() {
                let target_usd = global_state
                    .source_target_usd(&self.config.source_name, &metadata.position_key)
                    .unwrap_or_else(|| {
                        self.state
                            .market_exposure_usd(&metadata.position_key)
                            .min(self.config.copy_target_cap_usd)
                    });
                let snapshot = self.global_snapshot_for_source(metadata, target_usd, None, now);
                global_state.upsert_source_outcome(snapshot);
            }
        })
    }

    fn should_handle(&self, employee: &WatchedEmployee, trade: &UserTrade) -> bool {
        if !self.config.enabled {
            return false;
        }

        if !same_wallet(&employee.wallet, &self.config.source_wallet) {
            return false;
        }

        if !employee.domain.eq_ignore_ascii_case(&self.config.domain) {
            return false;
        }

        if self.is_position_blocked(&position_key(trade)) {
            return false;
        }

        matches_employee_keywords(employee, trade)
    }

    fn is_position_blocked(&self, key: &str) -> bool {
        self.config
            .blocked_position_keys
            .iter()
            .any(|blocked| blocked.eq_ignore_ascii_case(key))
    }

    fn handle_buy(&mut self, trade: &UserTrade, now: u64, event_time: u64) -> Vec<AutoCopyReport> {
        let Some(source_price) = trade.price.filter(|price| *price > 0.0 && *price <= 1.0) else {
            return vec![self.skip_report(
                "缺少价格",
                trade,
                format!("{} BUY 缺少有效成交价格。", self.config.source_name),
            )];
        };
        let Some(source_size) = trade.size.filter(|size| *size > 0.0) else {
            return vec![self.skip_report(
                "缺少数量",
                trade,
                format!("{} BUY 缺少有效成交数量。", self.config.source_name),
            )];
        };
        self.state.clear_source_sell_coverage(&trade.asset);

        let source_notional = source_price * source_size;
        if should_skip_small_buy(source_notional, self.config.min_buy_source_notional_usd) {
            return vec![self.skip_report(
                "买入金额过小",
                trade,
                format!(
                    "{} BUY 金额 {:.4}U，低于自动跟单阈值 {:.4}U；当前配置一般应为 0，只有手动打开该阈值时才跳过。",
                    self.config.source_name,
                    source_notional,
                    self.config.min_buy_source_notional_usd
                ),
            )];
        }

        if should_skip_near_zero_buy(source_price, self.config.skip_buy_price_at_or_below) {
            return vec![self.skip_report(
                "近零低价尾部",
                trade,
                format!(
                    "{} BUY 价格 {:.2}c，达到/低于低价跳过线 {:.2}c；这种近零赔率单可能是极小概率尾部或挂单噪音，不适合自动跟。",
                    self.config.source_name,
                    source_price * 100.0,
                    self.config.skip_buy_price_at_or_below * 100.0
                ),
            )];
        }

        if should_skip_low_edge_buy(source_price, self.config.skip_buy_price_at_or_above) {
            return vec![self.skip_report(
                "高概率低收益",
                trade,
                format!(
                    "{} BUY 价格 {:.2}c，超过高价跳过线 {:.2}c；98c 以上高概率单胜率高但收益空间极薄，不适合自动跟。",
                    self.config.source_name,
                    source_price * 100.0,
                    self.config.skip_buy_price_at_or_above * 100.0
                ),
            )];
        }

        let key = position_key(trade);
        if let Some((title, reason)) =
            self.source_buy_guard_reason(&key, source_notional, event_time)
        {
            return vec![self.skip_report(title, trade, reason)];
        }

        let effective_max_chase_pct = self.effective_buy_chase_pct(source_price);
        let normal_direct_limit_price =
            price_with_pct_upside(source_price, effective_max_chase_pct);
        let normal_passive_limit_price = price_with_capped_upside(
            source_price,
            self.config.passive_offset_pct,
            self.config.passive_offset,
        )
        .min(normal_direct_limit_price);
        let high_price_reference = source_price.max(normal_passive_limit_price);
        let direct_limit_price = high_price_guarded_direct_limit(
            source_price,
            normal_direct_limit_price,
            self.config.high_price_exposure_threshold,
            self.config.high_price_max_chase_pct,
        );
        let passive_limit_price = high_price_guarded_passive_limit(
            source_price,
            normal_passive_limit_price,
            direct_limit_price,
            self.config.high_price_exposure_threshold,
        );
        let (mut reports, reprice_failed) =
            self.reprice_pending_buys_for_source_buy(&key, source_price, passive_limit_price, now);
        if reprice_failed {
            return reports;
        }

        let source_buy_target = self.state.record_source_buy_target(
            &key,
            &trade.asset,
            source_notional,
            source_size,
            event_time,
        );
        let sizing = self.buy_sizing_decision(
            trade,
            source_price,
            &source_buy_target,
            high_price_reference,
        );
        let mut target_copy_amount = sizing.target_copy_amount_usd;
        let mut target_cap_note = sizing.target_cap_note.clone();
        if self.config.strategy.enabled && target_copy_amount <= 0.000_001 {
            reports.push(self.skip_report(
                "策略价格档跳过",
                trade,
                format!(
                    "{} BUY @ {:.2}c，{}；本次不买。{}",
                    self.config.source_name,
                    source_price * 100.0,
                    sizing.target_description,
                    target_cap_note
                ),
            ));
            return reports;
        }

        let min_copy_amount =
            MIN_STARTER_COPY_USD.max(MIN_CLOB_ORDER_SIZE_SHARES * passive_limit_price);
        if self.config.strategy.enabled
            && target_copy_amount > 0.000_001
            && target_copy_amount + 0.000_001 < min_copy_amount
        {
            target_cap_note.push_str(&format!(
                "；交易所最低订单约束：策略目标 {:.4}U 低于最低 {:.4}U（{:.0}份 × {:.2}c），先抬到最低可下单金额",
                target_copy_amount,
                min_copy_amount,
                MIN_CLOB_ORDER_SIZE_SHARES,
                passive_limit_price * 100.0
            ));
            target_copy_amount = min_copy_amount;
        }

        let market_exposure = self.state.market_exposure_usd(&key);
        if let Some(event_remaining) = sizing.event_remaining_usd {
            let event_limited_target = (market_exposure + event_remaining.max(0.0))
                .min(target_copy_amount)
                .max(market_exposure.min(target_copy_amount));
            if event_limited_target + 0.000_001 < target_copy_amount {
                target_cap_note.push_str(&format!(
                    "；事件预算风控：事件预算 {:.2}U，已承诺 {:.2}U，剩余 {:.4}U，本 outcome 目标从 {:.4}U 裁剪到 {:.4}U",
                    sizing.event_budget_usd.unwrap_or(0.0),
                    sizing.event_committed_usd.unwrap_or(0.0),
                    event_remaining,
                    target_copy_amount,
                    event_limited_target
                ));
                target_copy_amount = event_limited_target;
            }
        }
        let committed_size = self.state.committed_buy_shares(&key);
        let target_size = if passive_limit_price > 0.0 {
            target_copy_amount / passive_limit_price
        } else {
            0.0
        };
        let daily_reserved = self.state.daily_reserved_buy_usd();
        let remaining_market = self.config.max_market_exposure_usd - market_exposure;
        let remaining_daily =
            self.config.max_daily_spend_usd - self.state.daily_spend_usd - daily_reserved;
        let local_target_gap = (target_copy_amount - market_exposure).max(0.0);
        let requested_copy_amount = local_target_gap
            .min(self.config.max_single_copy_usd)
            .min(remaining_market)
            .min(remaining_daily);
        let metadata = GlobalOutcomeMetadata::from_trade(trade);

        if self.state.daily_loss_usd() >= self.config.max_daily_loss_usd {
            if let Err(error) =
                self.sync_global_source_exposure(&metadata, Some(target_copy_amount), now)
            {
                reports.push(AutoCopyReport::system(format!(
                    "[{} 全局跟单状态刷新失败]\n市场: {}\n方向: {}\n原因: {error}",
                    self.config.source_name,
                    trade.title.as_deref().unwrap_or("-"),
                    trade.outcome.as_deref().unwrap_or("-")
                )));
            }
            reports.push(self.skip_report(
                "今日亏损上限",
                trade,
                format!(
                    "今日已实现亏损 ${:.2}，达到上限 ${:.2}。",
                    self.state.daily_loss_usd(),
                    self.config.max_daily_loss_usd
                ),
            ));
            return reports;
        }

        let global_plan = match self.reserve_global_buy(
            &metadata,
            target_copy_amount,
            requested_copy_amount,
            min_copy_amount,
            now,
        ) {
            Ok(plan) => plan,
            Err(error) => {
                reports.push(self.skip_report(
                    "全局协调不可用",
                    trade,
                    format!(
                        "全局同 outcome 跟单协调状态不可用，为避免多员工重复买入，本次不下单；原因: {error}"
                    ),
                ));
                return reports;
            }
        };
        let copy_amount = global_plan.reserved_amount_usd;
        let copy_size = if passive_limit_price > 0.0 {
            copy_amount.max(0.0) / passive_limit_price
        } else {
            0.0
        };

        if local_target_gap <= 0.000_001 {
            reports.push(self.skip_report(
                "累计目标已覆盖",
                trade,
                format!(
                    "同 outcome 本轮累计 BUY {:.4}U / {:.4}份，{}，最终目标 {:.4}U / {:.4}份；本员工视角我方已成交与挂单合计承诺 {:.4}U / {:.4}份，无需重复加挂{}。全局目标 {:.4}U，已占用 {:.4}U。",
                    source_buy_target.source_buy_notional_usd,
                    source_buy_target.source_buy_size_shares,
                    sizing.target_description,
                    target_copy_amount,
                    target_size,
                    market_exposure,
                    committed_size,
                    target_cap_note,
                    global_plan.global_target_usd,
                    global_plan.global_committed_usd
                ),
            ));
            return reports;
        }

        if global_plan.blocked_by_pending() {
            reports.push(self.skip_report(
                "全局已有挂单",
                trade,
                format!(
                    "同 outcome 当前已有 {} 笔全局 BUY 挂单{}，阶段 1 先保持单 outcome 只允许一个未成交 BUY；本员工目标 {:.4}U，已承诺 {:.4}U，本地缺口 {:.4}U；全局目标 {:.4}U，已占用 {:.4}U，暂不叠加新挂单。",
                    global_plan.active_pending_count,
                    global_plan
                        .active_pending_source
                        .as_deref()
                        .map(|source| format!("（来自 {source}）"))
                        .unwrap_or_default(),
                    global_plan.source_target_usd,
                    global_plan.source_committed_usd,
                    local_target_gap,
                    global_plan.global_target_usd,
                    global_plan.global_committed_usd
                ),
            ));
            return reports;
        }

        if global_plan.global_gap_usd <= 0.000_001 {
            reports.push(self.skip_report(
                "全局目标已覆盖",
                trade,
                format!(
                    "同 outcome 多员工合计目标 {:.4}U，当前全局已成交/挂单/预留 {:.4}U，已达到 {:.2}U 全局上限或合计目标；本次不再加仓。",
                    global_plan.global_target_usd,
                    global_plan.global_committed_usd,
                    self.config.copy_target_cap_usd
                ),
            ));
            return reports;
        }

        if copy_size + 0.000_001 < MIN_CLOB_ORDER_SIZE_SHARES
            || copy_amount + 0.000_001 < MIN_STARTER_COPY_USD
        {
            reports.push(self.skip_report(
                "累计等待最小订单",
                trade,
                format!(
                    "同 outcome 本轮累计 BUY {:.4}U / {:.4}份，{}，最终目标 {:.4}U / {:.4}份；本员工已承诺 {:.4}U / {:.4}份，当前缺口 {:.4}U；全局目标 {:.4}U，已占用 {:.4}U，全局当前可用缺口 {:.4}U / {:.4}份，尚未满足交易所最低 {:.0}份 / 1.00U 约束，先累计后续 BUY。",
                    source_buy_target.source_buy_notional_usd,
                    source_buy_target.source_buy_size_shares,
                    sizing.target_description,
                    target_copy_amount,
                    target_size,
                    market_exposure,
                    committed_size,
                    local_target_gap,
                    global_plan.global_target_usd,
                    global_plan.global_committed_usd,
                    global_plan.global_gap_usd.min(requested_copy_amount),
                    copy_size,
                    MIN_CLOB_ORDER_SIZE_SHARES
                ),
            ));
            return reports;
        }

        if copy_amount <= 0.0 {
            if self.config.strategy.enabled
                && sizing
                    .event_remaining_usd
                    .is_some_and(|remaining| remaining <= 0.000_001)
            {
                reports.push(self.skip_report(
                    "事件预算已满",
                    trade,
                    format!(
                        "同 event 预算已用完：事件预算 {:.2}U，已承诺 {:.2}U，剩余 {:.4}U；{}，本次不继续加仓。{}",
                        sizing.event_budget_usd.unwrap_or(0.0),
                        sizing.event_committed_usd.unwrap_or(0.0),
                        sizing.event_remaining_usd.unwrap_or(0.0),
                        sizing.target_description,
                        target_cap_note
                    ),
                ));
                return reports;
            }
            reports.push(self.skip_report(
                "额度不足",
                trade,
                format!(
                    "累计目标缺口 ${:.8}，但该市场/今日剩余额度只剩 ${:.2}/${:.2}。",
                    local_target_gap.min(self.config.max_single_copy_usd),
                    remaining_market.max(0.0),
                    remaining_daily.max(0.0)
                ),
            ));
            return reports;
        }
        let sizing_reason = format!(
            "本笔 {:.4}U / {:.4}份；同 outcome 本轮累计 BUY {:.4}U / {:.4}份，{}，最终目标 {:.4}U / {:.4}份；本员工此前已承诺 {:.4}U / {:.4}份，本地缺口 {:.4}U；多员工全局目标 {:.4}U / 上限 {:.2}U，已成交+挂单+预留 {:.4}U，全局剩余 {:.4}U；本次补足 {:.4}U / {:.4}份{}",
            source_notional,
            source_size,
            source_buy_target.source_buy_notional_usd,
            source_buy_target.source_buy_size_shares,
            sizing.target_description,
            target_copy_amount,
            target_size,
            market_exposure,
            committed_size,
            local_target_gap,
            global_plan.global_target_usd,
            self.config.copy_target_cap_usd,
            global_plan.global_committed_usd,
            global_plan.global_gap_usd,
            copy_amount,
            copy_size,
            target_cap_note
        );
        let request = AutoCopyExecutionRequest::buy(
            self.config.mode,
            self.config.source_name.clone(),
            trade,
            copy_amount,
            direct_limit_price,
            passive_limit_price,
            self.config.buy_take_enabled,
            self.config.passive_order_ttl_seconds,
        );
        let execution = self.execute_request(&request);
        let mut report = self.report_from_execution(
            "BUY",
            trade,
            source_notional,
            copy_amount,
            direct_limit_price,
            Some(passive_limit_price),
            Some(self.config.buy_take_enabled),
            &execution,
            Some(&sizing_reason),
        );

        self.apply_buy_execution(
            &key,
            trade,
            copy_amount,
            passive_limit_price,
            now,
            &execution,
        );
        if let Err(error) =
            self.sync_global_source_exposure(&metadata, Some(target_copy_amount), now)
        {
            reports.push(AutoCopyReport::system(format!(
                "[{} 全局跟单状态刷新失败]\n市场: {}\n方向: {}\n原因: {error}",
                self.config.source_name,
                trade.title.as_deref().unwrap_or("-"),
                trade.outcome.as_deref().unwrap_or("-")
            )));
        }
        report.market_exposure_after_usd = self.state.market_exposure_usd(&key);
        report.daily_spend_after_usd =
            self.state.daily_spend_usd + self.state.daily_reserved_buy_usd();
        reports.push(report);
        reports
    }

    fn reprice_pending_buys_for_source_buy(
        &mut self,
        key: &str,
        source_price: f64,
        passive_limit_price: f64,
        now: u64,
    ) -> (Vec<AutoCopyReport>, bool) {
        let stale_orders = self
            .state
            .pending_orders
            .iter()
            .filter(|order| {
                order.side == "BUY"
                    && order.position_key == key
                    && passive_limit_price
                        > order.requested_limit_price.unwrap_or(order.limit_price) + 0.01
            })
            .cloned()
            .collect::<Vec<_>>();
        let mut reports = Vec::new();
        let mut failed = false;

        for order in stale_orders {
            let reason = format!(
                "{} 新增 BUY @ {:.2}c，原挂单 {:.2}c 已明显低于最新补仓基准；先撤单，再按本次补仓价重算目标。",
                self.config.source_name,
                source_price * 100.0,
                order.requested_limit_price.unwrap_or(order.limit_price) * 100.0
            );
            let cancel_reports = self.cancel_pending_order(&order, &reason, now);
            failed |= cancel_reports
                .iter()
                .any(|report| report.status.eq_ignore_ascii_case("failed"));
            reports.extend(cancel_reports);
        }

        (reports, failed)
    }

    fn handle_sell(
        &mut self,
        trade: &UserTrade,
        now: u64,
        source_positions: Option<&HashMap<String, ObservedSourcePosition>>,
        source_ledger_before: Option<&SourcePositionLedger>,
    ) -> Vec<AutoCopyReport> {
        let key = position_key(trade);
        let Some(source_price) = trade.price.filter(|price| *price > 0.0 && *price <= 1.0) else {
            return vec![self.skip_report(
                "缺少价格",
                trade,
                format!(
                    "{} SELL 缺少有效成交价格；不取消挂单、不卖仓位。",
                    self.config.source_name
                ),
            )];
        };
        let source_notional = trade
            .size
            .filter(|size| *size > 0.0)
            .map(|size| source_price * size)
            .unwrap_or(0.0);
        let source_sell_size = trade.size.unwrap_or(0.0);
        let source_decision = self.source_sell_decision(
            &trade.asset,
            source_sell_size,
            source_positions,
            source_ledger_before,
            now,
        );
        self.state.clear_source_buy_target(&key);
        self.update_source_sell_guard(&key, source_notional, trade.timestamp.unwrap_or(now));

        let mut reports = self.cancel_pending_for_key(
            &key,
            &format!("{} 已卖出/减仓", self.config.source_name),
            now,
        );
        let Some(source_decision) = source_decision else {
            reports.push(self.skip_report(
                "源实际仓位不可用",
                trade,
                format!(
                    "已立即取消同 outcome 未成交买单，但本次无法取得 {} 当前或最近实际持仓，不能可靠计算减仓比例；不盲目清仓。",
                    self.config.source_name
                ),
            ));
            return reports;
        };
        if source_decision.sell_fraction <= 0.0 {
            reports.push(self.skip_report("源实际仓位未减少", trade, source_decision.reason));
            return reports;
        }

        let metadata = GlobalOutcomeMetadata::from_trade(trade);
        let fallback_source_target_before_usd = self.fallback_source_target_before_sell(
            source_price,
            source_notional,
            &source_decision,
        );
        let global_sell_plan = match self.plan_global_sell_for_source_decision(
            &metadata,
            &source_decision,
            fallback_source_target_before_usd,
            now,
        ) {
            Ok(plan) => plan,
            Err(error) => {
                reports.push(self.skip_report(
                    "全局卖出协调不可用",
                    trade,
                    format!(
                        "全局同 outcome 卖出协调状态不可用，为避免误伤其他员工仍支持的仓位，本次不卖；原因: {error}"
                    ),
                ));
                return reports;
            }
        };

        if !global_sell_plan.should_sell() {
            self.state.clear_exit_retry(&key);
            reports.push(self.skip_report(
                "全局目标仍覆盖",
                trade,
                format!(
                    "{}；{} 当前不需要真实卖出。{}",
                    source_decision.reason,
                    global_sell_plan.coordination_reason(),
                    global_sell_plan.supporting_sources_label()
                ),
            ));
            return reports;
        }

        let Some(position) = self
            .state
            .position(&key)
            .filter(|position| position.size_shares > 0.0)
            .cloned()
        else {
            reports.push(self.sell_untracked_actual_position(
                trade,
                source_notional,
                &source_decision,
                &global_sell_plan,
                now,
            ));
            return reports;
        };

        let failure_key = action_failure_cooldown_key("SELL", &key);
        // A newly observed source SELL is fresh risk information. Never let an
        // earlier execution failure suppress it; the new action supersedes the
        // old cooldown and gets its own immediate attempt.
        self.state.clear_failure(&failure_key);

        let sell_fraction = global_sell_plan.sell_fraction;
        let clear_all = global_sell_plan.clear_all;
        self.state.clear_exit_retry(&key);
        let lock_profit = source_price >= LOCK_PROFIT_SOURCE_PRICE;
        let passive_sell_price = self
            .should_use_passive_small_sell(&source_decision, &global_sell_plan)
            .then(|| self.passive_small_sell_limit_price(source_price, lock_profit));
        let min_sell_price = if passive_sell_price.is_some() {
            None
        } else if lock_profit {
            Some(LOCK_PROFIT_MIN_SELL_PRICE)
        } else if clear_all {
            None
        } else {
            Some(price_with_capped_downside(
                source_price,
                self.config.max_chase_pct,
                self.config.max_chase_delta,
            ))
        };
        let sizing_reason = format!(
            "{}；{}{}",
            source_decision.reason,
            global_sell_plan.coordination_reason(),
            passive_sell_price
                .map(|price| format!(
                    "；小比例试探性 SELL：不使用 FAK/市价退出，按员工卖价下浮 {:.1}% 在 {:.2}c 挂 GTC 限价卖单，同步卖出同等比例份额",
                    self.config.small_sell_passive_discount_pct * 100.0,
                    price * 100.0
                ))
                .unwrap_or_default()
        );
        let request = AutoCopyExecutionRequest::sell(
            self.config.mode,
            self.config.source_name.clone(),
            trade,
            sell_fraction,
            min_sell_price,
            passive_sell_price,
            clear_all,
            lock_profit,
            self.config.passive_order_ttl_seconds,
        );
        let execution = self.execute_request(&request);
        if clear_all && should_silently_finish_dust_exit(&execution) {
            self.state.sync_position_actual_balance(&key, 0.0);
            self.state.clear_exit_retry(&key);
            self.state.clear_failure(&failure_key);
            let _ = self.sync_global_source_exposure_for_key(&key, None, now);
            return reports;
        }
        let mut report = self.report_from_execution(
            "SELL",
            trade,
            source_notional,
            global_sell_plan.excess_usd,
            source_price,
            None,
            None,
            &execution,
            Some(&sizing_reason),
        );

        self.apply_sell_execution(
            &key,
            execution
                .target_size_shares
                .unwrap_or(position.size_shares * sell_fraction),
            source_price,
            &execution,
            &mut report,
        );
        self.apply_pending_sell_submission(
            &key,
            trade,
            execution
                .target_size_shares
                .unwrap_or(position.size_shares * sell_fraction),
            passive_sell_price
                .or(min_sell_price)
                .unwrap_or(source_price),
            now,
            &execution,
            &mut report,
        );
        if let Some(actual_balance) = execution.actual_balance_shares {
            self.state
                .sync_position_actual_balance(&key, actual_balance);
        }
        let _ = self.sync_global_source_exposure(
            &metadata,
            Some(global_sell_plan.source_target_after_usd),
            now,
        );
        let requested_target_size = execution
            .target_size_shares
            .unwrap_or(position.size_shares * sell_fraction);
        let remaining_target_size = remaining_exit_target_size(&execution);
        if execution.status == ExecutionStatus::Failed {
            self.state.record_exit_retry(PendingExitRetry {
                position_key: key.clone(),
                sell_fraction,
                target_size_shares: Some(requested_target_size),
                min_sell_price,
                force_market_sell: clear_all,
                lock_profit,
            });
            self.state.record_failure(
                failure_key,
                execution
                    .message
                    .clone()
                    .unwrap_or_else(|| report.reason.clone()),
                now,
            );
        } else if should_accumulate_small_exit(&execution) {
            self.state.record_exit_retry(PendingExitRetry {
                position_key: key.clone(),
                sell_fraction,
                target_size_shares: Some(requested_target_size),
                min_sell_price,
                force_market_sell: false,
                lock_profit,
            });
            self.state.clear_failure(&failure_key);
        } else if matches!(
            execution.status,
            ExecutionStatus::Pending | ExecutionStatus::Submitted
        ) {
            self.state.clear_exit_retry(&key);
            self.state.clear_failure(&failure_key);
        } else if remaining_target_size >= MIN_CLOB_ORDER_SIZE_SHARES {
            self.state.record_exit_retry(PendingExitRetry {
                position_key: key.clone(),
                sell_fraction,
                target_size_shares: Some(remaining_target_size),
                min_sell_price,
                force_market_sell: clear_all,
                lock_profit,
            });
            self.state.record_failure(
                failure_key,
                format!(
                    "partial FAK sell remaining {:.6} shares after source SELL",
                    remaining_target_size
                ),
                now,
            );
        } else if matches!(
            execution.status,
            ExecutionStatus::Filled
                | ExecutionStatus::DryRun
                | ExecutionStatus::Cancelled
                | ExecutionStatus::Skipped
        ) {
            self.state.clear_exit_retry(&key);
            self.state.clear_failure(&failure_key);
        }
        report.market_exposure_after_usd = self.state.market_exposure_usd(&key);
        report.daily_spend_after_usd =
            self.state.daily_spend_usd + self.state.daily_reserved_buy_usd();
        reports.push(report);
        reports
    }

    fn sell_untracked_actual_position(
        &mut self,
        trade: &UserTrade,
        source_notional: f64,
        source_decision: &SourceSellDecision,
        global_sell_plan: &GlobalSellPlan,
        now: u64,
    ) -> AutoCopyReport {
        let key = position_key(trade);
        let source_price = trade.price.unwrap_or(0.0);
        let clear_all = global_sell_plan.clear_all;
        let sell_fraction = global_sell_plan.sell_fraction;
        let lock_profit = source_price >= LOCK_PROFIT_SOURCE_PRICE;
        let passive_sell_price = self
            .should_use_passive_small_sell(source_decision, global_sell_plan)
            .then(|| self.passive_small_sell_limit_price(source_price, lock_profit));
        let min_sell_price = if passive_sell_price.is_some() {
            None
        } else if lock_profit {
            Some(LOCK_PROFIT_MIN_SELL_PRICE)
        } else if clear_all {
            None
        } else {
            Some(price_with_capped_downside(
                source_price,
                self.config.max_chase_pct,
                self.config.max_chase_delta,
            ))
        };
        let request = AutoCopyExecutionRequest::sell(
            self.config.mode,
            self.config.source_name.clone(),
            trade,
            sell_fraction,
            min_sell_price,
            passive_sell_price,
            clear_all,
            lock_profit,
            self.config.passive_order_ttl_seconds,
        );
        let execution = self.execute_request(&request);
        let observed_before = execution.actual_balance_shares.unwrap_or(0.0).max(0.0)
            + execution.filled_size.unwrap_or(0.0).max(0.0);
        let retryable_without_balance = execution.status == ExecutionStatus::Failed
            && is_retryable_exit_error(execution.message.as_deref());
        let tracked_size = if observed_before > MIN_TRACKED_ACTUAL_BALANCE_SHARES {
            observed_before
        } else if retryable_without_balance && clear_all {
            MIN_CLOB_ORDER_SIZE_SHARES
        } else {
            0.0
        };

        if tracked_size > 0.0 {
            if let Some(position) = self.state.position_mut(&key) {
                position.market_title = trade.title.clone();
                position.outcome = trade.outcome.clone();
                position.asset.clone_from(&trade.asset);
                position.condition_id.clone_from(&trade.condition_id);
                position.size_shares = tracked_size;
                position.cost_usd = tracked_size * source_price;
                position.updated_at_secs = now;
            } else {
                self.state.positions.push(CopyPosition {
                    position_key: key.clone(),
                    market_title: trade.title.clone(),
                    outcome: trade.outcome.clone(),
                    asset: trade.asset.clone(),
                    condition_id: trade.condition_id.clone(),
                    size_shares: tracked_size,
                    cost_usd: tracked_size * source_price,
                    realized_pnl_usd: 0.0,
                    updated_at_secs: now,
                });
            }
        }

        let target_size = execution
            .target_size_shares
            .unwrap_or(tracked_size * sell_fraction);
        let recovery_note = if retryable_without_balance && observed_before <= 0.0 {
            if clear_all {
                "本地 state 未记录该 outcome 仓位，真实 CLOB 余额查询遇到网络错误；源仓位已确认归零，保留清仓重试。".to_owned()
            } else {
                "本地 state 未记录该 outcome 仓位，真实 CLOB 余额查询遇到网络错误；本次未取得实际余额，不创建猜测性的全仓重试。".to_owned()
            }
        } else {
            format!(
                "本地 state 未记录该 outcome 仓位；先查询真实 CLOB token balance，再严格按 {} 本次减仓比例同步，不因本地缺记录而清仓。",
                self.config.source_name
            )
        };
        let recovery_reason = format!(
            "{} {}；{}{}",
            source_decision.reason,
            recovery_note,
            global_sell_plan.coordination_reason(),
            passive_sell_price
                .map(|price| format!(
                    "；小比例试探性 SELL：不使用 FAK/市价退出，按员工卖价下浮 {:.1}% 在 {:.2}c 挂 GTC 限价卖单，同步卖出同等比例份额",
                    self.config.small_sell_passive_discount_pct * 100.0,
                    price * 100.0
                ))
                .unwrap_or_default()
        );
        let mut report = self.report_from_execution(
            "SELL",
            trade,
            source_notional,
            global_sell_plan.excess_usd,
            source_price,
            None,
            None,
            &execution,
            Some(&recovery_reason),
        );

        if tracked_size > 0.0 {
            self.apply_sell_execution(&key, target_size, source_price, &execution, &mut report);
            self.apply_pending_sell_submission(
                &key,
                trade,
                target_size,
                passive_sell_price
                    .or(min_sell_price)
                    .unwrap_or(source_price),
                now,
                &execution,
                &mut report,
            );
            if let Some(actual_balance) = execution.actual_balance_shares {
                self.state
                    .sync_position_actual_balance(&key, actual_balance);
            }
        }
        let metadata = GlobalOutcomeMetadata::from_trade(trade);
        let _ = self.sync_global_source_exposure(
            &metadata,
            Some(global_sell_plan.source_target_after_usd),
            now,
        );

        let failure_key = action_failure_cooldown_key("SELL", &key);
        let remaining_target_size = remaining_exit_target_size(&execution);
        if clear_all && should_silently_finish_dust_exit(&execution) {
            self.state.sync_position_actual_balance(&key, 0.0);
            self.state.clear_exit_retry(&key);
            self.state.clear_failure(&failure_key);
            let _ = self.sync_global_source_exposure(
                &metadata,
                Some(global_sell_plan.source_target_after_usd),
                now,
            );
            report.market_exposure_after_usd = self.state.market_exposure_usd(&key);
            report.daily_spend_after_usd =
                self.state.daily_spend_usd + self.state.daily_reserved_buy_usd();
            return report;
        }
        if execution.status == ExecutionStatus::Failed {
            if tracked_size > 0.0 {
                self.state.record_exit_retry(PendingExitRetry {
                    position_key: key.clone(),
                    sell_fraction,
                    target_size_shares: Some(target_size),
                    min_sell_price,
                    force_market_sell: clear_all,
                    lock_profit,
                });
            }
            self.state.record_failure(
                failure_key,
                execution
                    .message
                    .clone()
                    .unwrap_or_else(|| report.reason.clone()),
                now,
            );
        } else if should_accumulate_small_exit(&execution) {
            self.state.record_exit_retry(PendingExitRetry {
                position_key: key.clone(),
                sell_fraction,
                target_size_shares: Some(target_size),
                min_sell_price,
                force_market_sell: false,
                lock_profit,
            });
            self.state.clear_failure(&failure_key);
        } else if matches!(
            execution.status,
            ExecutionStatus::Pending | ExecutionStatus::Submitted
        ) {
            self.state.clear_exit_retry(&key);
            self.state.clear_failure(&failure_key);
        } else if remaining_target_size >= MIN_CLOB_ORDER_SIZE_SHARES {
            self.state.record_exit_retry(PendingExitRetry {
                position_key: key.clone(),
                sell_fraction,
                target_size_shares: Some(remaining_target_size),
                min_sell_price,
                force_market_sell: clear_all,
                lock_profit,
            });
            self.state.record_failure(
                failure_key,
                format!(
                    "untracked proportional exit remaining {:.6} shares after source SELL",
                    remaining_target_size
                ),
                now,
            );
        } else {
            self.state.clear_exit_retry(&key);
            self.state.clear_failure(&failure_key);
        }

        report.market_exposure_after_usd = self.state.market_exposure_usd(&key);
        report.daily_spend_after_usd =
            self.state.daily_spend_usd + self.state.daily_reserved_buy_usd();
        report
    }

    fn source_sell_decision(
        &mut self,
        asset: &str,
        source_sell_size: f64,
        source_positions: Option<&HashMap<String, ObservedSourcePosition>>,
        source_ledger_before: Option<&SourcePositionLedger>,
        now: u64,
    ) -> Option<SourceSellDecision> {
        if source_sell_size <= 0.0 {
            return None;
        }

        let previous_size = self.state.source_position_size(asset);
        let fresh_size = source_positions.map(|positions| {
            positions
                .get(asset)
                .map(|position| position.size_shares)
                .unwrap_or(0.0)
        });

        if fresh_size == Some(0.0) {
            self.state.clear_source_sell_coverage(asset);
            return Some(SourceSellDecision {
                sell_fraction: 1.0,
                clear_all: true,
                reason: format!(
                    "{} 本笔卖出 {:.4} 份；实时 /positions 已不再持有该 outcome，判定该员工已全部退出此 outcome。",
                    self.config.source_name,
                    source_sell_size
                ),
            });
        }

        let covered_size = self.state.consume_source_sell_coverage(
            asset,
            source_sell_size,
            now,
            SOURCE_SELL_COVERAGE_TTL_SECONDS,
        );
        let uncovered_sell_size = (source_sell_size - covered_size).max(0.0);
        if uncovered_sell_size <= 0.000_001 {
            return Some(SourceSellDecision {
                sell_fraction: 0.0,
                clear_all: false,
                reason: format!(
                    "{} 本笔卖出 {:.4} 份已包含在同批 /positions 先前确认的净减仓中，本笔只消耗 {:.4} 份批次覆盖量，不重复卖出。",
                    self.config.source_name,
                    source_sell_size,
                    covered_size
                ),
            });
        }

        if let (Some(previous), Some(fresh)) = (previous_size, fresh_size) {
            if fresh + 0.000_001 >= previous {
                let position_before = fresh + uncovered_sell_size;
                let sell_fraction = (uncovered_sell_size / position_before).clamp(0.0, 1.0);
                return Some(SourceSellDecision {
                    sell_fraction,
                    clear_all: false,
                    reason: format!(
                        "{} 本笔成交记录卖出 {:.4} 份；/activity 晚于 /positions 到达，当前实际持仓已是卖出后的 {:.4} 份，因此用 当前剩余 + 本笔卖出 重建卖出前约 {:.4} 份，减仓比例 {:.2}%；{} 仍持有该 outcome，我方只按实际 CLOB 余额同比例减仓，不升级为清仓。",
                        self.config.source_name,
                        uncovered_sell_size,
                        fresh,
                        position_before,
                        sell_fraction * 100.0,
                        self.config.source_name
                    ),
                });
            }

            let actual_reduction = previous - fresh;
            let effective_reduction = actual_reduction.max(uncovered_sell_size);
            if actual_reduction > uncovered_sell_size + 0.000_001 {
                self.state.record_source_sell_coverage(
                    asset,
                    actual_reduction - uncovered_sell_size,
                    now,
                );
            }
            let position_before = previous.max(fresh + effective_reduction);
            let sell_fraction = (effective_reduction / position_before).clamp(0.0, 1.0);
            return Some(SourceSellDecision {
                sell_fraction,
                clear_all: false,
                reason: format!(
                    "{} 本笔成交记录卖出 {:.4} 份；实际 /positions 从 {:.4} 份降至 {:.4} 份，净减仓 {:.4} 份，比例 {:.2}%；{} 仍持有该 outcome，我方只按实际 CLOB 余额同比例减仓，不升级为清仓。",
                    self.config.source_name,
                    source_sell_size,
                    previous,
                    fresh,
                    effective_reduction,
                    sell_fraction * 100.0,
                    self.config.source_name
                ),
            });
        }

        if let Some(ledger) = source_ledger_before.filter(|ledger| ledger.net_size_shares > 0.0) {
            let position_before = ledger.net_size_shares.max(uncovered_sell_size);
            let sell_fraction = (uncovered_sell_size / position_before).clamp(0.0, 1.0);
            let clear_all = uncovered_sell_size + 0.000_001 >= ledger.net_size_shares;
            return Some(SourceSellDecision {
                sell_fraction,
                clear_all,
                reason: format!(
                    "{} 本笔卖出 {:.4} 份；本次没有可用的新鲜 /positions，使用本地 /activity 账本的卖出前净持仓约 {:.4} 份估算，减仓比例 {:.2}%{}。",
                    self.config.source_name,
                    uncovered_sell_size,
                    position_before,
                    sell_fraction * 100.0,
                    if clear_all {
                        "，本笔 SELL 已覆盖账本净持仓，按清仓处理"
                    } else {
                        "，不因 /positions 缺失升级为清仓"
                    }
                ),
            });
        }

        let position_before = match (previous_size, fresh_size) {
            (Some(previous), None) => previous,
            (None, Some(fresh)) => fresh + uncovered_sell_size,
            (None, None) => return None,
            (Some(_), Some(_)) => unreachable!("handled above"),
        };
        if position_before <= 0.0 {
            return None;
        }

        let sell_fraction = (uncovered_sell_size / position_before).clamp(0.0, 1.0);
        Some(SourceSellDecision {
            sell_fraction,
            clear_all: false,
            reason: format!(
                "{} 本笔卖出 {:.4} 份；本次 /positions 刷新不可用，使用最近实际持仓约 {:.4} 份估算，本笔减仓比例 {:.2}%；未确认源仓位归零，我方只按实际 CLOB 余额同比例减仓，不升级为清仓。",
                self.config.source_name,
                uncovered_sell_size,
                position_before,
                sell_fraction * 100.0
            ),
        })
    }

    fn sell_position_absent_from_source(
        &mut self,
        position: &CopyPosition,
        now: u64,
    ) -> Vec<AutoCopyReport> {
        if position.size_shares <= 0.0 {
            return Vec::new();
        }

        let failure_key = action_failure_cooldown_key("SELL", &position.position_key);
        if self.state.failure_in_cooldown(
            &failure_key,
            now,
            SOURCE_RECONCILE_ACTION_COOLDOWN_SECONDS,
        ) {
            return Vec::new();
        }

        let metadata = GlobalOutcomeMetadata::from_position(position);
        let global_sell_plan = match self.plan_global_sell_to_target(&metadata, 0.0, now) {
            Ok(plan) => plan,
            Err(error) => {
                return vec![AutoCopyReport::system(format!(
                    "[{} 源仓位对账跳过]\n市场: {}\n方向: {}\n原因: 全局卖出协调状态不可用，为避免误伤其他员工仍支持的仓位，本次不卖；{error}",
                    self.config.source_name,
                    position.market_title.as_deref().unwrap_or("-"),
                    position.outcome.as_deref().unwrap_or("-")
                ))];
            }
        };
        if self.config.strategy.enabled && !self.config.strategy.source_reconcile_sell_enabled {
            let _ = self.sync_global_source_exposure(
                &metadata,
                Some(global_sell_plan.source_target_after_usd),
                now,
            );
            return vec![AutoCopyReport {
                action: "SKIP:源仓位对账只校准".to_owned(),
                status: "skipped".to_owned(),
                reason: format!(
                    "源仓位对账显示 {} 当前已不持有该 outcome；事件篮子策略启用后 /positions 只降低员工目标，不直接卖出我方真实仓位。{}",
                    self.config.source_name,
                    global_sell_plan.coordination_reason()
                ),
                source_trade_key: "-".to_owned(),
                market_title: position.market_title.clone(),
                outcome: position.outcome.clone(),
                position_key: position.position_key.clone(),
                source_price: None,
                source_notional_usd: 0.0,
                copy_amount_usd: 0.0,
                copy_price: None,
                order_id: None,
                realized_pnl_usd: None,
                market_exposure_after_usd: self.state.market_exposure_usd(&position.position_key),
                daily_spend_after_usd: self.state.daily_spend_usd
                    + self.state.daily_reserved_buy_usd(),
                created_at_secs: now_secs(),
                text: format!(
                    "[{} 源仓位对账只校准 / {}]\n状态: skipped\n市场: {}\n方向: {}\n原因: /positions 不直接清仓，只降低员工目标；等待 /activity SELL 或全局协调后续动作。",
                    self.config.source_name,
                    self.config.mode.label(),
                    position.market_title.as_deref().unwrap_or("-"),
                    position.outcome.as_deref().unwrap_or("-")
                ),
            }];
        }
        if !global_sell_plan.should_sell() {
            self.state.clear_exit_retry(&position.position_key);
            return vec![AutoCopyReport {
                action: "SKIP:全局目标仍覆盖".to_owned(),
                status: "skipped".to_owned(),
                reason: format!(
                    "源仓位对账显示 {} 当前已不持有该 outcome；{} 当前不需要真实卖出。{}",
                    self.config.source_name,
                    global_sell_plan.coordination_reason(),
                    global_sell_plan.supporting_sources_label()
                ),
                source_trade_key: "-".to_owned(),
                market_title: position.market_title.clone(),
                outcome: position.outcome.clone(),
                position_key: position.position_key.clone(),
                source_price: None,
                source_notional_usd: 0.0,
                copy_amount_usd: 0.0,
                copy_price: None,
                order_id: None,
                realized_pnl_usd: None,
                market_exposure_after_usd: self.state.market_exposure_usd(&position.position_key),
                daily_spend_after_usd: self.state.daily_spend_usd
                    + self.state.daily_reserved_buy_usd(),
                created_at_secs: now_secs(),
                text: format!(
                    "[{} 源仓位对账跳过 / {}]\n状态: skipped\n市场: {}\n方向: {}\n原因: 源仓位对账显示该员工已不持有；{}",
                    self.config.source_name,
                    self.config.mode.label(),
                    position.market_title.as_deref().unwrap_or("-"),
                    position.outcome.as_deref().unwrap_or("-"),
                    global_sell_plan.coordination_reason()
                ),
            }];
        }

        let request = if global_sell_plan.clear_all {
            AutoCopyExecutionRequest::sell_position_absent_from_source(
                self.config.mode,
                self.config.source_name.clone(),
                position,
            )
        } else {
            AutoCopyExecutionRequest::sell_position_global_excess(
                self.config.mode,
                self.config.source_name.clone(),
                position,
                global_sell_plan.sell_fraction,
                false,
            )
        };
        let execution = self.execute_request(&request);
        if execution.status == ExecutionStatus::Failed
            && is_terminal_missing_token_error(execution.message.as_deref())
        {
            self.state
                .sync_position_actual_balance(&position.position_key, 0.0);
            self.state.clear_exit_retry(&position.position_key);
            self.state.clear_failure(&failure_key);
            let _ = self.sync_global_source_exposure(
                &metadata,
                Some(global_sell_plan.source_target_after_usd),
                now,
            );
            let mut reports = Vec::new();
            self.persist(&mut reports);
            return reports;
        }
        if should_silently_finish_dust_exit(&execution) {
            self.state
                .sync_position_actual_balance(&position.position_key, 0.0);
            self.state.clear_exit_retry(&position.position_key);
            self.state.clear_failure(&failure_key);
            let _ = self.sync_global_source_exposure(
                &metadata,
                Some(global_sell_plan.source_target_after_usd),
                now,
            );
            let mut reports = Vec::new();
            self.persist(&mut reports);
            return reports;
        }
        let mut report =
            self.report_for_position_reconcile_global_sell(position, &execution, &global_sell_plan);

        self.apply_sell_execution(
            &position.position_key,
            execution
                .target_size_shares
                .unwrap_or(position.size_shares * global_sell_plan.sell_fraction),
            position.avg_cost(),
            &execution,
            &mut report,
        );
        if let Some(actual_balance) = execution.actual_balance_shares {
            self.state
                .sync_position_actual_balance(&position.position_key, actual_balance);
        }
        let _ = self.sync_global_source_exposure(
            &metadata,
            Some(global_sell_plan.source_target_after_usd),
            now,
        );
        if matches!(
            execution.status,
            ExecutionStatus::Failed | ExecutionStatus::Skipped
        ) {
            self.state.record_failure(
                failure_key,
                execution
                    .message
                    .clone()
                    .unwrap_or_else(|| report.reason.clone()),
                now,
            );
        } else if matches!(
            execution.status,
            ExecutionStatus::Filled | ExecutionStatus::DryRun | ExecutionStatus::Cancelled
        ) {
            self.state.clear_failure(&failure_key);
        }
        report.market_exposure_after_usd = self.state.market_exposure_usd(&position.position_key);
        report.daily_spend_after_usd =
            self.state.daily_spend_usd + self.state.daily_reserved_buy_usd();

        vec![report]
    }

    fn clear_terminal_exit_failures(&mut self) -> bool {
        let position_keys = self
            .state
            .failure_cooldowns
            .iter()
            .filter(|failure| {
                failure.key.starts_with(SELL_ACTION_FAILURE_PREFIX)
                    && is_terminal_missing_token_error(Some(&failure.message))
            })
            .filter_map(|failure| {
                failure
                    .key
                    .strip_prefix(SELL_ACTION_FAILURE_PREFIX)
                    .map(str::to_owned)
            })
            .collect::<Vec<_>>();

        if position_keys.is_empty() {
            return false;
        }

        for position_key in &position_keys {
            self.state.sync_position_actual_balance(position_key, 0.0);
            self.state.clear_exit_retry(position_key);
            self.state
                .clear_failure(&action_failure_cooldown_key("SELL", position_key));
        }

        true
    }

    fn clear_stale_exit_retries(&mut self) -> bool {
        let before = self.state.pending_exit_retries.len();
        let active_positions = self
            .state
            .positions
            .iter()
            .filter(|position| position.size_shares > 0.0)
            .map(|position| position.position_key.clone())
            .collect::<Vec<_>>();
        self.state.pending_exit_retries.retain(|retry| {
            active_positions
                .iter()
                .any(|position_key| position_key == &retry.position_key)
        });
        self.state.pending_exit_retries.len() != before
    }

    fn source_buy_guard_reason(
        &self,
        key: &str,
        source_notional: f64,
        now: u64,
    ) -> Option<(&'static str, String)> {
        let guard = self.state.active_source_guard(key, now)?;
        let title = if source_notional >= self.config.source_reentry_alert_buy_usd {
            "可能重新建仓"
        } else {
            "卖压冷却"
        };
        let buy_label = if source_notional >= self.config.source_reentry_alert_buy_usd {
            format!(
                "当前 BUY {:.2}U 达到重新建仓提醒线 {:.2}U；初版先提醒/跳过，不自动追入，等这个 outcome 的卖压冷却结束后再恢复正常评估。",
                source_notional, self.config.source_reentry_alert_buy_usd
            )
        } else {
            format!(
                "当前 BUY {:.2}U 低于重新建仓提醒线 {:.2}U，更像卖出后的试探/库存回补/碎片成交。",
                source_notional, self.config.source_reentry_alert_buy_usd
            )
        };

        Some((
            title,
            format!(
                "{}；{} 保护只作用于这个 exact outcome，不影响其他温度档位。",
                guard.reason, buy_label
            ),
        ))
    }

    fn update_source_sell_guard(&mut self, key: &str, source_notional: f64, event_time: u64) {
        let stats =
            self.state
                .source_flow_stats(key, event_time, self.config.source_flow_window_seconds);
        if source_pressure_detected(&stats, &self.config) {
            let reason = format!(
                "同 outcome 在过去 {:.0}s 内连续 SELL {} 笔 / {:.2}U，平均间隔 {:.1}s；这更像挂单被吃、止盈出货或程序化库存调整。",
                self.config.source_flow_window_seconds,
                stats.sell_count,
                stats.sell_notional_usd,
                stats.avg_sell_gap_seconds().unwrap_or(0.0)
            );
            self.state.record_source_guard(
                key.to_owned(),
                SourceGuardKind::Pressure,
                reason,
                event_time,
                self.config.source_pressure_cooldown_seconds,
            );
            return;
        }

        let reason = format!(
            "{} 刚刚卖出同 outcome {:.2}U；第一笔 SELL 已先触发撤挂单/同步卖出，短窗口内暂停跟同 outcome 的后续 BUY。",
            self.config.source_name,
            source_notional
        );
        self.state.record_source_guard(
            key.to_owned(),
            SourceGuardKind::PostSell,
            reason,
            event_time,
            self.config.post_sell_buy_guard_seconds,
        );
    }

    fn sync_pending_orders(&mut self, now: u64, max_orders: usize) -> (Vec<AutoCopyReport>, bool) {
        if self.config.mode != AutoCopyMode::LiveExternal {
            return (Vec::new(), false);
        }

        let order_ids = self
            .state
            .pending_orders
            .iter()
            .filter(|order| {
                now.saturating_sub(order.last_sync_at_secs) >= self.config.pending_sync_seconds
            })
            .take(max_orders)
            .map(|order| order.local_order_id.clone())
            .collect::<Vec<_>>();
        let attempted = !order_ids.is_empty();
        let mut reports = Vec::new();

        for local_order_id in order_ids {
            let Some(order) = self.state.pending_order(&local_order_id).cloned() else {
                continue;
            };
            let request = AutoCopyExecutionRequest::sync(
                self.config.mode,
                self.config.source_name.clone(),
                &order,
            );
            let mut execution = self.execute_request(&request);
            normalize_pending_sync_status(&order, &mut execution);
            let mut report = self.report_for_pending_execution("SYNC", &order, &execution);
            let should_notify = should_report_pending_sync(&order, &execution);

            self.apply_pending_sync(&order.local_order_id, &execution, &mut report, now);
            if let Err(error) =
                self.sync_global_source_exposure_for_key(&order.position_key, None, now)
            {
                reports.push(AutoCopyReport::system(format!(
                    "[{} 全局跟单状态刷新失败]\n市场: {}\n方向: {}\n原因: {error}",
                    self.config.source_name,
                    order.market_title.as_deref().unwrap_or("-"),
                    order.outcome.as_deref().unwrap_or("-")
                )));
            }
            report.market_exposure_after_usd = self.state.market_exposure_usd(&order.position_key);
            report.daily_spend_after_usd =
                self.state.daily_spend_usd + self.state.daily_reserved_buy_usd();
            if should_notify {
                self.push_report(report, &mut reports, now);
            }
        }

        (reports, attempted)
    }

    fn cancel_expired_orders(
        &mut self,
        now: u64,
        max_orders: usize,
    ) -> (Vec<AutoCopyReport>, bool) {
        if self.config.passive_order_ttl_seconds == 0 {
            return (Vec::new(), false);
        }

        let expired = self
            .state
            .pending_orders
            .iter()
            .filter(|order| now >= order.expires_at_secs)
            .take(max_orders)
            .map(|order| order.local_order_id.clone())
            .collect::<Vec<_>>();
        let attempted = !expired.is_empty();
        let mut reports = Vec::new();

        for local_order_id in expired {
            let Some(order) = self.state.pending_order(&local_order_id).cloned() else {
                continue;
            };
            for report in self.cancel_pending_order(&order, "挂单 TTL 到期", now) {
                self.push_report(report, &mut reports, now);
            }
        }

        (reports, attempted)
    }

    fn retry_transient_exit_failures(
        &mut self,
        now: u64,
        max_retries: usize,
    ) -> (Vec<AutoCopyReport>, bool) {
        let retries = self
            .state
            .failure_cooldowns
            .iter()
            .filter(|failure| {
                failure.key.starts_with(SELL_ACTION_FAILURE_PREFIX)
                    && is_retryable_exit_error(Some(&failure.message))
                    && now.saturating_sub(failure.last_failed_at_secs)
                        >= TRANSIENT_EXIT_RETRY_SECONDS
            })
            .filter_map(|failure| {
                failure
                    .key
                    .strip_prefix(SELL_ACTION_FAILURE_PREFIX)
                    .map(str::to_owned)
            })
            .take(max_retries)
            .collect::<Vec<_>>();
        let attempted = !retries.is_empty();
        let mut reports = Vec::new();
        let mut state_changed = false;

        for position_key in retries {
            let Some(position) = self.state.position(&position_key).cloned() else {
                self.state
                    .clear_failure(&action_failure_cooldown_key("SELL", &position_key));
                self.state.clear_exit_retry(&position_key);
                state_changed = true;
                continue;
            };
            if position.size_shares <= 0.0 {
                self.state
                    .clear_failure(&action_failure_cooldown_key("SELL", &position_key));
                self.state.clear_exit_retry(&position_key);
                state_changed = true;
                continue;
            }

            let (report, transient_failure) =
                self.retry_exit_position_after_transient_failure(&position, now);
            state_changed = true;
            if !transient_failure && report.is_some() {
                let report = report.expect("checked report is present");
                self.push_report(report, &mut reports, now);
            }
        }

        if state_changed && reports.is_empty() {
            self.persist(&mut reports);
        }
        (reports, attempted)
    }

    fn retry_exit_position_after_transient_failure(
        &mut self,
        position: &CopyPosition,
        now: u64,
    ) -> (Option<AutoCopyReport>, bool) {
        let retry = self
            .state
            .exit_retry(&position.position_key)
            .cloned()
            .unwrap_or_else(|| PendingExitRetry {
                position_key: position.position_key.clone(),
                sell_fraction: 1.0,
                target_size_shares: None,
                min_sell_price: None,
                force_market_sell: true,
                lock_profit: false,
            });
        let request = AutoCopyExecutionRequest::sell_position_exit_retry(
            self.config.mode,
            self.config.source_name.clone(),
            position,
            &retry,
        );
        let execution = self.execute_request(&request);
        let failure_key = action_failure_cooldown_key("SELL", &position.position_key);
        if should_silently_finish_dust_exit(&execution) {
            self.state
                .sync_position_actual_balance(&position.position_key, 0.0);
            self.state.clear_exit_retry(&position.position_key);
            self.state.clear_failure(&failure_key);
            let _ = self.sync_global_source_exposure_for_key(&position.position_key, None, now);
            return (None, false);
        }
        let remaining_target_size = remaining_exit_target_size(&execution);
        let retryable_failure = execution.status == ExecutionStatus::Failed
            && is_retryable_exit_error(execution.message.as_deref());
        let mut report = self.report_for_exit_retry_sell(position, &retry, &execution);

        self.apply_sell_execution(
            &position.position_key,
            execution
                .target_size_shares
                .unwrap_or(position.size_shares * retry.sell_fraction),
            position.avg_cost(),
            &execution,
            &mut report,
        );
        let pending_exit_recorded = self.apply_pending_exit_retry_submission(
            position,
            &retry,
            now,
            &execution,
            &mut report,
        );
        if let Some(actual_balance) = execution.actual_balance_shares {
            self.state
                .sync_position_actual_balance(&position.position_key, actual_balance);
        }
        let _ = self.sync_global_source_exposure_for_key(&position.position_key, None, now);

        if execution.status == ExecutionStatus::Failed {
            self.state.record_failure(
                failure_key,
                execution
                    .message
                    .clone()
                    .unwrap_or_else(|| report.reason.clone()),
                now,
            );
        } else if matches!(
            execution.status,
            ExecutionStatus::Pending | ExecutionStatus::Submitted
        ) {
            if pending_exit_recorded || remaining_target_size < MIN_CLOB_ORDER_SIZE_SHARES {
                self.state.clear_exit_retry(&position.position_key);
                self.state.clear_failure(&failure_key);
            } else {
                let mut updated_retry = retry.clone();
                updated_retry.target_size_shares = Some(remaining_target_size);
                self.state.record_exit_retry(updated_retry);
                self.state.record_failure(
                    failure_key,
                    "exit retry returned pending/submitted without enough order details to track"
                        .to_owned(),
                    now,
                );
            }
        } else if remaining_target_size >= MIN_CLOB_ORDER_SIZE_SHARES {
            let mut updated_retry = retry.clone();
            updated_retry.target_size_shares = Some(remaining_target_size);
            self.state.record_exit_retry(updated_retry);
            self.state.record_failure(
                failure_key,
                format!(
                    "partial FAK sell remaining {:.6} shares after exit retry",
                    remaining_target_size
                ),
                now,
            );
        } else if matches!(
            execution.status,
            ExecutionStatus::Filled
                | ExecutionStatus::DryRun
                | ExecutionStatus::Cancelled
                | ExecutionStatus::Skipped
        ) {
            self.state.clear_exit_retry(&position.position_key);
            self.state.clear_failure(&failure_key);
        }

        report.market_exposure_after_usd = self.state.market_exposure_usd(&position.position_key);
        report.daily_spend_after_usd =
            self.state.daily_spend_usd + self.state.daily_reserved_buy_usd();
        (Some(report), retryable_failure)
    }

    fn cancel_pending_for_key(&mut self, key: &str, reason: &str, now: u64) -> Vec<AutoCopyReport> {
        let orders = self
            .state
            .pending_orders
            .iter()
            .filter(|order| order.position_key == key)
            .cloned()
            .collect::<Vec<_>>();
        let mut reports = Vec::new();

        for order in orders {
            reports.extend(self.cancel_pending_order(&order, reason, now));
        }

        reports
    }

    fn cancel_pending_order(
        &mut self,
        order: &PendingCopyOrder,
        reason: &str,
        now: u64,
    ) -> Vec<AutoCopyReport> {
        let request = AutoCopyExecutionRequest::cancel(
            self.config.mode,
            self.config.source_name.clone(),
            order,
            reason,
        );
        let execution = self.execute_request(&request);
        let mut report = self.report_for_pending_execution("CANCEL", order, &execution);
        report.reason = reason.to_owned();

        if execution.status != ExecutionStatus::Failed {
            self.apply_pending_sync(&order.local_order_id, &execution, &mut report, now);
        } else if let Some(existing) = self.state.pending_order_mut(&order.local_order_id) {
            existing.last_sync_at_secs = now;
        }
        if let Err(error) = self.sync_global_source_exposure_for_key(&order.position_key, None, now)
        {
            let mut reports = vec![report];
            reports.push(AutoCopyReport::system(format!(
                "[{} 全局跟单状态刷新失败]\n市场: {}\n方向: {}\n原因: {error}",
                self.config.source_name,
                order.market_title.as_deref().unwrap_or("-"),
                order.outcome.as_deref().unwrap_or("-")
            )));
            return reports;
        }
        report.market_exposure_after_usd = self.state.market_exposure_usd(&order.position_key);
        report.daily_spend_after_usd =
            self.state.daily_spend_usd + self.state.daily_reserved_buy_usd();

        vec![report]
    }

    fn execute_request(&self, request: &AutoCopyExecutionRequest) -> ExecutionResult {
        match self.config.mode {
            AutoCopyMode::DryRun => ExecutionResult {
                status: ExecutionStatus::DryRun,
                order_id: None,
                order_price: None,
                filled_amount_usd: None,
                filled_size: None,
                filled_price: None,
                realized_pnl_usd: None,
                actual_balance_shares: None,
                target_size_shares: request.order.size_shares,
                message: Some("dry-run: 未真实下单，只生成执行请求。".to_owned()),
            },
            AutoCopyMode::LiveExternal => self.execute_external(request),
        }
    }

    fn execute_external(&self, request: &AutoCopyExecutionRequest) -> ExecutionResult {
        let Some(command) = self
            .config
            .executor_command
            .as_deref()
            .map(str::trim)
            .filter(|command| !command.is_empty())
        else {
            return ExecutionResult::failed(
                "live-external mode enabled but executor command is missing",
            );
        };

        let request_json = match serde_json::to_vec(request) {
            Ok(json) => json,
            Err(error) => {
                return ExecutionResult::failed(format!(
                    "failed to encode executor request: {error}"
                ))
            }
        };
        let mut child = match Command::new("sh")
            .arg("-c")
            .arg(command)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(child) => child,
            Err(error) => {
                return ExecutionResult::failed(format!("failed to start executor: {error}"))
            }
        };

        if let Some(stdin) = child.stdin.as_mut() {
            if let Err(error) = stdin.write_all(&request_json) {
                return ExecutionResult::failed(format!(
                    "failed to write executor request: {error}"
                ));
            }
        }

        let output = match child.wait_with_output() {
            Ok(output) => output,
            Err(error) => {
                return ExecutionResult::failed(format!("failed to wait for executor: {error}"))
            }
        };

        if !output.status.success() {
            if !output.stdout.is_empty() {
                if let Ok(result) =
                    serde_json::from_slice::<ExternalExecutionResult>(&output.stdout)
                {
                    return result.into();
                }
            }
            return ExecutionResult::failed(format!(
                "executor exited with code {:?}: stdout={} stderr={}",
                output.status.code(),
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ));
        }

        if output.stdout.is_empty() {
            return ExecutionResult {
                status: ExecutionStatus::Submitted,
                order_id: None,
                order_price: None,
                filled_amount_usd: None,
                filled_size: None,
                filled_price: None,
                realized_pnl_usd: None,
                actual_balance_shares: None,
                target_size_shares: None,
                message: Some("executor returned empty success output".to_owned()),
            };
        }

        match serde_json::from_slice::<ExternalExecutionResult>(&output.stdout) {
            Ok(result) => result.into(),
            Err(error) => ExecutionResult::failed(format!(
                "executor output is not valid JSON: {error}; stdout={}",
                String::from_utf8_lossy(&output.stdout)
            )),
        }
    }

    fn apply_buy_execution(
        &mut self,
        key: &str,
        trade: &UserTrade,
        copy_amount: f64,
        passive_limit_price: f64,
        now: u64,
        execution: &ExecutionResult,
    ) {
        match execution.status {
            ExecutionStatus::Filled => {
                let filled_amount = execution.filled_amount_usd.unwrap_or(copy_amount);
                let filled_price = execution.filled_price.unwrap_or(passive_limit_price);
                let filled_size = execution
                    .filled_size
                    .unwrap_or_else(|| shares_for_amount(filled_amount, filled_price));
                self.state
                    .apply_buy_fill(key, trade, filled_amount, filled_size);
                self.state.daily_spend_usd += filled_amount;
            }
            ExecutionStatus::Pending | ExecutionStatus::Submitted => {
                let limit_price = execution.order_price.unwrap_or(passive_limit_price);
                let submitted_copy_amount = execution
                    .target_size_shares
                    .map(|size| size * limit_price)
                    .unwrap_or(copy_amount);
                let filled_amount = execution.filled_amount_usd.unwrap_or(0.0);
                let filled_size = execution.filled_size.unwrap_or_else(|| {
                    if filled_amount > 0.0 {
                        shares_for_amount(filled_amount, limit_price)
                    } else {
                        0.0
                    }
                });
                if filled_amount > 0.0 && filled_size > 0.0 {
                    self.state
                        .apply_buy_fill(key, trade, filled_amount, filled_size);
                    self.state.daily_spend_usd += filled_amount;
                }
                self.state.pending_orders.push(PendingCopyOrder {
                    local_order_id: local_order_id(trade, now),
                    external_order_id: execution.order_id.clone(),
                    position_key: key.to_owned(),
                    side: "BUY".to_owned(),
                    market_title: trade.title.clone(),
                    outcome: trade.outcome.clone(),
                    asset: trade.asset.clone(),
                    condition_id: trade.condition_id.clone(),
                    copy_amount_usd: submitted_copy_amount,
                    limit_price,
                    requested_limit_price: Some(passive_limit_price),
                    filled_amount_usd: filled_amount,
                    filled_size,
                    created_at_secs: now,
                    expires_at_secs: pending_expires_at(now, self.config.passive_order_ttl_seconds),
                    last_sync_at_secs: now,
                    source_trade_key: source_trade_key(trade),
                    source_trade_at_secs: trade.timestamp.unwrap_or(now),
                });
            }
            ExecutionStatus::DryRun
            | ExecutionStatus::Cancelled
            | ExecutionStatus::Skipped
            | ExecutionStatus::Failed => {}
        }
    }

    fn apply_pending_sell_submission(
        &mut self,
        key: &str,
        trade: &UserTrade,
        requested_sell_size: f64,
        fallback_limit_price: f64,
        now: u64,
        execution: &ExecutionResult,
        report: &mut AutoCopyReport,
    ) {
        if !matches!(
            execution.status,
            ExecutionStatus::Pending | ExecutionStatus::Submitted
        ) {
            return;
        }

        let limit_price = execution.order_price.unwrap_or(fallback_limit_price);
        let target_size = execution
            .target_size_shares
            .unwrap_or(requested_sell_size)
            .max(0.0);
        if target_size <= 0.0 || limit_price <= 0.0 {
            return;
        }

        let filled_price = execution.filled_price.unwrap_or(limit_price);
        let filled_amount = execution
            .filled_amount_usd
            .or_else(|| execution.filled_size.map(|size| size * filled_price))
            .unwrap_or(0.0)
            .clamp(0.0, target_size * filled_price);
        let filled_size = execution
            .filled_size
            .unwrap_or_else(|| shares_for_amount(filled_amount, filled_price))
            .clamp(0.0, target_size);

        if filled_size > 0.0 {
            let Some(position) = self.state.position(key).cloned() else {
                return;
            };
            let avg_cost = position.avg_cost();
            let realized_pnl = execution
                .realized_pnl_usd
                .unwrap_or_else(|| filled_amount - filled_size * avg_cost);
            self.state
                .apply_sell_fill(key, filled_size, filled_amount, realized_pnl);
            self.state.daily_realized_pnl_usd += realized_pnl;
            report.realized_pnl_usd = Some(realized_pnl);
            report.copy_amount_usd = filled_amount;
            report.copy_price = Some(filled_price);
        }

        if target_size > filled_size + 0.000_001 {
            self.state.pending_orders.push(PendingCopyOrder {
                local_order_id: local_sell_order_id(trade, now),
                external_order_id: execution.order_id.clone(),
                position_key: key.to_owned(),
                side: "SELL".to_owned(),
                market_title: trade.title.clone(),
                outcome: trade.outcome.clone(),
                asset: trade.asset.clone(),
                condition_id: trade.condition_id.clone(),
                copy_amount_usd: target_size * limit_price,
                limit_price,
                requested_limit_price: Some(fallback_limit_price),
                filled_amount_usd: filled_amount,
                filled_size,
                created_at_secs: now,
                expires_at_secs: pending_expires_at(now, self.config.passive_order_ttl_seconds),
                last_sync_at_secs: now,
                source_trade_key: source_trade_key(trade),
                source_trade_at_secs: trade.timestamp.unwrap_or(now),
            });
        }
    }

    fn apply_pending_exit_retry_submission(
        &mut self,
        position: &CopyPosition,
        retry: &PendingExitRetry,
        now: u64,
        execution: &ExecutionResult,
        report: &mut AutoCopyReport,
    ) -> bool {
        if !matches!(
            execution.status,
            ExecutionStatus::Pending | ExecutionStatus::Submitted
        ) {
            return false;
        }

        let Some(limit_price) = execution
            .order_price
            .or(retry.min_sell_price)
            .filter(|price| *price > 0.0)
        else {
            return false;
        };

        let requested_size = execution
            .target_size_shares
            .or(retry.target_size_shares)
            .unwrap_or(position.size_shares * retry.sell_fraction)
            .max(0.0);
        if requested_size <= 0.0 {
            return false;
        }

        let filled_price = execution.filled_price.unwrap_or(limit_price);
        let filled_amount = execution
            .filled_amount_usd
            .or_else(|| execution.filled_size.map(|size| size * filled_price))
            .unwrap_or(0.0)
            .clamp(0.0, requested_size * filled_price);
        let filled_size = execution
            .filled_size
            .unwrap_or_else(|| shares_for_amount(filled_amount, filled_price))
            .clamp(0.0, requested_size);

        if filled_size > 0.0 {
            let avg_cost = position.avg_cost();
            let realized_pnl = execution
                .realized_pnl_usd
                .unwrap_or_else(|| filled_amount - filled_size * avg_cost);
            self.state.apply_sell_fill(
                &position.position_key,
                filled_size,
                filled_amount,
                realized_pnl,
            );
            self.state.daily_realized_pnl_usd += realized_pnl;
            report.realized_pnl_usd = Some(realized_pnl);
            report.copy_amount_usd = filled_amount;
            report.copy_price = Some(filled_price);
        }

        if requested_size <= filled_size + 0.000_001 {
            return true;
        }

        self.state.pending_orders.push(PendingCopyOrder {
            local_order_id: local_exit_retry_sell_order_id(&position.position_key, now),
            external_order_id: execution.order_id.clone(),
            position_key: position.position_key.clone(),
            side: "SELL".to_owned(),
            market_title: position.market_title.clone(),
            outcome: position.outcome.clone(),
            asset: position.asset.clone(),
            condition_id: position.condition_id.clone(),
            copy_amount_usd: requested_size * limit_price,
            limit_price,
            requested_limit_price: retry.min_sell_price,
            filled_amount_usd: filled_amount,
            filled_size,
            created_at_secs: now,
            expires_at_secs: pending_expires_at(now, self.config.passive_order_ttl_seconds),
            last_sync_at_secs: now,
            source_trade_key: format!("exit-retry:{}", position.position_key),
            source_trade_at_secs: position.updated_at_secs,
        });
        true
    }

    fn apply_pending_sync(
        &mut self,
        local_order_id: &str,
        execution: &ExecutionResult,
        report: &mut AutoCopyReport,
        now: u64,
    ) {
        let Some(order) = self.state.pending_order(local_order_id).cloned() else {
            return;
        };

        if order.side == "BUY" {
            let filled_price = execution.filled_price.unwrap_or(order.limit_price);
            let filled_amount = execution
                .filled_amount_usd
                .or_else(|| execution.filled_size.map(|size| size * filled_price))
                .unwrap_or(order.filled_amount_usd);
            let filled_delta = (filled_amount - order.filled_amount_usd).max(0.0);
            if filled_delta > 0.0 {
                let filled_size_delta = execution
                    .filled_size
                    .map(|size| (size - order.filled_size).max(0.0))
                    .unwrap_or_else(|| shares_for_amount(filled_delta, filled_price));
                self.state.apply_buy_fill_from_order(
                    &order,
                    filled_delta,
                    filled_size_delta,
                    filled_price,
                );
                self.state.daily_spend_usd += filled_delta;
                report.copy_amount_usd = filled_delta;
                report.copy_price = Some(filled_price);
            }
        } else if order.side == "SELL" {
            let filled_price = execution.filled_price.unwrap_or(order.limit_price);
            let filled_amount = execution
                .filled_amount_usd
                .or_else(|| execution.filled_size.map(|size| size * filled_price))
                .unwrap_or(order.filled_amount_usd);
            let filled_delta = (filled_amount - order.filled_amount_usd).max(0.0);
            if filled_delta > 0.0 {
                let filled_size_delta = execution
                    .filled_size
                    .map(|size| (size - order.filled_size).max(0.0))
                    .unwrap_or_else(|| shares_for_amount(filled_delta, filled_price));
                if let Some(position) = self.state.position(&order.position_key).cloned() {
                    let avg_cost = position.avg_cost();
                    let realized_pnl = execution
                        .realized_pnl_usd
                        .unwrap_or_else(|| filled_delta - filled_size_delta * avg_cost);
                    self.state.apply_sell_fill(
                        &order.position_key,
                        filled_size_delta,
                        filled_delta,
                        realized_pnl,
                    );
                    self.state.daily_realized_pnl_usd += realized_pnl;
                    report.realized_pnl_usd = Some(realized_pnl);
                }
                report.copy_amount_usd = filled_delta;
                report.copy_price = Some(filled_price);
            }
        }

        match execution.status {
            ExecutionStatus::Filled | ExecutionStatus::Cancelled | ExecutionStatus::Skipped => {
                self.state.remove_pending_order(local_order_id);
            }
            ExecutionStatus::Pending
            | ExecutionStatus::Submitted
            | ExecutionStatus::DryRun
            | ExecutionStatus::Failed => {
                if let Some(existing) = self.state.pending_order_mut(local_order_id) {
                    existing.last_sync_at_secs = now;
                    let filled_price = execution.filled_price.unwrap_or(existing.limit_price);
                    if let Some(filled_amount) = execution
                        .filled_amount_usd
                        .or_else(|| execution.filled_size.map(|size| size * filled_price))
                    {
                        existing.filled_amount_usd = filled_amount;
                    }
                    if let Some(filled_size) = execution.filled_size {
                        existing.filled_size = filled_size;
                    }
                }
            }
        }
    }

    fn apply_sell_execution(
        &mut self,
        key: &str,
        requested_sell_size: f64,
        fallback_price: f64,
        execution: &ExecutionResult,
        report: &mut AutoCopyReport,
    ) {
        if !matches!(
            execution.status,
            ExecutionStatus::Filled | ExecutionStatus::DryRun
        ) {
            return;
        }

        if execution.status == ExecutionStatus::DryRun {
            return;
        }

        let Some(position) = self.state.position(key).cloned() else {
            return;
        };
        let sell_price = execution.filled_price.unwrap_or(fallback_price);
        let Some(sell_size) = execution
            .filled_size
            .filter(|size| *size > 0.0)
            .or_else(|| {
                execution
                    .filled_amount_usd
                    .filter(|amount| *amount > 0.0 && sell_price > 0.0)
                    .map(|amount| amount / sell_price)
            })
        else {
            return;
        };
        let sell_size = sell_size.min(requested_sell_size);
        let filled_amount = execution
            .filled_amount_usd
            .unwrap_or_else(|| sell_size * sell_price);
        let avg_cost = position.avg_cost();
        let realized_pnl = execution
            .realized_pnl_usd
            .unwrap_or_else(|| filled_amount - sell_size * avg_cost);

        self.state
            .apply_sell_fill(key, sell_size, filled_amount, realized_pnl);
        self.state.daily_realized_pnl_usd += realized_pnl;
        report.realized_pnl_usd = Some(realized_pnl);
        report.copy_price = Some(sell_price);
        report.copy_amount_usd = filled_amount;
    }

    fn report_from_execution(
        &self,
        action: &str,
        trade: &UserTrade,
        source_notional: f64,
        copy_amount: f64,
        limit_price: f64,
        passive_limit_price: Option<f64>,
        buy_take_enabled: Option<bool>,
        execution: &ExecutionResult,
        sizing_reason: Option<&str>,
    ) -> AutoCopyReport {
        let title = trade.title.as_deref().unwrap_or("-");
        let outcome = trade.outcome.as_deref().unwrap_or("-");
        let source_price = trade.price.unwrap_or(0.0);
        let market_url = market_url(trade);
        let status = execution.status.label().to_owned();
        let copy_price = match action {
            "BUY" => execution
                .filled_price
                .or(execution.order_price)
                .or(passive_limit_price)
                .or(Some(limit_price)),
            "SELL" => execution.filled_price.or(execution.order_price),
            _ => execution.filled_price.or(execution.order_price),
        };
        let reason = match action {
            "BUY" => {
                let take_enabled = buy_take_enabled.unwrap_or(self.config.buy_take_enabled);
                let sizing = sizing_reason.map(str::to_owned).unwrap_or_else(|| {
                    format!(
                        "{source_notional:.4}U 按 {}",
                        copy_sizing_label(source_notional)
                    )
                });
                let entry_price_reference = source_price
                    .max(limit_price)
                    .max(passive_limit_price.unwrap_or(limit_price));
                if entry_price_reference > self.config.high_price_exposure_threshold {
                    let execution_mode = if take_enabled {
                        "盘口不高于该价格时允许 FOK，否则按风控价 post-only 挂单"
                    } else {
                        "只按风控价 post-only 挂单，不主动吃卖一"
                    };
                    format!(
                        "{}；高价谨慎模式: {} 源成交价 {:.2}c，入场风控参考价 {:.2}c，最高买价 {:.2}c；{}，挂单价 {:.2}c，不向 99c/100c 追价，{}。",
                        sizing,
                        self.config.source_name,
                        source_price * 100.0,
                        entry_price_reference * 100.0,
                        limit_price * 100.0,
                        execution_mode,
                        passive_limit_price.unwrap_or(limit_price) * 100.0,
                        buy_ttl_label(
                            execution.status,
                            self.config.passive_order_ttl_seconds,
                            &self.config.source_name,
                        )
                    )
                } else {
                    let effective_max_chase_pct = self.effective_buy_chase_pct(source_price);
                    format!(
                        "{}；买入模式: {}；直接追价上限 {:.2}c（+{:.1}%，FOK 不受 +{:.2}c 绝对值限制），挂单 {:.2}c（+{:.1}% 且最多 +{:.2}c），{}。",
                        sizing,
                        if take_enabled {
                            "允许 FOK 吃卖一"
                        } else {
                            "只挂 post-only，不主动吃卖一"
                        },
                        limit_price * 100.0,
                        effective_max_chase_pct * 100.0,
                        self.config.max_chase_delta * 100.0,
                        passive_limit_price.unwrap_or(limit_price) * 100.0,
                        self.config.passive_offset_pct * 100.0,
                        self.config.passive_offset * 100.0,
                        buy_ttl_label(
                            execution.status,
                            self.config.passive_order_ttl_seconds,
                            &self.config.source_name,
                        )
                    )
                }
            }
            "SELL" => sizing_reason.map(str::to_owned).unwrap_or_else(|| {
                format!(
                    "检测到 {} 卖出；先取消同 outcome 未成交买单，再按源实际持仓变化同步减仓。",
                    self.config.source_name
                )
            }),
            _ => "-".to_owned(),
        };
        let action_text = report_action_label(action, execution.status);
        let source_name = self.config.source_name.as_str();
        let mut text = format!(
            "[{} 自动跟随{} / {}]\n\
状态: {}\n\
市场: {}\n\
方向: {}\n\
链接: {}\n\
{}: {} {:.2}U @ {:.2}c\n\
我方: {} {:.2}U @ {}\n\
	检测延迟: {} 秒\n\
	原因: {}\n\
额度: 该市场敞口 {:.2}U / {:.2}U, 今日已用/预留 {:.2}U / {:.2}U",
            source_name,
            action_text,
            self.config.mode.label(),
            status,
            title,
            outcome,
            market_url,
            source_name,
            action_label(action),
            source_notional,
            source_price * 100.0,
            copy_action_label(action, execution.status),
            copy_amount,
            copy_price
                .map(|price| format!("{:.2}c", price * 100.0))
                .unwrap_or_else(|| "-".to_owned()),
            now_secs().saturating_sub(trade.timestamp.unwrap_or_else(now_secs)),
            reason,
            self.state.market_exposure_usd(&position_key(trade)),
            self.config.max_market_exposure_usd,
            self.state.daily_spend_usd + self.state.daily_reserved_buy_usd(),
            self.config.max_daily_spend_usd,
        );

        if let Some(message) = &execution.message {
            text.push_str(&format!("\n执行器: {message}"));
        }
        if let Some(actual_balance) = execution.actual_balance_shares {
            text.push_str(&format!("\n实际余额: {:.6} 份", actual_balance));
        }
        if let Some(order_id) = &execution.order_id {
            text.push_str(&format!("\n订单: {order_id}"));
        }

        AutoCopyReport {
            action: action.to_owned(),
            status,
            reason,
            source_trade_key: source_trade_key(trade),
            market_title: trade.title.clone(),
            outcome: trade.outcome.clone(),
            position_key: position_key(trade),
            source_price: trade.price,
            source_notional_usd: source_notional,
            copy_amount_usd: copy_amount,
            copy_price,
            order_id: execution.order_id.clone(),
            realized_pnl_usd: execution.realized_pnl_usd,
            market_exposure_after_usd: self.state.market_exposure_usd(&position_key(trade)),
            daily_spend_after_usd: self.state.daily_spend_usd + self.state.daily_reserved_buy_usd(),
            created_at_secs: now_secs(),
            text,
        }
    }

    fn report_for_pending_execution(
        &self,
        action: &str,
        order: &PendingCopyOrder,
        execution: &ExecutionResult,
    ) -> AutoCopyReport {
        let now = now_secs();
        let copy_price = execution
            .filled_price
            .or(execution.order_price)
            .or(Some(order.limit_price));
        let cumulative_filled = pending_cumulative_filled_amount(order, execution);
        let filled_delta = (cumulative_filled - order.filled_amount_usd).max(0.0);
        let remaining_amount = (order.copy_amount_usd - cumulative_filled).max(0.0);
        let cumulative_size = execution.filled_size.unwrap_or_else(|| {
            shares_for_amount(cumulative_filled, copy_price.unwrap_or(order.limit_price))
        });
        let filled_size_delta = (cumulative_size - order.filled_size).max(0.0);
        let remaining_size = shares_for_amount(remaining_amount, order.limit_price);
        let fill_pct = if order.copy_amount_usd > 0.0 {
            (cumulative_filled / order.copy_amount_usd * 100.0).clamp(0.0, 100.0)
        } else {
            0.0
        };
        let copy_amount = if action == "SYNC" && filled_delta > 0.0 {
            filled_delta
        } else {
            execution.filled_amount_usd.unwrap_or(order.copy_amount_usd)
        };
        let status = execution.status.label().to_owned();
        let lifecycle_label = if action == "SYNC" && filled_delta > 0.0 {
            if remaining_amount <= 0.000_001 {
                "全部成交"
            } else {
                "部分成交"
            }
        } else {
            pending_action_label(action)
        };
        let source_name = self.config.source_name.as_str();
        let mut text = format!(
            "[{} 挂单{} / {}]\n\
状态: {}\n\
市场: {}\n\
方向: {}\n\
原挂单: {:.2}U @ {:.2}c\n\
{} 成交距今: {}\n\
我方挂单距今: {}\n\
挂单等待: {}\n\
订单: {}\n\
原因: {}",
            source_name,
            lifecycle_label,
            self.config.mode.label(),
            status,
            order.market_title.as_deref().unwrap_or("-"),
            order.outcome.as_deref().unwrap_or("-"),
            order.copy_amount_usd,
            order.limit_price * 100.0,
            source_name,
            timestamp_age_label(order.source_trade_at_secs, now),
            timestamp_age_label(order.created_at_secs, now),
            timestamp_age_label(order.created_at_secs, now),
            order
                .external_order_id
                .as_deref()
                .unwrap_or(&order.local_order_id),
            execution
                .message
                .as_deref()
                .unwrap_or("同步/撤单请求已提交。"),
        );

        if action == "SYNC" && filled_delta > 0.0 {
            text.push_str(&format!(
                "\n本次成交: {:.4}U / {:.4}份 @ {:.2}c\n累计成交: {:.4}U / {:.4}U（{:.1}%）\n剩余挂单: {:.4}U / {:.4}份",
                filled_delta,
                filled_size_delta,
                copy_price.unwrap_or(order.limit_price) * 100.0,
                cumulative_filled,
                order.copy_amount_usd,
                fill_pct,
                remaining_amount,
                remaining_size
            ));
        } else {
            if let Some(filled_price) = execution.filled_price {
                text.push_str(&format!("\n成交价: {:.2}c", filled_price * 100.0));
            }
            if let Some(filled_amount) = execution.filled_amount_usd {
                text.push_str(&format!("\n累计成交金额: {:.4}U", filled_amount));
            }
        }

        AutoCopyReport {
            action: action.to_owned(),
            status,
            reason: execution
                .message
                .clone()
                .unwrap_or_else(|| "pending order lifecycle update".to_owned()),
            source_trade_key: order.source_trade_key.clone(),
            market_title: order.market_title.clone(),
            outcome: order.outcome.clone(),
            position_key: order.position_key.clone(),
            source_price: None,
            source_notional_usd: 0.0,
            copy_amount_usd: copy_amount,
            copy_price,
            order_id: order.external_order_id.clone(),
            realized_pnl_usd: execution.realized_pnl_usd,
            market_exposure_after_usd: self.state.market_exposure_usd(&order.position_key),
            daily_spend_after_usd: self.state.daily_spend_usd + self.state.daily_reserved_buy_usd(),
            created_at_secs: now_secs(),
            text,
        }
    }

    fn report_for_position_reconcile_global_sell(
        &self,
        position: &CopyPosition,
        execution: &ExecutionResult,
        global_sell_plan: &GlobalSellPlan,
    ) -> AutoCopyReport {
        let status = execution.status.label().to_owned();
        let copy_price = execution.filled_price.or(execution.order_price);
        let sell_size = execution
            .target_size_shares
            .unwrap_or(position.size_shares * global_sell_plan.sell_fraction);
        let copy_amount = execution
            .filled_amount_usd
            .unwrap_or(global_sell_plan.excess_usd);
        let reason = format!(
            "源仓位对账显示 {} 当前已不持有该 outcome；不直接清仓，先将该员工贡献目标降为 0，再只卖多员工全局目标之外的超额。{}",
            self.config.source_name,
            global_sell_plan.coordination_reason()
        );
        let source_name = self.config.source_name.as_str();
        let mut text = format!(
            "[{} 源仓位对账减仓 / {}]\n\
状态: {}\n\
市场: {}\n\
方向: {}\n\
源仓位: {} 当前 /positions 未持有该 outcome\n\
我方: 卖出 {:.4} 份 @ {}\n\
原因: {}\n\
额度: 该市场敞口 {:.2}U / {:.2}U, 今日已用/预留 {:.2}U / {:.2}U",
            source_name,
            self.config.mode.label(),
            status,
            position.market_title.as_deref().unwrap_or("-"),
            position.outcome.as_deref().unwrap_or("-"),
            source_name,
            sell_size,
            copy_price
                .map(|price| format!("{:.2}c", price * 100.0))
                .unwrap_or_else(|| "当前盘口".to_owned()),
            reason,
            self.state.market_exposure_usd(&position.position_key),
            self.config.max_market_exposure_usd,
            self.state.daily_spend_usd + self.state.daily_reserved_buy_usd(),
            self.config.max_daily_spend_usd,
        );

        if let Some(message) = &execution.message {
            text.push_str(&format!("\n执行器: {message}"));
        }
        if let Some(actual_balance) = execution.actual_balance_shares {
            text.push_str(&format!("\n执行器实仓余额: {actual_balance:.4} 份"));
        }
        if let Some(order_id) = &execution.order_id {
            text.push_str(&format!("\n订单: {order_id}"));
        }

        AutoCopyReport {
            action: "SELL".to_owned(),
            status,
            reason,
            source_trade_key: "-".to_owned(),
            market_title: position.market_title.clone(),
            outcome: position.outcome.clone(),
            position_key: position.position_key.clone(),
            source_price: None,
            source_notional_usd: 0.0,
            copy_amount_usd: copy_amount,
            copy_price,
            order_id: execution.order_id.clone(),
            realized_pnl_usd: execution.realized_pnl_usd,
            market_exposure_after_usd: self.state.market_exposure_usd(&position.position_key),
            daily_spend_after_usd: self.state.daily_spend_usd + self.state.daily_reserved_buy_usd(),
            created_at_secs: now_secs(),
            text,
        }
    }

    fn report_for_exit_retry_sell(
        &self,
        position: &CopyPosition,
        retry: &PendingExitRetry,
        execution: &ExecutionResult,
    ) -> AutoCopyReport {
        let status = execution.status.label().to_owned();
        let copy_price = execution.filled_price.or(execution.order_price);
        let copy_amount = execution.filled_amount_usd.unwrap_or_else(|| {
            execution
                .target_size_shares
                .unwrap_or(position.size_shares * retry.sell_fraction)
                * position.avg_cost()
        });
        let retry_size = execution
            .target_size_shares
            .unwrap_or(position.size_shares * retry.sell_fraction);
        let reason = if retry.sell_fraction >= 0.999_999 {
            format!(
                "此前 {} SELL 触发的清仓因网络失败未完成；继续按原意图重试清仓{}。",
                self.config.source_name,
                retry
                    .min_sell_price
                    .map(|price| format!("，最低卖价保护 {:.2}c", price * 100.0))
                    .unwrap_or_else(|| "，使用市场退出模式".to_owned())
            )
        } else {
            format!(
                "此前 {} SELL 触发的同比例减仓因网络失败未完成；继续按原比例 {:.2}% 重试，最低卖价保护 {}。",
                self.config.source_name,
                retry.sell_fraction * 100.0,
                retry
                    .min_sell_price
                    .map(|price| format!("{:.2}c", price * 100.0))
                    .unwrap_or_else(|| "当前盘口".to_owned())
            )
        };
        let source_name = self.config.source_name.as_str();
        let mut text = format!(
            "[{} 退出重试 / {}]\n\
状态: {}\n\
市场: {}\n\
方向: {}\n\
我方: 卖出 {:.4} 份 @ {}\n\
原因: {}\n\
额度: 该市场敞口 {:.2}U / {:.2}U, 今日已用/预留 {:.2}U / {:.2}U",
            source_name,
            self.config.mode.label(),
            status,
            position.market_title.as_deref().unwrap_or("-"),
            position.outcome.as_deref().unwrap_or("-"),
            retry_size,
            copy_price
                .map(|price| format!("{:.2}c", price * 100.0))
                .unwrap_or_else(|| "当前盘口".to_owned()),
            reason,
            self.state.market_exposure_usd(&position.position_key),
            self.config.max_market_exposure_usd,
            self.state.daily_spend_usd + self.state.daily_reserved_buy_usd(),
            self.config.max_daily_spend_usd,
        );

        if let Some(message) = &execution.message {
            text.push_str(&format!("\n执行器: {message}"));
        }
        if let Some(actual_balance) = execution.actual_balance_shares {
            text.push_str(&format!("\n执行器实仓余额: {actual_balance:.4} 份"));
        }
        if let Some(order_id) = &execution.order_id {
            text.push_str(&format!("\n订单: {order_id}"));
        }

        AutoCopyReport {
            action: "SELL".to_owned(),
            status,
            reason,
            source_trade_key: "-".to_owned(),
            market_title: position.market_title.clone(),
            outcome: position.outcome.clone(),
            position_key: position.position_key.clone(),
            source_price: None,
            source_notional_usd: 0.0,
            copy_amount_usd: copy_amount,
            copy_price,
            order_id: execution.order_id.clone(),
            realized_pnl_usd: execution.realized_pnl_usd,
            market_exposure_after_usd: self.state.market_exposure_usd(&position.position_key),
            daily_spend_after_usd: self.state.daily_spend_usd + self.state.daily_reserved_buy_usd(),
            created_at_secs: now_secs(),
            text,
        }
    }

    fn skip_report(
        &self,
        title: &str,
        trade: &UserTrade,
        reason: impl Into<String>,
    ) -> AutoCopyReport {
        let reason = reason.into();
        let source_price = trade.price.unwrap_or(0.0);
        let source_notional = trade
            .size
            .filter(|size| *size > 0.0)
            .map(|size| source_price * size)
            .unwrap_or(0.0);
        let text = format!(
            "[{} 自动跟随跳过 / {}]\n\
原因: {}\n\
市场: {}\n\
方向: {}\n\
{}: {} {:.2}U @ {:.2}c",
            self.config.source_name,
            self.config.mode.label(),
            reason,
            trade.title.as_deref().unwrap_or("-"),
            trade.outcome.as_deref().unwrap_or("-"),
            self.config.source_name,
            trade.side,
            source_notional,
            source_price * 100.0,
        );

        AutoCopyReport {
            action: format!("SKIP:{title}"),
            status: "skipped".to_owned(),
            reason,
            source_trade_key: source_trade_key(trade),
            market_title: trade.title.clone(),
            outcome: trade.outcome.clone(),
            position_key: position_key(trade),
            source_price: trade.price,
            source_notional_usd: source_notional,
            copy_amount_usd: 0.0,
            copy_price: None,
            order_id: None,
            realized_pnl_usd: None,
            market_exposure_after_usd: self.state.market_exposure_usd(&position_key(trade)),
            daily_spend_after_usd: self.state.daily_spend_usd + self.state.daily_reserved_buy_usd(),
            created_at_secs: now_secs(),
            text,
        }
    }

    fn persist_if_needed(&self, reports: &mut Vec<AutoCopyReport>) {
        if reports.is_empty() {
            return;
        }

        self.persist(reports);
    }

    fn persist(&self, reports: &mut Vec<AutoCopyReport>) {
        if let Err(error) = self.state.save(&self.config.state_path) {
            reports.push(AutoCopyReport::system(format!(
                "[{} 自动跟随状态保存失败]\n原因: {error}",
                self.config.source_name
            )));
        }
    }

    fn push_report(&mut self, report: AutoCopyReport, reports: &mut Vec<AutoCopyReport>, now: u64) {
        self.state.append_log(&report);
        if self.should_emit_report(&report, now) {
            reports.push(report);
        }
    }

    fn should_emit_report(&mut self, report: &AutoCopyReport, now: u64) -> bool {
        if !report.status.eq_ignore_ascii_case("failed") {
            return true;
        }

        let cooldown_seconds = self.config.failed_action_cooldown_seconds;
        if cooldown_seconds == 0 {
            return true;
        }

        let key = report_failure_cooldown_key(report);
        if self.state.failure_in_cooldown(&key, now, cooldown_seconds) {
            return false;
        }

        self.state.record_failure(key, report.reason.clone(), now);
        true
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutoCopyState {
    #[serde(default)]
    pub schema_version: u8,
    pub day_bucket: u64,
    pub daily_spend_usd: f64,
    pub daily_realized_pnl_usd: f64,
    pub positions: Vec<CopyPosition>,
    pub pending_orders: Vec<PendingCopyOrder>,
    pub processed_source_trades: Vec<String>,
    pub logs: Vec<AutoCopyLog>,
    #[serde(default)]
    pub failure_cooldowns: Vec<AutoCopyFailureCooldown>,
    #[serde(default)]
    pub source_buy_targets: Vec<SourceBuyTarget>,
    #[serde(default)]
    pub source_position_snapshots: Vec<SourcePositionSnapshot>,
    #[serde(default)]
    pub source_position_absences: Vec<SourcePositionAbsence>,
    #[serde(default)]
    pub source_outcomes: Vec<SourceOutcomeMetadata>,
    #[serde(default)]
    pub source_sell_coverages: Vec<SourceSellCoverage>,
    #[serde(default)]
    pub pending_exit_retries: Vec<PendingExitRetry>,
    #[serde(default)]
    pub recent_source_flows: Vec<SourceFlowEvent>,
    #[serde(default)]
    pub source_guards: Vec<SourceOutcomeGuard>,
    #[serde(default)]
    pub source_position_ledgers: Vec<SourcePositionLedger>,
    #[serde(default)]
    pub source_event_baskets: Vec<SourceEventBasket>,
}

impl Default for AutoCopyState {
    fn default() -> Self {
        Self {
            schema_version: AUTO_COPY_STATE_SCHEMA_VERSION,
            day_bucket: now_secs() / 86_400,
            daily_spend_usd: 0.0,
            daily_realized_pnl_usd: 0.0,
            positions: Vec::new(),
            pending_orders: Vec::new(),
            processed_source_trades: Vec::new(),
            logs: Vec::new(),
            failure_cooldowns: Vec::new(),
            source_buy_targets: Vec::new(),
            source_position_snapshots: Vec::new(),
            source_position_absences: Vec::new(),
            source_outcomes: Vec::new(),
            source_sell_coverages: Vec::new(),
            pending_exit_retries: Vec::new(),
            recent_source_flows: Vec::new(),
            source_guards: Vec::new(),
            source_position_ledgers: Vec::new(),
            source_event_baskets: Vec::new(),
        }
    }
}

impl AutoCopyState {
    fn load(path: &Path) -> Result<Self, AutoCopyError> {
        if !path.exists() {
            return Ok(Self::default());
        }

        let bytes = fs::read(path).map_err(AutoCopyError::Io)?;
        let mut state: Self = serde_json::from_slice(&bytes).map_err(AutoCopyError::Json)?;
        state.migrate();
        Ok(state)
    }

    fn migrate(&mut self) {
        if self.schema_version >= AUTO_COPY_STATE_SCHEMA_VERSION {
            return;
        }

        if self.schema_version < 3 {
            self.pending_exit_retries.clear();
            self.source_position_snapshots.clear();
            self.source_sell_coverages.clear();
        }
        if self.schema_version < 4 {
            self.source_buy_targets.clear();
            self.recent_source_flows.clear();
        }
        if self.schema_version < 5 {
            self.source_position_absences.clear();
        }
        if self.schema_version < 6 {
            self.source_position_ledgers.clear();
        }
        if self.schema_version < 7 {
            self.source_event_baskets.clear();
        }
        self.schema_version = AUTO_COPY_STATE_SCHEMA_VERSION;
    }

    fn save(&self, path: &Path) -> Result<(), AutoCopyError> {
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent).map_err(AutoCopyError::Io)?;
        }

        let json = serde_json::to_vec_pretty(self).map_err(AutoCopyError::Json)?;
        fs::write(path, json).map_err(AutoCopyError::Io)
    }

    fn reset_day_if_needed(&mut self, now: u64) {
        let current_day = now / 86_400;
        if self.day_bucket != current_day {
            self.day_bucket = current_day;
            self.daily_spend_usd = 0.0;
            self.daily_realized_pnl_usd = 0.0;
        }
    }

    fn daily_loss_usd(&self) -> f64 {
        (-self.daily_realized_pnl_usd).max(0.0)
    }

    fn daily_reserved_buy_usd(&self) -> f64 {
        self.pending_orders
            .iter()
            .filter(|order| order.side == "BUY")
            .map(|order| (order.copy_amount_usd - order.filled_amount_usd).max(0.0))
            .sum()
    }

    fn market_exposure_usd(&self, key: &str) -> f64 {
        let position_exposure = self
            .positions
            .iter()
            .find(|position| position.position_key == key)
            .map(|position| position.cost_usd.max(0.0))
            .unwrap_or(0.0);
        let pending_exposure = self
            .pending_orders
            .iter()
            .filter(|order| order.position_key == key && order.side == "BUY")
            .map(|order| (order.copy_amount_usd - order.filled_amount_usd).max(0.0))
            .sum::<f64>();

        position_exposure + pending_exposure
    }

    fn committed_buy_shares(&self, key: &str) -> f64 {
        let position_size = self
            .positions
            .iter()
            .find(|position| position.position_key == key)
            .map(|position| position.size_shares.max(0.0))
            .unwrap_or(0.0);
        let pending_size = self
            .pending_orders
            .iter()
            .filter(|order| order.position_key == key && order.side == "BUY")
            .map(|order| {
                let remaining_amount = (order.copy_amount_usd - order.filled_amount_usd).max(0.0);
                if order.limit_price > 0.0 {
                    remaining_amount / order.limit_price
                } else {
                    0.0
                }
            })
            .sum::<f64>();

        position_size + pending_size
    }

    fn has_processed_source_trade(&self, source_key: &str) -> bool {
        self.processed_source_trades
            .iter()
            .any(|key| key == source_key)
    }

    fn remember_processed_source_trade(&mut self, source_key: String) {
        if !self.has_processed_source_trade(&source_key) {
            self.processed_source_trades.push(source_key);
            if self.processed_source_trades.len() > PROCESSED_TRADE_LIMIT {
                let excess = self.processed_source_trades.len() - PROCESSED_TRADE_LIMIT;
                self.processed_source_trades.drain(0..excess);
            }
        }
    }

    fn position(&self, key: &str) -> Option<&CopyPosition> {
        self.positions
            .iter()
            .find(|position| position.position_key == key)
    }

    fn position_mut(&mut self, key: &str) -> Option<&mut CopyPosition> {
        self.positions
            .iter_mut()
            .find(|position| position.position_key == key)
    }

    fn pending_order(&self, local_order_id: &str) -> Option<&PendingCopyOrder> {
        self.pending_orders
            .iter()
            .find(|order| order.local_order_id == local_order_id)
    }

    fn pending_order_mut(&mut self, local_order_id: &str) -> Option<&mut PendingCopyOrder> {
        self.pending_orders
            .iter_mut()
            .find(|order| order.local_order_id == local_order_id)
    }

    fn remove_pending_order(&mut self, local_order_id: &str) {
        self.pending_orders
            .retain(|order| order.local_order_id != local_order_id);
    }

    fn apply_buy_fill(
        &mut self,
        key: &str,
        trade: &UserTrade,
        filled_amount_usd: f64,
        filled_size: f64,
    ) {
        if filled_amount_usd <= 0.0 || filled_size <= 0.0 {
            return;
        }

        if let Some(position) = self.position_mut(key) {
            position.size_shares += filled_size;
            position.cost_usd += filled_amount_usd;
            position.updated_at_secs = now_secs();
            return;
        }

        self.positions.push(CopyPosition {
            position_key: key.to_owned(),
            market_title: trade.title.clone(),
            outcome: trade.outcome.clone(),
            asset: trade.asset.clone(),
            condition_id: trade.condition_id.clone(),
            size_shares: filled_size,
            cost_usd: filled_amount_usd,
            realized_pnl_usd: 0.0,
            updated_at_secs: now_secs(),
        });
    }

    fn apply_buy_fill_from_order(
        &mut self,
        order: &PendingCopyOrder,
        filled_amount_usd: f64,
        filled_size: f64,
        filled_price: f64,
    ) {
        let synthetic_trade = UserTrade {
            proxy_wallet: WEATHERHK_WALLET.to_owned(),
            side: "BUY".to_owned(),
            asset: order.asset.clone(),
            condition_id: order.condition_id.clone(),
            size: Some(filled_size),
            price: Some(filled_price),
            timestamp: Some(now_secs()),
            title: order.market_title.clone(),
            slug: None,
            event_slug: None,
            outcome: order.outcome.clone(),
            outcome_index: None,
            name: None,
            pseudonym: None,
            transaction_hash: None,
        };
        self.apply_buy_fill(
            &order.position_key,
            &synthetic_trade,
            filled_amount_usd,
            filled_size,
        );
    }

    fn apply_sell_fill(
        &mut self,
        key: &str,
        sell_size: f64,
        _filled_amount_usd: f64,
        realized_pnl_usd: f64,
    ) {
        let Some(position) = self.position_mut(key) else {
            return;
        };
        if sell_size <= 0.0 || position.size_shares <= 0.0 {
            return;
        }

        let actual_sell_size = sell_size.min(position.size_shares);
        let avg_cost = position.avg_cost();
        position.size_shares -= actual_sell_size;
        position.cost_usd -= actual_sell_size * avg_cost;
        position.realized_pnl_usd += realized_pnl_usd;
        position.updated_at_secs = now_secs();

        if position.size_shares <= 0.000_001 {
            position.size_shares = 0.0;
            position.cost_usd = 0.0;
        }
    }

    fn sync_position_actual_balance(&mut self, key: &str, actual_balance_shares: f64) {
        let Some(position) = self.position_mut(key) else {
            return;
        };

        let actual_balance_shares = if actual_balance_shares <= MIN_TRACKED_ACTUAL_BALANCE_SHARES {
            0.0
        } else {
            actual_balance_shares
        };
        let avg_cost = position.avg_cost();
        position.size_shares = actual_balance_shares;
        position.cost_usd = actual_balance_shares * avg_cost;
        position.updated_at_secs = now_secs();
    }

    fn append_log(&mut self, report: &AutoCopyReport) {
        self.logs.push(AutoCopyLog {
            created_at_secs: report.created_at_secs,
            action: report.action.clone(),
            status: report.status.clone(),
            reason: report.reason.clone(),
            source_trade_key: report.source_trade_key.clone(),
            position_key: report.position_key.clone(),
            source_notional_usd: report.source_notional_usd,
            copy_amount_usd: report.copy_amount_usd,
            copy_price: report.copy_price,
            order_id: report.order_id.clone(),
            realized_pnl_usd: report.realized_pnl_usd,
        });

        if self.logs.len() > STATE_LOG_LIMIT {
            let excess = self.logs.len() - STATE_LOG_LIMIT;
            self.logs.drain(0..excess);
        }
    }

    fn failure_in_cooldown(&self, key: &str, now: u64, cooldown_seconds: u64) -> bool {
        if cooldown_seconds == 0 {
            return false;
        }

        if let Some(cooldown) = self
            .failure_cooldowns
            .iter()
            .find(|cooldown| cooldown.key == key)
        {
            now.saturating_sub(cooldown.last_failed_at_secs) < cooldown_seconds
        } else {
            false
        }
    }

    fn record_failure(&mut self, key: String, message: String, now: u64) {
        if let Some(cooldown) = self
            .failure_cooldowns
            .iter_mut()
            .find(|cooldown| cooldown.key == key)
        {
            cooldown.last_failed_at_secs = now;
            cooldown.message = message;
        } else {
            self.failure_cooldowns.push(AutoCopyFailureCooldown {
                key,
                last_failed_at_secs: now,
                message,
            });
        }

        if self.failure_cooldowns.len() > FAILURE_COOLDOWN_LIMIT {
            let excess = self.failure_cooldowns.len() - FAILURE_COOLDOWN_LIMIT;
            self.failure_cooldowns.drain(0..excess);
        }
    }

    fn clear_failure(&mut self, key: &str) {
        self.failure_cooldowns
            .retain(|cooldown| cooldown.key != key);
    }

    fn prune_source_memory(&mut self, now: u64, retention_seconds: u64) {
        self.recent_source_flows.retain(|flow| {
            retention_seconds == 0 || now.saturating_sub(flow.timestamp_secs) <= retention_seconds
        });
        self.source_outcomes.retain(|outcome| {
            SOURCE_OUTCOME_METADATA_RETENTION_SECONDS == 0
                || now.saturating_sub(outcome.last_seen_at_secs)
                    <= SOURCE_OUTCOME_METADATA_RETENTION_SECONDS
        });
        self.source_guards
            .retain(|guard| guard.expires_at_secs == 0 || guard.expires_at_secs > now);
        self.source_position_absences.retain(|absence| {
            now.saturating_sub(absence.last_missing_at_secs)
                <= SOURCE_POSITION_ABSENCE_RETENTION_SECONDS
        });
        self.source_event_baskets.retain(|basket| {
            SOURCE_OUTCOME_METADATA_RETENTION_SECONDS == 0
                || now.saturating_sub(basket.last_seen_at_secs)
                    <= SOURCE_OUTCOME_METADATA_RETENTION_SECONDS
        });
    }

    fn record_source_outcome(&mut self, trade: &UserTrade, event_time: u64) {
        let key = position_key(trade);
        if trade.side.eq_ignore_ascii_case("BUY") {
            self.clear_source_position_absence(&key, &trade.asset);
        }
        if let Some(outcome) = self
            .source_outcomes
            .iter_mut()
            .find(|outcome| outcome.position_key == key)
        {
            outcome.market_title = trade.title.clone().or_else(|| outcome.market_title.clone());
            outcome.outcome = trade.outcome.clone().or_else(|| outcome.outcome.clone());
            outcome.slug = trade.slug.clone().or_else(|| outcome.slug.clone());
            outcome.event_slug = trade
                .event_slug
                .clone()
                .or_else(|| outcome.event_slug.clone());
            outcome.last_price = trade.price.or(outcome.last_price);
            if trade.side.eq_ignore_ascii_case("BUY") {
                outcome.last_buy_price = trade.price.or(outcome.last_buy_price);
                outcome.last_buy_at_secs = Some(event_time);
            } else if trade.side.eq_ignore_ascii_case("SELL") {
                outcome.last_sell_at_secs = Some(event_time);
            }
            outcome.last_seen_at_secs = event_time;
            return;
        }

        self.source_outcomes.push(SourceOutcomeMetadata {
            position_key: key,
            asset: trade.asset.clone(),
            condition_id: trade.condition_id.clone(),
            market_title: trade.title.clone(),
            outcome: trade.outcome.clone(),
            slug: trade.slug.clone(),
            event_slug: trade.event_slug.clone(),
            last_price: trade.price,
            last_buy_price: trade
                .side
                .eq_ignore_ascii_case("BUY")
                .then_some(trade.price)
                .flatten(),
            first_seen_at_secs: event_time,
            last_seen_at_secs: event_time,
            last_buy_at_secs: trade.side.eq_ignore_ascii_case("BUY").then_some(event_time),
            last_sell_at_secs: trade
                .side
                .eq_ignore_ascii_case("SELL")
                .then_some(event_time),
        });

        if self.source_outcomes.len() > SOURCE_OUTCOME_METADATA_LIMIT {
            let excess = self.source_outcomes.len() - SOURCE_OUTCOME_METADATA_LIMIT;
            self.source_outcomes.drain(0..excess);
        }
    }

    fn source_outcome_by_asset(&self, asset: &str) -> Option<&SourceOutcomeMetadata> {
        self.source_outcomes
            .iter()
            .filter(|outcome| outcome.asset == asset)
            .max_by_key(|outcome| outcome.last_seen_at_secs)
    }

    fn record_source_flow(&mut self, trade: &UserTrade, source_trade_key: String, now: u64) {
        if self
            .recent_source_flows
            .iter()
            .any(|flow| flow.source_trade_key == source_trade_key)
        {
            return;
        }

        let price = trade.price.unwrap_or(0.0);
        let notional_usd = trade
            .size
            .filter(|size| *size > 0.0)
            .map(|size| size * price)
            .unwrap_or(0.0);

        self.recent_source_flows.push(SourceFlowEvent {
            source_trade_key,
            position_key: position_key(trade),
            side: trade.side.to_uppercase(),
            notional_usd,
            size_shares: trade.size.unwrap_or(0.0).max(0.0),
            price: trade.price,
            timestamp_secs: trade.timestamp.unwrap_or(now),
            recorded_at_secs: now,
        });

        if self.recent_source_flows.len() > SOURCE_FLOW_LIMIT {
            let excess = self.recent_source_flows.len() - SOURCE_FLOW_LIMIT;
            self.recent_source_flows.drain(0..excess);
        }
    }

    fn record_source_event_basket_trade(
        &mut self,
        trade: &UserTrade,
        event_time: u64,
    ) -> Option<SourceEventBasket> {
        let event_slug = event_slug_for_trade(trade)?;
        let size = trade.size.filter(|size| *size > 0.0)?;
        let price = trade.price.filter(|price| *price > 0.0 && *price <= 1.0)?;
        let side = trade.side.to_uppercase();
        if side != "BUY" && side != "SELL" {
            return None;
        }

        let notional = size * price;
        let key = position_key(trade);
        if let Some(basket) = self
            .source_event_baskets
            .iter_mut()
            .find(|basket| basket.event_slug == event_slug)
        {
            basket.last_seen_at_secs = basket.last_seen_at_secs.max(event_time);
            if side == "BUY" {
                basket.buy_notional_usd += notional;
                basket.buy_size_shares += size;
            } else {
                basket.sell_notional_usd += notional;
                basket.sell_size_shares += size;
            }
            basket.upsert_trade_outcome(trade, &key, price, size, notional, &side, event_time);
            return Some(basket.clone());
        }

        let mut basket = SourceEventBasket {
            event_slug,
            first_seen_at_secs: event_time,
            last_seen_at_secs: event_time,
            buy_notional_usd: if side == "BUY" { notional } else { 0.0 },
            sell_notional_usd: if side == "SELL" { notional } else { 0.0 },
            buy_size_shares: if side == "BUY" { size } else { 0.0 },
            sell_size_shares: if side == "SELL" { size } else { 0.0 },
            outcomes: Vec::new(),
        };
        basket.upsert_trade_outcome(trade, &key, price, size, notional, &side, event_time);
        self.source_event_baskets.push(basket.clone());
        if self.source_event_baskets.len() > SOURCE_EVENT_BASKET_LIMIT {
            let excess = self.source_event_baskets.len() - SOURCE_EVENT_BASKET_LIMIT;
            self.source_event_baskets.drain(0..excess);
        }
        Some(basket)
    }

    fn source_event_basket(&self, event_slug: &str) -> Option<&SourceEventBasket> {
        self.source_event_baskets
            .iter()
            .find(|basket| basket.event_slug == event_slug)
    }

    fn event_committed_usd(&self, event_slug: &str) -> f64 {
        let Some(basket) = self.source_event_basket(event_slug) else {
            return 0.0;
        };
        basket
            .outcomes
            .iter()
            .map(|outcome| self.market_exposure_usd(&outcome.position_key))
            .sum()
    }

    fn source_position_ledger(&self, asset: &str) -> Option<&SourcePositionLedger> {
        self.source_position_ledgers
            .iter()
            .find(|ledger| ledger.asset == asset)
    }

    fn record_source_position_ledger_trade(
        &mut self,
        trade: &UserTrade,
        event_time: u64,
    ) -> Option<SourcePositionLedger> {
        let size = trade.size.filter(|size| *size > 0.0)?;
        let price = trade.price.filter(|price| *price > 0.0 && *price <= 1.0)?;
        let side = trade.side.to_uppercase();
        if side != "BUY" && side != "SELL" {
            return None;
        }

        let key = position_key(trade);
        let notional = size * price;
        if let Some(ledger) = self
            .source_position_ledgers
            .iter_mut()
            .find(|ledger| ledger.position_key == key)
        {
            ledger.market_title = trade.title.clone().or_else(|| ledger.market_title.clone());
            ledger.outcome = trade.outcome.clone().or_else(|| ledger.outcome.clone());
            ledger.last_price = Some(price);
            ledger.last_trade_at_secs = ledger.last_trade_at_secs.max(event_time);

            if side == "BUY" {
                let previous_net = ledger.net_size_shares.max(0.0);
                let previous_cost = ledger
                    .avg_entry_price
                    .map(|avg| avg * previous_net)
                    .unwrap_or(0.0);
                ledger.net_size_shares = previous_net + size;
                ledger.avg_entry_price = if ledger.net_size_shares > 0.0 {
                    Some((previous_cost + notional) / ledger.net_size_shares)
                } else {
                    Some(price)
                };
                ledger.buy_notional_usd += notional;
                ledger.buy_size_shares += size;
                ledger.last_buy_at_secs = Some(event_time);
            } else {
                ledger.net_size_shares = (ledger.net_size_shares - size).max(0.0);
                if ledger.net_size_shares <= 0.000_001 {
                    ledger.net_size_shares = 0.0;
                }
                ledger.sell_notional_usd += notional;
                ledger.sell_size_shares += size;
                ledger.last_sell_at_secs = Some(event_time);
            }

            return Some(ledger.clone());
        }

        let net_size_shares = if side == "BUY" { size } else { 0.0 };
        let ledger = SourcePositionLedger {
            position_key: key,
            asset: trade.asset.clone(),
            condition_id: trade.condition_id.clone(),
            market_title: trade.title.clone(),
            outcome: trade.outcome.clone(),
            net_size_shares,
            avg_entry_price: (side == "BUY").then_some(price),
            last_price: Some(price),
            buy_notional_usd: if side == "BUY" { notional } else { 0.0 },
            sell_notional_usd: if side == "SELL" { notional } else { 0.0 },
            buy_size_shares: if side == "BUY" { size } else { 0.0 },
            sell_size_shares: if side == "SELL" { size } else { 0.0 },
            first_seen_at_secs: event_time,
            last_trade_at_secs: event_time,
            last_buy_at_secs: (side == "BUY").then_some(event_time),
            last_sell_at_secs: (side == "SELL").then_some(event_time),
            last_calibrated_at_secs: None,
        };
        self.source_position_ledgers.push(ledger.clone());
        if self.source_position_ledgers.len() > SOURCE_POSITION_LEDGER_LIMIT {
            let excess = self.source_position_ledgers.len() - SOURCE_POSITION_LEDGER_LIMIT;
            self.source_position_ledgers.drain(0..excess);
        }
        Some(ledger)
    }

    fn calibrate_source_position_ledgers(
        &mut self,
        positions: &HashMap<String, ObservedSourcePosition>,
        observed_at_secs: u64,
    ) {
        for (asset, position) in positions {
            if position.size_shares <= 0.0 {
                continue;
            }
            let metadata = position
                .to_metadata(asset, observed_at_secs)
                .unwrap_or_else(|| SourceOutcomeMetadata {
                    position_key: asset.to_owned(),
                    asset: asset.clone(),
                    condition_id: String::new(),
                    market_title: position.market_title.clone(),
                    outcome: position.outcome.clone(),
                    slug: position.slug.clone(),
                    event_slug: position.event_slug.clone(),
                    last_price: position.current_price,
                    last_buy_price: position.avg_price,
                    first_seen_at_secs: observed_at_secs,
                    last_seen_at_secs: observed_at_secs,
                    last_buy_at_secs: None,
                    last_sell_at_secs: None,
                });
            if let Some(ledger) = self
                .source_position_ledgers
                .iter_mut()
                .find(|ledger| ledger.asset == *asset)
            {
                ledger.position_key = metadata.position_key;
                ledger.condition_id = metadata.condition_id;
                ledger.market_title = position
                    .market_title
                    .clone()
                    .or_else(|| ledger.market_title.clone());
                ledger.outcome = position.outcome.clone().or_else(|| ledger.outcome.clone());
                ledger.net_size_shares = position.size_shares;
                ledger.avg_entry_price = position.avg_price.or(ledger.avg_entry_price);
                ledger.last_price = position.current_price.or(ledger.last_price);
                ledger.last_trade_at_secs = ledger.last_trade_at_secs.max(observed_at_secs);
                ledger.last_calibrated_at_secs = Some(observed_at_secs);
            } else {
                self.source_position_ledgers.push(SourcePositionLedger {
                    position_key: metadata.position_key,
                    asset: asset.clone(),
                    condition_id: metadata.condition_id,
                    market_title: position.market_title.clone(),
                    outcome: position.outcome.clone(),
                    net_size_shares: position.size_shares,
                    avg_entry_price: position.avg_price,
                    last_price: position.current_price,
                    buy_notional_usd: position
                        .avg_price
                        .map(|avg| avg * position.size_shares)
                        .unwrap_or(0.0),
                    sell_notional_usd: 0.0,
                    buy_size_shares: position.size_shares,
                    sell_size_shares: 0.0,
                    first_seen_at_secs: observed_at_secs,
                    last_trade_at_secs: observed_at_secs,
                    last_buy_at_secs: None,
                    last_sell_at_secs: None,
                    last_calibrated_at_secs: Some(observed_at_secs),
                });
            }
        }

        if self.source_position_ledgers.len() > SOURCE_POSITION_LEDGER_LIMIT {
            let excess = self.source_position_ledgers.len() - SOURCE_POSITION_LEDGER_LIMIT;
            self.source_position_ledgers.drain(0..excess);
        }
    }

    fn record_source_buy_target(
        &mut self,
        position_key: &str,
        asset: &str,
        source_notional_usd: f64,
        source_size_shares: f64,
        event_time: u64,
    ) -> SourceBuyTarget {
        if let Some(target) = self
            .source_buy_targets
            .iter_mut()
            .find(|target| target.position_key == position_key)
        {
            target.source_buy_notional_usd += source_notional_usd;
            target.source_buy_size_shares += source_size_shares;
            target.last_buy_at_secs = target.last_buy_at_secs.max(event_time);
            return target.clone();
        }

        let cutoff = event_time.saturating_sub(SOURCE_BUY_BOOTSTRAP_WINDOW_SECONDS);
        let last_sell_at = self
            .recent_source_flows
            .iter()
            .filter(|flow| {
                flow.position_key == position_key
                    && flow.side == "SELL"
                    && flow.timestamp_secs >= cutoff
                    && flow.timestamp_secs <= event_time
            })
            .map(|flow| flow.timestamp_secs)
            .max();
        let first_allowed_buy_at = last_sell_at.unwrap_or(cutoff);
        let recent_buys = self
            .recent_source_flows
            .iter()
            .filter(|flow| {
                flow.position_key == position_key
                    && flow.side == "BUY"
                    && flow.timestamp_secs >= first_allowed_buy_at
                    && flow.timestamp_secs <= event_time
            })
            .collect::<Vec<_>>();
        let accumulated = recent_buys
            .iter()
            .map(|flow| flow.notional_usd)
            .sum::<f64>()
            .max(source_notional_usd);
        let accumulated_size = recent_buys
            .iter()
            .map(|flow| flow.size_shares)
            .sum::<f64>()
            .max(source_size_shares);
        let first_buy_at_secs = recent_buys
            .iter()
            .map(|flow| flow.timestamp_secs)
            .min()
            .unwrap_or(event_time);

        let target = SourceBuyTarget {
            position_key: position_key.to_owned(),
            asset: asset.to_owned(),
            source_buy_notional_usd: accumulated,
            source_buy_size_shares: accumulated_size,
            first_buy_at_secs,
            last_buy_at_secs: event_time,
        };
        self.source_buy_targets.push(target.clone());
        target
    }

    fn clear_source_buy_target(&mut self, position_key: &str) {
        self.source_buy_targets
            .retain(|target| target.position_key != position_key);
    }

    fn source_position_size(&self, asset: &str) -> Option<f64> {
        self.source_position_snapshots
            .iter()
            .find(|snapshot| snapshot.asset == asset)
            .map(|snapshot| snapshot.size_shares)
    }

    fn source_position_snapshot(&self, asset: &str) -> Option<&SourcePositionSnapshot> {
        self.source_position_snapshots
            .iter()
            .find(|snapshot| snapshot.asset == asset)
    }

    fn clear_source_position_absence(&mut self, position_key: &str, asset: &str) {
        self.source_position_absences
            .retain(|absence| absence.position_key != position_key && absence.asset != asset);
    }

    fn record_source_position_absence(
        &mut self,
        position: &CopyPosition,
        previous_snapshot: Option<&SourcePositionSnapshot>,
        now: u64,
    ) -> SourcePositionAbsence {
        if let Some(absence) = self
            .source_position_absences
            .iter_mut()
            .find(|absence| absence.position_key == position.position_key)
        {
            absence.last_missing_at_secs = now;
            absence.missing_count = absence.missing_count.saturating_add(1);
            if let Some(snapshot) = previous_snapshot {
                absence.last_seen_source_size_shares = Some(snapshot.size_shares);
                absence.last_seen_source_at_secs = Some(snapshot.observed_at_secs);
            }
            return absence.clone();
        }

        let absence = SourcePositionAbsence {
            position_key: position.position_key.clone(),
            asset: position.asset.clone(),
            first_missing_at_secs: now,
            last_missing_at_secs: now,
            missing_count: 1,
            last_seen_source_size_shares: previous_snapshot.map(|snapshot| snapshot.size_shares),
            last_seen_source_at_secs: previous_snapshot.map(|snapshot| snapshot.observed_at_secs),
        };
        self.source_position_absences.push(absence.clone());
        absence
    }

    fn replace_source_position_snapshots(
        &mut self,
        positions: &HashMap<String, ObservedSourcePosition>,
        observed_at_secs: u64,
    ) {
        self.calibrate_source_position_ledgers(positions, observed_at_secs);
        self.source_position_snapshots = positions
            .iter()
            .filter(|(_, position)| position.size_shares > 0.0)
            .map(|(asset, position)| SourcePositionSnapshot {
                asset: asset.clone(),
                size_shares: position.size_shares,
                observed_at_secs,
            })
            .collect();
        self.source_position_absences
            .retain(|absence| !positions.contains_key(&absence.asset));
    }

    fn consume_source_sell_coverage(
        &mut self,
        asset: &str,
        requested_size: f64,
        now: u64,
        ttl_seconds: u64,
    ) -> f64 {
        self.source_sell_coverages.retain(|coverage| {
            coverage.remaining_shares > 0.000_001
                && now.saturating_sub(coverage.created_at_secs) <= ttl_seconds
        });
        let Some(coverage) = self
            .source_sell_coverages
            .iter_mut()
            .find(|coverage| coverage.asset == asset)
        else {
            return 0.0;
        };
        let consumed = requested_size.min(coverage.remaining_shares).max(0.0);
        coverage.remaining_shares -= consumed;
        if coverage.remaining_shares <= 0.000_001 {
            self.source_sell_coverages
                .retain(|coverage| coverage.asset != asset);
        }
        consumed
    }

    fn record_source_sell_coverage(&mut self, asset: &str, shares: f64, now: u64) {
        if shares <= 0.000_001 {
            return;
        }
        if let Some(coverage) = self
            .source_sell_coverages
            .iter_mut()
            .find(|coverage| coverage.asset == asset)
        {
            coverage.remaining_shares += shares;
            coverage.created_at_secs = now;
        } else {
            self.source_sell_coverages.push(SourceSellCoverage {
                asset: asset.to_owned(),
                remaining_shares: shares,
                created_at_secs: now,
            });
        }
    }

    fn clear_source_sell_coverage(&mut self, asset: &str) {
        self.source_sell_coverages
            .retain(|coverage| coverage.asset != asset);
    }

    fn record_exit_retry(&mut self, retry: PendingExitRetry) {
        if let Some(existing) = self
            .pending_exit_retries
            .iter_mut()
            .find(|existing| existing.position_key == retry.position_key)
        {
            *existing = retry;
        } else {
            self.pending_exit_retries.push(retry);
        }
    }

    fn exit_retry(&self, position_key: &str) -> Option<&PendingExitRetry> {
        self.pending_exit_retries
            .iter()
            .find(|retry| retry.position_key == position_key)
    }

    fn clear_exit_retry(&mut self, position_key: &str) {
        self.pending_exit_retries
            .retain(|retry| retry.position_key != position_key);
    }

    fn source_flow_stats(
        &self,
        position_key: &str,
        now: u64,
        window_seconds: u64,
    ) -> SourceFlowStats {
        let mut stats = SourceFlowStats::default();
        for flow in &self.recent_source_flows {
            if flow.position_key != position_key {
                continue;
            }
            if window_seconds > 0 && now.saturating_sub(flow.timestamp_secs) > window_seconds {
                continue;
            }

            match flow.side.as_str() {
                "BUY" => {
                    stats.buy_count += 1;
                    stats.buy_notional_usd += flow.notional_usd;
                }
                "SELL" => {
                    stats.sell_count += 1;
                    stats.sell_notional_usd += flow.notional_usd;
                    stats.first_sell_at_secs = Some(
                        stats
                            .first_sell_at_secs
                            .map_or(flow.timestamp_secs, |first| first.min(flow.timestamp_secs)),
                    );
                    stats.last_sell_at_secs = Some(
                        stats
                            .last_sell_at_secs
                            .map_or(flow.timestamp_secs, |last| last.max(flow.timestamp_secs)),
                    );
                }
                _ => {}
            }
        }

        stats
    }

    fn active_source_guard(&self, position_key: &str, now: u64) -> Option<&SourceOutcomeGuard> {
        self.source_guards.iter().find(|guard| {
            guard.position_key == position_key
                && (guard.expires_at_secs == 0 || guard.expires_at_secs > now)
        })
    }

    fn record_source_guard(
        &mut self,
        position_key: String,
        kind: SourceGuardKind,
        reason: String,
        event_time: u64,
        guard_seconds: u64,
    ) {
        if guard_seconds == 0 {
            return;
        }

        let expires_at_secs = event_time.saturating_add(guard_seconds);
        if let Some(guard) = self
            .source_guards
            .iter_mut()
            .find(|guard| guard.position_key == position_key)
        {
            if guard.kind == SourceGuardKind::Pressure && kind == SourceGuardKind::PostSell {
                guard.expires_at_secs = guard.expires_at_secs.max(expires_at_secs);
                return;
            }

            guard.kind = kind;
            guard.reason = reason;
            guard.created_at_secs = event_time;
            guard.expires_at_secs = expires_at_secs;
            return;
        }

        self.source_guards.push(SourceOutcomeGuard {
            position_key,
            kind,
            reason,
            created_at_secs: event_time,
            expires_at_secs,
        });
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CopyPosition {
    pub position_key: String,
    pub market_title: Option<String>,
    pub outcome: Option<String>,
    pub asset: String,
    pub condition_id: String,
    pub size_shares: f64,
    pub cost_usd: f64,
    pub realized_pnl_usd: f64,
    pub updated_at_secs: u64,
}

impl CopyPosition {
    fn avg_cost(&self) -> f64 {
        if self.size_shares > 0.0 {
            self.cost_usd / self.size_shares
        } else {
            0.0
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingCopyOrder {
    pub local_order_id: String,
    pub external_order_id: Option<String>,
    pub position_key: String,
    pub side: String,
    pub market_title: Option<String>,
    pub outcome: Option<String>,
    pub asset: String,
    pub condition_id: String,
    pub copy_amount_usd: f64,
    pub limit_price: f64,
    #[serde(default)]
    pub requested_limit_price: Option<f64>,
    pub filled_amount_usd: f64,
    pub filled_size: f64,
    pub created_at_secs: u64,
    pub expires_at_secs: u64,
    pub last_sync_at_secs: u64,
    pub source_trade_key: String,
    #[serde(default)]
    pub source_trade_at_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutoCopyLog {
    pub created_at_secs: u64,
    pub action: String,
    pub status: String,
    pub reason: String,
    pub source_trade_key: String,
    pub position_key: String,
    pub source_notional_usd: f64,
    pub copy_amount_usd: f64,
    pub copy_price: Option<f64>,
    pub order_id: Option<String>,
    pub realized_pnl_usd: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutoCopyFailureCooldown {
    pub key: String,
    pub last_failed_at_secs: u64,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceBuyTarget {
    pub position_key: String,
    pub asset: String,
    pub source_buy_notional_usd: f64,
    #[serde(default)]
    pub source_buy_size_shares: f64,
    pub first_buy_at_secs: u64,
    pub last_buy_at_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourcePositionSnapshot {
    pub asset: String,
    pub size_shares: f64,
    pub observed_at_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourcePositionLedger {
    pub position_key: String,
    pub asset: String,
    pub condition_id: String,
    pub market_title: Option<String>,
    pub outcome: Option<String>,
    pub net_size_shares: f64,
    pub avg_entry_price: Option<f64>,
    pub last_price: Option<f64>,
    pub buy_notional_usd: f64,
    pub sell_notional_usd: f64,
    pub buy_size_shares: f64,
    pub sell_size_shares: f64,
    pub first_seen_at_secs: u64,
    pub last_trade_at_secs: u64,
    pub last_buy_at_secs: Option<u64>,
    pub last_sell_at_secs: Option<u64>,
    #[serde(default)]
    pub last_calibrated_at_secs: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourcePositionAbsence {
    pub position_key: String,
    pub asset: String,
    pub first_missing_at_secs: u64,
    pub last_missing_at_secs: u64,
    pub missing_count: u32,
    #[serde(default)]
    pub last_seen_source_size_shares: Option<f64>,
    #[serde(default)]
    pub last_seen_source_at_secs: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct ObservedSourcePosition {
    pub size_shares: f64,
    pub avg_price: Option<f64>,
    pub current_price: Option<f64>,
    pub end_date: Option<String>,
    pub condition_id: Option<String>,
    pub market_title: Option<String>,
    pub outcome: Option<String>,
    pub slug: Option<String>,
    pub event_slug: Option<String>,
}

impl ObservedSourcePosition {
    fn to_metadata(&self, asset: &str, now: u64) -> Option<SourceOutcomeMetadata> {
        let condition_id = self
            .condition_id
            .as_deref()
            .filter(|value| !value.trim().is_empty())?
            .to_owned();
        Some(SourceOutcomeMetadata {
            position_key: format!("{condition_id}:{asset}"),
            asset: asset.to_owned(),
            condition_id,
            market_title: self.market_title.clone(),
            outcome: self.outcome.clone(),
            slug: self.slug.clone(),
            event_slug: self.event_slug.clone(),
            last_price: self.current_price,
            last_buy_price: None,
            first_seen_at_secs: now,
            last_seen_at_secs: now,
            last_buy_at_secs: None,
            last_sell_at_secs: None,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceOutcomeMetadata {
    pub position_key: String,
    pub asset: String,
    pub condition_id: String,
    pub market_title: Option<String>,
    pub outcome: Option<String>,
    pub slug: Option<String>,
    pub event_slug: Option<String>,
    pub last_price: Option<f64>,
    pub last_buy_price: Option<f64>,
    pub first_seen_at_secs: u64,
    pub last_seen_at_secs: u64,
    pub last_buy_at_secs: Option<u64>,
    #[serde(default)]
    pub last_sell_at_secs: Option<u64>,
}

impl SourceOutcomeMetadata {
    fn synthetic_trade(
        &self,
        side: &str,
        price: f64,
        source_size_shares: f64,
        now: u64,
    ) -> UserTrade {
        UserTrade {
            proxy_wallet: WEATHERHK_WALLET.to_owned(),
            side: side.to_owned(),
            asset: self.asset.clone(),
            condition_id: self.condition_id.clone(),
            size: Some(source_size_shares),
            price: Some(price),
            timestamp: Some(now),
            title: self.market_title.clone(),
            slug: self.slug.clone(),
            event_slug: self.event_slug.clone(),
            outcome: self.outcome.clone(),
            outcome_index: None,
            name: Some("WeatherHK".to_owned()),
            pseudonym: None,
            transaction_hash: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceSellCoverage {
    pub asset: String,
    pub remaining_shares: f64,
    pub created_at_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingExitRetry {
    pub position_key: String,
    pub sell_fraction: f64,
    #[serde(default)]
    pub target_size_shares: Option<f64>,
    pub min_sell_price: Option<f64>,
    pub force_market_sell: bool,
    pub lock_profit: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceFlowEvent {
    pub source_trade_key: String,
    pub position_key: String,
    pub side: String,
    pub notional_usd: f64,
    #[serde(default)]
    pub size_shares: f64,
    pub price: Option<f64>,
    pub timestamp_secs: u64,
    pub recorded_at_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceEventBasket {
    pub event_slug: String,
    pub first_seen_at_secs: u64,
    pub last_seen_at_secs: u64,
    pub buy_notional_usd: f64,
    pub sell_notional_usd: f64,
    pub buy_size_shares: f64,
    pub sell_size_shares: f64,
    pub outcomes: Vec<SourceEventBasketOutcome>,
}

impl SourceEventBasket {
    fn buy_outcome_count(&self) -> usize {
        self.outcomes
            .iter()
            .filter(|outcome| outcome.buy_notional_usd > 0.0)
            .count()
    }

    fn outcome(&self, position_key: &str) -> Option<&SourceEventBasketOutcome> {
        self.outcomes
            .iter()
            .find(|outcome| outcome.position_key == position_key)
    }

    fn upsert_trade_outcome(
        &mut self,
        trade: &UserTrade,
        position_key: &str,
        price: f64,
        size: f64,
        notional: f64,
        side: &str,
        event_time: u64,
    ) {
        if let Some(outcome) = self
            .outcomes
            .iter_mut()
            .find(|outcome| outcome.position_key == position_key)
        {
            outcome.market_title = trade.title.clone().or_else(|| outcome.market_title.clone());
            outcome.outcome = trade.outcome.clone().or_else(|| outcome.outcome.clone());
            outcome.last_price = Some(price);
            outcome.last_seen_at_secs = outcome.last_seen_at_secs.max(event_time);
            if side == "BUY" {
                let previous_cost = outcome
                    .avg_buy_price
                    .map(|avg| avg * outcome.buy_size_shares)
                    .unwrap_or(0.0);
                outcome.buy_notional_usd += notional;
                outcome.buy_size_shares += size;
                outcome.avg_buy_price = if outcome.buy_size_shares > 0.0 {
                    Some((previous_cost + notional) / outcome.buy_size_shares)
                } else {
                    Some(price)
                };
                outcome.last_buy_at_secs = Some(event_time);
            } else {
                outcome.sell_notional_usd += notional;
                outcome.sell_size_shares += size;
                outcome.last_sell_at_secs = Some(event_time);
            }
            return;
        }

        self.outcomes.push(SourceEventBasketOutcome {
            position_key: position_key.to_owned(),
            asset: trade.asset.clone(),
            condition_id: trade.condition_id.clone(),
            market_title: trade.title.clone(),
            outcome: trade.outcome.clone(),
            buy_notional_usd: if side == "BUY" { notional } else { 0.0 },
            sell_notional_usd: if side == "SELL" { notional } else { 0.0 },
            buy_size_shares: if side == "BUY" { size } else { 0.0 },
            sell_size_shares: if side == "SELL" { size } else { 0.0 },
            avg_buy_price: (side == "BUY").then_some(price),
            last_price: Some(price),
            first_seen_at_secs: event_time,
            last_seen_at_secs: event_time,
            last_buy_at_secs: (side == "BUY").then_some(event_time),
            last_sell_at_secs: (side == "SELL").then_some(event_time),
        });
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceEventBasketOutcome {
    pub position_key: String,
    pub asset: String,
    pub condition_id: String,
    pub market_title: Option<String>,
    pub outcome: Option<String>,
    pub buy_notional_usd: f64,
    pub sell_notional_usd: f64,
    pub buy_size_shares: f64,
    pub sell_size_shares: f64,
    pub avg_buy_price: Option<f64>,
    pub last_price: Option<f64>,
    pub first_seen_at_secs: u64,
    pub last_seen_at_secs: u64,
    pub last_buy_at_secs: Option<u64>,
    pub last_sell_at_secs: Option<u64>,
}

impl SourceEventBasketOutcome {
    fn net_buy_notional_usd(&self) -> f64 {
        (self.buy_notional_usd - self.sell_notional_usd).max(0.0)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceOutcomeGuard {
    pub position_key: String,
    pub kind: SourceGuardKind,
    pub reason: String,
    pub created_at_secs: u64,
    pub expires_at_secs: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SourceGuardKind {
    PostSell,
    Pressure,
}

#[derive(Debug, Default, Clone)]
struct SourceFlowStats {
    buy_count: usize,
    sell_count: usize,
    buy_notional_usd: f64,
    sell_notional_usd: f64,
    first_sell_at_secs: Option<u64>,
    last_sell_at_secs: Option<u64>,
}

#[derive(Debug, Clone)]
struct SourceSellDecision {
    sell_fraction: f64,
    clear_all: bool,
    reason: String,
}

#[derive(Debug, Clone)]
enum SourcePositionAbsenceAction {
    Sell,
    Wait(AutoCopyReport),
    Ignore,
}

#[derive(Debug, Clone)]
struct TargetReconcileBuy {
    metadata: SourceOutcomeMetadata,
    source_size_shares: f64,
    target_amount_usd: f64,
    committed_amount_usd: f64,
    target_size_shares: f64,
    committed_size_shares: f64,
    missing_size_shares: f64,
    source_avg_cost: f64,
    source_entry_price: f64,
    high_price_reference: f64,
    copy_fraction: f64,
    proportional_target_amount_usd: f64,
    limit_price: f64,
    copy_amount_usd: f64,
}

#[derive(Debug, Clone)]
struct BuySizingDecision {
    target_copy_amount_usd: f64,
    target_cap_note: String,
    target_description: String,
    event_budget_usd: Option<f64>,
    event_committed_usd: Option<f64>,
    event_remaining_usd: Option<f64>,
}

impl SourceFlowStats {
    fn avg_sell_gap_seconds(&self) -> Option<f64> {
        if self.sell_count < 2 {
            return None;
        }

        let first = self.first_sell_at_secs?;
        let last = self.last_sell_at_secs?;
        Some(last.saturating_sub(first) as f64 / (self.sell_count - 1) as f64)
    }
}

#[derive(Debug, Clone)]
pub struct AutoCopyReport {
    pub action: String,
    pub status: String,
    pub reason: String,
    pub source_trade_key: String,
    pub market_title: Option<String>,
    pub outcome: Option<String>,
    pub position_key: String,
    pub source_price: Option<f64>,
    pub source_notional_usd: f64,
    pub copy_amount_usd: f64,
    pub copy_price: Option<f64>,
    pub order_id: Option<String>,
    pub realized_pnl_usd: Option<f64>,
    pub market_exposure_after_usd: f64,
    pub daily_spend_after_usd: f64,
    pub created_at_secs: u64,
    pub text: String,
}

impl AutoCopyReport {
    fn system(text: String) -> Self {
        Self {
            action: "SYSTEM".to_owned(),
            status: "failed".to_owned(),
            reason: text.clone(),
            source_trade_key: "-".to_owned(),
            market_title: None,
            outcome: None,
            position_key: "-".to_owned(),
            source_price: None,
            source_notional_usd: 0.0,
            copy_amount_usd: 0.0,
            copy_price: None,
            order_id: None,
            realized_pnl_usd: None,
            market_exposure_after_usd: 0.0,
            daily_spend_after_usd: 0.0,
            created_at_secs: now_secs(),
            text,
        }
    }

    pub fn should_notify(&self) -> bool {
        if self.status != "skipped" {
            return true;
        }

        if self.action == "SELL"
            && (self.copy_amount_usd > 0.0
                || self.order_id.is_some()
                || self.reason.contains("源仓位对账"))
        {
            return true;
        }

        matches!(
            self.action.as_str(),
            "SKIP:源实际仓位不可用" | "SKIP:今日亏损上限" | "SKIP:额度不足"
        )
    }
}

#[derive(Debug, Clone, Serialize)]
struct AutoCopyExecutionRequest {
    schema_version: u8,
    mode: AutoCopyMode,
    action: String,
    source_name: String,
    source_trade: Option<SourceTradeSnapshot>,
    order: CopyOrderIntent,
}

impl AutoCopyExecutionRequest {
    fn buy(
        mode: AutoCopyMode,
        source_name: String,
        trade: &UserTrade,
        copy_amount_usd: f64,
        direct_limit_price: f64,
        passive_limit_price: f64,
        take_enabled: bool,
        ttl_seconds: u64,
    ) -> Self {
        Self {
            schema_version: 1,
            mode,
            action: "buy".to_owned(),
            source_name,
            source_trade: Some(SourceTradeSnapshot::from_trade(trade)),
            order: CopyOrderIntent {
                position_key: position_key(trade),
                side: "BUY".to_owned(),
                asset: trade.asset.clone(),
                condition_id: trade.condition_id.clone(),
                copy_amount_usd: Some(copy_amount_usd),
                size_shares: None,
                sell_fraction: None,
                source_price: trade.price,
                direct_limit_price: Some(direct_limit_price),
                passive_limit_price: Some(passive_limit_price),
                take_enabled: Some(take_enabled),
                min_sell_price: None,
                force_market_sell: None,
                lock_profit: None,
                ttl_seconds: Some(ttl_seconds),
                order_id: None,
                reason: Some(if take_enabled {
                    "If current ask <= direct_limit_price, buy immediately; otherwise place passive limit order at passive_limit_price and keep it until ttl/cancel."
                } else {
                    "Passive-only mode: never take the current ask; place post-only at passive_limit_price unless that would cross the current ask."
                }
                .to_owned()),
            },
        }
    }

    fn sell(
        mode: AutoCopyMode,
        source_name: String,
        trade: &UserTrade,
        sell_fraction: f64,
        min_sell_price: Option<f64>,
        passive_limit_price: Option<f64>,
        clear_all: bool,
        lock_profit: bool,
        ttl_seconds: u64,
    ) -> Self {
        Self {
            schema_version: 1,
            mode,
            action: "sell".to_owned(),
            source_name,
            source_trade: Some(SourceTradeSnapshot::from_trade(trade)),
            order: CopyOrderIntent {
                position_key: position_key(trade),
                side: "SELL".to_owned(),
                asset: trade.asset.clone(),
                condition_id: trade.condition_id.clone(),
                copy_amount_usd: None,
                size_shares: None,
                sell_fraction: Some(if clear_all { 1.0 } else { sell_fraction }),
                source_price: trade.price,
                direct_limit_price: None,
                passive_limit_price,
                take_enabled: None,
                min_sell_price,
                force_market_sell: Some(clear_all),
                lock_profit: Some(lock_profit),
                ttl_seconds: Some(ttl_seconds),
                order_id: None,
                reason: Some(
                    "Source sold this market/outcome; cancel pending buys first, then size the sell from the follower's actual CLOB balance."
                        .to_owned(),
                ),
            },
        }
    }

    fn sell_position_absent_from_source(
        mode: AutoCopyMode,
        source_name: String,
        position: &CopyPosition,
    ) -> Self {
        Self {
            schema_version: 1,
            mode,
            action: "sell".to_owned(),
            source_name,
            source_trade: None,
            order: CopyOrderIntent {
                position_key: position.position_key.clone(),
                side: "SELL".to_owned(),
                asset: position.asset.clone(),
                condition_id: position.condition_id.clone(),
                copy_amount_usd: None,
                size_shares: None,
                sell_fraction: Some(1.0),
                source_price: None,
                direct_limit_price: None,
                passive_limit_price: None,
                take_enabled: None,
                min_sell_price: None,
                force_market_sell: Some(true),
                lock_profit: Some(false),
                ttl_seconds: None,
                order_id: None,
                reason: Some(
                    "Source position reconciliation says WeatherHK no longer holds this market/outcome; sell the tracked follower position immediately using market exit mode without a min sell price."
                        .to_owned(),
                ),
            },
        }
    }

    fn sell_position_global_excess(
        mode: AutoCopyMode,
        source_name: String,
        position: &CopyPosition,
        sell_fraction: f64,
        clear_all: bool,
    ) -> Self {
        Self {
            schema_version: 1,
            mode,
            action: "sell".to_owned(),
            source_name,
            source_trade: None,
            order: CopyOrderIntent {
                position_key: position.position_key.clone(),
                side: "SELL".to_owned(),
                asset: position.asset.clone(),
                condition_id: position.condition_id.clone(),
                copy_amount_usd: None,
                size_shares: None,
                sell_fraction: Some(if clear_all { 1.0 } else { sell_fraction }),
                source_price: None,
                direct_limit_price: None,
                passive_limit_price: None,
                take_enabled: None,
                min_sell_price: None,
                force_market_sell: Some(clear_all),
                lock_profit: Some(false),
                ttl_seconds: None,
                order_id: None,
                reason: Some(
                    "Source position reconcile lowered this employee target; sell only the global excess after other employees' targets are preserved."
                        .to_owned(),
                ),
            },
        }
    }

    fn sell_position_exit_retry(
        mode: AutoCopyMode,
        source_name: String,
        position: &CopyPosition,
        retry: &PendingExitRetry,
    ) -> Self {
        Self {
            schema_version: 1,
            mode,
            action: "sell".to_owned(),
            source_name,
            source_trade: None,
            order: CopyOrderIntent {
                position_key: position.position_key.clone(),
                side: "SELL".to_owned(),
                asset: position.asset.clone(),
                condition_id: position.condition_id.clone(),
                copy_amount_usd: None,
                size_shares: retry.target_size_shares,
                sell_fraction: retry
                    .target_size_shares
                    .is_none()
                    .then_some(retry.sell_fraction),
                source_price: None,
                direct_limit_price: None,
                passive_limit_price: None,
                take_enabled: None,
                min_sell_price: retry.min_sell_price,
                force_market_sell: Some(retry.force_market_sell),
                lock_profit: Some(retry.lock_profit),
                ttl_seconds: None,
                order_id: None,
                reason: Some(
                    "Retry the previously failed WeatherHK-triggered sell with the original fraction and price protection."
                        .to_owned(),
                ),
            },
        }
    }

    fn cancel(
        mode: AutoCopyMode,
        source_name: String,
        order: &PendingCopyOrder,
        reason: &str,
    ) -> Self {
        Self {
            schema_version: 1,
            mode,
            action: "cancel".to_owned(),
            source_name,
            source_trade: None,
            order: CopyOrderIntent::from_pending(order, Some(reason.to_owned())),
        }
    }

    fn sync(mode: AutoCopyMode, source_name: String, order: &PendingCopyOrder) -> Self {
        Self {
            schema_version: 1,
            mode,
            action: "sync".to_owned(),
            source_name,
            source_trade: None,
            order: CopyOrderIntent::from_pending(
                order,
                Some("Report whether this pending order is filled, partially filled, pending, or cancelled.".to_owned()),
            ),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
struct SourceTradeSnapshot {
    source_trade_key: String,
    proxy_wallet: String,
    side: String,
    asset: String,
    condition_id: String,
    price: Option<f64>,
    size: Option<f64>,
    notional_usd: Option<f64>,
    timestamp: Option<u64>,
    title: Option<String>,
    slug: Option<String>,
    event_slug: Option<String>,
    outcome: Option<String>,
    transaction_hash: Option<String>,
}

impl SourceTradeSnapshot {
    fn from_trade(trade: &UserTrade) -> Self {
        Self {
            source_trade_key: source_trade_key(trade),
            proxy_wallet: trade.proxy_wallet.clone(),
            side: trade.side.clone(),
            asset: trade.asset.clone(),
            condition_id: trade.condition_id.clone(),
            price: trade.price,
            size: trade.size,
            notional_usd: trade
                .price
                .zip(trade.size)
                .map(|(price, size)| price * size),
            timestamp: trade.timestamp,
            title: trade.title.clone(),
            slug: trade.slug.clone(),
            event_slug: trade.event_slug.clone(),
            outcome: trade.outcome.clone(),
            transaction_hash: trade.transaction_hash.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
struct CopyOrderIntent {
    position_key: String,
    side: String,
    asset: String,
    condition_id: String,
    copy_amount_usd: Option<f64>,
    size_shares: Option<f64>,
    sell_fraction: Option<f64>,
    source_price: Option<f64>,
    direct_limit_price: Option<f64>,
    passive_limit_price: Option<f64>,
    take_enabled: Option<bool>,
    min_sell_price: Option<f64>,
    force_market_sell: Option<bool>,
    lock_profit: Option<bool>,
    ttl_seconds: Option<u64>,
    order_id: Option<String>,
    reason: Option<String>,
}

impl CopyOrderIntent {
    fn from_pending(order: &PendingCopyOrder, reason: Option<String>) -> Self {
        Self {
            position_key: order.position_key.clone(),
            side: order.side.clone(),
            asset: order.asset.clone(),
            condition_id: order.condition_id.clone(),
            copy_amount_usd: Some(order.copy_amount_usd),
            size_shares: None,
            sell_fraction: None,
            source_price: None,
            direct_limit_price: None,
            passive_limit_price: Some(order.limit_price),
            take_enabled: None,
            min_sell_price: None,
            force_market_sell: None,
            lock_profit: None,
            ttl_seconds: Some(order_remaining_ttl(order.expires_at_secs)),
            order_id: order.external_order_id.clone(),
            reason,
        }
    }
}

fn order_remaining_ttl(expires_at_secs: u64) -> u64 {
    if expires_at_secs == 0 {
        0
    } else {
        expires_at_secs.saturating_sub(now_secs())
    }
}

#[derive(Debug, Clone, Deserialize)]
struct ExternalExecutionResult {
    status: Option<String>,
    order_id: Option<String>,
    order_price: Option<f64>,
    limit_price: Option<f64>,
    filled_amount_usd: Option<f64>,
    filled_size: Option<f64>,
    filled_price: Option<f64>,
    realized_pnl_usd: Option<f64>,
    actual_balance_shares: Option<f64>,
    target_size_shares: Option<f64>,
    message: Option<String>,
}

impl From<ExternalExecutionResult> for ExecutionResult {
    fn from(value: ExternalExecutionResult) -> Self {
        let status = value
            .status
            .as_deref()
            .map(ExecutionStatus::parse)
            .unwrap_or(ExecutionStatus::Submitted);
        Self {
            status,
            order_id: value.order_id,
            order_price: value.order_price.or(value.limit_price),
            filled_amount_usd: value.filled_amount_usd,
            filled_size: value.filled_size,
            filled_price: value.filled_price,
            realized_pnl_usd: value.realized_pnl_usd,
            actual_balance_shares: value.actual_balance_shares,
            target_size_shares: value.target_size_shares,
            message: value.message,
        }
    }
}

#[derive(Debug, Clone)]
struct ExecutionResult {
    status: ExecutionStatus,
    order_id: Option<String>,
    order_price: Option<f64>,
    filled_amount_usd: Option<f64>,
    filled_size: Option<f64>,
    filled_price: Option<f64>,
    realized_pnl_usd: Option<f64>,
    actual_balance_shares: Option<f64>,
    target_size_shares: Option<f64>,
    message: Option<String>,
}

impl ExecutionResult {
    fn failed(message: impl Into<String>) -> Self {
        Self {
            status: ExecutionStatus::Failed,
            order_id: None,
            order_price: None,
            filled_amount_usd: None,
            filled_size: None,
            filled_price: None,
            realized_pnl_usd: None,
            actual_balance_shares: None,
            target_size_shares: None,
            message: Some(message.into()),
        }
    }

    fn skipped(message: impl Into<String>) -> Self {
        Self {
            status: ExecutionStatus::Skipped,
            order_id: None,
            order_price: None,
            filled_amount_usd: None,
            filled_size: None,
            filled_price: None,
            realized_pnl_usd: None,
            actual_balance_shares: None,
            target_size_shares: None,
            message: Some(message.into()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExecutionStatus {
    DryRun,
    Submitted,
    Pending,
    Filled,
    Cancelled,
    Skipped,
    Failed,
}

impl ExecutionStatus {
    fn parse(value: &str) -> Self {
        match value.trim().to_lowercase().as_str() {
            "dry-run" | "dryrun" => Self::DryRun,
            "submitted" | "submit" | "placed" => Self::Submitted,
            "pending" | "open" | "partial" | "partially-filled" => Self::Pending,
            "filled" | "done" | "success" => Self::Filled,
            "cancelled" | "canceled" | "cancelled-by-user" => Self::Cancelled,
            "skipped" | "skip" => Self::Skipped,
            "failed" | "error" => Self::Failed,
            _ => Self::Submitted,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::DryRun => "dry-run",
            Self::Submitted => "submitted",
            Self::Pending => "pending",
            Self::Filled => "filled",
            Self::Cancelled => "cancelled",
            Self::Skipped => "skipped",
            Self::Failed => "failed",
        }
    }
}

#[derive(Debug)]
pub enum AutoCopyError {
    Io(std::io::Error),
    Json(serde_json::Error),
}

impl fmt::Display for AutoCopyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "I/O error: {error}"),
            Self::Json(error) => write!(f, "JSON error: {error}"),
        }
    }
}

impl std::error::Error for AutoCopyError {}

fn copy_fraction_for_source_notional(source_notional: f64) -> f64 {
    copy_fraction_for_source_notional_with_policy(source_notional, true)
}

fn copy_fraction_for_source_notional_with_policy(
    source_notional: f64,
    small_buy_full_copy_enabled: bool,
) -> f64 {
    if small_buy_full_copy_enabled
        && source_notional > 0.0
        && source_notional < SMALL_BUY_FULL_COPY_THRESHOLD_USD
    {
        1.0
    } else {
        COPY_FRACTION
    }
}

fn target_reconcile_amount_tolerance(target_amount_usd: f64) -> f64 {
    (target_amount_usd * TARGET_RECONCILE_RELATIVE_TOLERANCE).max(TARGET_RECONCILE_MIN_NOTIONAL_USD)
}

fn high_price_guarded_direct_limit(
    source_price: f64,
    normal_direct_limit_price: f64,
    threshold: f64,
    high_price_max_chase_pct: f64,
) -> f64 {
    if source_price > threshold {
        price_with_pct_upside(source_price, high_price_max_chase_pct)
    } else {
        normal_direct_limit_price.min(threshold)
    }
}

fn high_price_guarded_passive_limit(
    source_price: f64,
    normal_passive_limit_price: f64,
    direct_limit_price: f64,
    threshold: f64,
) -> f64 {
    if source_price > threshold {
        normal_passive_limit_price.min(direct_limit_price)
    } else {
        normal_passive_limit_price
            .min(direct_limit_price)
            .min(threshold)
    }
}

fn high_price_copy_target(
    target_copy_amount: f64,
    source_price: f64,
    threshold: f64,
    exposure_cap_usd: f64,
) -> f64 {
    if source_price > threshold {
        target_copy_amount.min(exposure_cap_usd)
    } else {
        target_copy_amount
    }
}

fn capped_copy_target(target_copy_amount: f64, cap_usd: f64) -> f64 {
    target_copy_amount.min(cap_usd)
}

fn copy_target_cap_note(
    original_target_amount: f64,
    final_target_amount: f64,
    reference_price: f64,
    threshold: f64,
    high_price_cap_usd: f64,
    global_cap_usd: f64,
) -> String {
    if reference_price > threshold && original_target_amount > high_price_cap_usd {
        format!(
            "；高价风控：参考价 {:.2}c 超过 {:.2}c，该 outcome 目标封顶 {:.2}U",
            reference_price * 100.0,
            threshold * 100.0,
            final_target_amount
        )
    } else if original_target_amount > global_cap_usd {
        format!(
            "；全员仓位风控：按比例目标 {:.4}U 超过 {:.2}U，该 outcome 目标封顶 {:.2}U",
            original_target_amount, global_cap_usd, final_target_amount
        )
    } else {
        String::new()
    }
}

fn copy_sizing_label(source_notional: f64) -> String {
    let fraction = copy_fraction_for_source_notional(source_notional);
    format!(
        "{:.0}% 比例跟单 => {:.8}U",
        fraction * 100.0,
        source_notional * fraction
    )
}

fn env_string(key: &str) -> Option<String> {
    env::var(key)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn env_keywords(key: &str, default: &str) -> Vec<String> {
    env_string(key)
        .unwrap_or_else(|| default.to_owned())
        .split([',', '|'])
        .map(|keyword| keyword.trim().to_lowercase())
        .filter(|keyword| !keyword.is_empty())
        .collect()
}

fn env_bool(key: &str, default: bool) -> bool {
    match env_string(key)
        .map(|value| value.to_ascii_lowercase())
        .as_deref()
    {
        Some("1" | "true" | "yes" | "on") => true,
        Some("0" | "false" | "no" | "off") => false,
        Some(value) => {
            eprintln!("invalid bool for {key}: {value}; using {default}");
            default
        }
        None => default,
    }
}

fn env_f64(key: &str, default: f64) -> f64 {
    env_string(key)
        .and_then(|value| match value.parse::<f64>() {
            Ok(parsed) => Some(parsed),
            Err(error) => {
                eprintln!("invalid number for {key}: {error}; using {default}");
                None
            }
        })
        .unwrap_or(default)
}

fn env_u64(key: &str, default: u64) -> u64 {
    env_string(key)
        .and_then(|value| match value.parse::<u64>() {
            Ok(parsed) => Some(parsed),
            Err(error) => {
                eprintln!("invalid integer for {key}: {error}; using {default}");
                None
            }
        })
        .unwrap_or(default)
}

fn env_usize(key: &str, default: usize) -> usize {
    env_string(key)
        .and_then(|value| match value.parse::<usize>() {
            Ok(parsed) => Some(parsed),
            Err(error) => {
                eprintln!("invalid integer for {key}: {error}; using {default}");
                None
            }
        })
        .unwrap_or(default)
}

fn shares_for_amount(amount_usd: f64, price: f64) -> f64 {
    if price > 0.0 {
        amount_usd / price
    } else {
        0.0
    }
}

fn same_wallet(left: &str, right: &str) -> bool {
    left.trim().eq_ignore_ascii_case(right.trim())
}

fn matches_employee_keywords(employee: &WatchedEmployee, trade: &UserTrade) -> bool {
    if employee.keywords.is_empty() {
        return true;
    }

    let haystack = format!(
        "{} {} {}",
        trade.title.as_deref().unwrap_or(""),
        trade.slug.as_deref().unwrap_or(""),
        trade.event_slug.as_deref().unwrap_or("")
    )
    .to_lowercase();

    employee
        .keywords
        .iter()
        .any(|keyword| haystack.contains(&keyword.to_lowercase()))
}

fn matches_specialty_keywords(keywords: &[String], haystack: &str) -> bool {
    if keywords.is_empty() {
        return true;
    }

    let haystack = haystack.to_lowercase();
    keywords
        .iter()
        .any(|keyword| haystack.contains(&keyword.to_lowercase()))
}

fn position_key(trade: &UserTrade) -> String {
    format!("{}:{}", trade.condition_id, trade.asset)
}

fn event_slug_for_trade(trade: &UserTrade) -> Option<String> {
    trade
        .event_slug
        .as_deref()
        .or(trade.slug.as_deref())
        .map(str::trim)
        .filter(|slug| !slug.is_empty())
        .map(str::to_owned)
}

fn source_trade_key(trade: &UserTrade) -> String {
    if let Some(tx) = trade
        .transaction_hash
        .as_deref()
        .map(str::trim)
        .filter(|tx| !tx.is_empty())
    {
        return format!(
            "{}:{}:{}:{:.8}:{:.8}",
            tx,
            trade.condition_id,
            trade.asset,
            trade.price.unwrap_or(0.0),
            trade.size.unwrap_or(0.0)
        );
    }

    format!(
        "{}:{}:{}:{}:{:.8}:{:.8}",
        trade.timestamp.unwrap_or(0),
        trade.side,
        trade.condition_id,
        trade.asset,
        trade.price.unwrap_or(0.0),
        trade.size.unwrap_or(0.0)
    )
}

fn local_order_id(trade: &UserTrade, now: u64) -> String {
    format!("weatherhk:{}:{}:{}", now, trade.condition_id, trade.asset)
}

fn local_sell_order_id(trade: &UserTrade, now: u64) -> String {
    format!(
        "weatherhk-sell:{}:{}:{}",
        now, trade.condition_id, trade.asset
    )
}

fn local_exit_retry_sell_order_id(position_key: &str, now: u64) -> String {
    format!("weatherhk-exit-retry-sell:{now}:{position_key}")
}

fn clamp_price(price: f64) -> f64 {
    price.clamp(0.01, 0.99)
}

fn price_with_pct_upside(source_price: f64, pct: f64) -> f64 {
    clamp_price(source_price + source_price * pct)
}

fn price_with_capped_upside(source_price: f64, pct: f64, absolute_cap: f64) -> f64 {
    let delta = (source_price * pct).min(absolute_cap);
    clamp_price(source_price + delta)
}

fn price_with_capped_downside(source_price: f64, pct: f64, absolute_cap: f64) -> f64 {
    let delta = (source_price * pct).min(absolute_cap);
    clamp_price(source_price - delta)
}

fn price_with_pct_downside(source_price: f64, pct: f64) -> f64 {
    clamp_price(source_price * (1.0 - pct.clamp(0.0, 0.95)))
}

fn should_skip_low_edge_buy(source_price: f64, skip_price_at_or_above: f64) -> bool {
    source_price > skip_price_at_or_above
}

fn should_skip_near_zero_buy(source_price: f64, skip_price_at_or_below: f64) -> bool {
    skip_price_at_or_below > 0.0 && source_price <= skip_price_at_or_below
}

fn should_skip_small_buy(source_notional: f64, min_buy_source_notional_usd: f64) -> bool {
    min_buy_source_notional_usd > 0.0 && source_notional < min_buy_source_notional_usd
}

fn source_pressure_detected(stats: &SourceFlowStats, config: &AutoCopyConfig) -> bool {
    if stats.sell_count < config.source_pressure_min_sell_count {
        return false;
    }
    if stats.sell_notional_usd < config.source_pressure_min_sell_notional_usd {
        return false;
    }

    stats.avg_sell_gap_seconds().map_or(false, |avg_gap| {
        avg_gap <= config.source_pressure_max_avg_sell_gap_seconds as f64
    })
}

fn should_report_pending_sync(order: &PendingCopyOrder, execution: &ExecutionResult) -> bool {
    match execution.status {
        ExecutionStatus::Filled => true,
        ExecutionStatus::Cancelled | ExecutionStatus::Skipped => {
            let filled_price = execution.filled_price.unwrap_or(order.limit_price);
            let filled_amount = execution
                .filled_amount_usd
                .or_else(|| execution.filled_size.map(|size| size * filled_price))
                .unwrap_or(order.filled_amount_usd);
            filled_amount > order.filled_amount_usd + 0.000001
        }
        ExecutionStatus::Failed => !is_transient_execution_error(execution.message.as_deref()),
        ExecutionStatus::Pending | ExecutionStatus::Submitted => {
            let filled_price = execution.filled_price.unwrap_or(order.limit_price);
            let filled_amount = execution
                .filled_amount_usd
                .or_else(|| execution.filled_size.map(|size| size * filled_price))
                .unwrap_or(order.filled_amount_usd);
            filled_amount > order.filled_amount_usd + 0.000001
        }
        ExecutionStatus::DryRun => false,
    }
}

fn pending_cumulative_filled_amount(order: &PendingCopyOrder, execution: &ExecutionResult) -> f64 {
    if execution.status == ExecutionStatus::Filled
        && execution.filled_amount_usd.is_none()
        && execution.filled_size.is_none()
    {
        return order.copy_amount_usd;
    }

    execution
        .filled_amount_usd
        .or_else(|| {
            execution.filled_size.map(|size| {
                size * execution
                    .filled_price
                    .or(execution.order_price)
                    .unwrap_or(order.limit_price)
            })
        })
        .unwrap_or(order.filled_amount_usd)
        .clamp(0.0, order.copy_amount_usd)
}

fn normalize_pending_sync_status(order: &PendingCopyOrder, execution: &mut ExecutionResult) {
    if execution.status != ExecutionStatus::Filled {
        return;
    }

    let cumulative_filled = pending_cumulative_filled_amount(order, execution);
    if cumulative_filled + 0.000_001 < order.copy_amount_usd {
        execution.status = ExecutionStatus::Pending;
    }
}

fn elapsed_label(seconds: u64) -> String {
    let hours = seconds / 3_600;
    let minutes = (seconds % 3_600) / 60;
    let seconds = seconds % 60;
    if hours > 0 {
        format!("{hours}小时{minutes}分{seconds}秒")
    } else if minutes > 0 {
        format!("{minutes}分{seconds}秒")
    } else {
        format!("{seconds}秒")
    }
}

fn timestamp_age_label(timestamp: u64, now: u64) -> String {
    if timestamp == 0 {
        "未知（旧状态未记录）".to_owned()
    } else {
        elapsed_label(now.saturating_sub(timestamp))
    }
}

fn is_transient_execution_error(message: Option<&str>) -> bool {
    let Some(message) = message else {
        return false;
    };
    let text = message.to_ascii_lowercase();
    let non_transient_markers = [
        "trading restricted",
        "geoblock",
        "not enough balance",
        "allowance",
        "lower than the minimum",
        "invalid token",
        "invalid. size",
        "no orders found to match",
        "no orderbook",
        "orderbook",
        "does not exist",
        "fully filled or killed",
    ];
    if non_transient_markers
        .iter()
        .any(|marker| text.contains(marker))
    {
        return false;
    }

    [
        "request exception",
        "status_code=none",
        "timed out",
        "timeout",
        "connection reset",
        "connection aborted",
        "connection error",
        "ssl",
        "remote end closed",
        "temporarily unavailable",
        "temporary failure",
        "max retries exceeded",
    ]
    .iter()
    .any(|marker| text.contains(marker))
}

fn is_retryable_exit_error(message: Option<&str>) -> bool {
    if is_transient_execution_error(message) {
        return true;
    }

    let Some(message) = message else {
        return false;
    };
    let text = message.to_ascii_lowercase();
    text.contains("no orders found to match with fak order")
        || text.contains("partial fak sell remaining")
        || text.contains("fok orders are fully filled or killed")
}

fn is_no_match_fak_error(message: Option<&str>) -> bool {
    let Some(message) = message else {
        return false;
    };
    let text = message.to_ascii_lowercase();
    text.contains("no orders found to match with fak order")
        || text.contains("no orders found to match")
}

fn remaining_exit_target_size(execution: &ExecutionResult) -> f64 {
    let target = execution.target_size_shares.unwrap_or(0.0);
    let filled = execution.filled_size.unwrap_or(0.0).clamp(0.0, target);
    (target - filled).max(0.0)
}

fn is_terminal_missing_token_error(message: Option<&str>) -> bool {
    let Some(message) = message else {
        return false;
    };
    let text = message.to_ascii_lowercase();
    text.contains("invalid token id")
        || text.contains("token id does not exist")
        || text.contains("token does not exist")
}

fn should_silently_finish_dust_exit(execution: &ExecutionResult) -> bool {
    if execution.status == ExecutionStatus::Failed
        && is_no_match_fak_error(execution.message.as_deref())
        && execution
            .actual_balance_shares
            .is_some_and(|balance| balance < MIN_CLOB_ORDER_SIZE_SHARES)
    {
        return true;
    }

    if execution.status != ExecutionStatus::Skipped {
        return false;
    }

    if execution
        .actual_balance_shares
        .is_some_and(|balance| balance < MIN_CLOB_ORDER_SIZE_SHARES)
    {
        return true;
    }

    let text = execution
        .message
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();
    text.contains("actual clob token balance is zero")
        || text.contains("below minimum sell size")
        || text.contains("requested sell size is zero")
}

fn should_accumulate_small_exit(execution: &ExecutionResult) -> bool {
    execution.status == ExecutionStatus::Skipped
        && execution
            .actual_balance_shares
            .is_some_and(|balance| balance >= MIN_CLOB_ORDER_SIZE_SHARES)
        && execution
            .target_size_shares
            .is_some_and(|target| target > 0.0 && target < MIN_CLOB_ORDER_SIZE_SHARES)
        && execution
            .message
            .as_deref()
            .unwrap_or_default()
            .contains("accumulate future source SELL")
}

fn should_cancel_pending_buy_absent_from_source_position(
    order: &PendingCopyOrder,
    source_positions: &HashMap<String, ObservedSourcePosition>,
    now: u64,
) -> bool {
    order.side == "BUY"
        && now.saturating_sub(order.created_at_secs) >= SOURCE_POSITION_RECONCILE_GRACE_SECONDS
        && !source_positions.contains_key(&order.asset)
}

fn pending_expires_at(now: u64, ttl_seconds: u64) -> u64 {
    if ttl_seconds == 0 {
        0
    } else {
        now.saturating_add(ttl_seconds)
    }
}

fn passive_ttl_label(ttl_seconds: u64, source_name: &str) -> String {
    if ttl_seconds == 0 {
        format!("无 TTL，{source_name} 未卖出则继续挂单")
    } else {
        format!("TTL {ttl_seconds}s")
    }
}

fn buy_ttl_label(status: ExecutionStatus, ttl_seconds: u64, source_name: &str) -> String {
    match status {
        ExecutionStatus::Pending | ExecutionStatus::Submitted => {
            passive_ttl_label(ttl_seconds, source_name)
        }
        ExecutionStatus::Filled => "已成交，TTL 不适用".to_owned(),
        ExecutionStatus::DryRun => "dry-run 未实际挂单，TTL 未生效".to_owned(),
        ExecutionStatus::Cancelled | ExecutionStatus::Skipped | ExecutionStatus::Failed => {
            "未成功挂单，TTL 未生效".to_owned()
        }
    }
}

fn market_url(trade: &UserTrade) -> String {
    let slug = trade
        .event_slug
        .as_deref()
        .or(trade.slug.as_deref())
        .unwrap_or("-");

    if slug == "-" {
        "-".to_owned()
    } else {
        format!("https://polymarket.com/event/{slug}")
    }
}

fn action_label(action: &str) -> &'static str {
    match action {
        "BUY" => "买入",
        "SELL" => "卖出",
        _ => "操作",
    }
}

fn report_action_label(action: &str, status: ExecutionStatus) -> &'static str {
    match status {
        ExecutionStatus::Skipped => "跳过",
        ExecutionStatus::Failed => "失败",
        _ => action_label(action),
    }
}

fn copy_action_label(action: &str, status: ExecutionStatus) -> &'static str {
    match (action, status) {
        ("BUY", ExecutionStatus::Pending | ExecutionStatus::Submitted) => "挂单/提交",
        ("BUY", ExecutionStatus::Skipped) => "跳过",
        ("BUY", ExecutionStatus::Failed) => "失败",
        ("BUY", _) => "买入",
        ("SELL", ExecutionStatus::Skipped) => "跳过",
        ("SELL", ExecutionStatus::Failed) => "失败",
        ("SELL", _) => "卖出",
        _ => "操作",
    }
}

fn action_failure_cooldown_key(action: &str, position_key: &str) -> String {
    format!("action:{action}:{position_key}")
}

fn report_failure_cooldown_key(report: &AutoCopyReport) -> String {
    format!("report:{}:{}", report.action, report.position_key)
}

fn pending_action_label(action: &str) -> &'static str {
    match action {
        "SYNC" => "同步",
        "CANCEL" => "取消",
        _ => "更新",
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static TEST_PATH_COUNTER: AtomicUsize = AtomicUsize::new(0);

    #[test]
    fn copy_fraction_uses_full_small_sizing_then_half() {
        assert_near(copy_fraction_for_source_notional(0.001), 1.0);
        assert_near(copy_fraction_for_source_notional(1.0), 1.0);
        assert_near(copy_fraction_for_source_notional(14.999), 1.0);
        assert_near(copy_fraction_for_source_notional(15.0), 0.5);
        assert_near(copy_fraction_for_source_notional(30.0), 0.5);
        assert_near(copy_fraction_for_source_notional(200.0), 0.5);
    }

    #[test]
    fn copy_fraction_can_disable_full_small_sizing() {
        assert_near(
            copy_fraction_for_source_notional_with_policy(0.001, false),
            0.5,
        );
        assert_near(
            copy_fraction_for_source_notional_with_policy(1.0, false),
            0.5,
        );
        assert_near(
            copy_fraction_for_source_notional_with_policy(14.999, false),
            0.5,
        );
        assert_near(
            copy_fraction_for_source_notional_with_policy(15.0, false),
            0.5,
        );
    }

    #[test]
    fn high_price_copy_target_caps_exact_outcome_at_fifty_usd() {
        assert_near(high_price_copy_target(35.0, 0.91, 0.90, 50.0), 35.0);
        assert_near(high_price_copy_target(50.0, 0.91, 0.90, 50.0), 50.0);
        assert_near(high_price_copy_target(100.0, 0.91, 0.90, 50.0), 50.0);
        assert_near(high_price_copy_target(100.0, 0.90, 0.90, 50.0), 100.0);
    }

    #[test]
    fn global_copy_target_cap_applies_below_high_price_zone() {
        let mut engine = test_engine();
        let employee = test_employee();
        let reports = engine.handle_trade(
            &employee,
            &test_trade("BUY", "weather-market", "asset-40", 0.40, 200.0, 100),
        );
        let buy = reports
            .iter()
            .find(|report| report.action == "BUY")
            .expect("expected globally capped buy");

        assert_eq!(buy.status, "dry-run");
        assert_near(buy.copy_amount_usd, 50.0);
        assert!(buy.reason.contains("金额原始目标 100.0000U"));
        assert!(buy.reason.contains("最终目标 50.0000U"));
        assert!(buy.reason.contains("全员仓位风控"));
        assert!(!buy.reason.contains("高价谨慎模式"));
    }

    #[test]
    fn global_outcome_cap_counts_other_employee_committed_exposure() {
        let shared_global_state = test_state_path("shared-global");
        let key = "weather-market:asset-shared";
        let now = now_secs();
        let mut first = test_engine();
        first.config.source_name = "WeatherHK".to_owned();
        first.config.global_state_path = shared_global_state.clone();
        first.state.positions.push(CopyPosition {
            position_key: key.to_owned(),
            market_title: Some("Weather market".to_owned()),
            outcome: Some("Yes".to_owned()),
            asset: "asset-shared".to_owned(),
            condition_id: "weather-market".to_owned(),
            size_shares: 75.0,
            cost_usd: 30.0,
            realized_pnl_usd: 0.0,
            updated_at_secs: now,
        });
        first
            .sync_global_source_exposure_for_key(key, Some(30.0), now)
            .expect("first source should publish global exposure");

        let mut second = test_engine();
        second.config.source_name = "OlympusHive".to_owned();
        second.config.global_state_path = shared_global_state;
        let reports = second.handle_trade(
            &test_employee(),
            &test_trade("BUY", "weather-market", "asset-shared", 0.40, 60.0, 101),
        );
        let buy = reports
            .iter()
            .find(|report| report.action == "BUY")
            .expect("second source should buy only remaining global gap");

        assert_near(buy.copy_amount_usd, 20.0);
        assert!(buy.reason.contains("多员工全局目标 50.0000U"));
        assert!(buy.reason.contains("已成交+挂单+预留 30.0000U"));
    }

    #[test]
    fn global_pending_buy_blocks_second_pending_for_same_outcome() {
        let shared_global_state = test_state_path("shared-global-pending");
        let key = "weather-market:asset-pending";
        let now = now_secs();
        let mut first = test_engine();
        first.config.source_name = "WeatherHK".to_owned();
        first.config.global_state_path = shared_global_state.clone();
        first.state.pending_orders.push(PendingCopyOrder {
            local_order_id: "local-pending".to_owned(),
            external_order_id: Some("external-pending".to_owned()),
            position_key: key.to_owned(),
            side: "BUY".to_owned(),
            market_title: Some("Weather market".to_owned()),
            outcome: Some("Yes".to_owned()),
            asset: "asset-pending".to_owned(),
            condition_id: "weather-market".to_owned(),
            copy_amount_usd: 10.0,
            limit_price: 0.40,
            requested_limit_price: Some(0.40),
            filled_amount_usd: 0.0,
            filled_size: 0.0,
            created_at_secs: now,
            expires_at_secs: 0,
            last_sync_at_secs: now,
            source_trade_key: "source-pending".to_owned(),
            source_trade_at_secs: now,
        });
        first
            .sync_global_source_exposure_for_key(key, Some(10.0), now)
            .expect("first source should publish pending order");

        let mut second = test_engine();
        second.config.source_name = "OlympusHive".to_owned();
        second.config.global_state_path = shared_global_state;
        let reports = second.handle_trade(
            &test_employee(),
            &test_trade("BUY", "weather-market", "asset-pending", 0.40, 40.0, 101),
        );

        assert!(reports.iter().any(|report| {
            report.action == "SKIP:全局已有挂单" && report.reason.contains("来自 WeatherHK")
        }));
        assert!(!reports.iter().any(|report| report.action == "BUY"));
    }

    #[test]
    fn global_sell_keeps_position_when_other_employee_still_supports_cap() {
        let shared_global_state = test_state_path("shared-global-sell-covered");
        let key = "weather-market:asset-covered";
        let now = now_secs();

        let mut weather = test_engine();
        weather.config.source_name = "WeatherHK".to_owned();
        weather.config.global_state_path = shared_global_state.clone();
        weather.state.positions.push(CopyPosition {
            position_key: key.to_owned(),
            market_title: Some("Weather market".to_owned()),
            outcome: Some("Yes".to_owned()),
            asset: "asset-covered".to_owned(),
            condition_id: "weather-market".to_owned(),
            size_shares: 125.0,
            cost_usd: 50.0,
            realized_pnl_usd: 0.0,
            updated_at_secs: now,
        });
        weather
            .sync_global_source_exposure_for_key(key, Some(50.0), now)
            .expect("weather should publish target");

        let mut olympus = test_engine();
        olympus.config.source_name = "OlympusHive".to_owned();
        olympus.config.global_state_path = shared_global_state.clone();
        olympus.state.source_outcomes.push(SourceOutcomeMetadata {
            position_key: key.to_owned(),
            asset: "asset-covered".to_owned(),
            condition_id: "weather-market".to_owned(),
            market_title: Some("Weather market".to_owned()),
            outcome: Some("Yes".to_owned()),
            slug: None,
            event_slug: None,
            last_price: Some(0.40),
            last_buy_price: Some(0.40),
            first_seen_at_secs: now,
            last_seen_at_secs: now,
            last_buy_at_secs: Some(now),
            last_sell_at_secs: None,
        });
        olympus
            .sync_global_source_exposure_for_key(key, Some(50.0), now)
            .expect("olympus should publish target");

        let source_positions = HashMap::new();
        let reports = weather.handle_trade_with_source_positions(
            &test_employee(),
            &test_trade("SELL", "weather-market", "asset-covered", 0.40, 50.0, now),
            Some(&source_positions),
        );

        assert!(reports.iter().any(|report| {
            report.action == "SKIP:全局目标仍覆盖" && report.reason.contains("OlympusHive 50.00U")
        }));
        assert!(!reports.iter().any(|report| report.action == "SELL"));
    }

    #[test]
    fn global_sell_sells_only_excess_when_one_employee_exits() {
        let shared_global_state = test_state_path("shared-global-sell-excess");
        let key = "weather-market:asset-excess";
        let now = now_secs();

        let mut weather = test_engine();
        weather.config.source_name = "WeatherHK".to_owned();
        weather.config.global_state_path = shared_global_state.clone();
        weather.state.positions.push(CopyPosition {
            position_key: key.to_owned(),
            market_title: Some("Weather market".to_owned()),
            outcome: Some("Yes".to_owned()),
            asset: "asset-excess".to_owned(),
            condition_id: "weather-market".to_owned(),
            size_shares: 62.5,
            cost_usd: 25.0,
            realized_pnl_usd: 0.0,
            updated_at_secs: now,
        });
        weather
            .sync_global_source_exposure_for_key(key, Some(25.0), now)
            .expect("weather should publish target");

        let mut olympus = test_engine();
        olympus.config.source_name = "OlympusHive".to_owned();
        olympus.config.global_state_path = shared_global_state.clone();
        olympus.state.positions.push(CopyPosition {
            position_key: key.to_owned(),
            market_title: Some("Weather market".to_owned()),
            outcome: Some("Yes".to_owned()),
            asset: "asset-excess".to_owned(),
            condition_id: "weather-market".to_owned(),
            size_shares: 62.5,
            cost_usd: 25.0,
            realized_pnl_usd: 0.0,
            updated_at_secs: now,
        });
        olympus
            .sync_global_source_exposure_for_key(key, Some(25.0), now)
            .expect("olympus should publish target");

        let source_positions = HashMap::new();
        let reports = weather.handle_trade_with_source_positions(
            &test_employee(),
            &test_trade("SELL", "weather-market", "asset-excess", 0.40, 25.0, now),
            Some(&source_positions),
        );
        let sell = reports
            .iter()
            .find(|report| report.action == "SELL")
            .expect("global excess should trigger a sell");

        assert_eq!(sell.status, "dry-run");
        assert_near(sell.copy_amount_usd, 25.0);
        assert!(sell.reason.contains("多员工全局目标 50.0000U -> 25.0000U"));
        assert!(sell.reason.contains("卖出 50.00%"));
        assert!(sell.reason.contains("OlympusHive 25.00U"));
    }

    #[test]
    fn high_price_buy_only_fills_remaining_gap_to_fifty_usd() {
        let mut engine = test_engine();
        let employee = test_employee();
        engine.state.positions.push(CopyPosition {
            position_key: "weather-market:asset-90".to_owned(),
            market_title: Some("Weather market".to_owned()),
            outcome: Some("Yes".to_owned()),
            asset: "asset-90".to_owned(),
            condition_id: "weather-market".to_owned(),
            size_shares: 40.0,
            cost_usd: 35.0,
            realized_pnl_usd: 0.0,
            updated_at_secs: 90,
        });

        let reports = engine.handle_trade(
            &employee,
            &test_trade("BUY", "weather-market", "asset-90", 0.92, 200.0, 100),
        );
        let buy = reports
            .iter()
            .find(|report| report.action == "BUY")
            .expect("expected capped high-price buy");

        assert_near(buy.copy_amount_usd, 15.0);
        assert!(buy.reason.contains("高价风控"));
        assert!(buy.reason.contains("目标封顶 50.00U"));
    }

    #[test]
    fn high_price_buy_never_chases_above_source_price_by_default() {
        for (price, asset) in [(0.95, "asset-95"), (0.98, "asset-98")] {
            let mut engine = test_engine();
            let employee = test_employee();
            let reports = engine.handle_trade(
                &employee,
                &test_trade("BUY", "weather-market", asset, price, 70.0, 100),
            );
            let buy = reports
                .iter()
                .find(|report| report.action == "BUY")
                .expect("expected high-price buy");

            assert_near(buy.copy_price.expect("expected limit price"), price);
            assert!(buy.reason.contains("高价谨慎模式"));
            assert!(buy.reason.contains("不向 99c/100c 追价"));
        }
    }

    #[test]
    fn below_entry_guard_keeps_normal_amount_but_caps_take_price() {
        let mut engine = test_engine();
        let employee = test_employee();
        let reports = engine.handle_trade(
            &employee,
            &test_trade("BUY", "weather-market", "asset-87", 0.87, 70.0, 100),
        );
        let buy = reports
            .iter()
            .find(|report| report.action == "BUY")
            .expect("expected below-guard buy");

        assert_near(buy.copy_price.expect("expected passive limit"), 0.89);
        assert!(buy.reason.contains("直接追价上限 90.00c"));
        assert!(!buy.reason.contains("高价谨慎模式"));
    }

    #[test]
    fn near_ninety_buy_caps_amount_when_our_entry_would_cross_ninety() {
        let mut engine = test_engine();
        let employee = test_employee();
        let reports = engine.handle_trade(
            &employee,
            &test_trade("BUY", "weather-market", "asset-8865", 0.8865, 408.7817, 100),
        );
        let buy = reports
            .iter()
            .find(|report| report.action == "BUY")
            .expect("expected entry-guarded buy");

        assert_eq!(buy.status, "dry-run");
        assert_near(buy.copy_amount_usd, 50.0);
        assert_near(buy.copy_price.expect("expected guarded entry price"), 0.90);
        assert!(buy.reason.contains("金额原始目标 204.3909U"));
        assert!(buy.reason.contains("最终目标 50.0000U"));
        assert!(buy.reason.contains("高价风控"));
        assert!(buy.reason.contains("直接追价上限 90.00c"));
    }

    #[test]
    fn small_buys_accumulate_shares_at_one_hundred_percent() {
        let mut state = AutoCopyState::default();
        let key = "weather-market:asset-30";

        let first = state.record_source_buy_target(key, "asset-30", 1.50, 5.0, 100);
        assert_near(first.source_buy_notional_usd, 1.50);
        assert_near(first.source_buy_size_shares, 5.0);

        let accumulated = state.record_source_buy_target(key, "asset-30", 1.75, 5.0, 110);
        assert_near(accumulated.source_buy_notional_usd, 3.25);
        assert_near(accumulated.source_buy_size_shares, 10.0);
        assert_near(
            copy_fraction_for_source_notional(accumulated.source_buy_notional_usd),
            1.0,
        );
        assert_near(accumulated.source_buy_size_shares, 10.0);
        assert_near(copy_fraction_for_source_notional(15.0), 0.5);
    }

    #[test]
    fn five_share_small_buy_is_copied_at_one_hundred_percent() {
        let mut engine = test_engine();
        let reports = engine.handle_trade(
            &test_employee(),
            &test_trade("BUY", "weather-market", "asset-small", 0.30, 1.50, 100),
        );
        let skip = reports
            .iter()
            .find(|report| report.action == "SKIP:累计等待最小订单")
            .expect("expected small buy to accumulate");

        assert_eq!(skip.status, "skipped");
        assert!(skip.reason.contains("按 100% 金额目标 1.5000U"));
        assert!(skip.reason.contains("当前缺口 1.5000U"));
    }

    #[test]
    fn strict_fraction_buy_targets_source_notional_not_source_shares() {
        let mut engine = test_engine();
        engine.config.small_buy_full_copy_enabled = false;

        let reports = engine.handle_trade(
            &test_employee(),
            &test_trade("BUY", "weather-market", "asset-low", 0.05, 10.0, 100),
        );
        let buy = reports
            .iter()
            .find(|report| report.action == "BUY")
            .expect("expected strict half buy");

        assert_eq!(buy.status, "dry-run");
        assert_near(buy.copy_amount_usd, 5.0);
        assert!(buy.reason.contains("按 50% 金额原始目标 5.0000U"));
        assert!(buy.reason.contains("最终目标 5.0000U"));
    }

    #[test]
    fn event_strategy_low_price_tiny_buy_gets_minimum_leg() {
        let mut engine = test_engine_with_event_strategy();
        let reports = engine.handle_trade(
            &test_employee(),
            &test_trade("BUY", "weather-market", "asset-low-leg", 0.05, 0.20, 100),
        );
        let buy = reports
            .iter()
            .find(|report| report.action == "BUY")
            .expect("expected low-price event strategy buy");

        assert_eq!(buy.status, "dry-run");
        assert_near(buy.copy_amount_usd, 1.0);
        assert!(buy.reason.contains("策略目标原始 1.0000U"));
        assert!(buy.reason.contains("价格档 0-10c"));
        assert_eq!(engine.state.source_event_baskets.len(), 1);
        assert_eq!(engine.state.source_event_baskets[0].buy_outcome_count(), 1);
    }

    #[test]
    fn event_strategy_high_price_positive_target_rounds_to_exchange_minimum() {
        let mut engine = test_engine_with_event_strategy();
        let reports = engine.handle_trade(
            &test_employee(),
            &test_trade("BUY", "weather-market", "asset-high", 0.92, 200.0, 100),
        );
        let buy = reports
            .iter()
            .find(|report| report.action == "BUY")
            .expect("expected high-price minimum-sized buy");

        assert_eq!(buy.status, "dry-run");
        assert_near(buy.copy_amount_usd, 4.6);
        assert!(buy.reason.contains("价格档 85-95c"));
        assert!(buy.reason.contains("交易所最低订单约束"));
        assert!(!buy.reason.contains("最终目标 50.0000U"));
    }

    #[test]
    fn event_strategy_rebalances_after_second_outcome_enters_basket() {
        let mut engine = test_engine_with_event_strategy();
        let employee = test_employee();
        let first = engine.handle_trade(
            &employee,
            &test_trade("BUY", "weather-market", "asset-main", 0.40, 50.0, 100),
        );
        let first_buy = first
            .iter()
            .find(|report| report.action == "BUY")
            .expect("expected first hot-path buy");
        assert_near(first_buy.copy_amount_usd, 6.0);
        assert!(first_buy
            .reason
            .contains("未达到 2 个 outcome 的篮子重算门槛"));

        let second = engine.handle_trade(
            &employee,
            &test_trade("BUY", "weather-market", "asset-tail", 0.10, 10.0, 110),
        );
        let second_buy = second
            .iter()
            .find(|report| report.action == "BUY")
            .expect("expected basket-weighted second outcome buy");

        assert_near(second_buy.copy_amount_usd, 2.5);
        assert!(second_buy.reason.contains("事件篮子"));
        assert!(second_buy.reason.contains("当前 outcome 权重 16.67%"));
        assert_eq!(engine.state.source_event_baskets[0].buy_outcome_count(), 2);
    }

    #[test]
    fn tiny_remaining_buy_gap_waits_for_one_usd_exchange_minimum() {
        let mut engine = test_engine();
        let trade = test_trade("BUY", "weather-market", "asset-30", 0.05, 0.50, 120);
        let key = position_key(&trade);
        engine.state.source_buy_targets.push(SourceBuyTarget {
            position_key: key.clone(),
            asset: trade.asset.clone(),
            source_buy_notional_usd: 20.0,
            source_buy_size_shares: 400.0,
            first_buy_at_secs: 100,
            last_buy_at_secs: 100,
        });
        engine.state.positions.push(CopyPosition {
            position_key: key,
            market_title: trade.title.clone(),
            outcome: trade.outcome.clone(),
            asset: trade.asset.clone(),
            condition_id: trade.condition_id.clone(),
            size_shares: 192.0,
            cost_usd: 9.60,
            realized_pnl_usd: 0.0,
            updated_at_secs: 100,
        });

        let reports = engine.handle_trade_with_source_positions(&test_employee(), &trade, None);

        assert!(reports
            .iter()
            .any(|report| report.reason.contains("1.00U 约束")));
        assert!(!reports
            .iter()
            .any(|report| report.action == "BUY" && report.status != "skipped"));
    }

    #[test]
    fn partial_fill_status_stays_pending_and_keeps_order() {
        let mut engine = test_engine();
        let order = PendingCopyOrder {
            local_order_id: "local-partial".to_owned(),
            external_order_id: Some("external-partial".to_owned()),
            position_key: "market:asset".to_owned(),
            side: "BUY".to_owned(),
            market_title: Some("Weather market".to_owned()),
            outcome: Some("Yes".to_owned()),
            asset: "asset".to_owned(),
            condition_id: "market".to_owned(),
            copy_amount_usd: 4.58,
            limit_price: 0.2415,
            requested_limit_price: None,
            filled_amount_usd: 0.0,
            filled_size: 0.0,
            created_at_secs: 100,
            expires_at_secs: 0,
            last_sync_at_secs: 100,
            source_trade_key: "source".to_owned(),
            source_trade_at_secs: 98,
        };
        engine.state.pending_orders.push(order.clone());
        let mut execution = ExecutionResult {
            status: ExecutionStatus::Filled,
            order_id: order.external_order_id.clone(),
            order_price: Some(order.limit_price),
            filled_amount_usd: Some(1.20),
            filled_size: Some(1.20 / order.limit_price),
            filled_price: Some(order.limit_price),
            realized_pnl_usd: None,
            actual_balance_shares: None,
            target_size_shares: None,
            message: Some("synced order".to_owned()),
        };

        normalize_pending_sync_status(&order, &mut execution);
        assert_eq!(execution.status, ExecutionStatus::Pending);
        let mut report = engine.report_for_pending_execution("SYNC", &order, &execution);
        engine.apply_pending_sync(&order.local_order_id, &execution, &mut report, 130);

        let pending = engine
            .state
            .pending_order(&order.local_order_id)
            .expect("partial order should remain pending");
        assert_near(pending.filled_amount_usd, 1.20);
        assert!(report.text.contains("挂单部分成交"));
        assert!(report.text.contains("本次成交"));
        assert!(report.text.contains("剩余挂单"));
    }

    #[test]
    fn cancelled_order_applies_late_fill_before_removal() {
        let mut engine = test_engine();
        let order = PendingCopyOrder {
            local_order_id: "local-cancelled-fill".to_owned(),
            external_order_id: Some("external-cancelled-fill".to_owned()),
            position_key: "market:asset".to_owned(),
            side: "BUY".to_owned(),
            market_title: Some("Weather market".to_owned()),
            outcome: Some("Yes".to_owned()),
            asset: "asset".to_owned(),
            condition_id: "market".to_owned(),
            copy_amount_usd: 4.0,
            limit_price: 0.04,
            requested_limit_price: None,
            filled_amount_usd: 0.0,
            filled_size: 0.0,
            created_at_secs: 100,
            expires_at_secs: 0,
            last_sync_at_secs: 100,
            source_trade_key: "source".to_owned(),
            source_trade_at_secs: 98,
        };
        engine.state.pending_orders.push(order.clone());
        let execution = ExecutionResult {
            status: ExecutionStatus::Cancelled,
            order_id: order.external_order_id.clone(),
            order_price: Some(order.limit_price),
            filled_amount_usd: Some(2.0),
            filled_size: Some(50.0),
            filled_price: Some(order.limit_price),
            realized_pnl_usd: None,
            actual_balance_shares: None,
            target_size_shares: None,
            message: Some("cancelled and reconciled final matched size".to_owned()),
        };
        let mut report = engine.report_for_pending_execution("CANCEL", &order, &execution);

        engine.apply_pending_sync(&order.local_order_id, &execution, &mut report, 130);

        assert!(engine.state.pending_order(&order.local_order_id).is_none());
        let position = engine
            .state
            .position(&order.position_key)
            .expect("late fill should become a tracked position");
        assert_near(position.size_shares, 50.0);
        assert_near(position.cost_usd, 2.0);
    }

    #[test]
    fn source_sell_without_local_or_source_position_does_not_blind_clear() {
        let mut engine = test_engine();
        let employee = test_employee();
        let trade = test_trade("SELL", "weather-market", "asset-33", 0.051, 191.76, 100);

        let reports = engine.handle_trade_with_source_positions(&employee, &trade, None);

        assert!(!reports.iter().any(|report| report.action == "SELL"));
        assert!(reports
            .iter()
            .any(|report| report.reason.contains("不能可靠计算减仓比例")));
    }

    #[test]
    fn source_sell_uses_fresh_proportion_when_local_position_is_missing() {
        let mut engine = test_engine();
        let employee = test_employee();
        let trade = test_trade("SELL", "weather-market", "asset-33", 0.10, 1.0, 100);
        let source_positions =
            HashMap::from([("asset-33".to_owned(), test_source_position(90.0, 0.10))]);

        let reports =
            engine.handle_trade_with_source_positions(&employee, &trade, Some(&source_positions));

        let sell = reports
            .iter()
            .find(|report| report.action == "SELL")
            .expect("fresh source position should produce a proportional sell");
        assert_eq!(sell.status, "dry-run");
        assert!(sell.reason.contains("减仓比例 10.00%"));
        assert!(sell.reason.contains("不因本地缺记录而清仓"));
    }

    #[test]
    fn source_sell_uses_activity_ledger_when_positions_are_unavailable() {
        let mut engine = test_engine();
        let employee = test_employee();
        let key = "weather-market:asset-ledger";

        let buy = test_trade("BUY", "weather-market", "asset-ledger", 0.40, 100.0, 100);
        let _ = engine.handle_trade(&employee, &buy);
        engine.state.positions.push(CopyPosition {
            position_key: key.to_owned(),
            market_title: buy.title.clone(),
            outcome: buy.outcome.clone(),
            asset: buy.asset.clone(),
            condition_id: buy.condition_id.clone(),
            size_shares: 50.0,
            cost_usd: 20.0,
            realized_pnl_usd: 0.0,
            updated_at_secs: 100,
        });

        let reports = engine.handle_trade_with_source_positions(
            &employee,
            &test_trade("SELL", "weather-market", "asset-ledger", 0.40, 10.0, 110),
            None,
        );
        let sell = reports
            .iter()
            .find(|report| report.action == "SELL")
            .expect("activity ledger should produce a proportional sell");

        assert_eq!(sell.status, "dry-run");
        assert!(sell.reason.contains("本地 /activity 账本"));
        assert!(sell.reason.contains("减仓比例 10.00%"));
    }

    #[test]
    fn small_source_sell_uses_passive_limit_note() {
        let mut engine = test_engine();
        let employee = test_employee();
        let key = "weather-market:asset-small-sell";
        let buy = test_trade(
            "BUY",
            "weather-market",
            "asset-small-sell",
            0.40,
            100.0,
            100,
        );
        let _ = engine.handle_trade(&employee, &buy);
        engine.state.positions.push(CopyPosition {
            position_key: key.to_owned(),
            market_title: buy.title.clone(),
            outcome: buy.outcome.clone(),
            asset: buy.asset.clone(),
            condition_id: buy.condition_id.clone(),
            size_shares: 50.0,
            cost_usd: 20.0,
            realized_pnl_usd: 0.0,
            updated_at_secs: 100,
        });

        let reports = engine.handle_trade_with_source_positions(
            &employee,
            &test_trade(
                "SELL",
                "weather-market",
                "asset-small-sell",
                0.40,
                10.0,
                110,
            ),
            None,
        );
        let sell = reports
            .iter()
            .find(|report| report.action == "SELL")
            .expect("small sell should still produce a sell report");

        assert!(sell.reason.contains("小比例试探性 SELL"));
        assert!(sell.reason.contains("34.00c 挂 GTC 限价卖单"));
    }

    #[test]
    fn pending_sell_sync_applies_filled_delta_to_position() {
        let mut engine = test_engine();
        let order = PendingCopyOrder {
            local_order_id: "local-sell-partial".to_owned(),
            external_order_id: Some("external-sell-partial".to_owned()),
            position_key: "market:asset-sell".to_owned(),
            side: "SELL".to_owned(),
            market_title: Some("Weather market".to_owned()),
            outcome: Some("Yes".to_owned()),
            asset: "asset-sell".to_owned(),
            condition_id: "market".to_owned(),
            copy_amount_usd: 4.0,
            limit_price: 0.40,
            requested_limit_price: Some(0.40),
            filled_amount_usd: 0.0,
            filled_size: 0.0,
            created_at_secs: 100,
            expires_at_secs: 0,
            last_sync_at_secs: 100,
            source_trade_key: "source-sell".to_owned(),
            source_trade_at_secs: 98,
        };
        engine.state.positions.push(CopyPosition {
            position_key: order.position_key.clone(),
            market_title: order.market_title.clone(),
            outcome: order.outcome.clone(),
            asset: order.asset.clone(),
            condition_id: order.condition_id.clone(),
            size_shares: 25.0,
            cost_usd: 5.0,
            realized_pnl_usd: 0.0,
            updated_at_secs: 100,
        });
        engine.state.pending_orders.push(order.clone());
        let execution = ExecutionResult {
            status: ExecutionStatus::Pending,
            order_id: order.external_order_id.clone(),
            order_price: Some(order.limit_price),
            filled_amount_usd: Some(2.0),
            filled_size: Some(5.0),
            filled_price: Some(order.limit_price),
            realized_pnl_usd: None,
            actual_balance_shares: None,
            target_size_shares: Some(10.0),
            message: Some("synced partially filled sell order".to_owned()),
        };
        let mut report = engine.report_for_pending_execution("SYNC", &order, &execution);

        engine.apply_pending_sync(&order.local_order_id, &execution, &mut report, 130);

        let position = engine
            .state
            .position(&order.position_key)
            .expect("position should remain after partial sell");
        assert_near(position.size_shares, 20.0);
        assert_near(position.cost_usd, 4.0);
        assert_near(engine.state.daily_realized_pnl_usd, 1.0);
        assert!(engine.state.pending_order(&order.local_order_id).is_some());
    }

    #[test]
    fn dust_exit_is_silently_finished() {
        let execution = ExecutionResult {
            status: ExecutionStatus::Skipped,
            order_id: None,
            order_price: None,
            filled_amount_usd: None,
            filled_size: None,
            filled_price: None,
            realized_pnl_usd: None,
            actual_balance_shares: Some(0.009043),
            target_size_shares: Some(0.009043),
            message: Some(
                "actual CLOB token balance 0.009043 is below minimum sell size 5.00".to_owned(),
            ),
        };

        assert!(should_silently_finish_dust_exit(&execution));
    }

    #[test]
    fn sub_minimum_proportional_sell_is_accumulated_not_rounded_up() {
        let execution = ExecutionResult {
            status: ExecutionStatus::Skipped,
            order_id: None,
            order_price: None,
            filled_amount_usd: None,
            filled_size: None,
            filled_price: None,
            realized_pnl_usd: None,
            actual_balance_shares: Some(50.0),
            target_size_shares: Some(1.12),
            message: Some(
                "proportional sell target 1.120000 shares is below exchange minimum 5.00; accumulate future source SELL before submitting"
                    .to_owned(),
            ),
        };

        assert!(should_accumulate_small_exit(&execution));
        assert!(!should_silently_finish_dust_exit(&execution));
    }

    #[test]
    fn direct_chase_uses_pct_without_absolute_cap() {
        assert_near(price_with_pct_upside(0.7706, 0.15), 0.88619);
        assert_near(price_with_pct_upside(0.95, 0.15), 0.99);
    }

    #[test]
    fn passive_limits_use_pct_with_absolute_cap() {
        assert_near(price_with_capped_upside(0.064, 0.15, 0.03), 0.0736);
        assert_near(price_with_capped_upside(0.50, 0.15, 0.03), 0.53);
    }

    #[test]
    fn low_price_buys_use_tiered_aggressive_chase() {
        let engine = test_engine();

        assert_near(engine.effective_buy_chase_pct(0.10), 1.0);
        assert_near(engine.effective_buy_chase_pct(0.20), 0.5);
        assert_near(engine.effective_buy_chase_pct(0.40), 0.15);
        assert_near(engine.effective_buy_chase_pct(0.95), 0.0);
    }

    #[test]
    fn source_position_target_reconcile_uses_employee_avg_cost_and_posts_only() {
        let mut engine = test_engine();
        let mut source_position = test_source_position(100.0, 0.15);
        source_position.current_price = Some(0.50);
        let source_positions = HashMap::from([("asset-30".to_owned(), source_position)]);

        let reports = engine.reconcile_absent_from_source_positions_step(&source_positions);

        let report = reports
            .iter()
            .find(|report| report.action == "BUY")
            .expect("expected target reconcile buy");
        assert_eq!(report.status, "dry-run");
        assert_near(report.copy_amount_usd, 7.5);
        assert_near(report.copy_price.expect("expected passive price"), 0.15);
        assert!(report.reason.contains("持仓均价 15.00c"));
        assert!(report.reason.contains("金额原始目标 7.5000U"));
        assert!(report.reason.contains("风控后目标 7.5000U"));
        assert!(report.reason.contains("挂 post-only"));
        assert!(report.reason.contains("不按当前市价/FOK 追买"));
    }

    #[test]
    fn source_position_reconcile_uses_latest_supplement_price() {
        let mut engine = test_engine();
        let supplement = test_trade("BUY", "weather-market", "asset-30", 0.87, 34.8, 200);
        engine.state.record_source_outcome(&supplement, 200);
        let mut source_position = test_source_position(50.54, 0.8512);
        source_position.current_price = Some(0.9995);
        let source_positions = HashMap::from([("asset-30".to_owned(), source_position)]);

        let reports = engine.reconcile_absent_from_source_positions_step(&source_positions);
        let buy = reports
            .iter()
            .find(|report| report.action == "BUY")
            .expect("expected supplement reconcile buy");

        assert_near(buy.copy_price.expect("expected latest-entry limit"), 0.89);
        assert!(buy.reason.contains("持仓均价 85.12c"));
        assert!(buy.reason.contains("最近补仓/买入价 87.00c"));
    }

    #[test]
    fn source_position_reconcile_caps_high_price_target_at_fifty_usd() {
        let mut engine = test_engine();
        engine.config.small_buy_full_copy_enabled = false;
        let supplement = test_trade("BUY", "weather-market", "asset-945", 0.945, 500.0, 200);
        engine.state.record_source_outcome(&supplement, 200);
        let source_position = test_source_position(600.0, 0.945);
        let source_positions = HashMap::from([("asset-945".to_owned(), source_position)]);

        let reports = engine.reconcile_absent_from_source_positions_step(&source_positions);
        let buy = reports
            .iter()
            .find(|report| report.action == "BUY")
            .expect("expected capped high-price reconcile buy");

        assert_eq!(buy.status, "dry-run");
        assert_near(buy.copy_amount_usd, 50.0);
        assert!(buy.reason.contains("金额原始目标 283.5000U"));
        assert!(buy.reason.contains("风控后目标 50.0000U"));
        assert!(buy.reason.contains("高价风控"));
    }

    #[test]
    fn source_position_reconcile_global_cap_applies_below_high_price_zone() {
        let mut engine = test_engine();
        engine.config.small_buy_full_copy_enabled = false;
        let source_position = test_source_position(500.0, 0.40);
        let source_positions = HashMap::from([("asset-40".to_owned(), source_position)]);

        let reports = engine.reconcile_absent_from_source_positions_step(&source_positions);
        let buy = reports
            .iter()
            .find(|report| report.action == "BUY")
            .expect("expected globally capped reconcile buy");

        assert_eq!(buy.status, "dry-run");
        assert_near(buy.copy_amount_usd, 50.0);
        assert!(buy.reason.contains("金额原始目标 100.0000U"));
        assert!(buy.reason.contains("风控后目标 50.0000U"));
        assert!(buy.reason.contains("全员仓位风控"));
    }

    #[test]
    fn source_position_reconcile_does_not_reprice_pending_from_positions() {
        let mut engine = test_engine();
        let supplement = test_trade("BUY", "weather-market", "asset-30", 0.87, 34.8, 200);
        engine.state.record_source_outcome(&supplement, 200);
        engine.state.pending_orders.push(PendingCopyOrder {
            local_order_id: "old-low-bid".to_owned(),
            external_order_id: Some("external-low-bid".to_owned()),
            position_key: position_key(&supplement),
            side: "BUY".to_owned(),
            market_title: supplement.title.clone(),
            outcome: supplement.outcome.clone(),
            asset: supplement.asset.clone(),
            condition_id: supplement.condition_id.clone(),
            copy_amount_usd: 16.0,
            limit_price: 0.80,
            requested_limit_price: None,
            filled_amount_usd: 0.0,
            filled_size: 0.0,
            created_at_secs: 100,
            expires_at_secs: 0,
            last_sync_at_secs: 100,
            source_trade_key: source_trade_key(&supplement),
            source_trade_at_secs: 100,
        });
        let mut source_position = test_source_position(50.54, 0.8512);
        source_position.current_price = Some(0.9995);
        let source_positions = HashMap::from([("asset-30".to_owned(), source_position)]);

        let reports = engine.reconcile_absent_from_source_positions_step(&source_positions);
        let skip = reports
            .iter()
            .find(|report| report.action == "SKIP:对账不抬价重挂")
            .expect("expected no-reprice notice");

        assert!(skip.reason.contains("不再因为 /positions"));
        assert!(skip.reason.contains("保留原挂单"));
        assert!(engine.state.pending_order("old-low-bid").is_some());
        assert!(!reports.iter().any(|report| report.action == "CANCEL"));
    }

    #[test]
    fn post_only_market_adjustment_is_not_mistaken_for_stale_strategy_price() {
        let mut engine = test_engine();
        let latest_buy = test_trade("BUY", "weather-market", "asset-30", 0.35, 1.75, 200);
        engine.state.record_source_outcome(&latest_buy, 200);
        engine.state.pending_orders.push(PendingCopyOrder {
            local_order_id: "market-adjusted".to_owned(),
            external_order_id: Some("external-adjusted".to_owned()),
            position_key: position_key(&latest_buy),
            side: "BUY".to_owned(),
            market_title: latest_buy.title.clone(),
            outcome: latest_buy.outcome.clone(),
            asset: latest_buy.asset.clone(),
            condition_id: latest_buy.condition_id.clone(),
            copy_amount_usd: 2.70,
            limit_price: 0.27,
            requested_limit_price: Some(0.3675),
            filled_amount_usd: 0.0,
            filled_size: 0.0,
            created_at_secs: 200,
            expires_at_secs: 0,
            last_sync_at_secs: 200,
            source_trade_key: source_trade_key(&latest_buy),
            source_trade_at_secs: 200,
        });
        let mut source_position = test_source_position(10.0, 0.325);
        source_position.current_price = Some(0.28);
        let source_positions = HashMap::from([("asset-30".to_owned(), source_position)]);

        let reports = engine.reconcile_absent_from_source_positions_step(&source_positions);

        assert!(reports.is_empty(), "unexpected reports: {reports:?}");
    }

    #[test]
    fn source_position_target_reconcile_skips_high_cost_low_edge_positions() {
        let mut engine = test_engine();
        let source_positions =
            HashMap::from([("asset-30".to_owned(), test_source_position(100.0, 0.97))]);

        let reports = engine.reconcile_absent_from_source_positions_step(&source_positions);

        assert!(!reports.iter().any(|report| report.action == "BUY"));
    }

    #[test]
    fn source_position_target_reconcile_does_not_add_two_cents_above_high_price_cost() {
        let mut engine = test_engine();
        let source_positions =
            HashMap::from([("asset-30".to_owned(), test_source_position(20.0, 0.94))]);

        let reports = engine.reconcile_absent_from_source_positions_step(&source_positions);
        let buy = reports
            .iter()
            .find(|report| report.action == "BUY")
            .expect("expected conservative high-price reconcile buy");

        assert_near(buy.copy_price.expect("expected price"), 0.94);
        assert_near(buy.copy_amount_usd, 9.4);
    }

    #[test]
    fn source_position_target_reconcile_skips_outside_employee_specialty() {
        let mut engine = test_engine();
        let mut source_position = test_source_position(100.0, 0.15);
        source_position.market_title = Some("Will Bitcoin reach $200,000 this year?".to_owned());
        source_position.slug = Some("will-bitcoin-reach-200000".to_owned());
        source_position.event_slug = source_position.slug.clone();
        let source_positions = HashMap::from([("asset-30".to_owned(), source_position)]);

        let reports = engine.reconcile_absent_from_source_positions_step(&source_positions);

        assert!(!reports.iter().any(|report| report.action == "BUY"));
    }

    #[test]
    fn source_position_target_reconcile_skips_positions_down_more_than_ninety_five_percent() {
        let mut engine = test_engine();
        let mut source_position = test_source_position(20.0, 0.34);
        source_position.current_price = Some(0.002);
        let source_positions = HashMap::from([("asset-30".to_owned(), source_position)]);

        let reports = engine.reconcile_absent_from_source_positions_step(&source_positions);

        assert!(!reports.iter().any(|report| report.action == "BUY"));
    }

    #[test]
    fn source_position_target_reconcile_keeps_exact_ninety_five_percent_boundary() {
        let mut engine = test_engine();
        let mut source_position = test_source_position(20.0, 0.34);
        source_position.current_price = Some(0.017);
        let source_positions = HashMap::from([("asset-30".to_owned(), source_position)]);

        let reports = engine.reconcile_absent_from_source_positions_step(&source_positions);

        assert!(reports.iter().any(|report| report.action == "BUY"));
    }

    #[test]
    fn blocked_outcome_is_ignored_by_live_trade_and_position_reconcile() {
        let mut engine = test_engine();
        engine.config.blocked_position_keys = vec!["weather-market:asset-30".to_owned()];
        let trade = test_trade("BUY", "weather-market", "asset-30", 0.34, 5.06, 100);
        let employee = test_employee();

        assert!(engine.handle_trade(&employee, &trade).is_empty());

        let source_positions =
            HashMap::from([("asset-30".to_owned(), test_source_position(20.0, 0.34))]);
        assert!(engine
            .reconcile_absent_from_source_positions_step(&source_positions)
            .is_empty());
    }

    #[test]
    fn failed_reconcile_buy_cooldown_blocks_immediate_retry() {
        let mut engine = test_engine();
        engine.state.record_failure(
            action_failure_cooldown_key("BUY", "weather-market:asset-30"),
            "network failure".to_owned(),
            now_secs(),
        );
        let source_positions =
            HashMap::from([("asset-30".to_owned(), test_source_position(20.0, 0.34))]);

        let reports = engine.reconcile_absent_from_source_positions_step(&source_positions);

        assert!(reports.is_empty(), "unexpected reports: {reports:?}");
    }

    #[test]
    fn source_position_reconcile_cancels_existing_ineligible_buy() {
        let mut engine = test_engine();
        let trade = test_trade("BUY", "weather-market", "asset-30", 0.99, 10.0, 100);
        engine.state.pending_orders.push(PendingCopyOrder {
            local_order_id: "local-high".to_owned(),
            external_order_id: Some("external-high".to_owned()),
            position_key: position_key(&trade),
            side: "BUY".to_owned(),
            market_title: trade.title.clone(),
            outcome: trade.outcome.clone(),
            asset: trade.asset.clone(),
            condition_id: trade.condition_id.clone(),
            copy_amount_usd: 10.0,
            limit_price: 0.99,
            requested_limit_price: None,
            filled_amount_usd: 0.0,
            filled_size: 0.0,
            created_at_secs: 100,
            expires_at_secs: 0,
            last_sync_at_secs: 100,
            source_trade_key: source_trade_key(&trade),
            source_trade_at_secs: 100,
        });
        let source_positions =
            HashMap::from([("asset-30".to_owned(), test_source_position(100.0, 0.99))]);

        let reports = engine.reconcile_absent_from_source_positions_step(&source_positions);

        assert!(reports.iter().any(|report| {
            report.action == "CANCEL" && report.reason.contains("高价低收益")
        }));
    }

    #[test]
    fn source_position_target_reconcile_does_not_rebuy_after_sell_until_new_buy() {
        let mut engine = test_engine();
        let buy = test_trade("BUY", "weather-market", "asset-30", 0.15, 15.0, 100);
        let sell = test_trade("SELL", "weather-market", "asset-30", 0.50, 5.0, 110);
        engine.state.record_source_outcome(&buy, 100);
        engine.state.record_source_outcome(&sell, 110);
        engine.state.positions.push(CopyPosition {
            position_key: position_key(&buy),
            market_title: buy.title.clone(),
            outcome: buy.outcome.clone(),
            asset: buy.asset.clone(),
            condition_id: buy.condition_id.clone(),
            size_shares: 20.0,
            cost_usd: 3.0,
            realized_pnl_usd: 0.0,
            updated_at_secs: 110,
        });
        let source_positions =
            HashMap::from([("asset-30".to_owned(), test_source_position(100.0, 0.15))]);

        let blocked = engine.reconcile_absent_from_source_positions_step(&source_positions);
        assert!(blocked.is_empty());

        let reentry = test_trade("BUY", "weather-market", "asset-30", 0.16, 16.0, 120);
        engine.state.record_source_outcome(&reentry, 120);
        let resumed = engine.reconcile_absent_from_source_positions_step(&source_positions);
        assert!(resumed.iter().any(|report| report.action == "BUY"));
    }

    #[test]
    fn source_position_target_reconcile_counts_pending_buy_as_committed() {
        let mut engine = test_engine();
        let trade = test_trade("BUY", "weather-market", "asset-30", 0.10, 10.0, 100);
        engine.state.record_source_outcome(&trade, 100);
        engine.state.pending_orders.push(PendingCopyOrder {
            local_order_id: "local".to_owned(),
            external_order_id: Some("external".to_owned()),
            position_key: position_key(&trade),
            side: "BUY".to_owned(),
            market_title: trade.title.clone(),
            outcome: trade.outcome.clone(),
            asset: trade.asset.clone(),
            condition_id: trade.condition_id.clone(),
            copy_amount_usd: 10.0,
            limit_price: 0.10,
            requested_limit_price: None,
            filled_amount_usd: 0.0,
            filled_size: 0.0,
            created_at_secs: 100,
            expires_at_secs: 0,
            last_sync_at_secs: 100,
            source_trade_key: source_trade_key(&trade),
            source_trade_at_secs: 100,
        });
        let source_positions =
            HashMap::from([("asset-30".to_owned(), test_source_position(100.0, 0.10))]);

        let reports = engine.reconcile_absent_from_source_positions_step(&source_positions);

        assert!(reports.is_empty(), "unexpected reports: {reports:?}");
    }

    #[test]
    fn source_position_target_reconcile_never_sells_excess_while_source_still_holds() {
        let mut engine = test_engine();
        engine.state.positions.push(CopyPosition {
            position_key: "weather-market:asset-30".to_owned(),
            market_title: Some("Weather market".to_owned()),
            outcome: Some("Yes".to_owned()),
            asset: "asset-30".to_owned(),
            condition_id: "weather-market".to_owned(),
            size_shares: 120.0,
            cost_usd: 12.0,
            realized_pnl_usd: 0.0,
            updated_at_secs: 100,
        });
        let source_positions =
            HashMap::from([("asset-30".to_owned(), test_source_position(100.0, 0.10))]);

        let reports = engine.reconcile_absent_from_source_positions_step(&source_positions);

        assert!(reports.is_empty());
    }

    #[test]
    fn cooled_down_stale_exit_does_not_block_target_reconcile_buy() {
        let mut engine = test_engine();
        let stale_key = "old-market:old-asset";
        engine.state.positions.push(CopyPosition {
            position_key: stale_key.to_owned(),
            market_title: Some("Old market".to_owned()),
            outcome: Some("Yes".to_owned()),
            asset: "old-asset".to_owned(),
            condition_id: "old-market".to_owned(),
            size_shares: 50.0,
            cost_usd: 5.0,
            realized_pnl_usd: 0.0,
            updated_at_secs: 100,
        });
        engine.state.record_failure(
            action_failure_cooldown_key("SELL", stale_key),
            "no orders found to match with FAK order".to_owned(),
            now_secs(),
        );
        let source_positions =
            HashMap::from([("asset-30".to_owned(), test_source_position(20.0, 0.46))]);

        let reports = engine.reconcile_absent_from_source_positions_step(&source_positions);

        assert!(reports.iter().any(|report| report.action == "BUY"));
        assert!(!reports.iter().any(|report| report.action == "SELL"));
    }

    #[test]
    fn target_reconcile_prioritizes_recent_source_buy_over_larger_history() {
        let mut engine = test_engine();
        let older = test_trade("BUY", "weather-market", "asset-old", 0.50, 500.0, 100);
        let newer = test_trade("BUY", "weather-market", "asset-new", 0.46, 9.20, 200);
        engine.state.record_source_outcome(&older, 100);
        engine.state.record_source_outcome(&newer, 200);
        let source_positions = HashMap::from([
            ("asset-old".to_owned(), test_source_position(1_000.0, 0.50)),
            ("asset-new".to_owned(), test_source_position(20.0, 0.46)),
        ]);

        let reports = engine.reconcile_absent_from_source_positions_step(&source_positions);
        let buy = reports
            .iter()
            .find(|report| report.action == "BUY")
            .expect("expected recent target reconcile buy");

        assert_eq!(buy.position_key, position_key(&newer));
        assert_near(buy.copy_amount_usd, 9.2);
    }

    #[test]
    fn buy_ttl_label_only_applies_to_pending_orders() {
        assert_eq!(
            buy_ttl_label(ExecutionStatus::Failed, 0, "WeatherHK"),
            "未成功挂单，TTL 未生效"
        );
        assert_eq!(
            buy_ttl_label(ExecutionStatus::Filled, 0, "WeatherHK"),
            "已成交，TTL 不适用"
        );
        assert_eq!(
            buy_ttl_label(ExecutionStatus::Pending, 0, "WeatherHK"),
            "无 TTL，WeatherHK 未卖出则继续挂单"
        );
        assert_eq!(
            buy_ttl_label(ExecutionStatus::Pending, 60, "WeatherHK"),
            "TTL 60s"
        );
    }

    #[test]
    fn low_edge_high_probability_buys_are_skipped() {
        assert!(!should_skip_low_edge_buy(0.979, 0.98));
        assert!(!should_skip_low_edge_buy(0.98, 0.98));
        assert!(should_skip_low_edge_buy(0.980_001, 0.98));
        assert!(should_skip_low_edge_buy(0.99, 0.98));
    }

    #[test]
    fn routine_skip_reports_stay_out_of_telegram() {
        let engine = test_engine();
        let trade = test_trade("BUY", "weather-market", "asset-30", 0.99, 10.0, 100);
        let routine = engine.skip_report("高概率低收益", &trade, "routine");
        let critical = engine.skip_report("源实际仓位不可用", &trade, "critical");

        assert!(!routine.should_notify());
        assert!(critical.should_notify());
    }

    #[test]
    fn source_reconcile_sell_skipped_reports_notify() {
        let report = AutoCopyReport {
            action: "SELL".to_owned(),
            status: "skipped".to_owned(),
            reason: "源仓位对账显示 OlympusHive 当前已不持有该 outcome".to_owned(),
            source_trade_key: "-".to_owned(),
            market_title: Some("Weather market".to_owned()),
            outcome: Some("Yes".to_owned()),
            position_key: "market:asset".to_owned(),
            source_price: None,
            source_notional_usd: 0.0,
            copy_amount_usd: 6.3,
            copy_price: None,
            order_id: Some("0xabc".to_owned()),
            realized_pnl_usd: None,
            market_exposure_after_usd: 6.3,
            daily_spend_after_usd: 6.3,
            created_at_secs: now_secs(),
            text: "source reconcile sell skipped".to_owned(),
        };

        assert!(report.should_notify());
    }

    #[test]
    fn near_zero_tail_filter_is_configurable_and_disabled_at_zero() {
        assert!(should_skip_near_zero_buy(0.004, 0.005));
        assert!(should_skip_near_zero_buy(0.005, 0.005));
        assert!(!should_skip_near_zero_buy(0.006, 0.005));
        assert!(!should_skip_near_zero_buy(0.004, 0.0));
        assert!(!should_skip_near_zero_buy(0.005, 0.0));
    }

    #[test]
    fn small_source_buy_filter_is_configurable_and_disabled_at_zero() {
        assert!(should_skip_small_buy(0.99, 1.0));
        assert!(!should_skip_small_buy(1.0, 1.0));
        assert!(!should_skip_small_buy(0.001, 0.0));
        assert!(!should_skip_small_buy(0.01, 0.0));
    }

    #[test]
    fn source_sell_reduces_follower_by_actual_source_fraction() {
        let mut engine = test_engine();
        let employee = test_employee();
        engine
            .state
            .source_position_snapshots
            .push(SourcePositionSnapshot {
                asset: "asset-33".to_owned(),
                size_shares: 100.0,
                observed_at_secs: 90,
            });
        engine.state.positions.push(CopyPosition {
            position_key: "weather-market:asset-33".to_owned(),
            market_title: Some(
                "Will the highest temperature in Hong Kong be 33°C on June 3?".to_owned(),
            ),
            outcome: Some("Yes".to_owned()),
            asset: "asset-33".to_owned(),
            condition_id: "weather-market".to_owned(),
            size_shares: 4.0,
            cost_usd: 1.48,
            realized_pnl_usd: 0.0,
            updated_at_secs: 90,
        });

        let source_positions =
            HashMap::from([("asset-33".to_owned(), test_source_position(90.0, 0.10))]);
        let reports = engine.handle_trade_with_source_positions(
            &employee,
            &test_trade("SELL", "weather-market", "asset-33", 0.10, 0.20, 100),
            Some(&source_positions),
        );
        let sell = reports
            .iter()
            .find(|report| report.action == "SELL")
            .expect("expected sell report");

        assert_eq!(sell.status, "dry-run");
        assert_near(sell.copy_amount_usd, 1.48 * 0.10);
        assert!(sell.reason.contains("比例 10.00%"));
        assert!(sell.reason.contains("同比例减仓"));
    }

    #[test]
    fn absent_actual_source_position_triggers_full_exit() {
        let mut engine = test_engine();
        engine
            .state
            .source_position_snapshots
            .push(SourcePositionSnapshot {
                asset: "asset-33".to_owned(),
                size_shares: 100.0,
                observed_at_secs: 90,
            });

        let decision = engine
            .source_sell_decision("asset-33", 10.0, Some(&HashMap::new()), None, 100)
            .expect("expected full exit decision");

        assert_near(decision.sell_fraction, 1.0);
        assert!(decision.clear_all);
        assert!(decision.reason.contains("全部退出"));
    }

    #[test]
    fn large_actual_source_reduction_stays_proportional_while_source_holds() {
        let mut engine = test_engine();
        engine
            .state
            .source_position_snapshots
            .push(SourcePositionSnapshot {
                asset: "asset-33".to_owned(),
                size_shares: 100.0,
                observed_at_secs: 90,
            });
        let source_positions =
            HashMap::from([("asset-33".to_owned(), test_source_position(55.0, 0.10))]);

        let decision = engine
            .source_sell_decision("asset-33", 45.0, Some(&source_positions), None, 100)
            .expect("expected large reduction decision");

        assert_near(decision.sell_fraction, 0.45);
        assert!(!decision.clear_all);
        assert!(decision.reason.contains("不升级为清仓"));
    }

    #[test]
    fn delayed_activity_reconstructs_pre_sell_position_from_current_plus_trade() {
        let mut engine = test_engine();
        engine
            .state
            .source_position_snapshots
            .push(SourcePositionSnapshot {
                asset: "asset-30".to_owned(),
                size_shares: 7.8863,
                observed_at_secs: 90,
            });
        let source_positions =
            HashMap::from([("asset-30".to_owned(), test_source_position(7.8863, 0.10))]);

        let decision = engine
            .source_sell_decision("asset-30", 10.3291, Some(&source_positions), None, 100)
            .expect("expected delayed activity decision");

        assert!(!decision.clear_all);
        assert_near(decision.sell_fraction, 10.3291 / (7.8863 + 10.3291));
        assert!(decision.reason.contains("当前剩余 + 本笔卖出"));
        assert!(decision.reason.contains("56.71%"));
    }

    #[test]
    fn actual_position_drop_covers_later_sell_fragments_without_double_selling() {
        let mut engine = test_engine();
        engine
            .state
            .source_position_snapshots
            .push(SourcePositionSnapshot {
                asset: "asset-30".to_owned(),
                size_shares: 100.0,
                observed_at_secs: 90,
            });
        let source_positions =
            HashMap::from([("asset-30".to_owned(), test_source_position(80.0, 0.10))]);

        let first = engine
            .source_sell_decision("asset-30", 10.0, Some(&source_positions), None, 100)
            .expect("expected first batch decision");
        engine
            .state
            .replace_source_position_snapshots(&source_positions, 100);
        let second = engine
            .source_sell_decision("asset-30", 10.0, Some(&source_positions), None, 101)
            .expect("expected covered fragment decision");

        assert_near(first.sell_fraction, 0.20);
        assert_near(second.sell_fraction, 0.0);
        assert!(second.reason.contains("不重复卖出"));
    }

    #[test]
    fn fresh_source_sell_ignores_previous_execution_failure_cooldown() {
        let mut engine = test_engine();
        let employee = test_employee();
        let position_key = "weather-market:asset-30";
        engine.state.positions.push(CopyPosition {
            position_key: position_key.to_owned(),
            market_title: Some("Weather market".to_owned()),
            outcome: Some("Yes".to_owned()),
            asset: "asset-30".to_owned(),
            condition_id: "weather-market".to_owned(),
            size_shares: 25.0,
            cost_usd: 20.0,
            realized_pnl_usd: 0.0,
            updated_at_secs: 90,
        });
        engine
            .state
            .source_position_snapshots
            .push(SourcePositionSnapshot {
                asset: "asset-30".to_owned(),
                size_shares: 7.8863,
                observed_at_secs: 90,
            });
        engine.state.record_failure(
            action_failure_cooldown_key("SELL", position_key),
            "previous FOK failure".to_owned(),
            now_secs(),
        );
        let source_positions =
            HashMap::from([("asset-30".to_owned(), test_source_position(6.7994, 0.10))]);

        let reports = engine.handle_trade_with_source_positions(
            &employee,
            &test_trade("SELL", "weather-market", "asset-30", 0.92, 1.0, 100),
            Some(&source_positions),
        );

        assert!(reports
            .iter()
            .any(|report| report.action == "SELL" && report.status == "dry-run"));
    }

    #[test]
    fn partial_exit_reports_only_unfilled_target_as_remaining() {
        let execution = ExecutionResult {
            status: ExecutionStatus::Filled,
            order_id: None,
            order_price: Some(0.90),
            filled_amount_usd: Some(5.4),
            filled_size: Some(6.0),
            filled_price: Some(0.90),
            realized_pnl_usd: None,
            actual_balance_shares: Some(19.0),
            target_size_shares: Some(10.0),
            message: None,
        };

        assert_near(remaining_exit_target_size(&execution), 4.0);
    }

    #[test]
    fn transient_sell_failure_is_retried_on_tick() {
        let mut engine = test_engine();
        let position_key = "weather-market:asset-32";
        engine.state.positions.push(CopyPosition {
            position_key: position_key.to_owned(),
            market_title: Some(
                "Will the highest temperature in Hong Kong be 32°C on June 10?".to_owned(),
            ),
            outcome: Some("Yes".to_owned()),
            asset: "asset-32".to_owned(),
            condition_id: "weather-market".to_owned(),
            size_shares: 688.55,
            cost_usd: 9.15,
            realized_pnl_usd: 0.0,
            updated_at_secs: 90,
        });
        engine.state.record_failure(
            action_failure_cooldown_key("SELL", position_key),
            "PolyApiException[status_code=None, error_message=Request exception!]".to_owned(),
            100,
        );

        let reports = engine.handle_tick();

        assert!(reports.iter().any(|report| {
            report.action == "SELL"
                && report.status == "dry-run"
                && report.reason.contains("继续按原意图重试清仓")
        }));
        assert!(!engine.state.failure_in_cooldown(
            &action_failure_cooldown_key("SELL", position_key),
            now_secs(),
            TRANSIENT_EXIT_RETRY_SECONDS
        ));
    }

    #[test]
    fn pending_gtc_from_exit_retry_is_tracked_and_clears_retry() {
        let mut engine = test_engine();
        engine.config.mode = AutoCopyMode::LiveExternal;
        engine.config.executor_command = Some(
            "printf '%s' '{\"status\":\"pending\",\"order_id\":\"0xprotect\",\"order_price\":0.998,\"target_size_shares\":53.26,\"message\":\"placed protective GTC\"}'"
                .to_owned(),
        );
        let position_key = "weather-market:asset-33";
        engine.state.positions.push(CopyPosition {
            position_key: position_key.to_owned(),
            market_title: Some("Weather market".to_owned()),
            outcome: Some("No".to_owned()),
            asset: "asset-33".to_owned(),
            condition_id: "weather-market".to_owned(),
            size_shares: 53.26,
            cost_usd: 49.99,
            realized_pnl_usd: 0.0,
            updated_at_secs: 100,
        });
        engine.state.record_exit_retry(PendingExitRetry {
            position_key: position_key.to_owned(),
            sell_fraction: 1.0,
            target_size_shares: Some(53.26),
            min_sell_price: Some(0.998),
            force_market_sell: true,
            lock_profit: true,
        });
        let failure_key = action_failure_cooldown_key("SELL", position_key);
        engine
            .state
            .record_failure(failure_key.clone(), "Request exception".to_owned(), 100);
        let position = engine.state.position(position_key).cloned().unwrap();

        let (report, retryable_failure) =
            engine.retry_exit_position_after_transient_failure(&position, 130);

        assert!(!retryable_failure);
        assert_eq!(report.unwrap().status, "pending");
        assert!(engine.state.exit_retry(position_key).is_none());
        assert!(!engine
            .state
            .failure_cooldowns
            .iter()
            .any(|failure| failure.key == failure_key));
        let pending = engine
            .state
            .pending_orders
            .iter()
            .find(|order| order.position_key == position_key && order.side == "SELL")
            .expect("protective GTC sell should be tracked");
        assert_eq!(pending.external_order_id.as_deref(), Some("0xprotect"));
        assert_near(pending.limit_price, 0.998);
        assert_near(pending.copy_amount_usd, 53.26 * 0.998);
        assert_eq!(
            pending.source_trade_key,
            format!("exit-retry:{position_key}")
        );
    }

    #[test]
    fn maintenance_step_retries_only_one_exit_at_a_time() {
        let mut engine = test_engine();
        for (asset, position_key) in [
            ("asset-31", "weather-market:asset-31"),
            ("asset-32", "weather-market:asset-32"),
        ] {
            engine.state.positions.push(CopyPosition {
                position_key: position_key.to_owned(),
                market_title: Some("Weather market".to_owned()),
                outcome: Some("Yes".to_owned()),
                asset: asset.to_owned(),
                condition_id: "weather-market".to_owned(),
                size_shares: 20.0,
                cost_usd: 10.0,
                realized_pnl_usd: 0.0,
                updated_at_secs: 90,
            });
            engine.state.record_failure(
                action_failure_cooldown_key("SELL", position_key),
                "PolyApiException[status_code=None, error_message=Request exception!]".to_owned(),
                100,
            );
        }

        let reports = engine.handle_maintenance_step();
        let remaining_exit_failures = engine
            .state
            .failure_cooldowns
            .iter()
            .filter(|failure| failure.key.starts_with(SELL_ACTION_FAILURE_PREFIX))
            .count();

        assert_eq!(reports.len(), 1);
        assert_eq!(remaining_exit_failures, 1);
    }

    #[test]
    fn fak_no_match_sell_failure_is_retried_on_tick() {
        let mut engine = test_engine();
        let position_key = "weather-market:asset-32";
        engine.state.positions.push(CopyPosition {
            position_key: position_key.to_owned(),
            market_title: Some(
                "Will the highest temperature in Hong Kong be 32°C on June 10?".to_owned(),
            ),
            outcome: Some("Yes".to_owned()),
            asset: "asset-32".to_owned(),
            condition_id: "weather-market".to_owned(),
            size_shares: 688.55,
            cost_usd: 9.15,
            realized_pnl_usd: 0.0,
            updated_at_secs: 90,
        });
        engine.state.record_failure(
            action_failure_cooldown_key("SELL", position_key),
            "PolyApiException[status_code=400, error_message={'error': 'no orders found to match with FAK order. FAK orders are partially filled or killed if no match is found.'}]"
                .to_owned(),
            100,
        );

        let reports = engine.handle_tick();

        assert!(reports.iter().any(|report| {
            report.action == "SELL"
                && report.status == "dry-run"
                && report.reason.contains("继续按原意图重试清仓")
        }));
    }

    #[test]
    fn fak_no_match_with_zero_actual_balance_finishes_silently() {
        let execution = ExecutionResult {
            status: ExecutionStatus::Failed,
            order_id: None,
            order_price: None,
            filled_amount_usd: None,
            filled_size: None,
            filled_price: None,
            realized_pnl_usd: None,
            actual_balance_shares: Some(0.0),
            target_size_shares: Some(15.0),
            message: Some(
                "PolyApiException[status_code=400, error_message={'error': 'no orders found to match with FAK order.'}]"
                    .to_owned(),
            ),
        };

        assert!(should_silently_finish_dust_exit(&execution));
    }

    #[test]
    fn fak_no_match_with_real_actual_balance_still_retries() {
        let execution = ExecutionResult {
            status: ExecutionStatus::Failed,
            order_id: None,
            order_price: None,
            filled_amount_usd: None,
            filled_size: None,
            filled_price: None,
            realized_pnl_usd: None,
            actual_balance_shares: Some(15.0),
            target_size_shares: Some(15.0),
            message: Some(
                "PolyApiException[status_code=400, error_message={'error': 'no orders found to match with FAK order.'}]"
                    .to_owned(),
            ),
        };

        assert!(!should_silently_finish_dust_exit(&execution));
        assert!(is_retryable_exit_error(execution.message.as_deref()));
    }

    #[test]
    fn proportional_exit_retry_preserves_original_fraction_and_floor() {
        let position = CopyPosition {
            position_key: "weather-market:asset-32".to_owned(),
            market_title: None,
            outcome: Some("Yes".to_owned()),
            asset: "asset-32".to_owned(),
            condition_id: "weather-market".to_owned(),
            size_shares: 100.0,
            cost_usd: 20.0,
            realized_pnl_usd: 0.0,
            updated_at_secs: 100,
        };
        let retry = PendingExitRetry {
            position_key: position.position_key.clone(),
            sell_fraction: 0.125,
            target_size_shares: None,
            min_sell_price: Some(0.42),
            force_market_sell: false,
            lock_profit: false,
        };

        let request = AutoCopyExecutionRequest::sell_position_exit_retry(
            AutoCopyMode::LiveExternal,
            "WeatherHK".to_owned(),
            &position,
            &retry,
        );
        let json = serde_json::to_value(request).expect("serialize retry request");

        assert_near(json["order"]["sell_fraction"].as_f64().unwrap(), 0.125);
        assert_near(json["order"]["min_sell_price"].as_f64().unwrap(), 0.42);
        assert_eq!(json["order"]["force_market_sell"], false);
    }

    #[test]
    fn exact_remaining_exit_retry_uses_share_size_not_fraction() {
        let position = CopyPosition {
            position_key: "weather-market:asset-32".to_owned(),
            market_title: None,
            outcome: Some("Yes".to_owned()),
            asset: "asset-32".to_owned(),
            condition_id: "weather-market".to_owned(),
            size_shares: 100.0,
            cost_usd: 20.0,
            realized_pnl_usd: 0.0,
            updated_at_secs: 100,
        };
        let retry = PendingExitRetry {
            position_key: position.position_key.clone(),
            sell_fraction: 0.125,
            target_size_shares: Some(7.5),
            min_sell_price: Some(0.42),
            force_market_sell: false,
            lock_profit: false,
        };

        let request = AutoCopyExecutionRequest::sell_position_exit_retry(
            AutoCopyMode::LiveExternal,
            "WeatherHK".to_owned(),
            &position,
            &retry,
        );
        let json = serde_json::to_value(request).expect("serialize exact retry request");

        assert_near(json["order"]["size_shares"].as_f64().unwrap(), 7.5);
        assert!(json["order"]["sell_fraction"].is_null());
    }

    #[test]
    fn stale_exit_retry_is_removed_when_position_is_already_zero() {
        let mut engine = test_engine();
        engine.state.positions.push(CopyPosition {
            position_key: "market:asset".to_owned(),
            market_title: None,
            outcome: Some("Yes".to_owned()),
            asset: "asset".to_owned(),
            condition_id: "market".to_owned(),
            size_shares: 0.0,
            cost_usd: 0.0,
            realized_pnl_usd: 0.0,
            updated_at_secs: 100,
        });
        engine.state.pending_exit_retries.push(PendingExitRetry {
            position_key: "market:asset".to_owned(),
            sell_fraction: 0.5,
            target_size_shares: Some(5.0),
            min_sell_price: Some(0.4),
            force_market_sell: false,
            lock_profit: false,
        });

        assert!(engine.clear_stale_exit_retries());
        assert!(engine.state.pending_exit_retries.is_empty());
    }

    #[test]
    fn invalid_token_sell_failure_clears_stale_position_without_report() {
        let mut engine = test_engine();
        let position_key = "weather-market:asset-32";
        engine.state.positions.push(CopyPosition {
            position_key: position_key.to_owned(),
            market_title: Some(
                "Will the highest temperature in Hong Kong be 32°C on June 10?".to_owned(),
            ),
            outcome: Some("Yes".to_owned()),
            asset: "asset-32".to_owned(),
            condition_id: "weather-market".to_owned(),
            size_shares: 688.55,
            cost_usd: 9.15,
            realized_pnl_usd: 0.0,
            updated_at_secs: 90,
        });
        engine.state.record_failure(
            action_failure_cooldown_key("SELL", position_key),
            "PolyApiException[status_code=400, error_message={'error': 'invalid token id'}]"
                .to_owned(),
            100,
        );

        let reports = engine.handle_tick();

        assert!(reports.is_empty());
        assert_near(
            engine
                .state
                .position(position_key)
                .expect("position should remain as zeroed history")
                .size_shares,
            0.0,
        );
        assert!(!engine.state.failure_in_cooldown(
            &action_failure_cooldown_key("SELL", position_key),
            now_secs(),
            u64::MAX
        ));
    }

    #[test]
    fn source_position_reconcile_waits_before_clearing_absent_tracked_position() {
        let mut engine = test_engine();
        let now = now_secs();
        engine.state.positions.push(CopyPosition {
            position_key: "weather-market:asset-33".to_owned(),
            market_title: Some(
                "Will the highest temperature in Hong Kong be 33°C on June 3?".to_owned(),
            ),
            outcome: Some("Yes".to_owned()),
            asset: "asset-33".to_owned(),
            condition_id: "weather-market".to_owned(),
            size_shares: 4.0,
            cost_usd: 1.48,
            realized_pnl_usd: 0.0,
            updated_at_secs: 90,
        });
        engine
            .state
            .source_position_snapshots
            .push(SourcePositionSnapshot {
                asset: "asset-33".to_owned(),
                size_shares: 8.0,
                observed_at_secs: now.saturating_sub(60),
            });
        let source_positions =
            HashMap::from([("other-asset".to_owned(), test_source_position(1.0, 0.10))]);

        let first = engine.reconcile_absent_from_source_positions(&source_positions);

        assert!(first
            .iter()
            .any(|report| report.action == "SKIP:源仓位缺失待确认"));
        assert!(!first.iter().any(|report| report.action == "SELL"));
        assert_near(
            engine
                .state
                .position("weather-market:asset-33")
                .expect("position should remain")
                .size_shares,
            4.0,
        );

        let absence = engine
            .state
            .source_position_absences
            .iter_mut()
            .find(|absence| absence.position_key == "weather-market:asset-33")
            .expect("absence should be tracked");
        absence.first_missing_at_secs =
            now_secs().saturating_sub(SOURCE_POSITION_ABSENCE_CONFIRM_SECONDS + 1);
        absence.missing_count = SOURCE_POSITION_ABSENCE_CONFIRM_MIN_COUNT - 1;

        let second = engine.reconcile_absent_from_source_positions(&source_positions);
        let sell = second
            .iter()
            .find(|report| report.action == "SELL")
            .expect("expected reconcile sell report");

        assert_eq!(sell.status, "dry-run");
        assert_near(sell.copy_amount_usd, 1.48);
        assert!(sell.reason.contains("源仓位对账"));
    }

    #[test]
    fn source_position_reconcile_does_not_sell_absence_never_seen_in_source_positions() {
        let mut engine = test_engine();
        let position_key = "weather-market:asset-nyc";
        engine.state.positions.push(CopyPosition {
            position_key: position_key.to_owned(),
            market_title: Some(
                "Will the highest temperature in New York City be between 82-83°F on July 8?"
                    .to_owned(),
            ),
            outcome: Some("No".to_owned()),
            asset: "asset-nyc".to_owned(),
            condition_id: "weather-market".to_owned(),
            size_shares: 15.4,
            cost_usd: 12.7,
            realized_pnl_usd: 0.0,
            updated_at_secs: 90,
        });
        engine.state.source_outcomes.push(SourceOutcomeMetadata {
            position_key: position_key.to_owned(),
            asset: "asset-nyc".to_owned(),
            condition_id: "weather-market".to_owned(),
            market_title: Some(
                "Will the highest temperature in New York City be between 82-83°F on July 8?"
                    .to_owned(),
            ),
            outcome: Some("No".to_owned()),
            slug: Some("highest-temperature-in-nyc-on-july-8-2026-82-83f".to_owned()),
            event_slug: Some("highest-temperature-in-nyc-on-july-8-2026".to_owned()),
            last_price: Some(0.80),
            last_buy_price: Some(0.80),
            first_seen_at_secs: now_secs().saturating_sub(600),
            last_seen_at_secs: now_secs().saturating_sub(600),
            last_buy_at_secs: Some(now_secs().saturating_sub(600)),
            last_sell_at_secs: None,
        });

        let first = engine.reconcile_absent_from_source_positions(&HashMap::new());
        assert!(first
            .iter()
            .any(|report| report.action == "SKIP:源仓位缺失待确认"));
        assert!(!first.iter().any(|report| report.action == "SELL"));

        let absence = engine
            .state
            .source_position_absences
            .iter_mut()
            .find(|absence| absence.position_key == position_key)
            .expect("absence should be tracked");
        absence.first_missing_at_secs =
            now_secs().saturating_sub(SOURCE_POSITION_ABSENCE_CONFIRM_SECONDS * 10);
        absence.last_missing_at_secs =
            now_secs().saturating_sub(SOURCE_POSITION_ABSENCE_CONFIRM_SECONDS * 2);
        absence.missing_count = SOURCE_POSITION_ABSENCE_CONFIRM_MIN_COUNT + 10;

        let second = engine.reconcile_absent_from_source_positions(&HashMap::new());

        assert!(!second.iter().any(|report| report.action == "SELL"));
        assert_near(
            engine
                .state
                .position(position_key)
                .expect("position should remain")
                .size_shares,
            15.4,
        );
    }

    #[test]
    fn source_position_reconcile_recent_buy_extends_absence_grace() {
        let mut engine = test_engine();
        let position_key = "weather-market:asset-33";
        engine.state.positions.push(CopyPosition {
            position_key: position_key.to_owned(),
            market_title: Some(
                "Will the highest temperature in Hong Kong be 33°C on June 3?".to_owned(),
            ),
            outcome: Some("Yes".to_owned()),
            asset: "asset-33".to_owned(),
            condition_id: "weather-market".to_owned(),
            size_shares: 4.0,
            cost_usd: 1.48,
            realized_pnl_usd: 0.0,
            updated_at_secs: 90,
        });
        engine
            .state
            .source_position_absences
            .push(SourcePositionAbsence {
                position_key: position_key.to_owned(),
                asset: "asset-33".to_owned(),
                first_missing_at_secs: now_secs()
                    .saturating_sub(SOURCE_POSITION_ABSENCE_CONFIRM_SECONDS + 30),
                last_missing_at_secs: now_secs().saturating_sub(30),
                missing_count: SOURCE_POSITION_ABSENCE_CONFIRM_MIN_COUNT + 1,
                last_seen_source_size_shares: Some(8.0),
                last_seen_source_at_secs: Some(now_secs().saturating_sub(300)),
            });
        engine.state.source_outcomes.push(SourceOutcomeMetadata {
            position_key: position_key.to_owned(),
            asset: "asset-33".to_owned(),
            condition_id: "weather-market".to_owned(),
            market_title: Some(
                "Will the highest temperature in Hong Kong be 33°C on June 3?".to_owned(),
            ),
            outcome: Some("Yes".to_owned()),
            slug: Some("weather-market".to_owned()),
            event_slug: Some("weather-market".to_owned()),
            last_price: Some(0.10),
            last_buy_price: Some(0.10),
            first_seen_at_secs: now_secs().saturating_sub(300),
            last_seen_at_secs: now_secs(),
            last_buy_at_secs: Some(now_secs()),
            last_sell_at_secs: None,
        });

        let reports = engine.reconcile_absent_from_source_positions(&HashMap::new());

        assert!(reports
            .iter()
            .any(|report| report.action == "SKIP:源仓位缺失待确认"));
        assert!(!reports.iter().any(|report| report.action == "SELL"));
    }

    #[test]
    fn routine_pending_sync_does_not_notify() {
        let order = PendingCopyOrder {
            local_order_id: "local".to_owned(),
            external_order_id: Some("external".to_owned()),
            position_key: "market:asset".to_owned(),
            side: "BUY".to_owned(),
            market_title: None,
            outcome: None,
            asset: "asset".to_owned(),
            condition_id: "market".to_owned(),
            copy_amount_usd: 2.0,
            limit_price: 0.05,
            requested_limit_price: None,
            filled_amount_usd: 0.0,
            filled_size: 0.0,
            created_at_secs: 0,
            expires_at_secs: 0,
            last_sync_at_secs: 0,
            source_trade_key: "source".to_owned(),
            source_trade_at_secs: 0,
        };
        let pending = ExecutionResult {
            status: ExecutionStatus::Pending,
            order_id: Some("external".to_owned()),
            order_price: Some(0.05),
            filled_amount_usd: None,
            filled_size: None,
            filled_price: None,
            realized_pnl_usd: None,
            actual_balance_shares: None,
            target_size_shares: None,
            message: Some("still pending".to_owned()),
        };
        let partial = ExecutionResult {
            filled_amount_usd: Some(0.5),
            ..pending.clone()
        };
        let cancelled = ExecutionResult {
            status: ExecutionStatus::Cancelled,
            ..pending.clone()
        };
        let cancelled_with_fill = ExecutionResult {
            status: ExecutionStatus::Cancelled,
            filled_amount_usd: Some(0.5),
            ..pending.clone()
        };
        let transient_failed = ExecutionResult {
            status: ExecutionStatus::Failed,
            message: Some(
                "PolyApiException[status_code=None, error_message=Request exception!]".to_owned(),
            ),
            ..pending.clone()
        };
        let deterministic_failed = ExecutionResult {
            status: ExecutionStatus::Failed,
            message: Some(
                "PolyApiException[status_code=400, error_message={'error': 'invalid token id'}]"
                    .to_owned(),
            ),
            ..pending.clone()
        };

        assert!(!should_report_pending_sync(&order, &pending));
        assert!(should_report_pending_sync(&order, &partial));
        assert!(!should_report_pending_sync(&order, &cancelled));
        assert!(should_report_pending_sync(&order, &cancelled_with_fill));
        assert!(!should_report_pending_sync(&order, &transient_failed));
        assert!(should_report_pending_sync(&order, &deterministic_failed));
    }

    #[test]
    fn missing_source_position_cancels_old_pending_buy_only() {
        let mut source_positions = HashMap::new();
        source_positions.insert("held".to_owned(), test_source_position(1.0, 0.10));
        let old_missing = PendingCopyOrder {
            local_order_id: "old-missing".to_owned(),
            external_order_id: None,
            position_key: "market:missing".to_owned(),
            side: "BUY".to_owned(),
            market_title: None,
            outcome: None,
            asset: "missing".to_owned(),
            condition_id: "market".to_owned(),
            copy_amount_usd: 1.0,
            limit_price: 0.05,
            requested_limit_price: None,
            filled_amount_usd: 0.0,
            filled_size: 0.0,
            created_at_secs: 100,
            expires_at_secs: 0,
            last_sync_at_secs: 100,
            source_trade_key: "source".to_owned(),
            source_trade_at_secs: 90,
        };
        let new_missing = PendingCopyOrder {
            created_at_secs: 150,
            ..old_missing.clone()
        };
        let old_held = PendingCopyOrder {
            asset: "held".to_owned(),
            ..old_missing.clone()
        };

        assert!(should_cancel_pending_buy_absent_from_source_position(
            &old_missing,
            &source_positions,
            200
        ));
        assert!(!should_cancel_pending_buy_absent_from_source_position(
            &new_missing,
            &source_positions,
            200
        ));
        assert!(!should_cancel_pending_buy_absent_from_source_position(
            &old_held,
            &source_positions,
            200
        ));
    }

    #[test]
    fn zero_ttl_means_no_expiry() {
        assert_eq!(pending_expires_at(100, 0), 0);
        assert_eq!(pending_expires_at(100, 300), 400);
        assert_eq!(order_remaining_ttl(0), 0);
    }

    #[test]
    fn state_counts_pending_buy_as_reserved_budget() {
        let state = AutoCopyState {
            pending_orders: vec![PendingCopyOrder {
                local_order_id: "local".to_owned(),
                external_order_id: None,
                position_key: "market:asset".to_owned(),
                side: "BUY".to_owned(),
                market_title: None,
                outcome: None,
                asset: "asset".to_owned(),
                condition_id: "market".to_owned(),
                copy_amount_usd: 5.0,
                limit_price: 0.43,
                requested_limit_price: None,
                filled_amount_usd: 2.0,
                filled_size: 4.0,
                created_at_secs: 1,
                expires_at_secs: 2,
                last_sync_at_secs: 1,
                source_trade_key: "source".to_owned(),
                source_trade_at_secs: 1,
            }],
            ..AutoCopyState::default()
        };

        assert!((state.daily_reserved_buy_usd() - 3.0).abs() < f64::EPSILON);
        assert!((state.market_exposure_usd("market:asset") - 3.0).abs() < f64::EPSILON);
    }

    #[test]
    fn failure_cooldown_blocks_repeated_failures() {
        let mut state = AutoCopyState::default();

        assert!(!state.failure_in_cooldown("sell:market", 100, 900));
        state.record_failure(
            "sell:market".to_owned(),
            "not enough balance".to_owned(),
            100,
        );

        assert!(state.failure_in_cooldown("sell:market", 150, 900));
        assert!(!state.failure_in_cooldown("sell:market", 1_000, 900));

        state.clear_failure("sell:market");
        assert!(!state.failure_in_cooldown("sell:market", 150, 900));
    }

    #[test]
    fn old_state_json_without_failure_cooldowns_still_loads() {
        let json = r#"{
            "day_bucket": 1,
            "daily_spend_usd": 0.0,
            "daily_realized_pnl_usd": 0.0,
            "positions": [],
            "pending_orders": [],
            "processed_source_trades": [],
            "logs": []
        }"#;

        let state: AutoCopyState = serde_json::from_str(json).unwrap();

        assert!(state.failure_cooldowns.is_empty());
        assert!(state.recent_source_flows.is_empty());
        assert!(state.source_position_absences.is_empty());
        assert!(state.source_guards.is_empty());
    }

    #[test]
    fn state_migration_discards_legacy_sell_retries_and_source_snapshots() {
        let mut state = AutoCopyState::default();
        state.schema_version = 0;
        state.pending_exit_retries.push(PendingExitRetry {
            position_key: "market:asset".to_owned(),
            sell_fraction: 1.0,
            target_size_shares: Some(10.0),
            min_sell_price: None,
            force_market_sell: true,
            lock_profit: false,
        });
        state
            .source_position_snapshots
            .push(SourcePositionSnapshot {
                asset: "asset".to_owned(),
                size_shares: 10.0,
                observed_at_secs: 1,
            });
        state.source_sell_coverages.push(SourceSellCoverage {
            asset: "asset".to_owned(),
            remaining_shares: 5.0,
            created_at_secs: 1,
        });
        state.source_position_absences.push(SourcePositionAbsence {
            position_key: "market:asset".to_owned(),
            asset: "asset".to_owned(),
            first_missing_at_secs: 1,
            last_missing_at_secs: 1,
            missing_count: 1,
            last_seen_source_size_shares: Some(10.0),
            last_seen_source_at_secs: Some(1),
        });

        state.migrate();

        assert_eq!(state.schema_version, AUTO_COPY_STATE_SCHEMA_VERSION);
        assert!(state.pending_exit_retries.is_empty());
        assert!(state.source_position_snapshots.is_empty());
        assert!(state.source_sell_coverages.is_empty());
        assert!(state.source_position_absences.is_empty());
    }

    #[test]
    fn failed_status_is_labelled_as_failure_not_skip() {
        assert_eq!(report_action_label("SELL", ExecutionStatus::Failed), "失败");
        assert_eq!(copy_action_label("SELL", ExecutionStatus::Failed), "失败");
    }

    #[test]
    fn first_sell_guards_only_the_same_outcome_buy() {
        let mut engine = test_engine();
        let employee = test_employee();

        engine.handle_trade(
            &employee,
            &test_trade("SELL", "weather-market", "asset-32", 0.20, 2.0, 100),
        );

        let guarded = engine.handle_trade(
            &employee,
            &test_trade("BUY", "weather-market", "asset-32", 0.21, 5.0, 110),
        );
        assert!(guarded
            .iter()
            .any(|report| report.action == "SKIP:卖压冷却"));

        let other_outcome = engine.handle_trade(
            &employee,
            &test_trade("BUY", "weather-market", "asset-33", 0.11, 5.0, 111),
        );
        assert!(other_outcome
            .iter()
            .any(|report| report.action == "BUY" && report.status == "dry-run"));
    }

    #[test]
    fn rapid_sells_escalate_to_exact_outcome_pressure_guard() {
        let mut engine = test_engine();
        let employee = test_employee();

        for (index, timestamp) in [100, 110, 120].into_iter().enumerate() {
            engine.handle_trade(
                &employee,
                &test_trade(
                    "SELL",
                    "weather-market",
                    "asset-32",
                    0.20,
                    1.20 + index as f64,
                    timestamp,
                ),
            );
        }

        let guard = engine
            .state
            .active_source_guard("weather-market:asset-32", 121)
            .unwrap();
        assert_eq!(guard.kind, SourceGuardKind::Pressure);
        assert!(engine
            .state
            .active_source_guard("weather-market:asset-33", 121)
            .is_none());
    }

    #[test]
    fn large_buy_during_guard_alerts_but_does_not_copy() {
        let mut engine = test_engine();
        let employee = test_employee();

        engine.handle_trade(
            &employee,
            &test_trade("SELL", "weather-market", "asset-32", 0.20, 2.0, 100),
        );
        let reports = engine.handle_trade(
            &employee,
            &test_trade("BUY", "weather-market", "asset-32", 0.21, 35.0, 110),
        );

        assert!(reports
            .iter()
            .any(|report| report.action == "SKIP:可能重新建仓"));
        assert!(!reports.iter().any(|report| report.action == "BUY"));
    }

    #[test]
    fn source_pressure_requires_fast_sell_cadence() {
        let config = test_config();
        let slow_stats = SourceFlowStats {
            sell_count: 3,
            sell_notional_usd: 20.0,
            first_sell_at_secs: Some(100),
            last_sell_at_secs: Some(220),
            ..SourceFlowStats::default()
        };
        let fast_stats = SourceFlowStats {
            last_sell_at_secs: Some(140),
            ..slow_stats.clone()
        };

        assert!(!source_pressure_detected(&slow_stats, &config));
        assert!(source_pressure_detected(&fast_stats, &config));
    }

    fn assert_near(left: f64, right: f64) {
        assert!(
            (left - right).abs() < 0.000_000_1,
            "expected {left} to be near {right}"
        );
    }

    fn test_engine() -> AutoCopyEngine {
        AutoCopyEngine {
            config: test_config(),
            state: AutoCopyState::default(),
        }
    }

    fn test_engine_with_event_strategy() -> AutoCopyEngine {
        let mut config = test_config();
        config.strategy = AutoCopyStrategyConfig {
            enabled: true,
            shadow_event_baskets: true,
            ..AutoCopyStrategyConfig::default()
        };
        AutoCopyEngine {
            config,
            state: AutoCopyState::default(),
        }
    }

    fn test_source_position(size_shares: f64, avg_price: f64) -> ObservedSourcePosition {
        ObservedSourcePosition {
            size_shares,
            avg_price: Some(avg_price),
            current_price: Some(avg_price),
            end_date: Some("2099-12-31".to_owned()),
            condition_id: Some("weather-market".to_owned()),
            market_title: Some("Weather market".to_owned()),
            outcome: Some("Yes".to_owned()),
            slug: Some("weather-market".to_owned()),
            event_slug: Some("weather-market".to_owned()),
        }
    }

    fn test_config() -> AutoCopyConfig {
        let mut config = AutoCopyConfig::weatherhk_default();
        config.enabled = true;
        config.mode = AutoCopyMode::DryRun;
        config.source_wallet = WEATHERHK_WALLET.to_owned();
        config.domain = "WEATHER".to_owned();
        config.copy_target_cap_usd = DEFAULT_COPY_TARGET_CAP_USD;
        config.min_buy_source_notional_usd = 0.0;
        config.min_sell_sync_notional_usd = 1.0;
        config.source_flow_window_seconds = 120;
        config.post_sell_buy_guard_seconds = 120;
        config.source_pressure_cooldown_seconds = 300;
        config.source_pressure_min_sell_count = 3;
        config.source_pressure_min_sell_notional_usd = 3.0;
        config.source_pressure_max_avg_sell_gap_seconds = 30;
        config.source_reentry_alert_buy_usd = 30.0;
        config.state_path = test_state_path("local");
        config.global_state_path = test_state_path("global");
        config
    }

    fn test_state_path(label: &str) -> PathBuf {
        let counter = TEST_PATH_COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "smart-wallet-discovery-autocopy-{label}-{}-{counter}.json",
            std::process::id()
        ))
    }

    fn test_employee() -> WatchedEmployee {
        WatchedEmployee {
            wallet: WEATHERHK_WALLET.to_owned(),
            name: Some("WeatherHK".to_owned()),
            domain: "WEATHER".to_owned(),
            keywords: vec!["temperature".to_owned()],
            poll_seconds: None,
            min_notional_usd: None,
        }
    }

    fn test_trade(
        side: &str,
        condition_id: &str,
        asset: &str,
        price: f64,
        notional: f64,
        timestamp: u64,
    ) -> UserTrade {
        UserTrade {
            proxy_wallet: WEATHERHK_WALLET.to_owned(),
            side: side.to_owned(),
            asset: asset.to_owned(),
            condition_id: condition_id.to_owned(),
            size: Some(notional / price),
            price: Some(price),
            timestamp: Some(timestamp),
            title: Some("Will the highest temperature in Hong Kong be 32°C on June 3?".to_owned()),
            slug: Some("highest-temperature-in-hong-kong-on-june-3-2026".to_owned()),
            event_slug: Some("highest-temperature-in-hong-kong-on-june-3-2026".to_owned()),
            outcome: Some(asset.to_owned()),
            outcome_index: None,
            name: Some("WeatherHK".to_owned()),
            pseudonym: Some("WeatherHK".to_owned()),
            transaction_hash: Some(format!("0x{side}-{condition_id}-{asset}-{timestamp}")),
        }
    }
}
