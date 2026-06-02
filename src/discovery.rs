use crate::polymarket::{ClosedPosition, CurrentPosition, LeaderboardEntry, PolymarketDataClient};
use serde::Serialize;
use std::{
    collections::{BTreeMap, HashSet},
    thread::sleep,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const SECONDS_PER_DAY: u64 = 86_400;
const SECONDS_PER_MONTH_BUCKET: u64 = 30 * SECONDS_PER_DAY;
const DEFAULT_RECENT_WINDOW_DAYS: u64 = 30;

#[derive(Debug, Clone)]
pub struct DiscoveryRunConfig {
    pub categories: Vec<String>,
    pub time_period: String,
    pub candidate_limit: usize,
    pub closed_position_pages: usize,
    pub closed_position_page_size: usize,
    pub top_per_category: usize,
    pub pause_between_wallets_ms: u64,
    pub scoring: SmartMoneyConfig,
}

impl Default for DiscoveryRunConfig {
    fn default() -> Self {
        Self {
            categories: default_categories(),
            time_period: "MONTH".to_owned(),
            candidate_limit: 5,
            closed_position_pages: 2,
            closed_position_page_size: 50,
            top_per_category: 1,
            pause_between_wallets_ms: 120,
            scoring: SmartMoneyConfig::default(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct SmartMoneyConfig {
    pub min_closed_positions: usize,
    pub min_realized_pnl_usd: f64,
    pub min_realized_roi: f64,
    pub max_drawdown_ratio: f64,
    pub max_inactive_days: u64,
    pub max_recent_loss_streak: usize,
    pub recent_window_days: u64,
    pub min_recent_closed_positions: usize,
    pub min_recent_pnl_usd: f64,
    pub min_recent_roi: f64,
    pub min_recent_win_position_ratio: f64,
    pub max_current_loss_usd: f64,
    pub max_current_loss_ratio: f64,
}

impl Default for SmartMoneyConfig {
    fn default() -> Self {
        Self {
            min_closed_positions: 8,
            min_realized_pnl_usd: 100.0,
            min_realized_roi: 0.05,
            max_drawdown_ratio: 0.35,
            max_inactive_days: 60,
            max_recent_loss_streak: 3,
            recent_window_days: DEFAULT_RECENT_WINDOW_DAYS,
            min_recent_closed_positions: 3,
            min_recent_pnl_usd: 0.0,
            min_recent_roi: 0.0,
            min_recent_win_position_ratio: 0.45,
            max_current_loss_usd: 50_000.0,
            max_current_loss_ratio: 0.20,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SmartMoneyRoster {
    pub generated_at_secs: u64,
    pub categories: Vec<CategoryRoster>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct EmployeeScan {
    pub generated_at_secs: u64,
    pub employees: Vec<WalletEvaluation>,
    pub scanned_wallets: usize,
    pub eligible_wallets: usize,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CategoryRoster {
    pub category: String,
    pub picks: Vec<WalletEvaluation>,
    pub scanned_wallets: usize,
    pub eligible_wallets: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct WalletEvaluation {
    pub category: String,
    pub wallet: String,
    pub user_name: Option<String>,
    pub source_rank: Option<String>,
    pub source_pnl_usd: Option<f64>,
    pub source_volume_usd: Option<f64>,
    pub score: f64,
    pub eligible: bool,
    pub metrics: SimpleWalletMetrics,
    pub flags: Vec<EmployeeFlag>,
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SimpleWalletMetrics {
    pub closed_positions: usize,
    pub realized_pnl_usd: f64,
    pub invested_usd: f64,
    pub realized_roi: f64,
    pub recent_window_days: u64,
    pub recent_closed_positions: usize,
    pub recent_pnl_usd: f64,
    pub recent_invested_usd: f64,
    pub recent_roi: f64,
    pub recent_win_position_ratio: f64,
    pub current_positions: usize,
    pub current_cash_pnl_usd: f64,
    pub current_loss_usd: f64,
    pub current_initial_value_usd: f64,
    pub current_loss_ratio: f64,
    pub worst_current_position_pnl_usd: f64,
    pub max_drawdown_usd: f64,
    pub max_drawdown_ratio: f64,
    pub win_position_ratio: f64,
    pub positive_month_ratio: f64,
    pub recent_loss_streak: usize,
    pub last_activity_days: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum EmployeeFlag {
    InsufficientSample,
    InsufficientRecentSample,
    LowRealizedPnl,
    LowRealizedRoi,
    RecentLoss,
    LowRecentRoi,
    LowRecentWinRate,
    LargeCurrentLoss,
    HighCurrentLossRatio,
    HighDrawdown,
    Inactive,
    ColdStreak,
}

pub fn default_categories() -> Vec<String> {
    [
        "POLITICS",
        "SPORTS",
        "CRYPTO",
        "CULTURE",
        "MENTIONS",
        "WEATHER",
        "ECONOMICS",
        "TECH",
        "FINANCE",
    ]
    .iter()
    .map(|category| (*category).to_owned())
    .collect()
}

pub fn discover_smart_money(
    client: &PolymarketDataClient,
    config: &DiscoveryRunConfig,
) -> SmartMoneyRoster {
    let generated_at_secs = now_secs();
    let mut categories = Vec::new();
    let mut warnings = Vec::new();
    let mut selected_wallets = HashSet::new();

    for category in &config.categories {
        let leaderboard = match client.leaderboard(
            category,
            &config.time_period,
            "PNL",
            config.candidate_limit,
            0,
        ) {
            Ok(entries) => entries,
            Err(error) => {
                warnings.push(format!(
                    "failed to load leaderboard for {category}: {error}"
                ));
                continue;
            }
        };

        let mut evaluations = Vec::new();

        for entry in leaderboard {
            let positions = match load_closed_positions(client, &entry, config) {
                Ok(positions) => positions,
                Err(error) => {
                    warnings.push(format!(
                        "failed to load closed positions for {} in {category}: {error}",
                        entry.proxy_wallet
                    ));
                    Vec::new()
                }
            };
            let current_positions = match client.positions(&entry.proxy_wallet, 50, 0) {
                Ok(positions) => positions,
                Err(error) => {
                    warnings.push(format!(
                        "failed to load current positions for {} in {category}: {error}",
                        entry.proxy_wallet
                    ));
                    Vec::new()
                }
            };

            let evaluation = evaluate_wallet(
                category,
                &entry,
                &positions,
                &current_positions,
                &config.scoring,
                generated_at_secs,
            );
            evaluations.push(evaluation);

            if config.pause_between_wallets_ms > 0 {
                sleep(Duration::from_millis(config.pause_between_wallets_ms));
            }
        }

        let scanned_wallets = evaluations.len();
        let eligible_wallets = evaluations.iter().filter(|eval| eval.eligible).count();
        let picks = select_category_picks(&mut evaluations, config, &mut selected_wallets);

        categories.push(CategoryRoster {
            category: category.clone(),
            picks,
            scanned_wallets,
            eligible_wallets,
        });
    }

    SmartMoneyRoster {
        generated_at_secs,
        categories,
        warnings,
    }
}

pub fn scan_smart_money_employees(
    client: &PolymarketDataClient,
    config: &DiscoveryRunConfig,
    top_n: usize,
) -> EmployeeScan {
    let generated_at_secs = now_secs();
    let mut warnings = Vec::new();
    let mut evaluations = Vec::new();
    let mut seen_wallets = HashSet::new();
    let mut scanned_wallets = 0;

    for category in &config.categories {
        let leaderboard = match client.leaderboard(
            category,
            &config.time_period,
            "PNL",
            config.candidate_limit,
            0,
        ) {
            Ok(entries) => entries,
            Err(error) => {
                warnings.push(format!(
                    "failed to load leaderboard for {category}: {error}"
                ));
                continue;
            }
        };

        for entry in leaderboard {
            scanned_wallets += 1;

            if seen_wallets.contains(&entry.proxy_wallet) {
                continue;
            }

            let positions = match load_closed_positions(client, &entry, config) {
                Ok(positions) => positions,
                Err(error) => {
                    warnings.push(format!(
                        "failed to load closed positions for {} in {category}: {error}",
                        entry.proxy_wallet
                    ));
                    Vec::new()
                }
            };
            let current_positions = match client.positions(&entry.proxy_wallet, 50, 0) {
                Ok(positions) => positions,
                Err(error) => {
                    warnings.push(format!(
                        "failed to load current positions for {} in {category}: {error}",
                        entry.proxy_wallet
                    ));
                    Vec::new()
                }
            };

            let evaluation = evaluate_wallet(
                category,
                &entry,
                &positions,
                &current_positions,
                &config.scoring,
                generated_at_secs,
            );

            seen_wallets.insert(entry.proxy_wallet.clone());
            evaluations.push(evaluation);

            if config.pause_between_wallets_ms > 0 {
                sleep(Duration::from_millis(config.pause_between_wallets_ms));
            }
        }
    }

    evaluations.sort_by(|left, right| {
        right
            .eligible
            .cmp(&left.eligible)
            .then_with(|| right.score.total_cmp(&left.score))
            .then_with(|| {
                right
                    .metrics
                    .recent_pnl_usd
                    .total_cmp(&left.metrics.recent_pnl_usd)
            })
    });

    let eligible_wallets = evaluations.iter().filter(|eval| eval.eligible).count();
    let employees = evaluations
        .into_iter()
        .filter(|eval| eval.eligible)
        .take(top_n)
        .collect::<Vec<_>>();

    EmployeeScan {
        generated_at_secs,
        employees,
        scanned_wallets,
        eligible_wallets,
        warnings,
    }
}

fn load_closed_positions(
    client: &PolymarketDataClient,
    entry: &LeaderboardEntry,
    config: &DiscoveryRunConfig,
) -> Result<Vec<ClosedPosition>, crate::polymarket::PolymarketError> {
    let mut positions = Vec::new();

    for page in 0..config.closed_position_pages {
        let offset = page * config.closed_position_page_size;
        let page_positions = client.closed_positions(
            &entry.proxy_wallet,
            config.closed_position_page_size,
            offset,
        )?;

        let page_len = page_positions.len();
        positions.extend(page_positions);

        if page_len < config.closed_position_page_size {
            break;
        }
    }

    Ok(positions)
}

pub fn evaluate_wallet(
    category: &str,
    entry: &LeaderboardEntry,
    positions: &[ClosedPosition],
    current_positions: &[CurrentPosition],
    config: &SmartMoneyConfig,
    now_secs: u64,
) -> WalletEvaluation {
    let metrics = derive_metrics(
        positions,
        current_positions,
        now_secs,
        config.recent_window_days,
    );
    let flags = collect_flags(&metrics, config);
    let score = score_metrics(&metrics, config);
    let eligible = flags.is_empty();
    let reasons = build_reasons(&metrics, &flags);

    WalletEvaluation {
        category: category.to_owned(),
        wallet: entry.proxy_wallet.clone(),
        user_name: entry.user_name.clone(),
        source_rank: entry.rank.clone(),
        source_pnl_usd: entry.pnl,
        source_volume_usd: entry.vol,
        score,
        eligible,
        metrics,
        flags,
        reasons,
    }
}

pub fn derive_metrics(
    positions: &[ClosedPosition],
    current_positions: &[CurrentPosition],
    now_secs: u64,
    recent_window_days: u64,
) -> SimpleWalletMetrics {
    let closed_positions = positions.len();
    let realized_pnl_usd = positions
        .iter()
        .map(|position| position.realized_pnl.unwrap_or(0.0))
        .sum::<f64>();
    let invested_usd = positions
        .iter()
        .map(|position| position.total_bought.unwrap_or(0.0).max(0.0))
        .sum::<f64>();
    let realized_roi = if invested_usd > 0.0 {
        realized_pnl_usd / invested_usd
    } else {
        0.0
    };
    let recent_positions = positions
        .iter()
        .filter(|position| {
            normalized_timestamp_secs(position.timestamp)
                .map(|timestamp| {
                    now_secs.saturating_sub(timestamp)
                        <= recent_window_days.saturating_mul(SECONDS_PER_DAY)
                })
                .unwrap_or(false)
        })
        .collect::<Vec<_>>();
    let recent_closed_positions = recent_positions.len();
    let recent_pnl_usd = recent_positions
        .iter()
        .map(|position| position.realized_pnl.unwrap_or(0.0))
        .sum::<f64>();
    let recent_invested_usd = recent_positions
        .iter()
        .map(|position| position.total_bought.unwrap_or(0.0).max(0.0))
        .sum::<f64>();
    let recent_roi = if recent_invested_usd > 0.0 {
        recent_pnl_usd / recent_invested_usd
    } else {
        0.0
    };
    let recent_win_position_ratio = if recent_closed_positions > 0 {
        recent_positions
            .iter()
            .filter(|position| position.realized_pnl.unwrap_or(0.0) > 0.0)
            .count() as f64
            / recent_closed_positions as f64
    } else {
        0.0
    };
    let current_positions_count = current_positions.len();
    let current_cash_pnl_usd = current_positions
        .iter()
        .map(|position| position.cash_pnl.unwrap_or(0.0))
        .sum::<f64>();
    let current_loss_usd = current_positions
        .iter()
        .map(|position| position.cash_pnl.unwrap_or(0.0).min(0.0).abs())
        .sum::<f64>();
    let current_initial_value_usd = current_positions
        .iter()
        .map(|position| position.initial_value.unwrap_or(0.0).max(0.0))
        .sum::<f64>();
    let current_loss_ratio = if current_initial_value_usd > 0.0 {
        current_loss_usd / current_initial_value_usd
    } else {
        0.0
    };
    let worst_current_position_pnl_usd = current_positions
        .iter()
        .filter_map(|position| position.cash_pnl)
        .min_by(|left, right| left.total_cmp(right))
        .unwrap_or(0.0);

    let win_position_ratio = if closed_positions > 0 {
        positions
            .iter()
            .filter(|position| position.realized_pnl.unwrap_or(0.0) > 0.0)
            .count() as f64
            / closed_positions as f64
    } else {
        0.0
    };

    let mut ordered = positions.to_vec();
    ordered.sort_by_key(|position| normalized_timestamp_secs(position.timestamp).unwrap_or(0));

    let mut equity = 0.0;
    let mut peak = 0.0;
    let mut max_drawdown_usd = 0.0;

    for position in &ordered {
        equity += position.realized_pnl.unwrap_or(0.0);
        if equity > peak {
            peak = equity;
        }
        let drawdown = peak - equity;
        if drawdown > max_drawdown_usd {
            max_drawdown_usd = drawdown;
        }
    }

    let max_drawdown_ratio = if invested_usd > 0.0 {
        max_drawdown_usd / invested_usd
    } else if max_drawdown_usd > 0.0 {
        1.0
    } else {
        0.0
    };

    let positive_month_ratio = positive_month_ratio(positions);
    let recent_loss_streak = recent_loss_streak(positions);
    let last_activity_days = positions
        .iter()
        .filter_map(|position| normalized_timestamp_secs(position.timestamp))
        .max()
        .map(|last_seen| now_secs.saturating_sub(last_seen) / SECONDS_PER_DAY);

    SimpleWalletMetrics {
        closed_positions,
        realized_pnl_usd: round2(realized_pnl_usd),
        invested_usd: round2(invested_usd),
        realized_roi: round4(realized_roi),
        recent_window_days,
        recent_closed_positions,
        recent_pnl_usd: round2(recent_pnl_usd),
        recent_invested_usd: round2(recent_invested_usd),
        recent_roi: round4(recent_roi),
        recent_win_position_ratio: round4(recent_win_position_ratio),
        current_positions: current_positions_count,
        current_cash_pnl_usd: round2(current_cash_pnl_usd),
        current_loss_usd: round2(current_loss_usd),
        current_initial_value_usd: round2(current_initial_value_usd),
        current_loss_ratio: round4(current_loss_ratio),
        worst_current_position_pnl_usd: round2(worst_current_position_pnl_usd),
        max_drawdown_usd: round2(max_drawdown_usd),
        max_drawdown_ratio: round4(max_drawdown_ratio),
        win_position_ratio: round4(win_position_ratio),
        positive_month_ratio: round4(positive_month_ratio),
        recent_loss_streak,
        last_activity_days,
    }
}

fn positive_month_ratio(positions: &[ClosedPosition]) -> f64 {
    let mut months: BTreeMap<u64, f64> = BTreeMap::new();

    for position in positions {
        let Some(timestamp) = normalized_timestamp_secs(position.timestamp) else {
            continue;
        };
        let month = timestamp / SECONDS_PER_MONTH_BUCKET;
        *months.entry(month).or_default() += position.realized_pnl.unwrap_or(0.0);
    }

    if months.is_empty() {
        return 0.0;
    }

    let positive_months = months.values().filter(|pnl| **pnl > 0.0).count();
    positive_months as f64 / months.len() as f64
}

fn recent_loss_streak(positions: &[ClosedPosition]) -> usize {
    let mut newest_first = positions.to_vec();
    newest_first.sort_by_key(|position| {
        std::cmp::Reverse(normalized_timestamp_secs(position.timestamp).unwrap_or(0))
    });

    newest_first
        .iter()
        .take_while(|position| position.realized_pnl.unwrap_or(0.0) < 0.0)
        .count()
}

fn collect_flags(metrics: &SimpleWalletMetrics, config: &SmartMoneyConfig) -> Vec<EmployeeFlag> {
    let mut flags = Vec::new();

    if metrics.closed_positions < config.min_closed_positions {
        flags.push(EmployeeFlag::InsufficientSample);
    }

    if metrics.recent_closed_positions < config.min_recent_closed_positions {
        flags.push(EmployeeFlag::InsufficientRecentSample);
    }

    if metrics.realized_pnl_usd < config.min_realized_pnl_usd {
        flags.push(EmployeeFlag::LowRealizedPnl);
    }

    if metrics.realized_roi < config.min_realized_roi {
        flags.push(EmployeeFlag::LowRealizedRoi);
    }

    if metrics.recent_pnl_usd < config.min_recent_pnl_usd {
        flags.push(EmployeeFlag::RecentLoss);
    }

    if metrics.recent_roi < config.min_recent_roi {
        flags.push(EmployeeFlag::LowRecentRoi);
    }

    if metrics.recent_closed_positions >= config.min_recent_closed_positions
        && metrics.recent_win_position_ratio < config.min_recent_win_position_ratio
    {
        flags.push(EmployeeFlag::LowRecentWinRate);
    }

    if metrics.current_loss_usd > config.max_current_loss_usd {
        flags.push(EmployeeFlag::LargeCurrentLoss);
    }

    if metrics.current_loss_ratio > config.max_current_loss_ratio {
        flags.push(EmployeeFlag::HighCurrentLossRatio);
    }

    if metrics.max_drawdown_ratio > config.max_drawdown_ratio {
        flags.push(EmployeeFlag::HighDrawdown);
    }

    if metrics
        .last_activity_days
        .map(|days| days > config.max_inactive_days)
        .unwrap_or(true)
    {
        flags.push(EmployeeFlag::Inactive);
    }

    if metrics.recent_loss_streak >= config.max_recent_loss_streak {
        flags.push(EmployeeFlag::ColdStreak);
    }

    flags
}

fn score_metrics(metrics: &SimpleWalletMetrics, config: &SmartMoneyConfig) -> f64 {
    let pnl_score = clamp01(metrics.realized_pnl_usd / 1_000.0);
    let roi_score = clamp01((metrics.realized_roi + 0.05) / 0.55);
    let profit_score =
        (pnl_score * 0.35) + (roi_score * 0.40) + (metrics.win_position_ratio * 0.25);
    let recent_pnl_score = clamp01((metrics.recent_pnl_usd + 250.0) / 2_250.0);
    let recent_roi_score = clamp01((metrics.recent_roi + 0.05) / 0.35);
    let recent_profit_score = (recent_pnl_score * 0.25)
        + (recent_roi_score * 0.45)
        + (metrics.recent_win_position_ratio * 0.30);

    let drawdown_score = 1.0 - clamp01(metrics.max_drawdown_ratio / config.max_drawdown_ratio);
    let sample_score =
        clamp01(metrics.closed_positions as f64 / (config.min_closed_positions * 3) as f64);
    let activity_score = metrics
        .last_activity_days
        .map(|days| 1.0 - clamp01(days as f64 / config.max_inactive_days as f64))
        .unwrap_or(0.0);
    let streak_penalty = 1.0 - (metrics.recent_loss_streak.min(4) as f64 * 0.07);
    let current_loss_penalty = 1.0
        - (clamp01(metrics.current_loss_usd / config.max_current_loss_usd) * 0.20)
        - (clamp01(metrics.current_loss_ratio / config.max_current_loss_ratio) * 0.20);

    round4(
        streak_penalty.max(0.0)
            * current_loss_penalty.max(0.55)
            * ((profit_score * 0.38)
                + (recent_profit_score * 0.28)
                + (drawdown_score * 0.14)
                + (metrics.positive_month_ratio * 0.10)
                + (activity_score * 0.06)
                + (sample_score * 0.04)),
    )
}

fn build_reasons(metrics: &SimpleWalletMetrics, flags: &[EmployeeFlag]) -> Vec<String> {
    if flags.is_empty() {
        return vec![format!(
            "stable enough: pnl=${:.2}, roi={:.1}%, recent_{}d_pnl=${:.2}, recent_roi={:.1}%, current_loss=${:.2}, max_dd={:.1}%, win_pos={:.1}%",
            metrics.realized_pnl_usd,
            metrics.realized_roi * 100.0,
            metrics.recent_window_days,
            metrics.recent_pnl_usd,
            metrics.recent_roi * 100.0,
            metrics.current_loss_usd,
            metrics.max_drawdown_ratio * 100.0,
            metrics.win_position_ratio * 100.0,
        )];
    }

    flags
        .iter()
        .map(|flag| match flag {
            EmployeeFlag::InsufficientSample => {
                format!(
                    "sample too thin: {} closed positions",
                    metrics.closed_positions
                )
            }
            EmployeeFlag::InsufficientRecentSample => {
                format!(
                    "recent sample too thin: {} closed positions in {}d",
                    metrics.recent_closed_positions, metrics.recent_window_days
                )
            }
            EmployeeFlag::LowRealizedPnl => {
                format!("low realized pnl: ${:.2}", metrics.realized_pnl_usd)
            }
            EmployeeFlag::LowRealizedRoi => {
                format!("low roi: {:.1}%", metrics.realized_roi * 100.0)
            }
            EmployeeFlag::RecentLoss => {
                format!(
                    "recent {}d pnl is not positive: ${:.2}",
                    metrics.recent_window_days, metrics.recent_pnl_usd
                )
            }
            EmployeeFlag::LowRecentRoi => {
                format!(
                    "recent {}d roi is too low: {:.1}%",
                    metrics.recent_window_days,
                    metrics.recent_roi * 100.0
                )
            }
            EmployeeFlag::LowRecentWinRate => {
                format!(
                    "recent {}d win rate is too low: {:.1}%",
                    metrics.recent_window_days,
                    metrics.recent_win_position_ratio * 100.0
                )
            }
            EmployeeFlag::LargeCurrentLoss => {
                format!(
                    "current/open loss too large: ${:.2}, worst_position=${:.2}",
                    metrics.current_loss_usd, metrics.worst_current_position_pnl_usd
                )
            }
            EmployeeFlag::HighCurrentLossRatio => {
                format!(
                    "current/open loss ratio too high: {:.1}%",
                    metrics.current_loss_ratio * 100.0
                )
            }
            EmployeeFlag::HighDrawdown => {
                format!(
                    "drawdown too high: {:.1}%",
                    metrics.max_drawdown_ratio * 100.0
                )
            }
            EmployeeFlag::Inactive => match metrics.last_activity_days {
                Some(days) => format!("inactive for {days} days"),
                None => "no recent activity timestamp".to_owned(),
            },
            EmployeeFlag::ColdStreak => {
                format!("recent loss streak: {}", metrics.recent_loss_streak)
            }
        })
        .collect()
}

fn select_category_picks(
    evaluations: &mut [WalletEvaluation],
    config: &DiscoveryRunConfig,
    selected_wallets: &mut HashSet<String>,
) -> Vec<WalletEvaluation> {
    evaluations.sort_by(|left, right| {
        right
            .eligible
            .cmp(&left.eligible)
            .then_with(|| right.score.total_cmp(&left.score))
            .then_with(|| {
                right
                    .metrics
                    .realized_pnl_usd
                    .total_cmp(&left.metrics.realized_pnl_usd)
            })
    });

    let mut picks = Vec::new();
    let limit = config.top_per_category.max(1);

    for evaluation in evaluations.iter() {
        if picks.len() >= limit {
            break;
        }

        if evaluation.eligible && !selected_wallets.contains(&evaluation.wallet) {
            selected_wallets.insert(evaluation.wallet.clone());
            picks.push(evaluation.clone());
        }
    }

    for evaluation in evaluations.iter() {
        if picks.len() >= limit {
            break;
        }

        if evaluation.eligible && !picks.iter().any(|pick| pick.wallet == evaluation.wallet) {
            picks.push(evaluation.clone());
        }
    }

    if picks.is_empty() {
        evaluations.iter().take(limit).cloned().collect()
    } else {
        picks
    }
}

fn normalized_timestamp_secs(timestamp: Option<u64>) -> Option<u64> {
    timestamp.map(|value| {
        if value > 10_000_000_000 {
            value / 1_000
        } else {
            value
        }
    })
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

fn clamp01(value: f64) -> f64 {
    value.max(0.0).min(1.0)
}

fn round2(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

fn round4(value: f64) -> f64 {
    (value * 10_000.0).round() / 10_000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn position(days_ago: u64, pnl: f64, bought: f64) -> ClosedPosition {
        let now = 1_800_000_000;
        ClosedPosition {
            proxy_wallet: "0x0000000000000000000000000000000000000001".to_owned(),
            asset: None,
            condition_id: None,
            avg_price: Some(0.52),
            total_bought: Some(bought),
            realized_pnl: Some(pnl),
            cur_price: None,
            timestamp: Some(now - (days_ago * SECONDS_PER_DAY)),
            title: None,
            slug: None,
            event_slug: None,
            outcome: None,
            outcome_index: None,
            opposite_outcome: None,
            opposite_asset: None,
            end_date: None,
        }
    }

    fn entry() -> LeaderboardEntry {
        LeaderboardEntry {
            rank: Some("1".to_owned()),
            proxy_wallet: "0x0000000000000000000000000000000000000001".to_owned(),
            user_name: Some("smart".to_owned()),
            vol: Some(10_000.0),
            pnl: Some(1_500.0),
            x_username: None,
            verified_badge: None,
        }
    }

    #[test]
    fn stable_profitable_wallet_is_eligible() {
        let positions = vec![
            position(50, 220.0, 400.0),
            position(45, -20.0, 200.0),
            position(40, 180.0, 300.0),
            position(35, 160.0, 300.0),
            position(25, 80.0, 250.0),
            position(20, 140.0, 250.0),
            position(10, 120.0, 250.0),
            position(2, 90.0, 250.0),
        ];

        let evaluation = evaluate_wallet(
            "SPORTS",
            &entry(),
            &positions,
            &[],
            &SmartMoneyConfig::default(),
            1_800_000_000,
        );

        assert!(evaluation.eligible);
        assert!(evaluation.score > 0.6);
        assert!(evaluation.flags.is_empty());
    }

    #[test]
    fn stale_cold_wallet_is_flagged() {
        let positions = vec![
            position(120, 300.0, 400.0),
            position(110, 150.0, 300.0),
            position(100, 200.0, 300.0),
            position(90, 180.0, 300.0),
            position(80, -30.0, 200.0),
            position(67, -40.0, 200.0),
            position(66, -50.0, 200.0),
            position(65, -60.0, 200.0),
        ];

        let evaluation = evaluate_wallet(
            "CRYPTO",
            &entry(),
            &positions,
            &[],
            &SmartMoneyConfig::default(),
            1_800_000_000,
        );

        assert!(!evaluation.eligible);
        assert!(evaluation.flags.contains(&EmployeeFlag::Inactive));
        assert!(evaluation.flags.contains(&EmployeeFlag::ColdStreak));
    }

    #[test]
    fn all_time_winner_with_recent_losses_is_not_hired() {
        let positions = vec![
            position(120, 1_500.0, 2_000.0),
            position(100, 900.0, 1_500.0),
            position(80, 700.0, 1_200.0),
            position(60, 500.0, 1_000.0),
            position(25, -300.0, 700.0),
            position(20, -250.0, 600.0),
            position(12, -200.0, 600.0),
            position(3, -100.0, 400.0),
        ];

        let evaluation = evaluate_wallet(
            "SPORTS",
            &entry(),
            &positions,
            &[],
            &SmartMoneyConfig::default(),
            1_800_000_000,
        );

        assert!(!evaluation.eligible);
        assert!(evaluation.metrics.realized_pnl_usd > 0.0);
        assert!(evaluation.metrics.recent_pnl_usd < 0.0);
        assert!(evaluation.flags.contains(&EmployeeFlag::RecentLoss));
        assert!(evaluation.flags.contains(&EmployeeFlag::LowRecentRoi));
    }

    #[test]
    fn huge_current_loss_blocks_hire_even_when_closed_positions_look_good() {
        let positions = vec![
            position(25, 80.0, 250.0),
            position(20, 140.0, 250.0),
            position(18, 120.0, 250.0),
            position(15, 90.0, 250.0),
            position(12, 60.0, 250.0),
            position(10, 75.0, 250.0),
            position(5, 110.0, 250.0),
            position(2, 95.0, 250.0),
        ];
        let current_positions = vec![CurrentPosition {
            proxy_wallet: "0x0000000000000000000000000000000000000001".to_owned(),
            asset: None,
            condition_id: None,
            size: Some(100_000.0),
            avg_price: Some(0.60),
            initial_value: Some(60_000.0),
            current_value: Some(0.0),
            cash_pnl: Some(-60_000.0),
            percent_pnl: Some(-100.0),
            total_bought: Some(100_000.0),
            realized_pnl: Some(0.0),
            cur_price: Some(0.0),
            redeemable: Some(true),
            mergeable: Some(false),
            title: Some("Bad current position".to_owned()),
            slug: None,
            event_slug: None,
            outcome: None,
            outcome_index: None,
            opposite_outcome: None,
            end_date: None,
        }];

        let evaluation = evaluate_wallet(
            "SPORTS",
            &entry(),
            &positions,
            &current_positions,
            &SmartMoneyConfig::default(),
            1_800_000_000,
        );

        assert!(!evaluation.eligible);
        assert!(evaluation.flags.contains(&EmployeeFlag::LargeCurrentLoss));
        assert!(evaluation
            .flags
            .contains(&EmployeeFlag::HighCurrentLossRatio));
    }
}
