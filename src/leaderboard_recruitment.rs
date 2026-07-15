use crate::polymarket::{
    ClosedPosition, CurrentPosition, LeaderboardEntry, PolymarketDataClient, PolymarketError,
};
use serde::Serialize;
use std::{
    collections::{BTreeMap, HashMap},
    thread::sleep,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const SECONDS_PER_DAY: u64 = 86_400;
const LEADERBOARD_PAGE_SIZE: usize = 50;

pub const ROTATING_RECRUITMENT_DOMAINS: [&str; 3] = ["WEATHER", "CRYPTO", "SPORTS"];

#[derive(Debug, Clone)]
pub struct LeaderboardRecruitmentConfig {
    pub domain: String,
    pub periods: Vec<String>,
    pub leaderboard_depth: usize,
    pub wallet_limit: usize,
    pub history_window_days: u64,
    pub closed_position_pages: usize,
    pub closed_position_page_size: usize,
    pub pause_between_wallets_ms: u64,
    pub top: usize,
    pub min_lifetime_pnl_usd: f64,
    pub max_lifetime_pnl_usd: f64,
    pub min_monthly_positions: usize,
    pub max_monthly_positions: usize,
    pub min_domain_14d_positions: usize,
    pub min_domain_14d_roi: f64,
    pub min_domain_gross_profit_share: f64,
    pub max_top5_profit_share: f64,
    pub max_high_price_profit_share: f64,
    pub high_price_threshold: f64,
    pub max_active_loss_usd: f64,
    pub max_active_loss_ratio: f64,
}

impl Default for LeaderboardRecruitmentConfig {
    fn default() -> Self {
        Self {
            domain: "WEATHER".to_owned(),
            periods: vec![
                "DAY".to_owned(),
                "WEEK".to_owned(),
                "MONTH".to_owned(),
                "ALL".to_owned(),
            ],
            leaderboard_depth: 1_000,
            wallet_limit: 1_000,
            history_window_days: 30,
            closed_position_pages: 20,
            closed_position_page_size: 50,
            pause_between_wallets_ms: 120,
            top: 20,
            min_lifetime_pnl_usd: 15_000.0,
            max_lifetime_pnl_usd: 400_000.0,
            min_monthly_positions: 30,
            max_monthly_positions: 300,
            min_domain_14d_positions: 8,
            min_domain_14d_roi: 0.015,
            min_domain_gross_profit_share: 0.60,
            max_top5_profit_share: 0.75,
            max_high_price_profit_share: 0.65,
            high_price_threshold: 0.80,
            max_active_loss_usd: 10_000.0,
            max_active_loss_ratio: 0.20,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct LeaderboardRecruitmentReport {
    pub generated_at_secs: u64,
    pub domain: String,
    pub periods: Vec<String>,
    pub leaderboard_depth: usize,
    pub wallet_limit: usize,
    pub history_window_days: u64,
    pub pool_wallets: usize,
    pub scanned_wallets: usize,
    pub eligible_wallets: usize,
    pub candidates: Vec<LeaderboardEmployeeCandidate>,
    pub near_misses: Vec<LeaderboardEmployeeCandidate>,
    pub special_observations: Vec<LeaderboardEmployeeCandidate>,
    pub ultra_fast_observations: Vec<LeaderboardEmployeeCandidate>,
    pub all_evaluations: Vec<LeaderboardEmployeeCandidate>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LeaderboardEmployeeCandidate {
    pub wallet: String,
    pub user_name: Option<String>,
    pub ranks: BTreeMap<String, usize>,
    pub leaderboard_pnls: BTreeMap<String, f64>,
    pub lifetime_pnl_usd: Option<f64>,
    pub score: f64,
    pub eligible: bool,
    pub total_1d: WindowProfitMetrics,
    pub total_7d: WindowProfitMetrics,
    pub total_14d: WindowProfitMetrics,
    pub total_30d: WindowProfitMetrics,
    pub domain_1d: WindowProfitMetrics,
    pub domain_7d: WindowProfitMetrics,
    pub domain_14d: WindowProfitMetrics,
    pub domain_30d: WindowProfitMetrics,
    pub domain_gross_profit_share_14d: f64,
    pub domain_invested_share_14d: f64,
    pub top5_profit_share_14d: f64,
    pub high_price_profit_share_14d: f64,
    pub ultra_fast_14d: WindowProfitMetrics,
    pub ultra_fast_position_share_14d: f64,
    pub ultra_fast_gross_profit_share_14d: f64,
    pub ultra_fast_invested_share_14d: f64,
    pub active_positions: usize,
    pub active_loss_usd: f64,
    pub active_initial_value_usd: f64,
    pub active_loss_ratio: f64,
    pub history_truncated: bool,
    pub candidate_tags: Vec<String>,
    pub specialty_segments: Vec<SpecialtySegmentMetrics>,
    pub flags: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct WindowProfitMetrics {
    pub days: u64,
    pub positions: usize,
    pub pnl_usd: f64,
    pub invested_usd: f64,
    pub roi: f64,
    pub gross_profit_usd: f64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct SpecialtySegmentMetrics {
    pub name: String,
    pub positions_30d: usize,
    pub pnl_usd_30d: f64,
    pub invested_usd_30d: f64,
    pub roi_30d: f64,
    pub gross_profit_share_30d: f64,
    pub top5_profit_share_30d: f64,
    pub high_price_profit_share_30d: f64,
    pub avg_entry_price_30d: f64,
    pub avg_position_usd_30d: f64,
    pub max_position_usd_30d: f64,
}

#[derive(Debug, Clone)]
struct RankedWallet {
    entry: LeaderboardEntry,
    ranks: BTreeMap<String, usize>,
    leaderboard_pnls: BTreeMap<String, f64>,
}

pub fn scan_leaderboard_employees(
    client: &PolymarketDataClient,
    config: &LeaderboardRecruitmentConfig,
) -> LeaderboardRecruitmentReport {
    let generated_at_secs = now_secs();
    let mut warnings = Vec::new();
    let pool = load_ranked_wallet_pool(client, config, &mut warnings);
    let pool_wallets = pool.len();
    let keywords = domain_keywords(&config.domain);
    let mut evaluations = Vec::new();

    for ranked in pool.into_iter().take(config.wallet_limit.max(1)) {
        let (closed_positions, history_truncated) =
            match load_recent_closed_positions(client, &ranked.entry.proxy_wallet, config) {
                Ok(result) => result,
                Err(error) => {
                    warnings.push(format!(
                        "failed to load closed positions for {}: {error}",
                        ranked.entry.proxy_wallet
                    ));
                    continue;
                }
            };
        let current_positions =
            match retry_request(3, || client.positions(&ranked.entry.proxy_wallet, 50, 0)) {
                Ok(positions) => positions,
                Err(error) => {
                    warnings.push(format!(
                        "failed to load current positions for {}: {error}",
                        ranked.entry.proxy_wallet
                    ));
                    Vec::new()
                }
            };

        evaluations.push(evaluate_ranked_wallet(
            &ranked,
            &closed_positions,
            &current_positions,
            &keywords,
            config,
            generated_at_secs,
            history_truncated,
        ));

        if config.pause_between_wallets_ms > 0 {
            sleep(Duration::from_millis(config.pause_between_wallets_ms));
        }
    }

    evaluations.sort_by(compare_candidates);
    let scanned_wallets = evaluations.len();
    let eligible_wallets = evaluations
        .iter()
        .filter(|candidate| candidate.eligible)
        .count();
    let mut ultra_fast_observations = evaluations
        .iter()
        .filter(|candidate| {
            candidate.ultra_fast_14d.positions >= 20 && candidate.ultra_fast_14d.pnl_usd > 0.0
        })
        .cloned()
        .collect::<Vec<_>>();
    ultra_fast_observations.sort_by(|left, right| {
        right
            .ultra_fast_14d
            .pnl_usd
            .total_cmp(&left.ultra_fast_14d.pnl_usd)
            .then_with(|| right.ultra_fast_14d.roi.total_cmp(&left.ultra_fast_14d.roi))
    });
    ultra_fast_observations.truncate(config.top.max(1));
    let mut special_observations = evaluations
        .iter()
        .filter(|candidate| is_special_observation(candidate, config))
        .cloned()
        .collect::<Vec<_>>();
    special_observations.sort_by(|left, right| {
        right
            .domain_30d
            .roi
            .total_cmp(&left.domain_30d.roi)
            .then_with(|| right.domain_30d.pnl_usd.total_cmp(&left.domain_30d.pnl_usd))
            .then_with(|| right.domain_30d.positions.cmp(&left.domain_30d.positions))
    });
    special_observations.truncate(config.top.max(1));
    let candidates = evaluations
        .iter()
        .filter(|candidate| candidate.eligible)
        .take(config.top.max(1))
        .cloned()
        .collect();
    let near_misses = evaluations
        .iter()
        .filter(|candidate| !candidate.eligible)
        .take(config.top.max(1))
        .cloned()
        .collect();
    let all_evaluations = evaluations;

    LeaderboardRecruitmentReport {
        generated_at_secs,
        domain: config.domain.to_uppercase(),
        periods: config.periods.clone(),
        leaderboard_depth: config.leaderboard_depth,
        wallet_limit: config.wallet_limit,
        history_window_days: config.history_window_days,
        pool_wallets,
        scanned_wallets,
        eligible_wallets,
        candidates,
        near_misses,
        special_observations,
        ultra_fast_observations,
        all_evaluations,
        warnings,
    }
}

fn load_ranked_wallet_pool(
    client: &PolymarketDataClient,
    config: &LeaderboardRecruitmentConfig,
    warnings: &mut Vec<String>,
) -> Vec<RankedWallet> {
    let mut wallets: HashMap<String, RankedWallet> = HashMap::new();
    let depth = config.leaderboard_depth.clamp(1, 1_000);

    for period in &config.periods {
        let period = period.to_uppercase();
        for offset in (0..depth).step_by(LEADERBOARD_PAGE_SIZE) {
            let limit = LEADERBOARD_PAGE_SIZE.min(depth - offset);
            let page = match retry_request(3, || {
                client.leaderboard(&config.domain, &period, "PNL", limit, offset)
            }) {
                Ok(entries) => entries,
                Err(error) => {
                    warnings.push(format!(
                        "failed to load {} {} leaderboard at offset {}: {error}",
                        config.domain, period, offset
                    ));
                    break;
                }
            };
            let page_len = page.len();

            for (index, entry) in page.into_iter().enumerate() {
                let rank = entry
                    .rank
                    .as_deref()
                    .and_then(|value| value.parse::<usize>().ok())
                    .unwrap_or(offset + index + 1);
                let key = entry.proxy_wallet.to_lowercase();
                let ranked = wallets.entry(key).or_insert_with(|| RankedWallet {
                    entry: entry.clone(),
                    ranks: BTreeMap::new(),
                    leaderboard_pnls: BTreeMap::new(),
                });
                ranked.ranks.insert(period.clone(), rank);
                if let Some(pnl) = entry.pnl {
                    ranked.leaderboard_pnls.insert(period.clone(), pnl);
                }
                if ranked.entry.user_name.is_none() && entry.user_name.is_some() {
                    ranked.entry.user_name = entry.user_name;
                }
            }

            if page_len < limit {
                break;
            }
        }
    }

    let mut pool = wallets.into_values().collect::<Vec<_>>();
    pool.sort_by(|left, right| compare_ranked_wallets(left, right, config));
    pool
}

fn load_recent_closed_positions(
    client: &PolymarketDataClient,
    wallet: &str,
    config: &LeaderboardRecruitmentConfig,
) -> Result<(Vec<ClosedPosition>, bool), PolymarketError> {
    let mut positions = Vec::new();
    let cutoff = now_secs().saturating_sub(config.history_window_days.max(14) * SECONDS_PER_DAY);
    let page_size = config.closed_position_page_size.clamp(1, 50);
    let mut history_truncated = false;

    for page in 0..config.closed_position_pages.max(1) {
        let page_positions = retry_request(3, || {
            client.closed_positions(wallet, page_size, page * page_size)
        })?;
        let page_len = page_positions.len();
        let reached_cutoff = page_positions.iter().any(|position| {
            normalized_timestamp_secs(position.timestamp)
                .map(|timestamp| timestamp < cutoff)
                .unwrap_or(false)
        });
        positions.extend(page_positions);

        if page_len < page_size || reached_cutoff {
            return Ok((positions, false));
        }

        if page + 1 == config.closed_position_pages.max(1) {
            history_truncated = true;
        }
    }

    Ok((positions, history_truncated))
}

fn evaluate_ranked_wallet(
    ranked: &RankedWallet,
    closed_positions: &[ClosedPosition],
    current_positions: &[CurrentPosition],
    keywords: &[&str],
    config: &LeaderboardRecruitmentConfig,
    now_secs: u64,
    history_truncated: bool,
) -> LeaderboardEmployeeCandidate {
    let exclude_ultra_fast = config.domain.eq_ignore_ascii_case("CRYPTO");
    let ultra_fast_positions = if exclude_ultra_fast {
        closed_positions
            .iter()
            .filter(|position| is_ultra_fast_crypto_position(position))
            .cloned()
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    let filtered_positions = closed_positions
        .iter()
        .filter(|position| !exclude_ultra_fast || !is_ultra_fast_crypto_position(position))
        .cloned()
        .collect::<Vec<_>>();
    let raw_domain_positions = closed_positions
        .iter()
        .filter(|position| position_matches_domain(position, keywords))
        .cloned()
        .collect::<Vec<_>>();
    let domain_positions = filtered_positions
        .iter()
        .filter(|position| position_matches_domain(position, keywords))
        .cloned()
        .collect::<Vec<_>>();
    let total_1d = window_metrics(&filtered_positions, now_secs, 1);
    let total_7d = window_metrics(&filtered_positions, now_secs, 7);
    let total_14d = window_metrics(&filtered_positions, now_secs, 14);
    let total_30d = window_metrics(&filtered_positions, now_secs, 30);
    let domain_1d = window_metrics(&domain_positions, now_secs, 1);
    let domain_7d = window_metrics(&domain_positions, now_secs, 7);
    let domain_14d = window_metrics(&domain_positions, now_secs, 14);
    let domain_30d = window_metrics(&domain_positions, now_secs, 30);
    let raw_domain_14d = window_metrics(&raw_domain_positions, now_secs, 14);
    let ultra_fast_14d = window_metrics(&ultra_fast_positions, now_secs, 14);
    let ultra_fast_position_share_14d = if raw_domain_14d.positions > 0 {
        ultra_fast_14d.positions as f64 / raw_domain_14d.positions as f64
    } else {
        0.0
    };
    let ultra_fast_gross_profit_share_14d = ratio(
        ultra_fast_14d.gross_profit_usd,
        raw_domain_14d.gross_profit_usd,
    );
    let ultra_fast_invested_share_14d =
        ratio(ultra_fast_14d.invested_usd, raw_domain_14d.invested_usd);
    let domain_gross_profit_share_14d =
        ratio(domain_14d.gross_profit_usd, total_14d.gross_profit_usd);
    let domain_invested_share_14d = ratio(domain_14d.invested_usd, total_14d.invested_usd);
    let recent_domain_positions = positions_in_window(&domain_positions, now_secs, 14);
    let recent_domain_positions_30d = positions_in_window(&domain_positions, now_secs, 30);
    let top5_profit_share_14d = top_profit_share(&recent_domain_positions, 5);
    let high_price_profit_share_14d =
        high_price_profit_share(&recent_domain_positions, config.high_price_threshold);
    let specialty_segments = specialty_segments(
        &config.domain,
        &recent_domain_positions_30d,
        domain_30d.gross_profit_usd,
        config.high_price_threshold,
    );
    let active_positions = current_positions
        .iter()
        .filter(|position| is_active_position(position))
        .collect::<Vec<_>>();
    let active_loss_usd = active_positions
        .iter()
        .map(|position| position.cash_pnl.unwrap_or(0.0).min(0.0).abs())
        .sum::<f64>();
    let active_initial_value_usd = active_positions
        .iter()
        .map(|position| position.initial_value.unwrap_or(0.0).max(0.0))
        .sum::<f64>();
    let active_loss_ratio = ratio(active_loss_usd, active_initial_value_usd);
    let lifetime_pnl_usd = ranked.leaderboard_pnls.get("ALL").copied();
    let effective_min_monthly_positions = effective_min_monthly_positions(&config.domain, config);
    let effective_max_monthly_positions = effective_max_monthly_positions(&config.domain, config);
    let effective_min_domain_positions = effective_min_domain_14d_positions(&config.domain, config);
    let effective_min_domain_roi = effective_min_domain_14d_roi(&config.domain, config);
    let effective_min_domain_profit_share =
        effective_min_domain_profit_share(&config.domain, config);
    let effective_max_top5_profit_share = effective_max_top5_profit_share(&config.domain, config);
    let effective_max_high_price_profit_share =
        effective_max_high_price_profit_share(&config.domain, config);
    let rising_star_candidate = is_rising_star_candidate(
        &domain_14d,
        &domain_30d,
        domain_gross_profit_share_14d,
        top5_profit_share_14d,
        high_price_profit_share_14d,
        active_loss_ratio,
        config,
        effective_max_monthly_positions,
    );
    let sports_league_candidate = is_sports_league_candidate(
        &specialty_segments,
        &domain_14d,
        &domain_30d,
        domain_gross_profit_share_14d,
        high_price_profit_share_14d,
        active_loss_ratio,
        config,
    );
    let mut candidate_tags = candidate_tags(
        &config.domain,
        rising_star_candidate,
        sports_league_candidate,
        &specialty_segments,
        lifetime_pnl_usd,
        config,
    );

    let mut flags = Vec::new();
    match lifetime_pnl_usd {
        Some(value)
            if value < config.min_lifetime_pnl_usd
                && !rising_star_candidate
                && !sports_league_candidate =>
        {
            flags.push("lifetime_pnl_too_low".to_owned());
        }
        Some(value) if value > config.max_lifetime_pnl_usd && !sports_league_candidate => {
            flags.push("lifetime_pnl_too_high".to_owned());
        }
        Some(_) => {}
        None => flags.push("lifetime_pnl_unknown".to_owned()),
    }
    if total_30d.positions < effective_min_monthly_positions && !sports_league_candidate {
        flags.push("monthly_frequency_too_low".to_owned());
    }
    if total_30d.positions > effective_max_monthly_positions {
        flags.push("monthly_frequency_too_high".to_owned());
    }
    if total_7d.pnl_usd <= 0.0 && !sports_league_candidate {
        flags.push("total_7d_not_profitable".to_owned());
    }
    if total_14d.pnl_usd <= 0.0 {
        flags.push("total_14d_not_profitable".to_owned());
    }
    if domain_7d.pnl_usd <= 0.0 && !sports_league_candidate {
        flags.push("domain_7d_not_profitable".to_owned());
    }
    if domain_14d.pnl_usd <= 0.0 {
        flags.push("domain_14d_not_profitable".to_owned());
    }
    if domain_14d.positions < effective_min_domain_positions && !sports_league_candidate {
        flags.push("domain_14d_sample_too_small".to_owned());
    }
    if domain_14d.roi < effective_min_domain_roi && !sports_league_candidate {
        flags.push("domain_14d_roi_too_low".to_owned());
    }
    if domain_gross_profit_share_14d < effective_min_domain_profit_share && !sports_league_candidate
    {
        flags.push("domain_profit_share_too_low".to_owned());
    }
    if top5_profit_share_14d > effective_max_top5_profit_share && !sports_league_candidate {
        flags.push("profit_too_concentrated".to_owned());
    }
    if high_price_profit_share_14d > effective_max_high_price_profit_share {
        flags.push("high_price_profit_dependency".to_owned());
    }
    if active_loss_usd > config.max_active_loss_usd {
        flags.push("active_loss_too_large".to_owned());
    }
    if active_loss_ratio > config.max_active_loss_ratio {
        flags.push("active_loss_ratio_too_high".to_owned());
    }
    if history_truncated {
        flags.push("history_truncated".to_owned());
    }
    if !flags.is_empty() {
        candidate_tags.retain(|tag| tag != "recommended");
    }

    let score = candidate_score(
        &total_7d,
        &total_14d,
        &domain_7d,
        &domain_14d,
        &total_30d,
        domain_gross_profit_share_14d,
        top5_profit_share_14d,
        high_price_profit_share_14d,
        active_loss_ratio,
        config,
    );

    LeaderboardEmployeeCandidate {
        wallet: ranked.entry.proxy_wallet.clone(),
        user_name: ranked.entry.user_name.clone(),
        ranks: ranked.ranks.clone(),
        leaderboard_pnls: ranked.leaderboard_pnls.clone(),
        lifetime_pnl_usd: lifetime_pnl_usd.map(round2),
        score,
        eligible: flags.is_empty(),
        total_1d,
        total_7d,
        total_14d,
        total_30d,
        domain_1d,
        domain_7d,
        domain_14d,
        domain_30d,
        domain_gross_profit_share_14d: round4(domain_gross_profit_share_14d),
        domain_invested_share_14d: round4(domain_invested_share_14d),
        top5_profit_share_14d: round4(top5_profit_share_14d),
        high_price_profit_share_14d: round4(high_price_profit_share_14d),
        ultra_fast_14d,
        ultra_fast_position_share_14d: round4(ultra_fast_position_share_14d),
        ultra_fast_gross_profit_share_14d: round4(ultra_fast_gross_profit_share_14d),
        ultra_fast_invested_share_14d: round4(ultra_fast_invested_share_14d),
        active_positions: active_positions.len(),
        active_loss_usd: round2(active_loss_usd),
        active_initial_value_usd: round2(active_initial_value_usd),
        active_loss_ratio: round4(active_loss_ratio),
        history_truncated,
        candidate_tags,
        specialty_segments,
        flags,
    }
}

fn window_metrics(positions: &[ClosedPosition], now_secs: u64, days: u64) -> WindowProfitMetrics {
    let recent = positions_in_window(positions, now_secs, days);
    let pnl_usd = recent
        .iter()
        .map(|position| position.realized_pnl.unwrap_or(0.0))
        .sum::<f64>();
    let invested_usd = recent
        .iter()
        .map(|position| position.total_bought.unwrap_or(0.0).max(0.0))
        .sum::<f64>();
    let gross_profit_usd = recent
        .iter()
        .map(|position| position.realized_pnl.unwrap_or(0.0).max(0.0))
        .sum::<f64>();

    WindowProfitMetrics {
        days,
        positions: recent.len(),
        pnl_usd: round2(pnl_usd),
        invested_usd: round2(invested_usd),
        roi: round4(ratio(pnl_usd, invested_usd)),
        gross_profit_usd: round2(gross_profit_usd),
    }
}

fn positions_in_window<'a>(
    positions: &'a [ClosedPosition],
    now_secs: u64,
    days: u64,
) -> Vec<&'a ClosedPosition> {
    let cutoff = now_secs.saturating_sub(days.saturating_mul(SECONDS_PER_DAY));
    positions
        .iter()
        .filter(|position| {
            normalized_timestamp_secs(position.timestamp)
                .map(|timestamp| timestamp >= cutoff && timestamp <= now_secs)
                .unwrap_or(false)
        })
        .collect()
}

fn position_matches_domain(position: &ClosedPosition, keywords: &[&str]) -> bool {
    let searchable = position_searchable_text(position);

    keywords
        .iter()
        .any(|keyword| contains_keyword(&searchable, keyword))
}

fn position_searchable_text(position: &ClosedPosition) -> String {
    format!(
        "{} {} {}",
        position.title.as_deref().unwrap_or_default(),
        position.slug.as_deref().unwrap_or_default(),
        position.event_slug.as_deref().unwrap_or_default()
    )
    .to_lowercase()
}

fn is_ultra_fast_crypto_position(position: &ClosedPosition) -> bool {
    position_matches_domain(position, &domain_keywords("CRYPTO"))
        && ultra_fast_market_minutes(position)
            .map(|minutes| minutes <= 15)
            .unwrap_or(false)
}

fn ultra_fast_market_minutes(position: &ClosedPosition) -> Option<u32> {
    let searchable = format!(
        "{} {} {}",
        position.title.as_deref().unwrap_or_default(),
        position.slug.as_deref().unwrap_or_default(),
        position.event_slug.as_deref().unwrap_or_default()
    )
    .to_lowercase();

    for minutes in [5, 10, 15] {
        let markers = [
            format!("{minutes}m"),
            format!("{minutes}-min"),
            format!("{minutes} min"),
            format!("{minutes}-minute"),
            format!("{minutes} minute"),
        ];
        if markers.iter().any(|marker| searchable.contains(marker)) {
            return Some(minutes);
        }
    }

    let clocks = clock_minutes_in_text(&searchable);
    for pair in clocks.windows(2) {
        let duration = (pair[1] + 24 * 60 - pair[0]) % (24 * 60);
        if (1..=15).contains(&duration) {
            return Some(duration);
        }
    }

    None
}

fn clock_minutes_in_text(text: &str) -> Vec<u32> {
    let bytes = text.as_bytes();
    let mut clocks = Vec::new();
    let mut index = 0;

    while index + 1 < bytes.len() {
        let is_am = bytes[index] == b'a' && bytes[index + 1] == b'm';
        let is_pm = bytes[index] == b'p' && bytes[index + 1] == b'm';
        if !is_am && !is_pm {
            index += 1;
            continue;
        }

        let mut start = index;
        while start > 0 {
            let byte = bytes[start - 1];
            if byte.is_ascii_digit() || byte == b':' {
                start -= 1;
            } else {
                break;
            }
        }

        if let Some((hour, minute)) =
            text[start..index]
                .split_once(':')
                .and_then(|(hour, minute)| {
                    Some((hour.parse::<u32>().ok()?, minute.parse::<u32>().ok()?))
                })
        {
            if (1..=12).contains(&hour) && minute < 60 {
                let hour = if is_am { hour % 12 } else { (hour % 12) + 12 };
                clocks.push(hour * 60 + minute);
            }
        }

        index += 2;
    }

    clocks
}

fn contains_keyword(searchable: &str, keyword: &str) -> bool {
    let keyword = keyword.to_lowercase();
    searchable.match_indices(&keyword).any(|(start, _)| {
        let end = start + keyword.len();
        let left_ok = start == 0
            || searchable[..start]
                .chars()
                .next_back()
                .map(|ch| !ch.is_ascii_alphanumeric())
                .unwrap_or(true);
        let right_ok = end == searchable.len()
            || searchable[end..]
                .chars()
                .next()
                .map(|ch| !ch.is_ascii_alphanumeric())
                .unwrap_or(true);
        left_ok && right_ok
    })
}

fn domain_keywords(domain: &str) -> Vec<&'static str> {
    match domain.to_uppercase().as_str() {
        "WEATHER" => vec![
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
        "SPORTS" => vec![
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
            "champions league",
            "premier league",
        ],
        "CRYPTO" => vec![
            "bitcoin", "btc", "ethereum", "eth", "solana", "sol", "xrp", "dogecoin", "crypto",
            "token", "airdrop", "defi",
        ],
        "FINANCE" => vec![
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
        ],
        "ECONOMICS" => vec![
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
        "TECH" => vec![
            "openai",
            "anthropic",
            "google",
            "microsoft",
            "apple",
            "meta",
            "nvidia",
            "ai",
            "artificial intelligence",
            "model",
            "iphone",
            "spacex",
        ],
        "MENTIONS" => vec![
            "say",
            "says",
            "said",
            "mention",
            "mentions",
            "mentioned",
            "speech",
            "tweet",
            "tweets",
            "post",
            "posts",
            "words",
            "phrase",
        ],
        _ => Vec::new(),
    }
}

fn is_active_position(position: &CurrentPosition) -> bool {
    !position.redeemable.unwrap_or(false) && position.size.unwrap_or(0.0) > 0.0
}

fn top_profit_share(positions: &[&ClosedPosition], count: usize) -> f64 {
    let mut profits = positions
        .iter()
        .map(|position| position.realized_pnl.unwrap_or(0.0).max(0.0))
        .filter(|profit| *profit > 0.0)
        .collect::<Vec<_>>();
    profits.sort_by(|left, right| right.total_cmp(left));
    let gross_profit = profits.iter().sum::<f64>();
    ratio(profits.into_iter().take(count).sum(), gross_profit)
}

fn high_price_profit_share(positions: &[&ClosedPosition], threshold: f64) -> f64 {
    let gross_profit = positions
        .iter()
        .map(|position| position.realized_pnl.unwrap_or(0.0).max(0.0))
        .sum::<f64>();
    let high_price_profit = positions
        .iter()
        .filter(|position| position.avg_price.unwrap_or(0.0) >= threshold)
        .map(|position| position.realized_pnl.unwrap_or(0.0).max(0.0))
        .sum::<f64>();
    ratio(high_price_profit, gross_profit)
}

fn specialty_segments(
    domain: &str,
    positions_30d: &[&ClosedPosition],
    domain_gross_profit_30d: f64,
    high_price_threshold: f64,
) -> Vec<SpecialtySegmentMetrics> {
    if !domain.eq_ignore_ascii_case("SPORTS") {
        return Vec::new();
    }

    let mut by_segment: BTreeMap<String, Vec<&ClosedPosition>> = BTreeMap::new();
    for position in positions_30d {
        by_segment
            .entry(sports_league_label(position).to_owned())
            .or_default()
            .push(*position);
    }

    let mut segments = by_segment
        .into_iter()
        .map(|(name, positions)| {
            segment_metrics(
                name,
                &positions,
                domain_gross_profit_30d,
                high_price_threshold,
            )
        })
        .collect::<Vec<_>>();
    segments.sort_by(|left, right| {
        right
            .pnl_usd_30d
            .total_cmp(&left.pnl_usd_30d)
            .then_with(|| right.roi_30d.total_cmp(&left.roi_30d))
            .then_with(|| right.positions_30d.cmp(&left.positions_30d))
            .then_with(|| left.name.cmp(&right.name))
    });
    segments.truncate(5);
    segments
}

fn segment_metrics(
    name: String,
    positions: &[&ClosedPosition],
    domain_gross_profit_30d: f64,
    high_price_threshold: f64,
) -> SpecialtySegmentMetrics {
    let pnl_usd = positions
        .iter()
        .map(|position| position.realized_pnl.unwrap_or(0.0))
        .sum::<f64>();
    let invested_usd = positions
        .iter()
        .map(|position| position.total_bought.unwrap_or(0.0).max(0.0))
        .sum::<f64>();
    let gross_profit_usd = positions
        .iter()
        .map(|position| position.realized_pnl.unwrap_or(0.0).max(0.0))
        .sum::<f64>();
    let weighted_price = positions
        .iter()
        .map(|position| {
            position.avg_price.unwrap_or(0.0).max(0.0)
                * position.total_bought.unwrap_or(0.0).max(0.0)
        })
        .sum::<f64>();
    let max_position_usd = positions
        .iter()
        .map(|position| position.total_bought.unwrap_or(0.0).max(0.0))
        .fold(0.0, f64::max);

    SpecialtySegmentMetrics {
        name,
        positions_30d: positions.len(),
        pnl_usd_30d: round2(pnl_usd),
        invested_usd_30d: round2(invested_usd),
        roi_30d: round4(ratio(pnl_usd, invested_usd)),
        gross_profit_share_30d: round4(ratio(gross_profit_usd, domain_gross_profit_30d)),
        top5_profit_share_30d: round4(top_profit_share(positions, 5)),
        high_price_profit_share_30d: round4(high_price_profit_share(
            positions,
            high_price_threshold,
        )),
        avg_entry_price_30d: round4(ratio(weighted_price, invested_usd)),
        avg_position_usd_30d: round2(ratio(invested_usd, positions.len() as f64)),
        max_position_usd_30d: round2(max_position_usd),
    }
}

fn sports_league_label(position: &ClosedPosition) -> &'static str {
    let searchable = position_searchable_text(position);
    for (label, keywords) in sports_league_keywords() {
        if keywords
            .iter()
            .any(|keyword| contains_keyword(&searchable, keyword))
        {
            return label;
        }
    }
    "OTHER_SPORTS"
}

fn sports_league_keywords() -> Vec<(&'static str, Vec<&'static str>)> {
    vec![
        ("NBA", vec!["nba"]),
        ("WNBA", vec!["wnba"]),
        ("NFL", vec!["nfl"]),
        ("MLB", vec!["mlb"]),
        ("NHL", vec!["nhl"]),
        (
            "TENNIS",
            vec![
                "tennis",
                "atp",
                "wta",
                "wimbledon",
                "us open",
                "french open",
                "australian open",
            ],
        ),
        ("UFC_MMA", vec!["ufc", "mma"]),
        ("BOXING", vec!["boxing", "boxer"]),
        (
            "SOCCER",
            vec![
                "soccer",
                "football",
                "fifa",
                "champions league",
                "premier league",
                "epl",
                "la liga",
                "serie a",
                "bundesliga",
                "mls",
            ],
        ),
        ("GOLF", vec!["golf", "pga", "masters"]),
        ("FORMULA_1", vec!["formula 1", "f1", "grand prix"]),
    ]
}

#[allow(clippy::too_many_arguments)]
fn candidate_score(
    total_7d: &WindowProfitMetrics,
    total_14d: &WindowProfitMetrics,
    domain_7d: &WindowProfitMetrics,
    domain_14d: &WindowProfitMetrics,
    total_30d: &WindowProfitMetrics,
    domain_profit_share: f64,
    top5_profit_share: f64,
    high_price_profit_share: f64,
    active_loss_ratio: f64,
    config: &LeaderboardRecruitmentConfig,
) -> f64 {
    let consistency = [
        total_7d.pnl_usd,
        total_14d.pnl_usd,
        domain_7d.pnl_usd,
        domain_14d.pnl_usd,
    ]
    .iter()
    .filter(|pnl| **pnl > 0.0)
    .count() as f64
        / 4.0;
    let roi_score = clamp01(domain_14d.roi / 0.10);
    let sample_score =
        clamp01(domain_14d.positions as f64 / (config.min_domain_14d_positions.max(1) * 4) as f64);
    let focus_score = clamp01(domain_profit_share);
    let concentration_score = 1.0 - clamp01(top5_profit_share);
    let price_score = 1.0 - clamp01(high_price_profit_share);
    let risk_score = 1.0 - clamp01(active_loss_ratio / config.max_active_loss_ratio.max(0.01));
    let frequency_score = frequency_fit_score(
        total_30d.positions,
        effective_min_monthly_positions(&config.domain, config),
        effective_max_monthly_positions(&config.domain, config),
    );

    round2(
        100.0
            * ((consistency * 0.25)
                + (roi_score * 0.16)
                + (sample_score * 0.12)
                + (focus_score * 0.18)
                + (concentration_score * 0.10)
                + (price_score * 0.10)
                + (risk_score * 0.06)
                + (frequency_score * 0.03)),
    )
}

fn frequency_fit_score(positions: usize, min_positions: usize, max_positions: usize) -> f64 {
    if min_positions == 0 || max_positions <= min_positions {
        return 1.0;
    }
    if positions < min_positions {
        return clamp01(positions as f64 / min_positions as f64);
    }
    if positions > max_positions {
        return clamp01(max_positions as f64 / positions as f64);
    }
    1.0
}

fn effective_min_monthly_positions(domain: &str, config: &LeaderboardRecruitmentConfig) -> usize {
    if domain.eq_ignore_ascii_case("SPORTS") {
        config.min_monthly_positions.min(5)
    } else {
        config.min_monthly_positions
    }
}

fn effective_max_monthly_positions(domain: &str, config: &LeaderboardRecruitmentConfig) -> usize {
    if domain.eq_ignore_ascii_case("WEATHER") {
        config.max_monthly_positions.max(500)
    } else {
        config.max_monthly_positions
    }
}

fn effective_min_domain_14d_positions(
    domain: &str,
    config: &LeaderboardRecruitmentConfig,
) -> usize {
    if domain.eq_ignore_ascii_case("SPORTS") {
        config.min_domain_14d_positions.min(3)
    } else {
        config.min_domain_14d_positions
    }
}

fn effective_min_domain_14d_roi(domain: &str, config: &LeaderboardRecruitmentConfig) -> f64 {
    if domain.eq_ignore_ascii_case("CRYPTO") {
        config.min_domain_14d_roi.max(0.05)
    } else if domain.eq_ignore_ascii_case("SPORTS") {
        config.min_domain_14d_roi.max(0.03)
    } else {
        config.min_domain_14d_roi
    }
}

fn effective_min_domain_profit_share(domain: &str, config: &LeaderboardRecruitmentConfig) -> f64 {
    if domain.eq_ignore_ascii_case("CRYPTO") {
        config.min_domain_gross_profit_share.max(0.70)
    } else if domain.eq_ignore_ascii_case("SPORTS") {
        config.min_domain_gross_profit_share.min(0.35)
    } else {
        config.min_domain_gross_profit_share
    }
}

fn effective_max_top5_profit_share(domain: &str, config: &LeaderboardRecruitmentConfig) -> f64 {
    if domain.eq_ignore_ascii_case("CRYPTO") {
        config.max_top5_profit_share.min(0.55)
    } else if domain.eq_ignore_ascii_case("SPORTS") {
        config.max_top5_profit_share.max(0.85)
    } else {
        config.max_top5_profit_share
    }
}

fn effective_max_high_price_profit_share(
    domain: &str,
    config: &LeaderboardRecruitmentConfig,
) -> f64 {
    if domain.eq_ignore_ascii_case("CRYPTO") {
        config.max_high_price_profit_share.min(0.30)
    } else if domain.eq_ignore_ascii_case("SPORTS") {
        config.max_high_price_profit_share.min(0.55)
    } else {
        config.max_high_price_profit_share
    }
}

fn is_rising_star_candidate(
    domain_14d: &WindowProfitMetrics,
    domain_30d: &WindowProfitMetrics,
    domain_profit_share_14d: f64,
    top5_profit_share_14d: f64,
    high_price_profit_share_14d: f64,
    active_loss_ratio: f64,
    config: &LeaderboardRecruitmentConfig,
    effective_max_monthly_positions: usize,
) -> bool {
    domain_14d.pnl_usd > 0.0
        && domain_14d.positions >= config.min_domain_14d_positions
        && domain_30d.positions >= config.min_monthly_positions
        && domain_30d.positions <= effective_max_monthly_positions
        && domain_30d.pnl_usd >= 1_000.0
        && domain_30d.roi >= 0.04
        && domain_profit_share_14d >= config.min_domain_gross_profit_share.max(0.75)
        && top5_profit_share_14d <= config.max_top5_profit_share.min(0.55)
        && high_price_profit_share_14d <= config.max_high_price_profit_share.min(0.35)
        && active_loss_ratio <= config.max_active_loss_ratio.min(0.05)
}

fn is_sports_league_candidate(
    specialty_segments: &[SpecialtySegmentMetrics],
    domain_14d: &WindowProfitMetrics,
    domain_30d: &WindowProfitMetrics,
    domain_profit_share_14d: f64,
    high_price_profit_share_14d: f64,
    active_loss_ratio: f64,
    config: &LeaderboardRecruitmentConfig,
) -> bool {
    if !config.domain.eq_ignore_ascii_case("SPORTS") {
        return false;
    }

    let Some(best) = specialty_segments.first() else {
        return false;
    };

    let league_sample_ok = best.positions_30d >= 5;
    let league_edge_ok =
        best.pnl_usd_30d >= 500.0 && best.roi_30d >= 0.05 && best.gross_profit_share_30d >= 0.35;
    let not_late_certainty =
        best.high_price_profit_share_30d <= 0.55 && high_price_profit_share_14d <= 0.60;
    let not_single_hit = best.top5_profit_share_30d <= 0.90;
    let recent_or_rolling_edge = domain_14d.pnl_usd > 0.0 || domain_30d.pnl_usd >= 1_500.0;
    let risk_ok = active_loss_ratio <= config.max_active_loss_ratio.min(0.20);
    let enough_sports_focus = domain_profit_share_14d >= 0.35 || best.pnl_usd_30d >= 2_000.0;

    league_sample_ok
        && league_edge_ok
        && not_late_certainty
        && not_single_hit
        && recent_or_rolling_edge
        && risk_ok
        && enough_sports_focus
}

fn candidate_tags(
    domain: &str,
    rising_star_candidate: bool,
    sports_league_candidate: bool,
    specialty_segments: &[SpecialtySegmentMetrics],
    lifetime_pnl_usd: Option<f64>,
    config: &LeaderboardRecruitmentConfig,
) -> Vec<String> {
    let mut tags = Vec::new();
    if rising_star_candidate {
        tags.push("rising_star".to_owned());
    }
    if sports_league_candidate {
        if let Some(best) = specialty_segments.first() {
            tags.push(format!("league_specialist:{}", best.name));
            if best.avg_position_usd_30d >= 1_000.0
                || best.max_position_usd_30d >= 5_000.0
                || lifetime_pnl_usd
                    .map(|pnl| pnl > config.max_lifetime_pnl_usd)
                    .unwrap_or(false)
            {
                tags.push("followable_whale".to_owned());
            }
        }
    }
    if domain.eq_ignore_ascii_case("CRYPTO") {
        tags.push("ordinary_crypto_only".to_owned());
    }
    tags
}

fn is_special_observation(
    candidate: &LeaderboardEmployeeCandidate,
    config: &LeaderboardRecruitmentConfig,
) -> bool {
    if candidate.eligible {
        return false;
    }
    if candidate.domain_30d.positions < 10 || candidate.domain_30d.pnl_usd <= 0.0 {
        return false;
    }

    let high_frequency =
        candidate.total_30d.positions > effective_max_monthly_positions(&config.domain, config);
    let low_frequency = candidate.total_30d.positions < config.min_monthly_positions;
    let concentrated = candidate.top5_profit_share_14d > config.max_top5_profit_share;
    let lifetime_outlier = candidate
        .lifetime_pnl_usd
        .map(|pnl| pnl > config.max_lifetime_pnl_usd)
        .unwrap_or(false);

    let exceptional_roi = candidate.domain_30d.roi >= 0.25 || candidate.domain_14d.roi >= 0.25;
    let strong_pnl =
        candidate.domain_30d.pnl_usd >= 1_000.0 || candidate.domain_14d.pnl_usd >= 1_000.0;
    let high_frequency_edge = high_frequency && candidate.domain_30d.roi >= 0.10 && strong_pnl;
    let lottery_like_edge = concentrated && exceptional_roi && strong_pnl;

    (high_frequency_edge || lottery_like_edge || (low_frequency && exceptional_roi && strong_pnl))
        || (lifetime_outlier && exceptional_roi && strong_pnl)
}

fn compare_candidates(
    left: &LeaderboardEmployeeCandidate,
    right: &LeaderboardEmployeeCandidate,
) -> std::cmp::Ordering {
    right
        .eligible
        .cmp(&left.eligible)
        .then_with(|| right.score.total_cmp(&left.score))
        .then_with(|| right.domain_14d.pnl_usd.total_cmp(&left.domain_14d.pnl_usd))
        .then_with(|| left.wallet.cmp(&right.wallet))
}

fn compare_ranked_wallets(
    left: &RankedWallet,
    right: &RankedWallet,
    config: &LeaderboardRecruitmentConfig,
) -> std::cmp::Ordering {
    lifetime_pnl_priority(left, config)
        .cmp(&lifetime_pnl_priority(right, config))
        .then_with(|| week_month_presence(right).cmp(&week_month_presence(left)))
        .then_with(|| right.ranks.len().cmp(&left.ranks.len()))
        .then_with(|| best_non_day_rank(left).cmp(&best_non_day_rank(right)))
        .then_with(|| best_rank(left).cmp(&best_rank(right)))
        .then_with(|| rank_sum(left).cmp(&rank_sum(right)))
        .then_with(|| left.entry.proxy_wallet.cmp(&right.entry.proxy_wallet))
}

fn lifetime_pnl_priority(wallet: &RankedWallet, config: &LeaderboardRecruitmentConfig) -> usize {
    match wallet.leaderboard_pnls.get("ALL").copied() {
        Some(pnl) if pnl >= config.min_lifetime_pnl_usd && pnl <= config.max_lifetime_pnl_usd => 0,
        Some(pnl) if pnl > 0.0 => 1,
        _ => 2,
    }
}

fn week_month_presence(wallet: &RankedWallet) -> usize {
    ["WEEK", "MONTH"]
        .iter()
        .filter(|period| wallet.ranks.contains_key(**period))
        .count()
}

fn best_non_day_rank(wallet: &RankedWallet) -> usize {
    ["WEEK", "MONTH", "ALL"]
        .iter()
        .filter_map(|period| wallet.ranks.get(*period).copied())
        .min()
        .unwrap_or_else(|| best_rank(wallet))
}

fn best_rank(wallet: &RankedWallet) -> usize {
    wallet.ranks.values().copied().min().unwrap_or(usize::MAX)
}

fn rank_sum(wallet: &RankedWallet) -> usize {
    wallet.ranks.values().copied().sum()
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

fn ratio(numerator: f64, denominator: f64) -> f64 {
    if denominator > 0.0 {
        numerator / denominator
    } else {
        0.0
    }
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

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

fn retry_request<T, F>(attempts: usize, mut operation: F) -> Result<T, PolymarketError>
where
    F: FnMut() -> Result<T, PolymarketError>,
{
    let attempts = attempts.max(1);
    let mut last_error = None;

    for attempt in 0..attempts {
        match operation() {
            Ok(value) => return Ok(value),
            Err(error) => last_error = Some(error),
        }

        if attempt + 1 < attempts {
            sleep(Duration::from_secs(1));
        }
    }

    Err(last_error.expect("at least one request attempt"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn closed(title: &str, days_ago: u64, pnl: f64, bought: f64, avg_price: f64) -> ClosedPosition {
        let now = 1_800_000_000;
        ClosedPosition {
            proxy_wallet: "0x1".to_owned(),
            asset: None,
            condition_id: None,
            avg_price: Some(avg_price),
            total_bought: Some(bought),
            realized_pnl: Some(pnl),
            cur_price: None,
            timestamp: Some(now - days_ago * SECONDS_PER_DAY),
            title: Some(title.to_owned()),
            slug: None,
            event_slug: None,
            outcome: None,
            outcome_index: None,
            opposite_outcome: None,
            opposite_asset: None,
            end_date: None,
        }
    }

    fn ranked() -> RankedWallet {
        ranked_with_all_pnl(50_000.0)
    }

    fn ranked_with_all_pnl(all_pnl: f64) -> RankedWallet {
        RankedWallet {
            entry: LeaderboardEntry {
                rank: Some("13".to_owned()),
                proxy_wallet: "0x1".to_owned(),
                user_name: Some("weather-worker".to_owned()),
                vol: None,
                pnl: None,
                x_username: None,
                verified_badge: None,
            },
            ranks: BTreeMap::from([("MONTH".to_owned(), 13)]),
            leaderboard_pnls: BTreeMap::from([("ALL".to_owned(), all_pnl)]),
        }
    }

    #[test]
    fn profitable_focused_wallet_passes() {
        let positions = (0..32)
            .map(|index| {
                closed(
                    "Highest temperature in London today?",
                    index,
                    25.0,
                    200.0,
                    0.55,
                )
            })
            .collect::<Vec<_>>();
        let candidate = evaluate_ranked_wallet(
            &ranked(),
            &positions,
            &[],
            &domain_keywords("WEATHER"),
            &LeaderboardRecruitmentConfig::default(),
            1_800_000_000,
            false,
        );

        assert!(candidate.eligible, "{:?}", candidate.flags);
        assert_eq!(candidate.domain_14d.positions, 15);
        assert_eq!(candidate.total_30d.positions, 31);
        assert_eq!(candidate.domain_gross_profit_share_14d, 1.0);
        assert_eq!(candidate.high_price_profit_share_14d, 0.0);
    }

    #[test]
    fn rising_weather_star_passes_despite_low_lifetime_pnl() {
        let positions = (0..120)
            .map(|index| {
                closed(
                    "Highest temperature in London today?",
                    index % 30,
                    15.0,
                    250.0,
                    0.45,
                )
            })
            .collect::<Vec<_>>();
        let candidate = evaluate_ranked_wallet(
            &ranked_with_all_pnl(4_000.0),
            &positions,
            &[],
            &domain_keywords("WEATHER"),
            &LeaderboardRecruitmentConfig::default(),
            1_800_000_000,
            false,
        );

        assert!(candidate.eligible, "{:?}", candidate.flags);
        assert_eq!(candidate.lifetime_pnl_usd, Some(4_000.0));
        assert!(!candidate.flags.contains(&"lifetime_pnl_too_low".to_owned()));
    }

    #[test]
    fn sports_league_specialist_can_pass_as_followable_whale() {
        let positions = (0..8)
            .map(|index| {
                closed(
                    "NBA game winner - Knicks vs Celtics",
                    index,
                    100.0,
                    1_200.0,
                    0.55,
                )
            })
            .collect::<Vec<_>>();
        let mut config = LeaderboardRecruitmentConfig {
            domain: "SPORTS".to_owned(),
            ..LeaderboardRecruitmentConfig::default()
        };
        config.min_monthly_positions = 30;
        config.max_lifetime_pnl_usd = 400_000.0;

        let candidate = evaluate_ranked_wallet(
            &ranked_with_all_pnl(800_000.0),
            &positions,
            &[],
            &domain_keywords("SPORTS"),
            &config,
            1_800_000_000,
            false,
        );

        assert!(candidate.eligible, "{:?}", candidate.flags);
        assert!(candidate
            .candidate_tags
            .contains(&"league_specialist:NBA".to_owned()));
        assert!(candidate
            .candidate_tags
            .contains(&"followable_whale".to_owned()));
    }

    #[test]
    fn high_price_tail_profit_is_rejected() {
        let mut positions = (0..8)
            .map(|index| closed("Weather in NYC today?", index, 10.0, 100.0, 0.90))
            .collect::<Vec<_>>();
        positions.extend(
            (0..4).map(|index| closed("Weather in London today?", index, 2.0, 100.0, 0.55)),
        );
        let candidate = evaluate_ranked_wallet(
            &ranked(),
            &positions,
            &[],
            &domain_keywords("WEATHER"),
            &LeaderboardRecruitmentConfig::default(),
            1_800_000_000,
            false,
        );

        assert!(!candidate.eligible);
        assert!(candidate
            .flags
            .contains(&"high_price_profit_dependency".to_owned()));
    }

    #[test]
    fn redeemable_positions_are_not_active_risk() {
        let position = CurrentPosition {
            proxy_wallet: "0x1".to_owned(),
            asset: None,
            condition_id: None,
            size: Some(100.0),
            avg_price: Some(0.9),
            initial_value: Some(90.0),
            current_value: Some(0.0),
            cash_pnl: Some(-90.0),
            percent_pnl: Some(-100.0),
            total_bought: Some(100.0),
            realized_pnl: None,
            cur_price: Some(0.0),
            redeemable: Some(true),
            mergeable: Some(false),
            title: None,
            slug: None,
            event_slug: None,
            outcome: None,
            outcome_index: None,
            opposite_outcome: None,
            end_date: None,
        };

        assert!(!is_active_position(&position));
    }

    #[test]
    fn crypto_five_ten_and_fifteen_minute_markets_are_ultra_fast() {
        for title in [
            "Bitcoin Up or Down - June 15, 3:25AM-3:30AM ET",
            "Ethereum Up or Down - June 15, 3:20AM-3:30AM ET",
            "Bitcoin Up or Down - June 15, 3:30AM-3:45AM ET",
            "BTC Up or Down 10m",
        ] {
            assert!(is_ultra_fast_crypto_position(&closed(
                title, 0, 10.0, 100.0, 0.55
            )));
        }
    }

    #[test]
    fn longer_crypto_and_short_sports_markets_are_not_ultra_fast_crypto() {
        assert!(!is_ultra_fast_crypto_position(&closed(
            "Bitcoin Up or Down - June 15, 3:00AM-3:30AM ET",
            0,
            10.0,
            100.0,
            0.55,
        )));
        assert!(!is_ultra_fast_crypto_position(&closed(
            "NBA game winner - June 15, 3:25AM-3:30AM ET",
            0,
            10.0,
            100.0,
            0.55,
        )));
    }

    #[test]
    fn ultra_fast_crypto_profit_does_not_qualify_ordinary_employee() {
        let mut positions = (0..20)
            .map(|index| {
                closed(
                    "Bitcoin Up or Down - June 15, 3:30AM-3:35AM ET",
                    index % 7,
                    20.0,
                    100.0,
                    0.55,
                )
            })
            .collect::<Vec<_>>();
        positions.extend((0..8).map(|index| {
            closed(
                "Will Bitcoin be above $100k on Friday?",
                index,
                -5.0,
                100.0,
                0.55,
            )
        }));
        let mut config = LeaderboardRecruitmentConfig::default();
        config.domain = "CRYPTO".to_owned();

        let candidate = evaluate_ranked_wallet(
            &ranked(),
            &positions,
            &[],
            &domain_keywords("CRYPTO"),
            &config,
            1_800_000_000,
            false,
        );

        assert!(!candidate.eligible);
        assert_eq!(candidate.ultra_fast_14d.positions, 20);
        assert_eq!(candidate.domain_14d.positions, 8);
        assert!(candidate.domain_14d.pnl_usd < 0.0);
        assert_eq!(candidate.ultra_fast_gross_profit_share_14d, 1.0);
    }
}
