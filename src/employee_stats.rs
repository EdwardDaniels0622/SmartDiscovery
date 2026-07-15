use crate::polymarket::{
    ClosedPosition, CurrentPosition, PolymarketDataClient, PolymarketError, UserTrade,
};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    error::Error,
    fmt, fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

const SECONDS_PER_DAY: u64 = 86_400;
const PAGE_SIZE: usize = 50;
const TRADE_PAGE_SIZE: usize = 100;
const OVERLAP_SECONDS: u64 = 6 * 3_600;
const METRIC_SCHEMA_VERSION: u32 = 2;
const RULES_VERSION: u32 = 2;
const DOMAIN_CLASSIFIER_VERSION: u32 = 2;
const SETTLEMENT_EPSILON: f64 = 0.000_001;

#[derive(Debug)]
pub enum EmployeeStatsError {
    Io(std::io::Error),
    Json(serde_json::Error),
    Sql(rusqlite::Error),
    Api(String),
    Invalid(String),
    NotFound(String),
}

impl fmt::Display for EmployeeStatsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "employee stats I/O error: {error}"),
            Self::Json(error) => write!(f, "employee stats JSON error: {error}"),
            Self::Sql(error) => write!(f, "employee stats SQLite error: {error}"),
            Self::Api(error) => write!(f, "employee stats API error: {error}"),
            Self::Invalid(error) => write!(f, "invalid employee stats request: {error}"),
            Self::NotFound(error) => write!(f, "employee stats cache not found: {error}"),
        }
    }
}

impl Error for EmployeeStatsError {}

impl From<std::io::Error> for EmployeeStatsError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<serde_json::Error> for EmployeeStatsError {
    fn from(value: serde_json::Error) -> Self {
        Self::Json(value)
    }
}

impl From<rusqlite::Error> for EmployeeStatsError {
    fn from(value: rusqlite::Error) -> Self {
        Self::Sql(value)
    }
}

#[derive(Debug, Clone)]
pub struct EmployeeStatsConfig {
    pub cache_dir: PathBuf,
    pub window_days: u64,
    pub retention_days: u64,
    pub action_gap_seconds: u64,
    pub max_pages: usize,
}

impl Default for EmployeeStatsConfig {
    fn default() -> Self {
        Self {
            cache_dir: PathBuf::from("logs/employee-stats"),
            window_days: 14,
            retention_days: 90,
            action_gap_seconds: 120,
            max_pages: 100,
        }
    }
}

impl EmployeeStatsConfig {
    pub fn database_path(&self) -> PathBuf {
        self.cache_dir.join("employee-stats.sqlite3")
    }

    pub fn validate(&self) -> Result<(), EmployeeStatsError> {
        if self.window_days == 0 || self.window_days > self.retention_days {
            return Err(EmployeeStatsError::Invalid(format!(
                "window days must be between 1 and retention days ({})",
                self.retention_days
            )));
        }
        if self.max_pages == 0 {
            return Err(EmployeeStatsError::Invalid(
                "max pages must be greater than zero".to_owned(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EmployeeIdentity {
    pub wallet: String,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub primary_domain: String,
    #[serde(default)]
    pub keywords: Vec<String>,
    #[serde(default = "provided_domain_source")]
    pub primary_domain_source: String,
}

fn provided_domain_source() -> String {
    "provided".to_owned()
}

impl EmployeeIdentity {
    pub fn new(wallet: impl Into<String>) -> Self {
        Self {
            wallet: wallet.into().to_lowercase(),
            display_name: None,
            username: None,
            primary_domain: String::new(),
            keywords: Vec::new(),
            primary_domain_source: "unknown".to_owned(),
        }
    }

    pub fn validate(&self) -> Result<(), EmployeeStatsError> {
        validate_wallet(&self.wallet)
    }

    fn merge_with_cached(mut self, cached: Option<&EmployeeIdentity>) -> Self {
        let Some(cached) = cached else {
            return self;
        };
        if self.display_name.is_none() {
            self.display_name = cached.display_name.clone();
        }
        if self.username.is_none() {
            self.username = cached.username.clone();
        }
        if self.primary_domain.trim().is_empty() {
            self.primary_domain = cached.primary_domain.clone();
            self.primary_domain_source = cached.primary_domain_source.clone();
        }
        if self.keywords.is_empty() {
            self.keywords = cached.keywords.clone();
        }
        self
    }
}

#[derive(Debug, Clone, Copy)]
pub struct RefreshSelection {
    pub activity: bool,
    pub trades: bool,
    pub closed_positions: bool,
    pub positions: bool,
}

impl RefreshSelection {
    pub fn all() -> Self {
        Self {
            activity: true,
            trades: true,
            closed_positions: true,
            positions: true,
        }
    }

    pub fn none() -> Self {
        Self {
            activity: false,
            trades: false,
            closed_positions: false,
            positions: false,
        }
    }

    pub fn any(self) -> bool {
        self.activity || self.trades || self.closed_positions || self.positions
    }
}

impl Default for RefreshSelection {
    fn default() -> Self {
        Self::all()
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ComponentFreshness {
    pub last_attempt_at: Option<u64>,
    pub last_success_at: Option<u64>,
    pub latest_source_event_at: Option<u64>,
    pub complete_from: Option<u64>,
    pub complete_through: Option<u64>,
    pub history_truncated: bool,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SyncMetadata {
    pub activity: ComponentFreshness,
    pub trades: ComponentFreshness,
    pub closed_positions: ComponentFreshness,
    pub positions: ComponentFreshness,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CachedDataQuality {
    pub invalid_trade_count: usize,
    pub duplicate_trade_count: usize,
    pub failed_components: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmployeeStatsDataset {
    pub schema_version: u32,
    pub employee: EmployeeIdentity,
    pub collected_at: u64,
    #[serde(default)]
    pub trades: Vec<UserTrade>,
    #[serde(default)]
    pub closed_positions: Vec<ClosedPosition>,
    #[serde(default)]
    pub current_positions: Vec<CurrentPosition>,
    #[serde(default)]
    pub sync: SyncMetadata,
    #[serde(default)]
    pub quality: CachedDataQuality,
}

impl EmployeeStatsDataset {
    fn empty(employee: EmployeeIdentity, now: u64) -> Self {
        Self {
            schema_version: 1,
            employee,
            collected_at: now,
            trades: Vec::new(),
            closed_positions: Vec::new(),
            current_positions: Vec::new(),
            sync: SyncMetadata::default(),
            quality: CachedDataQuality::default(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SegmentMetrics {
    pub gross_trade_notional_usd: f64,
    pub buy_notional_usd: f64,
    pub sell_notional_usd: f64,
    pub fill_count: usize,
    pub buy_fill_count: usize,
    pub sell_fill_count: usize,
    pub action_count: usize,
    pub unique_markets: usize,
    pub unique_outcomes: usize,
    pub active_days: usize,
    pub fills_per_active_day: f64,
    pub actions_per_active_day: f64,
    pub avg_fill_notional_usd: f64,
    pub median_fill_notional_usd: f64,
    pub p80_fill_notional_usd: f64,
    pub p95_fill_notional_usd: f64,
    pub max_fill_notional_usd: f64,
    pub avg_action_notional_usd: f64,
    pub median_action_notional_usd: f64,
    pub sell_notional_ratio: f64,
    pub net_buy_notional_usd: f64,
    pub net_flow_ratio: f64,
    pub repeated_market_ratio: f64,
    pub high_price_buy_notional_share_80: f64,
    pub high_price_buy_notional_share_95: f64,
    pub suspected_market_making: bool,
    pub settled_positions: usize,
    pub settled_markets: usize,
    pub realized_pnl_usd: f64,
    pub invested_usd: f64,
    pub realized_roi: f64,
    pub settled_position_win_rate: f64,
    pub settled_market_win_rate: f64,
    pub breakeven_positions: usize,
    pub gross_profit_usd: f64,
    pub gross_loss_usd: f64,
    pub profit_factor: Option<f64>,
    pub avg_win_usd: f64,
    pub avg_loss_usd: f64,
    pub payoff_ratio: Option<f64>,
    pub expectancy_per_settled_market_usd: f64,
    pub top_5_profit_share: f64,
    pub max_realized_drawdown_usd: f64,
    pub longest_win_streak: usize,
    pub longest_loss_streak: usize,
    pub open_positions: usize,
    pub open_initial_value_usd: f64,
    pub open_current_value_usd: f64,
    pub unrealized_pnl_usd: f64,
    pub open_profit_positions: usize,
    pub open_loss_positions: usize,
    pub open_loss_usd: f64,
    pub open_loss_ratio: f64,
    pub open_loss_position_ratio: f64,
    pub largest_open_position_usd: f64,
    pub largest_open_loss_usd: f64,
    pub open_position_concentration: f64,
    pub redeemable_positions: usize,
    pub redeemable_value_usd: f64,
    pub redeemable_pnl_usd: f64,
    pub mergeable_positions: usize,
    pub losing_positions_older_than_3d: usize,
    pub losing_positions_older_than_7d: usize,
    pub stale_losing_value_usd: f64,
    pub stale_losing_pnl_usd: f64,
    pub position_age_unknown_count: usize,
    pub marked_position_win_rate: f64,
    pub combined_pnl_usd: f64,
    pub hidden_loss_ratio: f64,
}

#[derive(Debug, Clone)]
struct SettledRecord {
    position_key: String,
    market_key: String,
    pnl_usd: f64,
    invested_usd: f64,
    settled_at: u64,
    from_redeemable: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DomainComparison {
    pub primary_trade_notional_share: f64,
    pub primary_action_share: f64,
    pub primary_realized_profit_share: f64,
    pub primary_combined_profit_share: f64,
    pub primary_vs_other_settled_win_rate_gap: f64,
    pub primary_vs_other_marked_win_rate_gap: f64,
    pub primary_vs_other_roi_gap: f64,
    pub primary_vs_other_expectancy_gap_usd: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DomainBreakdown {
    pub wallet_total: SegmentMetrics,
    pub primary_domain: SegmentMetrics,
    pub other_domains_total: SegmentMetrics,
    pub other_domains: BTreeMap<String, SegmentMetrics>,
    pub unknown_or_ambiguous: SegmentMetrics,
    #[serde(default)]
    pub specialties: BTreeMap<String, SegmentMetrics>,
    pub comparison: DomainComparison,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReportFreshness {
    pub report_generated_at: u64,
    pub latest_trade_at: Option<u64>,
    pub latest_closed_position_at: Option<u64>,
    pub positions_observed_at: Option<u64>,
    pub activity_last_success_at: Option<u64>,
    pub trades_last_success_at: Option<u64>,
    pub closed_positions_last_success_at: Option<u64>,
    pub positions_last_success_at: Option<u64>,
    pub data_complete_from: Option<u64>,
    pub data_complete_through: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReportDataQuality {
    pub report_status: String,
    pub history_truncated: bool,
    pub missing_timestamp_count: usize,
    pub invalid_trade_count: usize,
    pub duplicate_trade_count: usize,
    pub position_age_unknown_count: usize,
    pub failed_components: Vec<String>,
    pub metric_schema_version: u32,
    pub rules_version: u32,
    pub domain_classifier_version: u32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StatsConclusion {
    pub summary_level: String,
    pub flags: Vec<String>,
    pub facts: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmployeeStatsReport {
    pub schema_version: u32,
    pub window_days: u64,
    pub metric_schema_version: u32,
    pub rules_version: u32,
    pub domain_classifier_version: u32,
    pub employee: EmployeeIdentity,
    pub freshness: ReportFreshness,
    pub data_quality: ReportDataQuality,
    pub windows: BTreeMap<String, DomainBreakdown>,
    pub conclusion: StatsConclusion,
}

pub struct EmployeeStatsStore {
    connection: Connection,
    cache_dir: PathBuf,
}

impl EmployeeStatsStore {
    pub fn open(config: &EmployeeStatsConfig) -> Result<Self, EmployeeStatsError> {
        config.validate()?;
        fs::create_dir_all(&config.cache_dir)?;
        let connection = Connection::open(config.database_path())?;
        connection.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA foreign_keys=ON;
             CREATE TABLE IF NOT EXISTS employee_stats_cache (
                 wallet TEXT PRIMARY KEY,
                 dataset_json TEXT NOT NULL,
                 report_json TEXT NOT NULL,
                 updated_at INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS employee_aliases (
                 alias_normalized TEXT NOT NULL,
                 wallet TEXT NOT NULL,
                 alias_type TEXT NOT NULL,
                 updated_at INTEGER NOT NULL,
                 PRIMARY KEY(alias_normalized, wallet)
             );",
        )?;
        Ok(Self {
            connection,
            cache_dir: config.cache_dir.clone(),
        })
    }

    pub fn load_dataset(
        &self,
        wallet: &str,
    ) -> Result<Option<EmployeeStatsDataset>, EmployeeStatsError> {
        let json: Option<String> = self
            .connection
            .query_row(
                "SELECT dataset_json FROM employee_stats_cache WHERE wallet = ?1",
                params![wallet.to_lowercase()],
                |row| row.get(0),
            )
            .optional()?;
        json.map(|value| serde_json::from_str(&value).map_err(EmployeeStatsError::from))
            .transpose()
    }

    pub fn load_report(
        &self,
        wallet_or_alias: &str,
    ) -> Result<EmployeeStatsReport, EmployeeStatsError> {
        let wallet = self.resolve_wallet(wallet_or_alias)?;
        let json: Option<String> = self
            .connection
            .query_row(
                "SELECT report_json FROM employee_stats_cache WHERE wallet = ?1",
                params![wallet],
                |row| row.get(0),
            )
            .optional()?;
        let json = json.ok_or_else(|| EmployeeStatsError::NotFound(wallet_or_alias.to_owned()))?;
        Ok(serde_json::from_str(&json)?)
    }

    pub fn resolve_wallet(&self, wallet_or_alias: &str) -> Result<String, EmployeeStatsError> {
        let normalized = wallet_or_alias.trim().to_lowercase();
        if validate_wallet(&normalized).is_ok() {
            return Ok(normalized);
        }
        let mut statement = self.connection.prepare(
            "SELECT wallet FROM employee_aliases WHERE alias_normalized = ?1 ORDER BY wallet",
        )?;
        let wallets = statement
            .query_map(params![normalized], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        match wallets.as_slice() {
            [wallet] => Ok(wallet.clone()),
            [] => Err(EmployeeStatsError::NotFound(wallet_or_alias.to_owned())),
            _ => Err(EmployeeStatsError::Invalid(format!(
                "alias {wallet_or_alias:?} matches multiple wallets; use the wallet address"
            ))),
        }
    }

    pub fn save(
        &mut self,
        dataset: &EmployeeStatsDataset,
        report: &EmployeeStatsReport,
    ) -> Result<(), EmployeeStatsError> {
        let dataset_json = serde_json::to_string(dataset)?;
        let report_json = serde_json::to_string(report)?;
        let transaction = self.connection.transaction()?;
        transaction.execute(
            "INSERT INTO employee_stats_cache(wallet, dataset_json, report_json, updated_at)
             VALUES(?1, ?2, ?3, ?4)
             ON CONFLICT(wallet) DO UPDATE SET
               dataset_json=excluded.dataset_json,
               report_json=excluded.report_json,
               updated_at=excluded.updated_at",
            params![
                dataset.employee.wallet,
                dataset_json,
                report_json,
                report.freshness.report_generated_at as i64
            ],
        )?;
        for (alias, alias_type) in aliases_for_employee(&dataset.employee) {
            transaction.execute(
                "INSERT INTO employee_aliases(alias_normalized, wallet, alias_type, updated_at)
                 VALUES(?1, ?2, ?3, ?4)
                 ON CONFLICT(alias_normalized, wallet) DO UPDATE SET
                   alias_type=excluded.alias_type,
                   updated_at=excluded.updated_at",
                params![
                    alias.to_lowercase(),
                    dataset.employee.wallet,
                    alias_type,
                    report.freshness.report_generated_at as i64
                ],
            )?;
        }
        transaction.commit()?;
        write_report_files(&self.cache_dir, dataset, report)?;
        Ok(())
    }
}

fn aliases_for_employee(employee: &EmployeeIdentity) -> Vec<(&str, &str)> {
    let mut aliases = Vec::new();
    if let Some(name) = employee.display_name.as_deref() {
        if !name.trim().is_empty() {
            aliases.push((name, "display_name"));
        }
    }
    if let Some(username) = employee.username.as_deref() {
        if !username.trim().is_empty() {
            aliases.push((username, "username"));
        }
    }
    aliases
}

pub fn refresh_employee_stats(
    client: &PolymarketDataClient,
    store: &mut EmployeeStatsStore,
    requested_employee: EmployeeIdentity,
    config: &EmployeeStatsConfig,
    selection: RefreshSelection,
) -> Result<EmployeeStatsReport, EmployeeStatsError> {
    config.validate()?;
    requested_employee.validate()?;
    if !selection.any() {
        return Err(EmployeeStatsError::Invalid(
            "at least one refresh component is required".to_owned(),
        ));
    }

    let now = now_secs();
    let cached = store.load_dataset(&requested_employee.wallet)?;
    let employee = requested_employee.merge_with_cached(cached.as_ref().map(|data| &data.employee));
    let mut dataset = cached.unwrap_or_else(|| EmployeeStatsDataset::empty(employee.clone(), now));
    dataset.employee = employee;
    dataset.collected_at = now;

    let window_cutoff = now.saturating_sub(config.window_days * SECONDS_PER_DAY);
    let retention_cutoff = now.saturating_sub(config.retention_days * SECONDS_PER_DAY);
    dataset.quality.failed_components.clear();
    dataset.quality.invalid_trade_count = 0;
    dataset.quality.duplicate_trade_count = 0;
    let trade_refresh_cutoff = if (selection.activity && dataset.sync.activity.history_truncated)
        || (selection.trades && dataset.sync.trades.history_truncated)
    {
        window_cutoff
    } else {
        incremental_trade_cutoff(&dataset.trades, window_cutoff)
    };

    if selection.activity {
        let cutoff = trade_refresh_cutoff;
        refresh_trade_source(
            "activity",
            &mut dataset.sync.activity,
            &mut dataset.quality,
            now,
            cutoff,
            config.max_pages,
            || {
                fetch_trade_pages(cutoff, config.max_pages, |limit, offset| {
                    client.activity_history(&dataset.employee.wallet, limit, offset)
                })
            },
            &mut dataset.trades,
            retention_cutoff,
        );
    }

    if selection.trades {
        let cutoff = trade_refresh_cutoff;
        refresh_trade_source(
            "trades",
            &mut dataset.sync.trades,
            &mut dataset.quality,
            now,
            cutoff,
            config.max_pages,
            || {
                fetch_trade_pages(cutoff, config.max_pages, |limit, offset| {
                    client.trades_history(&dataset.employee.wallet, limit, offset, None)
                })
            },
            &mut dataset.trades,
            retention_cutoff,
        );
    }

    if selection.closed_positions {
        let cutoff = if dataset.sync.closed_positions.history_truncated {
            window_cutoff
        } else {
            incremental_closed_cutoff(&dataset.closed_positions, window_cutoff)
        };
        let freshness = &mut dataset.sync.closed_positions;
        freshness.last_attempt_at = Some(now);
        match fetch_closed_pages(&dataset.employee.wallet, cutoff, config.max_pages, client) {
            Ok(batch) => {
                merge_closed_positions(
                    &mut dataset.closed_positions,
                    batch.items,
                    retention_cutoff,
                );
                freshness.last_success_at = Some(now);
                freshness.latest_source_event_at =
                    latest_closed_timestamp(&dataset.closed_positions);
                freshness.complete_from = min_option(freshness.complete_from, Some(cutoff));
                freshness.complete_through = Some(now);
                freshness.history_truncated = batch.truncated;
                freshness.last_error = None;
            }
            Err(error) => {
                record_component_failure("closed_positions", freshness, &mut dataset.quality, error)
            }
        }
    }

    if selection.positions {
        let freshness = &mut dataset.sync.positions;
        freshness.last_attempt_at = Some(now);
        match fetch_position_pages(&dataset.employee.wallet, config.max_pages, client) {
            Ok(batch) => {
                dataset.current_positions = dedup_current_positions(batch.items);
                freshness.last_success_at = Some(now);
                freshness.latest_source_event_at = None;
                freshness.complete_from = Some(now);
                freshness.complete_through = Some(now);
                freshness.history_truncated = batch.truncated;
                freshness.last_error = None;
            }
            Err(error) => {
                record_component_failure("positions", freshness, &mut dataset.quality, error)
            }
        }
    }

    fill_employee_metadata(&mut dataset);
    if dataset.employee.primary_domain.trim().is_empty()
        || dataset
            .employee
            .primary_domain
            .eq_ignore_ascii_case("UNKNOWN")
    {
        if let Some(domain) = infer_primary_domain(&dataset) {
            dataset.employee.primary_domain = domain;
            dataset.employee.primary_domain_source = "inferred".to_owned();
        } else {
            dataset.employee.primary_domain = "UNKNOWN".to_owned();
            dataset.employee.primary_domain_source = "unknown".to_owned();
        }
    } else {
        dataset.employee.primary_domain = dataset.employee.primary_domain.to_uppercase();
    }

    let report = build_employee_stats_report(&dataset, config, now);
    store.save(&dataset, &report)?;
    Ok(report)
}

pub fn rebuild_employee_stats(
    store: &mut EmployeeStatsStore,
    wallet_or_alias: &str,
    config: &EmployeeStatsConfig,
) -> Result<EmployeeStatsReport, EmployeeStatsError> {
    let wallet = store.resolve_wallet(wallet_or_alias)?;
    let dataset = store
        .load_dataset(&wallet)?
        .ok_or_else(|| EmployeeStatsError::NotFound(wallet.clone()))?;
    let report = build_employee_stats_report(&dataset, config, now_secs());
    store.save(&dataset, &report)?;
    Ok(report)
}

struct PageBatch<T> {
    items: Vec<T>,
    truncated: bool,
}

fn fetch_trade_pages<F>(
    cutoff: u64,
    max_pages: usize,
    mut request: F,
) -> Result<PageBatch<UserTrade>, PolymarketError>
where
    F: FnMut(usize, usize) -> Result<Vec<UserTrade>, PolymarketError>,
{
    let mut items = Vec::new();
    for page in 0..max_pages {
        let page_items = request(TRADE_PAGE_SIZE, page * TRADE_PAGE_SIZE)?;
        let page_len = page_items.len();
        let reached_cutoff = page_items.iter().any(|trade| {
            normalized_timestamp(trade.timestamp)
                .map(|timestamp| timestamp < cutoff)
                .unwrap_or(false)
        });
        items.extend(page_items);
        if page_len < TRADE_PAGE_SIZE || reached_cutoff {
            return Ok(PageBatch {
                items,
                truncated: false,
            });
        }
    }
    Ok(PageBatch {
        items,
        truncated: true,
    })
}

fn fetch_closed_pages(
    wallet: &str,
    cutoff: u64,
    max_pages: usize,
    client: &PolymarketDataClient,
) -> Result<PageBatch<ClosedPosition>, PolymarketError> {
    let mut items = Vec::new();
    for page in 0..max_pages {
        let page_items = client.closed_positions_history(wallet, PAGE_SIZE, page * PAGE_SIZE)?;
        let page_len = page_items.len();
        let reached_cutoff = page_items.iter().any(|position| {
            normalized_timestamp(position.timestamp)
                .map(|timestamp| timestamp < cutoff)
                .unwrap_or(false)
        });
        items.extend(page_items);
        if page_len < PAGE_SIZE || reached_cutoff {
            return Ok(PageBatch {
                items,
                truncated: false,
            });
        }
    }
    Ok(PageBatch {
        items,
        truncated: true,
    })
}

fn fetch_position_pages(
    wallet: &str,
    max_pages: usize,
    client: &PolymarketDataClient,
) -> Result<PageBatch<CurrentPosition>, PolymarketError> {
    let mut items = Vec::new();
    for page in 0..max_pages {
        let page_items = client.positions_history(wallet, PAGE_SIZE, page * PAGE_SIZE)?;
        let page_len = page_items.len();
        items.extend(page_items);
        if page_len < PAGE_SIZE {
            return Ok(PageBatch {
                items,
                truncated: false,
            });
        }
    }
    Ok(PageBatch {
        items,
        truncated: true,
    })
}

#[allow(clippy::too_many_arguments)]
fn refresh_trade_source<F>(
    name: &str,
    freshness: &mut ComponentFreshness,
    quality: &mut CachedDataQuality,
    now: u64,
    cutoff: u64,
    _max_pages: usize,
    fetch: F,
    stored: &mut Vec<UserTrade>,
    retention_cutoff: u64,
) where
    F: FnOnce() -> Result<PageBatch<UserTrade>, PolymarketError>,
{
    freshness.last_attempt_at = Some(now);
    match fetch() {
        Ok(batch) => {
            let (duplicates, invalid) = merge_trades(stored, batch.items, retention_cutoff);
            quality.duplicate_trade_count += duplicates;
            quality.invalid_trade_count += invalid;
            freshness.last_success_at = Some(now);
            freshness.latest_source_event_at = latest_trade_timestamp(stored);
            freshness.complete_from = min_option(freshness.complete_from, Some(cutoff));
            freshness.complete_through = Some(now);
            freshness.history_truncated = batch.truncated;
            freshness.last_error = None;
        }
        Err(error) => record_component_failure(name, freshness, quality, error),
    }
}

fn record_component_failure(
    name: &str,
    freshness: &mut ComponentFreshness,
    quality: &mut CachedDataQuality,
    error: PolymarketError,
) {
    let message = error.to_string();
    freshness.last_error = Some(message);
    quality.failed_components.push(name.to_owned());
}

fn incremental_trade_cutoff(trades: &[UserTrade], window_cutoff: u64) -> u64 {
    latest_trade_timestamp(trades)
        .map(|latest| latest.saturating_sub(OVERLAP_SECONDS).max(window_cutoff))
        .unwrap_or(window_cutoff)
}

fn incremental_closed_cutoff(positions: &[ClosedPosition], window_cutoff: u64) -> u64 {
    latest_closed_timestamp(positions)
        .map(|latest| latest.saturating_sub(OVERLAP_SECONDS).max(window_cutoff))
        .unwrap_or(window_cutoff)
}

fn merge_trades(
    stored: &mut Vec<UserTrade>,
    incoming: Vec<UserTrade>,
    retention_cutoff: u64,
) -> (usize, usize) {
    let mut map = HashMap::new();
    let mut duplicates = 0;
    let mut invalid = 0;
    for mut trade in stored.drain(..).chain(incoming) {
        let Some(timestamp) = normalized_timestamp(trade.timestamp) else {
            invalid += 1;
            continue;
        };
        trade.timestamp = Some(timestamp);
        if timestamp < retention_cutoff || !valid_trade(&trade) {
            invalid += 1;
            continue;
        }
        let key = trade_key(&trade);
        if map.insert(key, trade).is_some() {
            duplicates += 1;
        }
    }
    let mut values = map.into_values().collect::<Vec<_>>();
    values.sort_by_key(|trade| std::cmp::Reverse(trade.timestamp.unwrap_or(0)));
    *stored = values;
    (duplicates, invalid)
}

fn merge_closed_positions(
    stored: &mut Vec<ClosedPosition>,
    incoming: Vec<ClosedPosition>,
    retention_cutoff: u64,
) {
    let mut map = HashMap::new();
    for mut position in stored.drain(..).chain(incoming) {
        let Some(timestamp) = normalized_timestamp(position.timestamp) else {
            continue;
        };
        if timestamp < retention_cutoff {
            continue;
        }
        position.timestamp = Some(timestamp);
        map.insert(closed_position_key(&position), position);
    }
    let mut values = map.into_values().collect::<Vec<_>>();
    values.sort_by_key(|position| std::cmp::Reverse(position.timestamp.unwrap_or(0)));
    *stored = values;
}

fn dedup_current_positions(positions: Vec<CurrentPosition>) -> Vec<CurrentPosition> {
    let mut map = HashMap::new();
    for position in positions {
        map.insert(current_position_key(&position), position);
    }
    map.into_values().collect()
}

fn valid_trade(trade: &UserTrade) -> bool {
    let side_valid =
        trade.side.eq_ignore_ascii_case("BUY") || trade.side.eq_ignore_ascii_case("SELL");
    let price_valid = trade
        .price
        .map(|value| value.is_finite() && (0.0..=1.0).contains(&value))
        .unwrap_or(false);
    let size_valid = trade
        .size
        .map(|value| value.is_finite() && value > 0.0)
        .unwrap_or(false);
    side_valid
        && price_valid
        && size_valid
        && !trade.asset.trim().is_empty()
        && !trade.condition_id.trim().is_empty()
}

fn trade_key(trade: &UserTrade) -> String {
    format!(
        "{}|{}|{}|{}|{}|{}|{}",
        trade.transaction_hash.as_deref().unwrap_or("-"),
        trade.condition_id,
        trade.asset,
        trade.side.to_uppercase(),
        trade.price.unwrap_or_default().to_bits(),
        trade.size.unwrap_or_default().to_bits(),
        trade.timestamp.unwrap_or_default()
    )
}

fn closed_position_key(position: &ClosedPosition) -> String {
    format!(
        "{}|{}|{}|{}",
        position.condition_id.as_deref().unwrap_or("-"),
        position.asset.as_deref().unwrap_or("-"),
        position.outcome.as_deref().unwrap_or("-"),
        position.timestamp.unwrap_or_default()
    )
}

fn current_position_key(position: &CurrentPosition) -> String {
    format!(
        "{}|{}|{}",
        position.condition_id.as_deref().unwrap_or("-"),
        position.asset.as_deref().unwrap_or("-"),
        position.outcome.as_deref().unwrap_or("-")
    )
}

fn settlement_key(condition_id: Option<&str>, asset: Option<&str>) -> String {
    format!("{}|{}", condition_id.unwrap_or("-"), asset.unwrap_or("-"))
}

fn fill_employee_metadata(dataset: &mut EmployeeStatsDataset) {
    let latest = dataset
        .trades
        .iter()
        .max_by_key(|trade| trade.timestamp.unwrap_or(0));
    if dataset.employee.display_name.is_none() {
        dataset.employee.display_name =
            latest.and_then(|trade| trade.name.clone().or_else(|| trade.pseudonym.clone()));
    }
    if dataset.employee.username.is_none() {
        dataset.employee.username = latest.and_then(|trade| trade.pseudonym.clone());
    }
}

pub fn build_employee_stats_report(
    dataset: &EmployeeStatsDataset,
    config: &EmployeeStatsConfig,
    now: u64,
) -> EmployeeStatsReport {
    let mut windows = BTreeMap::new();
    for days in [1, 7, config.window_days] {
        let label = format!("{days}d");
        windows.insert(
            label,
            calculate_domain_breakdown(dataset, config, now, days),
        );
    }

    let history_truncated = dataset.sync.activity.history_truncated
        || dataset.sync.trades.history_truncated
        || dataset.sync.closed_positions.history_truncated
        || dataset.sync.positions.history_truncated;
    let missing_timestamp_count = dataset
        .trades
        .iter()
        .filter(|trade| trade.timestamp.is_none())
        .count()
        + dataset
            .closed_positions
            .iter()
            .filter(|position| position.timestamp.is_none())
            .count();
    let failed_components = dataset.quality.failed_components.clone();
    let position_age_unknown_count = windows
        .get(&format!("{}d", config.window_days))
        .map(|metrics| metrics.wallet_total.position_age_unknown_count)
        .unwrap_or(0);
    let missing_core_component = (dataset.sync.activity.last_success_at.is_none()
        && dataset.sync.trades.last_success_at.is_none())
        || dataset.sync.closed_positions.last_success_at.is_none()
        || dataset.sync.positions.last_success_at.is_none();
    let status = if dataset.trades.is_empty()
        && dataset.closed_positions.is_empty()
        && dataset.current_positions.is_empty()
    {
        "empty"
    } else if history_truncated || !failed_components.is_empty() || missing_core_component {
        "partial"
    } else {
        "complete"
    };
    let freshness = ReportFreshness {
        report_generated_at: now,
        latest_trade_at: latest_trade_timestamp(&dataset.trades),
        latest_closed_position_at: latest_closed_timestamp(&dataset.closed_positions),
        positions_observed_at: dataset.sync.positions.last_success_at,
        activity_last_success_at: dataset.sync.activity.last_success_at,
        trades_last_success_at: dataset.sync.trades.last_success_at,
        closed_positions_last_success_at: dataset.sync.closed_positions.last_success_at,
        positions_last_success_at: dataset.sync.positions.last_success_at,
        data_complete_from: max_option_many([
            dataset.sync.activity.complete_from,
            dataset.sync.trades.complete_from,
            dataset.sync.closed_positions.complete_from,
        ]),
        data_complete_through: min_option_many([
            dataset.sync.activity.complete_through,
            dataset.sync.trades.complete_through,
            dataset.sync.closed_positions.complete_through,
            dataset.sync.positions.complete_through,
        ]),
    };
    let data_quality = ReportDataQuality {
        report_status: status.to_owned(),
        history_truncated,
        missing_timestamp_count,
        invalid_trade_count: dataset.quality.invalid_trade_count,
        duplicate_trade_count: dataset.quality.duplicate_trade_count,
        position_age_unknown_count,
        failed_components,
        metric_schema_version: METRIC_SCHEMA_VERSION,
        rules_version: RULES_VERSION,
        domain_classifier_version: DOMAIN_CLASSIFIER_VERSION,
    };
    let conclusion = build_conclusion(
        windows
            .get(&format!("{}d", config.window_days))
            .expect("configured window exists"),
        &dataset.employee,
        &data_quality,
    );

    EmployeeStatsReport {
        schema_version: 1,
        window_days: config.window_days,
        metric_schema_version: METRIC_SCHEMA_VERSION,
        rules_version: RULES_VERSION,
        domain_classifier_version: DOMAIN_CLASSIFIER_VERSION,
        employee: dataset.employee.clone(),
        freshness,
        data_quality,
        windows,
        conclusion,
    }
}

fn calculate_domain_breakdown(
    dataset: &EmployeeStatsDataset,
    config: &EmployeeStatsConfig,
    now: u64,
    days: u64,
) -> DomainBreakdown {
    let cutoff = now.saturating_sub(days * SECONDS_PER_DAY);
    let trades = dataset
        .trades
        .iter()
        .filter(|trade| timestamp_in_window(trade.timestamp, cutoff, now))
        .map(|trade| (trade, classify_trade(&dataset.employee, trade)))
        .collect::<Vec<_>>();
    let closed = dataset
        .closed_positions
        .iter()
        .filter(|position| timestamp_in_window(position.timestamp, cutoff, now))
        .map(|position| (position, classify_closed(&dataset.employee, position)))
        .collect::<Vec<_>>();
    let current = dataset
        .current_positions
        .iter()
        .map(|position| (position, classify_current(&dataset.employee, position)))
        .collect::<Vec<_>>();
    let primary = dataset.employee.primary_domain.to_uppercase();
    let known_other_domains = trades
        .iter()
        .map(|(_, domain)| domain.clone())
        .chain(closed.iter().map(|(_, domain)| domain.clone()))
        .chain(current.iter().map(|(_, domain)| domain.clone()))
        .filter(|domain| is_known_domain(domain) && domain != &primary)
        .collect::<HashSet<_>>();

    let wallet_total = calculate_segment(&trades, &closed, &current, config, cutoff, now, |_| true);
    let primary_domain =
        calculate_segment(&trades, &closed, &current, config, cutoff, now, |domain| {
            domain == primary
        });
    let other_domains_total =
        calculate_segment(&trades, &closed, &current, config, cutoff, now, |domain| {
            is_known_domain(domain) && domain != primary
        });
    let unknown_or_ambiguous =
        calculate_segment(&trades, &closed, &current, config, cutoff, now, |domain| {
            !is_known_domain(domain)
        });
    let topic_trades = dataset
        .trades
        .iter()
        .filter(|trade| timestamp_in_window(trade.timestamp, cutoff, now))
        .map(|trade| {
            (
                trade,
                if is_world_cup_trade(trade) {
                    "WORLD_CUP".to_owned()
                } else {
                    "OTHER".to_owned()
                },
            )
        })
        .collect::<Vec<_>>();
    let topic_closed = dataset
        .closed_positions
        .iter()
        .filter(|position| timestamp_in_window(position.timestamp, cutoff, now))
        .map(|position| {
            (
                position,
                if is_world_cup_closed(position) {
                    "WORLD_CUP".to_owned()
                } else {
                    "OTHER".to_owned()
                },
            )
        })
        .collect::<Vec<_>>();
    let topic_current = dataset
        .current_positions
        .iter()
        .map(|position| {
            (
                position,
                if is_world_cup_current(position) {
                    "WORLD_CUP".to_owned()
                } else {
                    "OTHER".to_owned()
                },
            )
        })
        .collect::<Vec<_>>();
    let world_cup = calculate_segment(
        &topic_trades,
        &topic_closed,
        &topic_current,
        config,
        cutoff,
        now,
        |topic| topic == "WORLD_CUP",
    );
    let mut specialties = BTreeMap::new();
    if segment_has_activity(&world_cup) {
        specialties.insert("WORLD_CUP".to_owned(), world_cup);
    }
    let mut other_domains = BTreeMap::new();
    let mut sorted_domains = known_other_domains.into_iter().collect::<Vec<_>>();
    sorted_domains.sort();
    for domain in sorted_domains {
        other_domains.insert(
            domain.clone(),
            calculate_segment(&trades, &closed, &current, config, cutoff, now, |value| {
                value == domain
            }),
        );
    }
    let comparison = DomainComparison {
        primary_trade_notional_share: round4(ratio(
            primary_domain.gross_trade_notional_usd,
            wallet_total.gross_trade_notional_usd,
        )),
        primary_action_share: round4(ratio(
            primary_domain.action_count as f64,
            wallet_total.action_count as f64,
        )),
        primary_realized_profit_share: round4(ratio(
            primary_domain.gross_profit_usd,
            wallet_total.gross_profit_usd,
        )),
        primary_combined_profit_share: round4(if wallet_total.combined_pnl_usd > 0.0 {
            primary_domain.combined_pnl_usd.max(0.0) / wallet_total.combined_pnl_usd
        } else {
            0.0
        }),
        primary_vs_other_settled_win_rate_gap: round4(
            primary_domain.settled_market_win_rate - other_domains_total.settled_market_win_rate,
        ),
        primary_vs_other_marked_win_rate_gap: round4(
            primary_domain.marked_position_win_rate - other_domains_total.marked_position_win_rate,
        ),
        primary_vs_other_roi_gap: round4(
            primary_domain.realized_roi - other_domains_total.realized_roi,
        ),
        primary_vs_other_expectancy_gap_usd: round2(
            primary_domain.expectancy_per_settled_market_usd
                - other_domains_total.expectancy_per_settled_market_usd,
        ),
    };

    DomainBreakdown {
        wallet_total,
        primary_domain,
        other_domains_total,
        other_domains,
        unknown_or_ambiguous,
        specialties,
        comparison,
    }
}

fn calculate_segment<F>(
    trades: &[(&UserTrade, String)],
    closed: &[(&ClosedPosition, String)],
    current: &[(&CurrentPosition, String)],
    config: &EmployeeStatsConfig,
    cutoff: u64,
    now: u64,
    matches: F,
) -> SegmentMetrics
where
    F: Fn(&str) -> bool,
{
    let segment_trades = trades
        .iter()
        .filter(|(_, domain)| matches(domain))
        .map(|(trade, _)| *trade)
        .collect::<Vec<_>>();
    let segment_closed = closed
        .iter()
        .filter(|(_, domain)| matches(domain))
        .map(|(position, _)| *position)
        .collect::<Vec<_>>();
    let segment_current = current
        .iter()
        .filter(|(_, domain)| matches(domain))
        .map(|(position, _)| *position)
        .collect::<Vec<_>>();

    let fill_amounts = segment_trades
        .iter()
        .filter_map(|trade| trade_notional(trade))
        .collect::<Vec<_>>();
    let gross_trade_notional_usd = fill_amounts.iter().sum::<f64>();
    let buy_notional_usd = segment_trades
        .iter()
        .filter(|trade| trade.side.eq_ignore_ascii_case("BUY"))
        .filter_map(|trade| trade_notional(trade))
        .sum::<f64>();
    let buy_fill_count = segment_trades
        .iter()
        .filter(|trade| trade.side.eq_ignore_ascii_case("BUY"))
        .count();
    let sell_fill_count = segment_trades
        .iter()
        .filter(|trade| trade.side.eq_ignore_ascii_case("SELL"))
        .count();
    let sell_notional_usd = segment_trades
        .iter()
        .filter(|trade| trade.side.eq_ignore_ascii_case("SELL"))
        .filter_map(|trade| trade_notional(trade))
        .sum::<f64>();
    let action_amounts = action_notionals(&segment_trades, config.action_gap_seconds);
    let high_price_buy_notional_80 = segment_trades
        .iter()
        .filter(|trade| {
            trade.side.eq_ignore_ascii_case("BUY") && trade.price.unwrap_or(0.0) >= 0.80
        })
        .filter_map(|trade| trade_notional(trade))
        .sum::<f64>();
    let high_price_buy_notional_95 = segment_trades
        .iter()
        .filter(|trade| {
            trade.side.eq_ignore_ascii_case("BUY") && trade.price.unwrap_or(0.0) >= 0.95
        })
        .filter_map(|trade| trade_notional(trade))
        .sum::<f64>();
    let repeated_market_ratio = repeated_market_ratio(&segment_trades);
    let net_buy_notional_usd = buy_notional_usd - sell_notional_usd;
    let net_flow_ratio = ratio(net_buy_notional_usd.abs(), gross_trade_notional_usd);
    let two_sided_fill_ratio = ratio(
        buy_fill_count.min(sell_fill_count) as f64 * 2.0,
        segment_trades.len() as f64,
    );
    let suspected_market_making = segment_trades.len() >= 10
        && two_sided_fill_ratio >= 0.50
        && net_flow_ratio <= 0.25
        && repeated_market_ratio >= 0.60;
    let active_days = segment_trades
        .iter()
        .filter_map(|trade| trade.timestamp.map(|timestamp| timestamp / SECONDS_PER_DAY))
        .collect::<HashSet<_>>()
        .len();
    let unique_markets = segment_trades
        .iter()
        .map(|trade| trade.condition_id.as_str())
        .chain(
            segment_closed
                .iter()
                .filter_map(|position| position.condition_id.as_deref()),
        )
        .collect::<HashSet<_>>()
        .len();
    let unique_outcomes = segment_trades
        .iter()
        .map(|trade| format!("{}|{}", trade.condition_id, trade.asset))
        .chain(segment_closed.iter().map(|position| {
            format!(
                "{}|{}",
                position.condition_id.as_deref().unwrap_or("-"),
                position.asset.as_deref().unwrap_or("-")
            )
        }))
        .collect::<HashSet<_>>()
        .len();

    let mut settled_records = segment_closed
        .iter()
        .map(|position| SettledRecord {
            position_key: settlement_key(
                position.condition_id.as_deref(),
                position.asset.as_deref(),
            ),
            market_key: position
                .condition_id
                .clone()
                .unwrap_or_else(|| closed_position_key(position)),
            pnl_usd: closed_position_pnl_usd(position),
            invested_usd: closed_position_invested_usd(position),
            settled_at: position.timestamp.unwrap_or(0),
            from_redeemable: false,
        })
        .collect::<Vec<_>>();
    let closed_keys = settled_records
        .iter()
        .map(|record| record.position_key.clone())
        .collect::<HashSet<_>>();
    let redeemable_positions = segment_current
        .iter()
        .filter(|position| position.redeemable.unwrap_or(false))
        .filter_map(|position| {
            let settled_at = end_date_timestamp(position.end_date.as_deref())?;
            (settled_at >= cutoff && settled_at <= now).then_some((*position, settled_at))
        })
        .collect::<Vec<_>>();
    for (position, settled_at) in &redeemable_positions {
        let position_key =
            settlement_key(position.condition_id.as_deref(), position.asset.as_deref());
        if closed_keys.contains(&position_key) {
            continue;
        }
        settled_records.push(SettledRecord {
            position_key,
            market_key: position
                .condition_id
                .clone()
                .unwrap_or_else(|| current_position_key(position)),
            pnl_usd: current_position_settled_pnl_usd(position),
            invested_usd: current_position_invested_usd(position),
            settled_at: *settled_at,
            from_redeemable: true,
        });
    }
    let realized_pnls = settled_records
        .iter()
        .map(|record| record.pnl_usd)
        .collect::<Vec<_>>();
    let realized_pnl_usd = realized_pnls.iter().sum::<f64>();
    let invested_usd = settled_records
        .iter()
        .map(|record| record.invested_usd)
        .sum::<f64>();
    let wins = realized_pnls.iter().filter(|value| **value > 0.0).count();
    let losses = realized_pnls.iter().filter(|value| **value < 0.0).count();
    let breakeven_positions = realized_pnls.iter().filter(|value| **value == 0.0).count();
    let gross_profit_usd = realized_pnls
        .iter()
        .filter(|value| **value > 0.0)
        .sum::<f64>();
    let gross_loss_usd = realized_pnls
        .iter()
        .filter(|value| **value < 0.0)
        .map(|value| value.abs())
        .sum::<f64>();
    let mut market_pnl: HashMap<String, (f64, u64)> = HashMap::new();
    for record in &settled_records {
        let entry = market_pnl
            .entry(record.market_key.clone())
            .or_insert((0.0, 0));
        entry.0 += record.pnl_usd;
        entry.1 = entry.1.max(record.settled_at);
    }
    let market_wins = market_pnl
        .values()
        .filter(|(value, _)| *value > 0.0)
        .count();
    let settled_markets = market_pnl.len();
    let mut ordered_markets = market_pnl.values().copied().collect::<Vec<_>>();
    ordered_markets.sort_by_key(|(_, timestamp)| *timestamp);
    let (max_realized_drawdown_usd, longest_win_streak, longest_loss_streak) =
        realized_sequence_metrics(&ordered_markets);

    let active_positions = segment_current
        .iter()
        .filter(|position| is_active_position(position))
        .copied()
        .collect::<Vec<_>>();
    let mergeable_positions = segment_current
        .iter()
        .filter(|position| {
            !position.redeemable.unwrap_or(false) && position.mergeable.unwrap_or(false)
        })
        .count();
    let open_initial_value_usd = active_positions
        .iter()
        .map(|position| position.initial_value.unwrap_or(0.0).max(0.0))
        .sum::<f64>();
    let open_current_value_usd = active_positions
        .iter()
        .map(|position| position.current_value.unwrap_or(0.0).max(0.0))
        .sum::<f64>();
    let unrealized_pnl_usd = active_positions
        .iter()
        .map(|position| position.cash_pnl.unwrap_or(0.0))
        .sum::<f64>();
    let open_profit_positions = active_positions
        .iter()
        .filter(|position| position.cash_pnl.unwrap_or(0.0) > 0.0)
        .count();
    let open_loss_positions = active_positions
        .iter()
        .filter(|position| position.cash_pnl.unwrap_or(0.0) < 0.0)
        .count();
    let open_loss_usd = active_positions
        .iter()
        .map(|position| position.cash_pnl.unwrap_or(0.0).min(0.0).abs())
        .sum::<f64>();
    let largest_open_position_usd = active_positions
        .iter()
        .map(|position| position.initial_value.unwrap_or(0.0).max(0.0))
        .fold(0.0, f64::max);
    let largest_open_loss_usd = active_positions
        .iter()
        .map(|position| position.cash_pnl.unwrap_or(0.0).min(0.0).abs())
        .fold(0.0, f64::max);
    let redeemable_value_usd = redeemable_positions
        .iter()
        .map(|(position, _)| position.current_value.unwrap_or(0.0).max(0.0))
        .sum::<f64>();
    let redeemable_pnl_usd = settled_records
        .iter()
        .filter(|record| record.from_redeemable)
        .map(|record| record.pnl_usd)
        .sum::<f64>();
    let first_buy_times = first_buy_times(&segment_trades);
    let mut losing_positions_older_than_3d = 0;
    let mut losing_positions_older_than_7d = 0;
    let mut stale_losing_value_usd = 0.0;
    let mut stale_losing_pnl_usd = 0.0;
    let mut position_age_unknown_count = 0;
    for position in active_positions
        .iter()
        .filter(|position| position.cash_pnl.unwrap_or(0.0) < 0.0)
    {
        let key = current_position_trade_key(position);
        match first_buy_times.get(&key) {
            Some(opened_at) => {
                let age = now.saturating_sub(*opened_at);
                if age >= 3 * SECONDS_PER_DAY {
                    losing_positions_older_than_3d += 1;
                }
                if age >= 7 * SECONDS_PER_DAY {
                    losing_positions_older_than_7d += 1;
                    stale_losing_value_usd += position.initial_value.unwrap_or(0.0).max(0.0);
                    stale_losing_pnl_usd += position.cash_pnl.unwrap_or(0.0);
                }
            }
            None => position_age_unknown_count += 1,
        }
    }
    let marked_denominator = settled_records.len() + active_positions.len();
    let marked_wins = wins + open_profit_positions;
    let avg_win = if wins > 0 {
        gross_profit_usd / wins as f64
    } else {
        0.0
    };
    let avg_loss = if losses > 0 {
        -(gross_loss_usd / losses as f64)
    } else {
        0.0
    };

    SegmentMetrics {
        gross_trade_notional_usd: round2(gross_trade_notional_usd),
        buy_notional_usd: round2(buy_notional_usd),
        sell_notional_usd: round2(sell_notional_usd),
        fill_count: segment_trades.len(),
        buy_fill_count,
        sell_fill_count,
        action_count: action_amounts.len(),
        unique_markets,
        unique_outcomes,
        active_days,
        fills_per_active_day: round2(ratio(segment_trades.len() as f64, active_days as f64)),
        actions_per_active_day: round2(ratio(action_amounts.len() as f64, active_days as f64)),
        avg_fill_notional_usd: round2(average(&fill_amounts)),
        median_fill_notional_usd: round2(percentile(fill_amounts.clone(), 0.50)),
        p80_fill_notional_usd: round2(percentile(fill_amounts.clone(), 0.80)),
        p95_fill_notional_usd: round2(percentile(fill_amounts.clone(), 0.95)),
        max_fill_notional_usd: round2(fill_amounts.iter().copied().fold(0.0, f64::max)),
        avg_action_notional_usd: round2(average(&action_amounts)),
        median_action_notional_usd: round2(percentile(action_amounts, 0.50)),
        sell_notional_ratio: round4(ratio(sell_notional_usd, gross_trade_notional_usd)),
        net_buy_notional_usd: round2(net_buy_notional_usd),
        net_flow_ratio: round4(net_flow_ratio),
        repeated_market_ratio: round4(repeated_market_ratio),
        high_price_buy_notional_share_80: round4(ratio(
            high_price_buy_notional_80,
            buy_notional_usd,
        )),
        high_price_buy_notional_share_95: round4(ratio(
            high_price_buy_notional_95,
            buy_notional_usd,
        )),
        suspected_market_making,
        settled_positions: settled_records.len(),
        settled_markets,
        realized_pnl_usd: round2(realized_pnl_usd),
        invested_usd: round2(invested_usd),
        realized_roi: round4(ratio(realized_pnl_usd, invested_usd)),
        settled_position_win_rate: round4(ratio(wins as f64, settled_records.len() as f64)),
        settled_market_win_rate: round4(ratio(market_wins as f64, settled_markets as f64)),
        breakeven_positions,
        gross_profit_usd: round2(gross_profit_usd),
        gross_loss_usd: round2(gross_loss_usd),
        profit_factor: if gross_loss_usd > 0.0 {
            Some(round4(gross_profit_usd / gross_loss_usd))
        } else {
            None
        },
        avg_win_usd: round2(avg_win),
        avg_loss_usd: round2(avg_loss),
        payoff_ratio: if avg_loss < 0.0 {
            Some(round4(avg_win / avg_loss.abs()))
        } else {
            None
        },
        expectancy_per_settled_market_usd: round2(ratio(realized_pnl_usd, settled_markets as f64)),
        top_5_profit_share: round4(top_profit_share(&realized_pnls, 5)),
        max_realized_drawdown_usd: round2(max_realized_drawdown_usd),
        longest_win_streak,
        longest_loss_streak,
        open_positions: active_positions.len(),
        open_initial_value_usd: round2(open_initial_value_usd),
        open_current_value_usd: round2(open_current_value_usd),
        unrealized_pnl_usd: round2(unrealized_pnl_usd),
        open_profit_positions,
        open_loss_positions,
        open_loss_usd: round2(open_loss_usd),
        open_loss_ratio: round4(ratio(open_loss_usd, open_initial_value_usd)),
        open_loss_position_ratio: round4(ratio(
            open_loss_positions as f64,
            active_positions.len() as f64,
        )),
        largest_open_position_usd: round2(largest_open_position_usd),
        largest_open_loss_usd: round2(largest_open_loss_usd),
        open_position_concentration: round4(ratio(
            largest_open_position_usd,
            open_initial_value_usd,
        )),
        redeemable_positions: redeemable_positions.len(),
        redeemable_value_usd: round2(redeemable_value_usd),
        redeemable_pnl_usd: round2(redeemable_pnl_usd),
        mergeable_positions,
        losing_positions_older_than_3d,
        losing_positions_older_than_7d,
        stale_losing_value_usd: round2(stale_losing_value_usd),
        stale_losing_pnl_usd: round2(stale_losing_pnl_usd),
        position_age_unknown_count,
        marked_position_win_rate: round4(ratio(marked_wins as f64, marked_denominator as f64)),
        combined_pnl_usd: round2(realized_pnl_usd + unrealized_pnl_usd),
        hidden_loss_ratio: round4(ratio(open_loss_usd, gross_profit_usd)),
    }
}

fn action_notionals(trades: &[&UserTrade], gap_seconds: u64) -> Vec<f64> {
    let mut ordered = trades.to_vec();
    ordered.sort_by_key(|trade| trade.timestamp.unwrap_or(0));
    let mut active: HashMap<String, (u64, f64)> = HashMap::new();
    let mut completed = Vec::new();
    for trade in ordered {
        let timestamp = trade.timestamp.unwrap_or(0);
        let key = format!(
            "{}|{}|{}",
            trade.condition_id,
            trade.asset,
            trade.side.to_uppercase()
        );
        let notional = trade_notional(trade).unwrap_or(0.0);
        match active.get_mut(&key) {
            Some((last_timestamp, total))
                if timestamp.saturating_sub(*last_timestamp) <= gap_seconds =>
            {
                *last_timestamp = timestamp;
                *total += notional;
            }
            Some((last_timestamp, total)) => {
                completed.push(*total);
                *last_timestamp = timestamp;
                *total = notional;
            }
            None => {
                active.insert(key, (timestamp, notional));
            }
        }
    }
    completed.extend(active.into_values().map(|(_, total)| total));
    completed
}

fn repeated_market_ratio(trades: &[&UserTrade]) -> f64 {
    if trades.is_empty() {
        return 0.0;
    }
    let mut counts = HashMap::new();
    for trade in trades {
        *counts.entry(trade.condition_id.as_str()).or_insert(0usize) += 1;
    }
    let repeated_fills = trades
        .iter()
        .filter(|trade| {
            counts
                .get(trade.condition_id.as_str())
                .copied()
                .unwrap_or(0)
                > 1
        })
        .count();
    ratio(repeated_fills as f64, trades.len() as f64)
}

fn realized_sequence_metrics(ordered_markets: &[(f64, u64)]) -> (f64, usize, usize) {
    let mut cumulative: f64 = 0.0;
    let mut peak: f64 = 0.0;
    let mut max_drawdown: f64 = 0.0;
    let mut current_wins = 0;
    let mut current_losses = 0;
    let mut longest_wins = 0;
    let mut longest_losses = 0;
    for (pnl, _) in ordered_markets {
        cumulative += pnl;
        peak = peak.max(cumulative);
        max_drawdown = max_drawdown.max(peak - cumulative);
        if *pnl > 0.0 {
            current_wins += 1;
            current_losses = 0;
            longest_wins = longest_wins.max(current_wins);
        } else if *pnl < 0.0 {
            current_losses += 1;
            current_wins = 0;
            longest_losses = longest_losses.max(current_losses);
        } else {
            current_wins = 0;
            current_losses = 0;
        }
    }
    (max_drawdown, longest_wins, longest_losses)
}

fn first_buy_times(trades: &[&UserTrade]) -> HashMap<String, u64> {
    let mut first = HashMap::new();
    for trade in trades
        .iter()
        .filter(|trade| trade.side.eq_ignore_ascii_case("BUY"))
    {
        let key = format!("{}|{}", trade.condition_id, trade.asset);
        let timestamp = trade.timestamp.unwrap_or(0);
        first
            .entry(key)
            .and_modify(|value: &mut u64| *value = (*value).min(timestamp))
            .or_insert(timestamp);
    }
    first
}

fn current_position_trade_key(position: &CurrentPosition) -> String {
    format!(
        "{}|{}",
        position.condition_id.as_deref().unwrap_or("-"),
        position.asset.as_deref().unwrap_or("-")
    )
}

fn build_conclusion(
    metrics: &DomainBreakdown,
    employee: &EmployeeIdentity,
    quality: &ReportDataQuality,
) -> StatsConclusion {
    let total = &metrics.wallet_total;
    let primary = &metrics.primary_domain;
    let other = &metrics.other_domains_total;
    let mut flags = Vec::new();
    let mut facts = Vec::new();

    if total.settled_markets < 8 {
        flags.push("sample_too_small".to_owned());
    }
    if primary.settled_markets < 8 {
        flags.push("primary_domain_sample_too_small".to_owned());
    } else if other.settled_markets >= 8
        && primary.realized_roi > other.realized_roi
        && primary.settled_market_win_rate > other.settled_market_win_rate
    {
        flags.push("primary_domain_edge_confirmed".to_owned());
    } else if metrics.comparison.primary_trade_notional_share >= 0.95 {
        flags.push("single_domain_specialist".to_owned());
    }
    if total.open_loss_ratio > 0.20 {
        flags.push("open_losses_high".to_owned());
    }
    if total.losing_positions_older_than_7d >= 3
        || (total.open_initial_value_usd > 0.0
            && total.stale_losing_value_usd / total.open_initial_value_usd > 0.15)
    {
        flags.push("stale_losses_high".to_owned());
    }
    if total.settled_position_win_rate - total.marked_position_win_rate > 0.15
        && total.open_loss_positions >= 3
    {
        flags.push("settled_win_rate_inflated_by_open_losses".to_owned());
    }
    if total.top_5_profit_share > 0.75 {
        flags.push("profit_concentrated".to_owned());
    }
    if total.settled_markets >= 8 && total.realized_roi < 0.02 {
        flags.push("low_realized_roi".to_owned());
    }
    if total.max_realized_drawdown_usd > total.realized_pnl_usd.max(1_000.0) {
        flags.push("large_realized_drawdown".to_owned());
    }
    if total.redeemable_pnl_usd < 0.0
        && total.redeemable_pnl_usd.abs() > total.gross_profit_usd * 0.20
    {
        flags.push("redeemable_losses_high".to_owned());
    }
    if total.high_price_buy_notional_share_80 > 0.65 {
        flags.push("high_price_dependency".to_owned());
    }
    if total.suspected_market_making {
        flags.push("suspected_market_making".to_owned());
    }
    if other.combined_pnl_usd < 0.0 && other.open_loss_usd > 0.0 {
        flags.push("outside_domain_losses_high".to_owned());
    }
    if total.gross_profit_usd > 0.0 && metrics.comparison.primary_realized_profit_share < 0.60 {
        flags.push("profit_not_from_primary_domain".to_owned());
    }
    if quality.history_truncated || !quality.failed_components.is_empty() {
        flags.push("history_incomplete".to_owned());
    }

    facts.push(format!(
        "14天已结算市场胜率 {:.1}%，含当前持仓估值胜率 {:.1}%",
        total.settled_market_win_rate * 100.0,
        total.marked_position_win_rate * 100.0
    ));
    facts.push(format!(
        "已实现 PnL ${:.2}，未实现 PnL ${:.2}，综合 PnL ${:.2}",
        total.realized_pnl_usd, total.unrealized_pnl_usd, total.combined_pnl_usd
    ));
    facts.push(format!(
        "最近窗口可赎回/留存结算仓位 PnL ${:.2}，最大已实现回撤 ${:.2}",
        total.redeemable_pnl_usd, total.max_realized_drawdown_usd
    ));
    facts.push(format!(
        "主领域 {}：胜率 {:.1}%，ROI {:.1}%，综合 PnL ${:.2}",
        employee.primary_domain,
        primary.settled_market_win_rate * 100.0,
        primary.realized_roi * 100.0,
        primary.combined_pnl_usd
    ));
    facts.push(format!(
        "其他领域：胜率 {:.1}%，ROI {:.1}%，综合 PnL ${:.2}",
        other.settled_market_win_rate * 100.0,
        other.realized_roi * 100.0,
        other.combined_pnl_usd
    ));

    let summary_level = if flags.iter().any(|flag| {
        matches!(
            flag.as_str(),
            "open_losses_high"
                | "settled_win_rate_inflated_by_open_losses"
                | "history_incomplete"
                | "low_realized_roi"
                | "large_realized_drawdown"
                | "redeemable_losses_high"
        )
    }) {
        "caution"
    } else if flags
        .iter()
        .any(|flag| flag == "primary_domain_edge_confirmed")
    {
        "positive"
    } else {
        "neutral"
    };

    StatsConclusion {
        summary_level: summary_level.to_owned(),
        flags,
        facts,
    }
}

pub fn compact_report_value(report: &EmployeeStatsReport, window_days: u64) -> Value {
    let key = format!("{window_days}d");
    let metrics = report.windows.get(&key);
    let compact_other_domains = metrics
        .map(|value| {
            value
                .other_domains
                .iter()
                .map(|(domain, segment)| {
                    (
                        domain.clone(),
                        json!({
                            "settled_markets": segment.settled_markets,
                            "settled_market_win_rate": segment.settled_market_win_rate,
                            "realized_pnl_usd": segment.realized_pnl_usd,
                            "realized_roi": segment.realized_roi,
                            "unrealized_pnl_usd": segment.unrealized_pnl_usd,
                            "open_loss_usd": segment.open_loss_usd,
                            "combined_pnl_usd": segment.combined_pnl_usd,
                        }),
                    )
                })
                .collect::<BTreeMap<_, _>>()
        })
        .unwrap_or_default();
    let compact_specialties = metrics
        .map(|value| {
            value
                .specialties
                .iter()
                .map(|(topic, segment)| (topic.clone(), compact_segment_value(segment)))
                .collect::<BTreeMap<_, _>>()
        })
        .unwrap_or_default();
    json!({
        "schema_version": report.schema_version,
        "employee": report.employee,
        "freshness": report.freshness,
        "data_quality": report.data_quality,
        "window": key,
        "wallet_total": metrics.map(|value| compact_segment_value(&value.wallet_total)),
        "primary_domain": metrics.map(|value| compact_segment_value(&value.primary_domain)),
        "other_domains_total": metrics.map(|value| compact_segment_value(&value.other_domains_total)),
        "other_domains": compact_other_domains,
        "specialties": compact_specialties,
        "comparison": metrics.map(|value| &value.comparison),
        "conclusion": report.conclusion,
    })
}

fn compact_segment_value(segment: &SegmentMetrics) -> Value {
    json!({
        "gross_trade_notional_usd": segment.gross_trade_notional_usd,
        "fill_count": segment.fill_count,
        "action_count": segment.action_count,
        "active_days": segment.active_days,
        "avg_fill_notional_usd": segment.avg_fill_notional_usd,
        "median_fill_notional_usd": segment.median_fill_notional_usd,
        "p80_fill_notional_usd": segment.p80_fill_notional_usd,
        "p95_fill_notional_usd": segment.p95_fill_notional_usd,
        "settled_markets": segment.settled_markets,
        "realized_pnl_usd": segment.realized_pnl_usd,
        "realized_roi": segment.realized_roi,
        "settled_market_win_rate": segment.settled_market_win_rate,
        "profit_factor": segment.profit_factor,
        "max_realized_drawdown_usd": segment.max_realized_drawdown_usd,
        "top_5_profit_share": segment.top_5_profit_share,
        "open_positions": segment.open_positions,
        "unrealized_pnl_usd": segment.unrealized_pnl_usd,
        "open_loss_usd": segment.open_loss_usd,
        "open_loss_ratio": segment.open_loss_ratio,
        "marked_position_win_rate": segment.marked_position_win_rate,
        "combined_pnl_usd": segment.combined_pnl_usd,
        "losing_positions_older_than_7d": segment.losing_positions_older_than_7d,
        "high_price_buy_notional_share_80": segment.high_price_buy_notional_share_80,
        "suspected_market_making": segment.suspected_market_making,
    })
}

pub fn render_employee_stats_text(report: &EmployeeStatsReport, window_days: u64) -> String {
    let key = format!("{window_days}d");
    let Some(metrics) = report.windows.get(&key) else {
        return format!("report does not contain window {key}");
    };
    let total = &metrics.wallet_total;
    let primary = &metrics.primary_domain;
    let other = &metrics.other_domains_total;
    let now = now_secs();
    let mut lines = vec![
        format!(
            "员工深查：{} ({})",
            report.employee.display_name.as_deref().unwrap_or("未命名"),
            report.employee.wallet
        ),
        format!(
            "主领域：{} ({})",
            report.employee.primary_domain, report.employee.primary_domain_source
        ),
        format!(
            "报告生成：{}",
            format_timestamp_pair(report.freshness.report_generated_at)
        ),
        freshness_line("最新成交", report.freshness.latest_trade_at, now),
        freshness_line("最新结算", report.freshness.latest_closed_position_at, now),
        freshness_line("当前仓位检查", report.freshness.positions_observed_at, now),
        format!(
            "数据状态：{}{}",
            report.data_quality.report_status,
            if report.data_quality.history_truncated {
                "（历史截断）"
            } else {
                ""
            }
        ),
        String::new(),
        format!("最近 {window_days} 天："),
        format!(
            "- 成交额 ${:.2}，成交 {} 笔，合并出手 {} 次，活跃 {} 天",
            total.gross_trade_notional_usd, total.fill_count, total.action_count, total.active_days
        ),
        format!(
            "- 已结算 PnL ${:.2}，ROI {:.1}%，市场胜率 {:.1}% ({})",
            total.realized_pnl_usd,
            total.realized_roi * 100.0,
            total.settled_market_win_rate * 100.0,
            total.settled_markets
        ),
        format!(
            "- 当前浮动 PnL ${:.2}，浮亏 ${:.2}，综合 PnL ${:.2}",
            total.unrealized_pnl_usd, total.open_loss_usd, total.combined_pnl_usd
        ),
        format!(
            "- 含持仓估值胜率 {:.1}%（非最终胜率）",
            total.marked_position_win_rate * 100.0
        ),
        format!(
            "- 单笔金额：平均 ${:.2} / 中位 ${:.2} / P80 ${:.2} / P95 ${:.2}",
            total.avg_fill_notional_usd,
            total.median_fill_notional_usd,
            total.p80_fill_notional_usd,
            total.p95_fill_notional_usd
        ),
        String::new(),
        format!(
            "主领域 {}：胜率 {:.1}%，ROI {:.1}%，PnL ${:.2}，当前浮亏 ${:.2}",
            report.employee.primary_domain,
            primary.settled_market_win_rate * 100.0,
            primary.realized_roi * 100.0,
            primary.combined_pnl_usd,
            primary.open_loss_usd
        ),
        format!(
            "其他领域：胜率 {:.1}%，ROI {:.1}%，PnL ${:.2}，当前浮亏 ${:.2}",
            other.settled_market_win_rate * 100.0,
            other.realized_roi * 100.0,
            other.combined_pnl_usd,
            other.open_loss_usd
        ),
    ];
    if !metrics.specialties.is_empty() {
        lines.push(String::new());
        lines.push("专项拆分：".to_owned());
        for (topic, segment) in &metrics.specialties {
            lines.push(format!(
                "- {}：成交额 ${:.2}，出手 {} 次，已结算胜率 {:.1}%（{} 市场），ROI {:.1}%，综合 PnL ${:.2}",
                topic_display_name(topic),
                segment.gross_trade_notional_usd,
                segment.action_count,
                segment.settled_market_win_rate * 100.0,
                segment.settled_markets,
                segment.realized_roi * 100.0,
                segment.combined_pnl_usd
            ));
        }
    }
    lines.extend([
        String::new(),
        format!(
            "结论等级：{}；标记：{}",
            report.conclusion.summary_level,
            if report.conclusion.flags.is_empty() {
                "无".to_owned()
            } else {
                report.conclusion.flags.join(", ")
            }
        ),
    ]);
    for fact in &report.conclusion.facts {
        lines.push(format!("- {fact}"));
    }
    lines.join("\n")
}

fn write_report_files(
    cache_dir: &Path,
    dataset: &EmployeeStatsDataset,
    report: &EmployeeStatsReport,
) -> Result<(), EmployeeStatsError> {
    let wallet_dir = cache_dir.join(&dataset.employee.wallet);
    let snapshots_dir = wallet_dir.join("snapshots");
    fs::create_dir_all(&snapshots_dir)?;
    let report_json = serde_json::to_vec_pretty(report)?;
    atomic_write(&wallet_dir.join("latest.json"), &report_json)?;
    let markdown = render_employee_stats_markdown(report, report.window_days);
    atomic_write(&wallet_dir.join("latest.md"), markdown.as_bytes())?;
    atomic_write(
        &snapshots_dir.join(format!("{}.json", report.freshness.report_generated_at)),
        &report_json,
    )?;
    Ok(())
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), EmployeeStatsError> {
    let temporary = path.with_extension("tmp");
    fs::write(&temporary, bytes)?;
    fs::rename(temporary, path)?;
    Ok(())
}

pub fn render_employee_stats_markdown(report: &EmployeeStatsReport, window_days: u64) -> String {
    let text = render_employee_stats_text(report, window_days);
    format!("# 员工深查报告\n\n```text\n{text}\n```\n\n完整结构化指标见同目录 `latest.json`。\n")
}

fn topic_display_name(topic: &str) -> &str {
    match topic {
        "WORLD_CUP" => "世界杯 / WORLD_CUP",
        _ => topic,
    }
}

fn infer_primary_domain(dataset: &EmployeeStatsDataset) -> Option<String> {
    let mut scores: HashMap<String, (usize, usize)> = HashMap::new();
    for position in &dataset.closed_positions {
        if let Some(domain) = classify_without_primary(
            position.title.as_deref(),
            position.slug.as_deref(),
            position.event_slug.as_deref(),
        ) {
            scores.entry(domain).or_default().0 += 1;
        }
    }
    for trade in &dataset.trades {
        if let Some(domain) = classify_without_primary(
            trade.title.as_deref(),
            trade.slug.as_deref(),
            trade.event_slug.as_deref(),
        ) {
            scores.entry(domain).or_default().1 += 1;
        }
    }
    scores
        .into_iter()
        .max_by(|left, right| {
            left.1
                 .0
                .cmp(&right.1 .0)
                .then_with(|| left.1 .1.cmp(&right.1 .1))
                .then_with(|| right.0.cmp(&left.0))
        })
        .map(|(domain, _)| domain)
}

fn classify_trade(employee: &EmployeeIdentity, trade: &UserTrade) -> String {
    classify_market(
        employee,
        trade.title.as_deref(),
        trade.slug.as_deref(),
        trade.event_slug.as_deref(),
    )
}

fn classify_closed(employee: &EmployeeIdentity, position: &ClosedPosition) -> String {
    classify_market(
        employee,
        position.title.as_deref(),
        position.slug.as_deref(),
        position.event_slug.as_deref(),
    )
}

fn classify_current(employee: &EmployeeIdentity, position: &CurrentPosition) -> String {
    classify_market(
        employee,
        position.title.as_deref(),
        position.slug.as_deref(),
        position.event_slug.as_deref(),
    )
}

fn is_world_cup_trade(trade: &UserTrade) -> bool {
    is_world_cup_market(
        trade.title.as_deref(),
        trade.slug.as_deref(),
        trade.event_slug.as_deref(),
    )
}

fn is_world_cup_closed(position: &ClosedPosition) -> bool {
    is_world_cup_market(
        position.title.as_deref(),
        position.slug.as_deref(),
        position.event_slug.as_deref(),
    )
}

fn is_world_cup_current(position: &CurrentPosition) -> bool {
    is_world_cup_market(
        position.title.as_deref(),
        position.slug.as_deref(),
        position.event_slug.as_deref(),
    )
}

fn is_world_cup_market(title: Option<&str>, slug: Option<&str>, event_slug: Option<&str>) -> bool {
    let haystack = market_haystack(title, slug, event_slug);
    contains_keyword(&haystack, "fifwc")
        || contains_keyword(&haystack, "world cup")
        || haystack.contains("world-cup")
        || haystack.contains("世界杯")
}

fn classify_market(
    employee: &EmployeeIdentity,
    title: Option<&str>,
    slug: Option<&str>,
    event_slug: Option<&str>,
) -> String {
    let haystack = market_haystack(title, slug, event_slug);
    let primary = employee.primary_domain.to_uppercase();
    let primary_keywords = if employee.keywords.is_empty() {
        domain_keywords(&primary)
            .iter()
            .map(|value| (*value).to_owned())
            .collect::<Vec<_>>()
    } else {
        employee.keywords.clone()
    };
    if primary != "UNKNOWN"
        && primary_keywords
            .iter()
            .any(|keyword| contains_keyword(&haystack, keyword))
    {
        return primary;
    }
    classify_without_primary(title, slug, event_slug).unwrap_or_else(|| "UNKNOWN".to_owned())
}

fn classify_without_primary(
    title: Option<&str>,
    slug: Option<&str>,
    event_slug: Option<&str>,
) -> Option<String> {
    let haystack = market_haystack(title, slug, event_slug);
    let matches = known_domains()
        .iter()
        .filter(|domain| {
            domain_keywords(domain)
                .iter()
                .any(|keyword| contains_keyword(&haystack, keyword))
        })
        .map(|domain| (*domain).to_owned())
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [domain] => Some(domain.clone()),
        [] => None,
        _ => Some("AMBIGUOUS".to_owned()),
    }
}

fn known_domains() -> &'static [&'static str] {
    &[
        "WEATHER",
        "SPORTS",
        "CRYPTO",
        "FINANCE",
        "ECONOMICS",
        "TECH",
        "MENTIONS",
        "POLITICS",
        "CULTURE",
    ]
}

fn is_known_domain(domain: &str) -> bool {
    known_domains().contains(&domain)
}

fn domain_keywords(domain: &str) -> &'static [&'static str] {
    match domain {
        "WEATHER" => &[
            "weather",
            "temperature",
            "hurricane",
            "storm",
            "rain",
            "snow",
            "precipitation",
            "heatwave",
            "tornado",
        ],
        "SPORTS" => &[
            "nba",
            "nfl",
            "nhl",
            "mlb",
            "wnba",
            "soccer",
            "football",
            "basketball",
            "baseball",
            "hockey",
            "tennis",
            "ufc",
            "fifa",
            "fifwc",
            "world cup",
            "world-cup",
            "champions league",
            "premier league",
        ],
        "CRYPTO" => &[
            "bitcoin", "btc", "ethereum", "eth", "solana", "sol", "xrp", "dogecoin", "crypto",
            "token", "airdrop", "defi",
        ],
        "FINANCE" => &[
            "stock",
            "stocks",
            "s&p",
            "nasdaq",
            "dow jones",
            "earnings",
            "ipo",
            "tesla",
            "nvidia",
            "apple",
            "amazon",
            "market cap",
            "gold",
            "oil",
        ],
        "ECONOMICS" => &[
            "federal reserve",
            "fed",
            "interest rate",
            "rate cut",
            "rate hike",
            "cpi",
            "inflation",
            "gdp",
            "unemployment",
            "jobs report",
            "recession",
            "payroll",
        ],
        "TECH" => &[
            "openai",
            "anthropic",
            "google",
            "microsoft",
            "meta",
            "ai",
            "artificial intelligence",
            "model",
            "iphone",
            "spacex",
        ],
        "MENTIONS" => &[
            "say",
            "says",
            "said",
            "mention",
            "mentions",
            "mentioned",
            "speech",
            "tweet",
        ],
        "POLITICS" => &[
            "trump",
            "biden",
            "election",
            "president",
            "senate",
            "congress",
            "democrat",
            "republican",
        ],
        "CULTURE" => &[
            "movie",
            "album",
            "celebrity",
            "oscars",
            "grammy",
            "box office",
            "youtube",
        ],
        _ => &[],
    }
}

fn market_haystack(title: Option<&str>, slug: Option<&str>, event_slug: Option<&str>) -> String {
    format!(
        "{} {} {}",
        title.unwrap_or_default(),
        slug.unwrap_or_default(),
        event_slug.unwrap_or_default()
    )
    .to_lowercase()
}

fn contains_keyword(haystack: &str, keyword: &str) -> bool {
    let keyword = keyword.to_lowercase();
    haystack.match_indices(&keyword).any(|(start, _)| {
        let end = start + keyword.len();
        let left_ok = start == 0
            || haystack[..start]
                .chars()
                .next_back()
                .map(|ch| !ch.is_ascii_alphanumeric())
                .unwrap_or(true);
        let right_ok = end == haystack.len()
            || haystack[end..]
                .chars()
                .next()
                .map(|ch| !ch.is_ascii_alphanumeric())
                .unwrap_or(true);
        left_ok && right_ok
    })
}

fn validate_wallet(wallet: &str) -> Result<(), EmployeeStatsError> {
    let wallet = wallet.trim();
    if wallet.len() != 42
        || !wallet.starts_with("0x")
        || !wallet[2..].chars().all(|ch| ch.is_ascii_hexdigit())
    {
        return Err(EmployeeStatsError::Invalid(format!(
            "wallet must be a 42-character 0x address, got {wallet:?}"
        )));
    }
    Ok(())
}

fn is_active_position(position: &CurrentPosition) -> bool {
    !position.redeemable.unwrap_or(false)
        && !position.mergeable.unwrap_or(false)
        && position.size.unwrap_or(0.0) > 0.000_001
}

fn segment_has_activity(segment: &SegmentMetrics) -> bool {
    segment.fill_count > 0
        || segment.settled_positions > 0
        || segment.open_positions > 0
        || segment.redeemable_positions > 0
}

fn closed_position_pnl_usd(position: &ClosedPosition) -> f64 {
    let reported = position.realized_pnl.unwrap_or(0.0);
    if reported.abs() > SETTLEMENT_EPSILON {
        return reported;
    }
    imputed_resolved_pnl(
        position.avg_price,
        position.total_bought,
        position.cur_price,
    )
    .unwrap_or(reported)
}

fn current_position_settled_pnl_usd(position: &CurrentPosition) -> f64 {
    let reported = position.cash_pnl.or(position.realized_pnl).unwrap_or(0.0);
    if reported.abs() > SETTLEMENT_EPSILON {
        return reported;
    }
    imputed_resolved_pnl(
        position.avg_price,
        position.total_bought.or(position.size),
        position.cur_price,
    )
    .unwrap_or(reported)
}

fn closed_position_invested_usd(position: &ClosedPosition) -> f64 {
    position_cost(position.avg_price, position.total_bought, None)
}

fn current_position_invested_usd(position: &CurrentPosition) -> f64 {
    position_cost(
        position.avg_price,
        position.total_bought.or(position.size),
        position.initial_value,
    )
}

fn imputed_resolved_pnl(
    avg_price: Option<f64>,
    total_bought: Option<f64>,
    cur_price: Option<f64>,
) -> Option<f64> {
    let avg_price = finite_in_unit_range(avg_price)?;
    let cur_price = finite_in_unit_range(cur_price)?;
    let shares = positive_finite(total_bought)?;
    let resolved = cur_price <= SETTLEMENT_EPSILON || cur_price >= 1.0 - SETTLEMENT_EPSILON;
    resolved.then_some((cur_price - avg_price) * shares)
}

fn position_cost(
    avg_price: Option<f64>,
    total_bought: Option<f64>,
    initial_value: Option<f64>,
) -> f64 {
    if let Some(initial_value) = positive_finite(initial_value) {
        return initial_value;
    }
    match (
        finite_in_unit_range(avg_price),
        positive_finite(total_bought),
    ) {
        (Some(avg_price), Some(shares)) => avg_price * shares,
        (_, Some(value)) => value,
        _ => 0.0,
    }
}

fn positive_finite(value: Option<f64>) -> Option<f64> {
    value.filter(|value| value.is_finite() && *value > 0.0)
}

fn finite_in_unit_range(value: Option<f64>) -> Option<f64> {
    value.filter(|value| value.is_finite() && (0.0..=1.0).contains(value))
}

fn trade_notional(trade: &UserTrade) -> Option<f64> {
    Some(trade.price? * trade.size?)
}

fn timestamp_in_window(timestamp: Option<u64>, cutoff: u64, now: u64) -> bool {
    normalized_timestamp(timestamp)
        .map(|value| value >= cutoff && value <= now)
        .unwrap_or(false)
}

fn normalized_timestamp(timestamp: Option<u64>) -> Option<u64> {
    timestamp.map(|value| {
        if value > 10_000_000_000 {
            value / 1_000
        } else {
            value
        }
    })
}

fn end_date_timestamp(end_date: Option<&str>) -> Option<u64> {
    let date = end_date?.get(..10)?;
    let mut parts = date.split('-');
    let year = parts.next()?.parse::<i64>().ok()?;
    let month = parts.next()?.parse::<u32>().ok()?;
    let day = parts.next()?.parse::<u32>().ok()?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let days = days_from_civil(year, month, day);
    (days >= 0).then_some(days as u64 * SECONDS_PER_DAY)
}

fn days_from_civil(mut year: i64, month: u32, day: u32) -> i64 {
    year -= i64::from(month <= 2);
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let month_prime = month as i64 + if month > 2 { -3 } else { 9 };
    let day_of_year = (153 * month_prime + 2) / 5 + day as i64 - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

fn latest_trade_timestamp(trades: &[UserTrade]) -> Option<u64> {
    trades
        .iter()
        .filter_map(|trade| normalized_timestamp(trade.timestamp))
        .max()
}

fn latest_closed_timestamp(positions: &[ClosedPosition]) -> Option<u64> {
    positions
        .iter()
        .filter_map(|position| normalized_timestamp(position.timestamp))
        .max()
}

fn min_option(left: Option<u64>, right: Option<u64>) -> Option<u64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    }
}

fn min_option_many<const N: usize>(values: [Option<u64>; N]) -> Option<u64> {
    values.into_iter().flatten().min()
}

fn max_option_many<const N: usize>(values: [Option<u64>; N]) -> Option<u64> {
    values.into_iter().flatten().max()
}

fn top_profit_share(values: &[f64], top: usize) -> f64 {
    let mut profits = values
        .iter()
        .copied()
        .filter(|value| *value > 0.0)
        .collect::<Vec<_>>();
    profits.sort_by(|left, right| right.total_cmp(left));
    ratio(
        profits.iter().take(top).sum::<f64>(),
        profits.iter().sum::<f64>(),
    )
}

fn average(values: &[f64]) -> f64 {
    ratio(values.iter().sum::<f64>(), values.len() as f64)
}

fn percentile(mut values: Vec<f64>, percentile: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.sort_by(f64::total_cmp);
    let index = ((values.len() - 1) as f64 * percentile.clamp(0.0, 1.0)).round() as usize;
    values[index]
}

fn ratio(numerator: f64, denominator: f64) -> f64 {
    if denominator > 0.0 {
        numerator / denominator
    } else {
        0.0
    }
}

fn round2(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

fn round4(value: f64) -> f64 {
    (value * 10_000.0).round() / 10_000.0
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn freshness_line(label: &str, timestamp: Option<u64>, now: u64) -> String {
    timestamp
        .map(|timestamp| {
            format!(
                "{label}：{}（{}前）",
                format_timestamp_pair(timestamp),
                human_age(now.saturating_sub(timestamp))
            )
        })
        .unwrap_or_else(|| format!("{label}：未知"))
}

fn human_age(seconds: u64) -> String {
    if seconds < 60 {
        format!("{seconds}秒")
    } else if seconds < 3_600 {
        format!("{}分{}秒", seconds / 60, seconds % 60)
    } else if seconds < SECONDS_PER_DAY {
        format!("{}小时{}分", seconds / 3_600, (seconds % 3_600) / 60)
    } else {
        format!(
            "{}天{}小时",
            seconds / SECONDS_PER_DAY,
            (seconds % SECONDS_PER_DAY) / 3_600
        )
    }
}

fn format_timestamp_pair(timestamp: u64) -> String {
    format!(
        "{} CST / {}Z",
        format_unix(timestamp.saturating_add(8 * 3_600)),
        format_unix(timestamp)
    )
}

fn format_unix(timestamp: u64) -> String {
    let days = (timestamp / SECONDS_PER_DAY) as i64;
    let seconds = timestamp % SECONDS_PER_DAY;
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02}",
        seconds / 3_600,
        (seconds % 3_600) / 60,
        seconds % 60
    )
}

fn civil_from_days(days_since_epoch: i64) -> (i64, u32, u32) {
    let z = days_since_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += if month <= 2 { 1 } else { 0 };
    (year, month as u32, day as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wallet() -> String {
        "0x1111111111111111111111111111111111111111".to_owned()
    }

    fn employee() -> EmployeeIdentity {
        EmployeeIdentity {
            wallet: wallet(),
            display_name: Some("tester".to_owned()),
            username: None,
            primary_domain: "WEATHER".to_owned(),
            keywords: vec!["weather".to_owned(), "temperature".to_owned()],
            primary_domain_source: "provided".to_owned(),
        }
    }

    fn sports_employee() -> EmployeeIdentity {
        EmployeeIdentity {
            wallet: wallet(),
            display_name: Some("sports-tester".to_owned()),
            username: None,
            primary_domain: "SPORTS".to_owned(),
            keywords: Vec::new(),
            primary_domain_source: "provided".to_owned(),
        }
    }

    fn trade(timestamp: u64, side: &str, price: f64, size: f64, title: &str) -> UserTrade {
        UserTrade {
            proxy_wallet: wallet(),
            side: side.to_owned(),
            asset: "asset-1".to_owned(),
            condition_id: "condition-1".to_owned(),
            size: Some(size),
            price: Some(price),
            timestamp: Some(timestamp),
            title: Some(title.to_owned()),
            slug: None,
            event_slug: None,
            outcome: Some("Yes".to_owned()),
            outcome_index: Some(0),
            name: None,
            pseudonym: None,
            transaction_hash: Some(format!("tx-{timestamp}-{side}-{size}")),
        }
    }

    fn closed(timestamp: u64, pnl: f64, title: &str) -> ClosedPosition {
        ClosedPosition {
            proxy_wallet: wallet(),
            asset: Some(format!("asset-{timestamp}")),
            condition_id: Some(format!("condition-{timestamp}")),
            avg_price: Some(0.5),
            total_bought: Some(100.0),
            realized_pnl: Some(pnl),
            cur_price: Some(if pnl > 0.0 { 1.0 } else { 0.0 }),
            timestamp: Some(timestamp),
            title: Some(title.to_owned()),
            slug: None,
            event_slug: None,
            outcome: Some("Yes".to_owned()),
            outcome_index: Some(0),
            opposite_outcome: None,
            opposite_asset: None,
            end_date: None,
        }
    }

    fn current(pnl: f64, title: &str) -> CurrentPosition {
        CurrentPosition {
            proxy_wallet: wallet(),
            asset: Some(format!("open-{pnl}")),
            condition_id: Some(format!("open-condition-{pnl}")),
            size: Some(100.0),
            avg_price: Some(0.5),
            initial_value: Some(50.0),
            current_value: Some((50.0 + pnl).max(0.0)),
            cash_pnl: Some(pnl),
            percent_pnl: Some(pnl / 50.0),
            total_bought: Some(50.0),
            realized_pnl: None,
            cur_price: Some((0.5 + pnl / 100.0).clamp(0.0, 1.0)),
            redeemable: Some(false),
            mergeable: Some(false),
            title: Some(title.to_owned()),
            slug: None,
            event_slug: None,
            outcome: Some("Yes".to_owned()),
            outcome_index: Some(0),
            opposite_outcome: None,
            end_date: None,
        }
    }

    #[test]
    fn split_fills_are_grouped_into_actions() {
        let trades = vec![
            trade(1_800_000_000, "BUY", 0.4, 10.0, "weather market"),
            trade(1_800_000_060, "BUY", 0.4, 20.0, "weather market"),
            trade(1_800_000_300, "BUY", 0.4, 30.0, "weather market"),
        ];
        let refs = trades.iter().collect::<Vec<_>>();
        let actions = action_notionals(&refs, 120);
        assert_eq!(actions.len(), 2);
        assert!((actions.iter().sum::<f64>() - 24.0).abs() < 0.001);
    }

    #[test]
    fn open_losses_reduce_marked_win_rate() {
        let now = 1_800_000_000;
        let dataset = EmployeeStatsDataset {
            schema_version: 1,
            employee: employee(),
            collected_at: now,
            trades: vec![],
            closed_positions: vec![
                closed(now - 100, 20.0, "weather temperature"),
                closed(now - 200, 10.0, "weather temperature"),
            ],
            current_positions: vec![
                current(-10.0, "weather temperature"),
                current(-15.0, "weather temperature"),
            ],
            sync: SyncMetadata::default(),
            quality: CachedDataQuality::default(),
        };
        let report = build_employee_stats_report(&dataset, &EmployeeStatsConfig::default(), now);
        let total = &report.windows["14d"].wallet_total;
        assert_eq!(total.settled_position_win_rate, 1.0);
        assert_eq!(total.marked_position_win_rate, 0.5);
        assert_eq!(total.combined_pnl_usd, 5.0);
    }

    #[test]
    fn primary_and_other_domains_are_separated() {
        let now = 1_800_000_000;
        let dataset = EmployeeStatsDataset {
            schema_version: 1,
            employee: employee(),
            collected_at: now,
            trades: vec![
                trade(now - 100, "BUY", 0.5, 20.0, "weather temperature"),
                trade(now - 200, "BUY", 0.5, 40.0, "NBA basketball"),
            ],
            closed_positions: vec![
                closed(now - 100, 30.0, "weather temperature"),
                closed(now - 200, -20.0, "NBA basketball"),
            ],
            current_positions: vec![],
            sync: SyncMetadata::default(),
            quality: CachedDataQuality::default(),
        };
        let report = build_employee_stats_report(&dataset, &EmployeeStatsConfig::default(), now);
        let breakdown = &report.windows["14d"];
        assert_eq!(breakdown.primary_domain.settled_market_win_rate, 1.0);
        assert_eq!(breakdown.other_domains_total.settled_market_win_rate, 0.0);
        assert_eq!(breakdown.other_domains["SPORTS"].realized_pnl_usd, -20.0);
        assert_eq!(breakdown.wallet_total.realized_pnl_usd, 10.0);
    }

    #[test]
    fn fifwc_markets_are_sports_and_world_cup_specialty() {
        let now = 1_800_000_000;
        let mut world_cup_trade = trade(
            now - 100,
            "BUY",
            0.67,
            100.0,
            "Will France win on 2026-06-16?",
        );
        world_cup_trade.slug = Some("fifwc-fra-sen-2026-06-16-fra".to_owned());
        world_cup_trade.event_slug = Some("fifwc-fra-sen-2026-06-16".to_owned());
        let mut world_cup_position = closed(now - 100, 0.0, "Will France win on 2026-06-16?");
        world_cup_position.slug = world_cup_trade.slug.clone();
        world_cup_position.event_slug = world_cup_trade.event_slug.clone();
        world_cup_position.avg_price = Some(0.67);
        world_cup_position.total_bought = Some(100.0);
        world_cup_position.cur_price = Some(1.0);
        world_cup_position.realized_pnl = Some(0.0);

        let dataset = EmployeeStatsDataset {
            schema_version: 1,
            employee: sports_employee(),
            collected_at: now,
            trades: vec![world_cup_trade],
            closed_positions: vec![world_cup_position],
            current_positions: vec![],
            sync: SyncMetadata::default(),
            quality: CachedDataQuality::default(),
        };
        let report = build_employee_stats_report(&dataset, &EmployeeStatsConfig::default(), now);
        let breakdown = &report.windows["14d"];
        assert_eq!(breakdown.primary_domain.settled_market_win_rate, 1.0);
        assert_eq!(
            breakdown.specialties["WORLD_CUP"].settled_market_win_rate,
            1.0
        );
        assert_eq!(breakdown.specialties["WORLD_CUP"].realized_pnl_usd, 33.0);
        assert_eq!(breakdown.specialties["WORLD_CUP"].invested_usd, 67.0);
    }

    #[test]
    fn zero_reported_pnl_uses_resolved_price_for_wins_and_losses() {
        let now = 1_800_000_000;
        let mut winner = closed(now - 100, 0.0, "world cup winner");
        winner.avg_price = Some(0.40);
        winner.total_bought = Some(100.0);
        winner.cur_price = Some(1.0);
        winner.realized_pnl = Some(0.0);
        let mut loser = closed(now - 200, 0.0, "world cup loser");
        loser.avg_price = Some(0.60);
        loser.total_bought = Some(100.0);
        loser.cur_price = Some(0.0);
        loser.realized_pnl = Some(0.0);

        let dataset = EmployeeStatsDataset {
            schema_version: 1,
            employee: sports_employee(),
            collected_at: now,
            trades: vec![],
            closed_positions: vec![winner, loser],
            current_positions: vec![],
            sync: SyncMetadata::default(),
            quality: CachedDataQuality::default(),
        };
        let report = build_employee_stats_report(&dataset, &EmployeeStatsConfig::default(), now);
        let total = &report.windows["14d"].wallet_total;
        assert_eq!(total.settled_positions, 2);
        assert_eq!(total.settled_position_win_rate, 0.5);
        assert_eq!(total.realized_pnl_usd, 0.0);
        assert_eq!(total.gross_profit_usd, 60.0);
        assert_eq!(total.gross_loss_usd, 60.0);
        assert_eq!(total.invested_usd, 100.0);
    }

    #[test]
    fn recent_redeemable_losses_are_included_in_settled_results() {
        let now = 1_800_000_000;
        let mut redeemable_loss = current(-50.0, "weather temperature");
        redeemable_loss.redeemable = Some(true);
        redeemable_loss.end_date = Some(format_unix(now)[..10].to_owned());
        let dataset = EmployeeStatsDataset {
            schema_version: 1,
            employee: employee(),
            collected_at: now,
            trades: vec![],
            closed_positions: vec![closed(now - 100, 20.0, "weather temperature")],
            current_positions: vec![redeemable_loss],
            sync: SyncMetadata::default(),
            quality: CachedDataQuality::default(),
        };
        let report = build_employee_stats_report(&dataset, &EmployeeStatsConfig::default(), now);
        let total = &report.windows["14d"].wallet_total;
        assert_eq!(total.settled_positions, 2);
        assert_eq!(total.realized_pnl_usd, -30.0);
        assert_eq!(total.redeemable_pnl_usd, -50.0);
        assert_eq!(total.settled_position_win_rate, 0.5);
    }

    #[test]
    fn duplicate_trades_from_two_sources_count_once() {
        let mut stored = vec![trade(
            1_800_000_000,
            "BUY",
            0.5,
            20.0,
            "weather temperature",
        )];
        let incoming = stored.clone();
        let (duplicates, invalid) = merge_trades(&mut stored, incoming, 1_700_000_000);
        assert_eq!(duplicates, 1);
        assert_eq!(invalid, 0);
        assert_eq!(stored.len(), 1);
    }

    #[test]
    fn timestamp_formatter_handles_epoch_and_shanghai_offset() {
        assert_eq!(format_unix(0), "1970-01-01 00:00:00");
        assert_eq!(
            format_timestamp_pair(0),
            "1970-01-01 08:00:00 CST / 1970-01-01 00:00:00Z"
        );
    }

    #[test]
    fn sqlite_cache_round_trips_report_and_alias_without_network() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let cache_dir = std::env::temp_dir().join(format!("employee-stats-test-{unique}"));
        let config = EmployeeStatsConfig {
            cache_dir: cache_dir.clone(),
            ..EmployeeStatsConfig::default()
        };
        let now = 1_800_000_000;
        let dataset = EmployeeStatsDataset {
            schema_version: 1,
            employee: employee(),
            collected_at: now,
            trades: vec![trade(now - 100, "BUY", 0.5, 20.0, "weather temperature")],
            closed_positions: vec![closed(now - 100, 20.0, "weather temperature")],
            current_positions: vec![],
            sync: SyncMetadata {
                activity: ComponentFreshness {
                    last_success_at: Some(now),
                    ..ComponentFreshness::default()
                },
                closed_positions: ComponentFreshness {
                    last_success_at: Some(now),
                    ..ComponentFreshness::default()
                },
                positions: ComponentFreshness {
                    last_success_at: Some(now),
                    ..ComponentFreshness::default()
                },
                ..SyncMetadata::default()
            },
            quality: CachedDataQuality::default(),
        };
        let report = build_employee_stats_report(&dataset, &config, now);
        let mut store = EmployeeStatsStore::open(&config).unwrap();
        store.save(&dataset, &report).unwrap();

        let loaded = store.load_report("tester").unwrap();
        assert_eq!(loaded.employee.wallet, wallet());
        assert_eq!(loaded.windows["14d"].wallet_total.realized_pnl_usd, 20.0);
        let compact = serde_json::to_string(&compact_report_value(&loaded, 14)).unwrap();
        assert!(compact.len() < 10_000);
        assert!(!compact.contains("transaction_hash"));
        assert!(cache_dir.join(wallet()).join("latest.json").exists());
        assert!(cache_dir.join(wallet()).join("latest.md").exists());

        drop(store);
        fs::remove_dir_all(cache_dir).unwrap();
    }
}
