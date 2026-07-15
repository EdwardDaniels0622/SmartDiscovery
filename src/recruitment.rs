use crate::{
    discovery::{derive_metrics, SimpleWalletMetrics},
    polymarket::{ClosedPosition, PolymarketDataClient, PolymarketError, UserTrade},
};
use serde::Serialize;
use std::{
    collections::{HashMap, HashSet},
    thread::sleep,
    time::Duration,
};

const DEFAULT_LOOKAHEAD_SECONDS: u64 = 30 * 60;
const DEFAULT_MIN_COPY_WINDOW_SECONDS: u64 = 20;
const DEFAULT_VETTING_POOL_MULTIPLIER: usize = 5;
const DEFAULT_REJECTED_WALLETS: &[&str] = &[
    // Large current/recent losses with superficially positive closed-position samples.
    "0x06dc51826bc524d9a83770e7de9dd7e005b04524",
];

#[derive(Debug, Clone)]
pub struct TapeRecruitmentConfig {
    pub domains: Vec<RecruitmentDomain>,
    pub trade_pages: usize,
    pub trade_page_size: usize,
    pub lookahead_seconds: u64,
    pub min_copy_window_seconds: u64,
    pub min_entry_price: f64,
    pub max_entry_price: f64,
    pub min_source_notional_usd: f64,
    pub max_source_notional_usd: f64,
    pub min_later_trades: usize,
    pub min_tape_move: f64,
    pub min_candidate_trades: usize,
    pub min_candidate_score: f64,
    pub top: usize,
    pub include_fast_markets: bool,
    pub exclude_keywords: Vec<String>,
    pub wallet_vetting: bool,
    pub closed_position_pages: usize,
    pub closed_position_page_size: usize,
    pub recent_window_days: u64,
    pub min_wallet_closed_positions: usize,
    pub min_wallet_realized_pnl_usd: f64,
    pub min_wallet_realized_roi: f64,
    pub min_recent_closed_positions_for_health: usize,
    pub min_wallet_recent_pnl_usd: f64,
    pub min_wallet_recent_roi: f64,
    pub max_wallet_current_loss_usd: f64,
    pub max_wallet_current_loss_ratio: f64,
    pub max_wallet_two_sided_condition_ratio: f64,
    pub rejected_wallets: Vec<String>,
    pub candidate_vet_pause_ms: u64,
}

impl Default for TapeRecruitmentConfig {
    fn default() -> Self {
        Self {
            domains: default_recruitment_domains(),
            trade_pages: 5,
            trade_page_size: 100,
            lookahead_seconds: DEFAULT_LOOKAHEAD_SECONDS,
            min_copy_window_seconds: DEFAULT_MIN_COPY_WINDOW_SECONDS,
            min_entry_price: 0.01,
            max_entry_price: 0.75,
            min_source_notional_usd: 5.0,
            max_source_notional_usd: 1_000.0,
            min_later_trades: 1,
            min_tape_move: 0.015,
            min_candidate_trades: 2,
            min_candidate_score: 60.0,
            top: 10,
            include_fast_markets: false,
            exclude_keywords: default_exclude_keywords(),
            wallet_vetting: true,
            closed_position_pages: 2,
            closed_position_page_size: 50,
            recent_window_days: 30,
            min_wallet_closed_positions: 3,
            min_wallet_realized_pnl_usd: 0.0,
            min_wallet_realized_roi: 0.0,
            min_recent_closed_positions_for_health: 3,
            min_wallet_recent_pnl_usd: 0.0,
            min_wallet_recent_roi: 0.0,
            max_wallet_current_loss_usd: 5_000.0,
            max_wallet_current_loss_ratio: 0.20,
            max_wallet_two_sided_condition_ratio: 0.25,
            rejected_wallets: DEFAULT_REJECTED_WALLETS
                .iter()
                .map(|wallet| (*wallet).to_owned())
                .collect(),
            candidate_vet_pause_ms: 120,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct RecruitmentDomain {
    pub name: String,
    pub keywords: Vec<String>,
}

impl RecruitmentDomain {
    pub fn new(name: &str, keywords: &[&str]) -> Self {
        Self {
            name: name.to_owned(),
            keywords: keywords
                .iter()
                .map(|keyword| (*keyword).to_owned())
                .collect(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct TapeRecruitmentReport {
    pub generated_at_secs: u64,
    pub scanned_trades: usize,
    pub evaluated_trades: usize,
    pub qualified_trades: usize,
    pub candidates: Vec<TapeEmployeeCandidate>,
    pub rejected_candidates: Vec<TapeCandidateRejection>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TapeEmployeeCandidate {
    pub wallet: String,
    pub name: Option<String>,
    pub pseudonym: Option<String>,
    pub domain: String,
    pub keywords: Vec<String>,
    pub score: f64,
    pub evaluated_trades: usize,
    pub qualified_trades: usize,
    pub positive_move_rate: f64,
    pub avg_entry_price: f64,
    pub avg_tape_move: f64,
    pub median_source_notional_usd: f64,
    pub avg_copy_window_seconds: f64,
    pub last_seen_secs: Option<u64>,
    pub watch_spec: String,
    pub wallet_health: Option<TapeCandidateWalletHealth>,
    pub reasons: Vec<String>,
    pub cautions: Vec<String>,
    pub examples: Vec<TapeTradeSignal>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TapeCandidateWalletHealth {
    pub closed_positions: usize,
    pub realized_pnl_usd: f64,
    pub invested_usd: f64,
    pub realized_roi: f64,
    pub recent_window_days: u64,
    pub recent_closed_positions: usize,
    pub recent_pnl_usd: f64,
    pub recent_roi: f64,
    pub current_positions: usize,
    pub current_cash_pnl_usd: f64,
    pub current_loss_usd: f64,
    pub current_initial_value_usd: f64,
    pub current_loss_ratio: f64,
    pub worst_current_position_pnl_usd: f64,
    pub max_drawdown_ratio: f64,
    pub recent_loss_streak: usize,
    pub two_sided_condition_ratio: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct TapeCandidateRejection {
    pub wallet: String,
    pub name: Option<String>,
    pub pseudonym: Option<String>,
    pub domain: String,
    pub score: f64,
    pub evaluated_trades: usize,
    pub qualified_trades: usize,
    pub wallet_health: Option<TapeCandidateWalletHealth>,
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TapeTradeSignal {
    pub wallet: String,
    pub name: Option<String>,
    pub pseudonym: Option<String>,
    pub domain: String,
    pub title: Option<String>,
    pub slug: Option<String>,
    pub outcome: Option<String>,
    pub price: f64,
    pub later_price: f64,
    pub tape_move: f64,
    pub size: f64,
    pub notional_usd: f64,
    pub timestamp: Option<u64>,
    pub seconds_to_later_price: u64,
    pub later_trade_count: usize,
    pub quality_score: f64,
    pub qualified: bool,
    pub transaction_hash: Option<String>,
}

#[derive(Debug)]
struct CandidateAccumulator {
    wallet: String,
    name: Option<String>,
    pseudonym: Option<String>,
    domain: RecruitmentDomain,
    evaluated_trades: usize,
    qualified_trades: usize,
    positive_moves: usize,
    entry_prices: Vec<f64>,
    tape_moves: Vec<f64>,
    source_notionals: Vec<f64>,
    copy_windows: Vec<u64>,
    quality_scores: Vec<f64>,
    last_seen_secs: Option<u64>,
    examples: Vec<TapeTradeSignal>,
}

pub fn default_recruitment_domains() -> Vec<RecruitmentDomain> {
    vec![
        RecruitmentDomain::new(
            "WEATHER",
            &[
                "weather",
                "temperature",
                "hurricane",
                "storm",
                "rain",
                "snow",
            ],
        ),
        RecruitmentDomain::new(
            "TECH",
            &[
                "ai",
                "gemini",
                "llm",
                "model",
                "arena",
                "openai",
                "anthropic",
                "xai",
                "grok",
                "google",
            ],
        ),
        RecruitmentDomain::new(
            "FINANCE",
            &[
                "fed",
                "rates",
                "cpi",
                "gdp",
                "oil",
                "wti",
                "gold",
                "stock",
                "inflation",
            ],
        ),
        RecruitmentDomain::new(
            "SPORTS",
            &[
                "nba",
                "nfl",
                "mlb",
                "nhl",
                "ufc",
                "soccer",
                "tennis",
                "championship",
            ],
        ),
        RecruitmentDomain::new(
            "CRYPTO",
            &[
                "bitcoin", "btc", "ethereum", "eth", "solana", "sol", "crypto", "etf",
            ],
        ),
        RecruitmentDomain::new(
            "POLITICS",
            &[
                "trump",
                "biden",
                "election",
                "senate",
                "house",
                "president",
                "china",
            ],
        ),
        RecruitmentDomain::new(
            "CULTURE",
            &[
                "twitter",
                "tweet",
                "album",
                "movie",
                "celebrity",
                "streaming",
            ],
        ),
        RecruitmentDomain::new(
            "ECONOMICS",
            &[
                "cpi",
                "fed",
                "gdp",
                "inflation",
                "unemployment",
                "jobs",
                "tariff",
            ],
        ),
    ]
}

pub fn recruit_from_tape(
    client: &PolymarketDataClient,
    config: &TapeRecruitmentConfig,
) -> TapeRecruitmentReport {
    let mut warnings = Vec::new();
    let mut trades = Vec::new();

    for page in 0..config.trade_pages {
        let offset = page * config.trade_page_size;
        match client.global_trades(config.trade_page_size, offset, None) {
            Ok(mut page_trades) => trades.append(&mut page_trades),
            Err(error) => warnings.push(format!(
                "failed to load global trades page {} offset {}: {error}",
                page + 1,
                offset
            )),
        }
    }

    let generated_at_secs = now_secs();
    let candidate_limit = if config.wallet_vetting {
        config
            .top
            .saturating_mul(DEFAULT_VETTING_POOL_MULTIPLIER)
            .max(config.top)
            .max(1)
    } else {
        config.top
    };
    let mut report =
        analyze_tape_trades_with_limit(&trades, config, generated_at_secs, candidate_limit);
    if config.wallet_vetting {
        apply_wallet_vetting(client, &mut report, config);
        report.candidates.truncate(config.top);
    }
    report.warnings.extend(warnings);
    report
}

pub fn analyze_tape_trades(
    trades: &[UserTrade],
    config: &TapeRecruitmentConfig,
    generated_at_secs: u64,
) -> TapeRecruitmentReport {
    analyze_tape_trades_with_limit(trades, config, generated_at_secs, config.top)
}

fn analyze_tape_trades_with_limit(
    trades: &[UserTrade],
    config: &TapeRecruitmentConfig,
    generated_at_secs: u64,
    candidate_limit: usize,
) -> TapeRecruitmentReport {
    let signals = evaluate_trades(trades, config);
    let evaluated_trades = signals.len();
    let qualified_trades = signals.iter().filter(|signal| signal.qualified).count();
    let candidates = build_candidates(signals, config, candidate_limit);
    let mut warnings = Vec::new();

    if evaluated_trades == 0 && !config.include_fast_markets {
        warnings.push(
            "no trades passed the default tape filters; recent tape may be dominated by excluded fast markets. Try more --trade-pages or use --include-fast-markets for diagnostics."
                .to_owned(),
        );
    } else if qualified_trades > 0 && candidates.is_empty() {
        warnings.push(
            "qualified trades were found, but no wallet met candidate thresholds. Lower --min-candidate-score, lower --min-candidate-trades, or sample more pages."
                .to_owned(),
        );
    }

    TapeRecruitmentReport {
        generated_at_secs,
        scanned_trades: trades.len(),
        evaluated_trades,
        qualified_trades,
        candidates,
        rejected_candidates: Vec::new(),
        warnings,
    }
}

fn evaluate_trades(trades: &[UserTrade], config: &TapeRecruitmentConfig) -> Vec<TapeTradeSignal> {
    let mut signals = Vec::new();

    for trade in trades {
        if !trade.side.eq_ignore_ascii_case("BUY") {
            continue;
        }

        let Some(price) = trade.price else {
            continue;
        };
        let Some(size) = trade.size else {
            continue;
        };
        let Some(timestamp) = trade.timestamp else {
            continue;
        };

        if price < config.min_entry_price || price > config.max_entry_price {
            continue;
        }

        let notional_usd = price * size;
        if notional_usd < config.min_source_notional_usd
            || notional_usd > config.max_source_notional_usd
        {
            continue;
        }

        let searchable = searchable_trade_text(trade);
        if !config.include_fast_markets
            && matches_any_keyword(&searchable, &config.exclude_keywords)
        {
            continue;
        }

        let matching_domains = config
            .domains
            .iter()
            .filter(|domain| matches_any_keyword(&searchable, &domain.keywords))
            .collect::<Vec<_>>();

        if matching_domains.is_empty() {
            continue;
        }

        let Some(later) = find_later_tape(trade, trades, config) else {
            continue;
        };

        let tape_move = later.price - price;
        let qualified = tape_move >= config.min_tape_move
            && later.seconds_after >= config.min_copy_window_seconds;
        let quality_score =
            trade_quality_score(price, notional_usd, tape_move, later.seconds_after, config);

        for domain in matching_domains {
            signals.push(TapeTradeSignal {
                wallet: trade.proxy_wallet.clone(),
                name: clean_optional_text(trade.name.as_deref()),
                pseudonym: clean_optional_text(trade.pseudonym.as_deref()),
                domain: domain.name.clone(),
                title: trade.title.clone(),
                slug: trade.slug.clone(),
                outcome: trade.outcome.clone(),
                price: round4(price),
                later_price: round4(later.price),
                tape_move: round4(tape_move),
                size: round4(size),
                notional_usd: round2(notional_usd),
                timestamp: Some(timestamp),
                seconds_to_later_price: later.seconds_after,
                later_trade_count: later.trade_count,
                quality_score: round1(quality_score),
                qualified,
                transaction_hash: trade.transaction_hash.clone(),
            });
        }
    }

    signals
}

fn build_candidates(
    signals: Vec<TapeTradeSignal>,
    config: &TapeRecruitmentConfig,
    candidate_limit: usize,
) -> Vec<TapeEmployeeCandidate> {
    let mut accumulators: HashMap<String, CandidateAccumulator> = HashMap::new();

    for signal in signals {
        let key = format!("{}:{}", signal.wallet.to_lowercase(), signal.domain);
        let domain = config
            .domains
            .iter()
            .find(|domain| domain.name == signal.domain)
            .cloned()
            .unwrap_or_else(|| RecruitmentDomain {
                name: signal.domain.clone(),
                keywords: Vec::new(),
            });

        let accumulator = accumulators
            .entry(key)
            .or_insert_with(|| CandidateAccumulator {
                wallet: signal.wallet.clone(),
                name: signal.name.clone(),
                pseudonym: signal.pseudonym.clone(),
                domain,
                evaluated_trades: 0,
                qualified_trades: 0,
                positive_moves: 0,
                entry_prices: Vec::new(),
                tape_moves: Vec::new(),
                source_notionals: Vec::new(),
                copy_windows: Vec::new(),
                quality_scores: Vec::new(),
                last_seen_secs: None,
                examples: Vec::new(),
            });

        accumulator.evaluated_trades += 1;
        if signal.tape_move > 0.0 {
            accumulator.positive_moves += 1;
        }
        accumulator.last_seen_secs = max_option(accumulator.last_seen_secs, signal.timestamp);

        if signal.qualified {
            accumulator.qualified_trades += 1;
            accumulator.entry_prices.push(signal.price);
            accumulator.tape_moves.push(signal.tape_move);
            accumulator.source_notionals.push(signal.notional_usd);
            accumulator.copy_windows.push(signal.seconds_to_later_price);
            accumulator.quality_scores.push(signal.quality_score);
            accumulator.examples.push(signal);
        }
    }

    let mut candidates = accumulators
        .into_values()
        .filter_map(|mut accumulator| {
            if accumulator.qualified_trades < config.min_candidate_trades {
                return None;
            }

            accumulator.examples.sort_by(|left, right| {
                right
                    .quality_score
                    .total_cmp(&left.quality_score)
                    .then_with(|| right.tape_move.total_cmp(&left.tape_move))
            });
            accumulator.examples.truncate(3);

            let avg_quality = average(&accumulator.quality_scores);
            let positive_move_rate = if accumulator.evaluated_trades > 0 {
                accumulator.positive_moves as f64 / accumulator.evaluated_trades as f64
            } else {
                0.0
            };
            let repeatability_score = clamp01(accumulator.qualified_trades as f64 / 6.0) * 100.0;
            let score = round1(
                (avg_quality * 0.60)
                    + (positive_move_rate * 100.0 * 0.25)
                    + (repeatability_score * 0.15),
            );

            if score < config.min_candidate_score {
                return None;
            }

            let avg_entry_price = average(&accumulator.entry_prices);
            let avg_tape_move = average(&accumulator.tape_moves);
            let median_source_notional_usd = median(accumulator.source_notionals.clone());
            let avg_copy_window_seconds = average_u64(&accumulator.copy_windows);
            let watch_spec = build_watch_spec(
                &accumulator.wallet,
                accumulator
                    .name
                    .as_deref()
                    .or(accumulator.pseudonym.as_deref()),
                &accumulator.domain,
                accumulator.qualified_trades,
                median_source_notional_usd,
                avg_copy_window_seconds,
            );
            let reasons = build_candidate_reasons(
                &accumulator,
                positive_move_rate,
                avg_tape_move,
                avg_copy_window_seconds,
            );
            let cautions = build_candidate_cautions(
                &accumulator,
                positive_move_rate,
                avg_entry_price,
                median_source_notional_usd,
            );

            Some(TapeEmployeeCandidate {
                wallet: accumulator.wallet,
                name: accumulator.name,
                pseudonym: accumulator.pseudonym,
                domain: accumulator.domain.name,
                keywords: accumulator.domain.keywords,
                score,
                evaluated_trades: accumulator.evaluated_trades,
                qualified_trades: accumulator.qualified_trades,
                positive_move_rate: round4(positive_move_rate),
                avg_entry_price: round4(avg_entry_price),
                avg_tape_move: round4(avg_tape_move),
                median_source_notional_usd: round2(median_source_notional_usd),
                avg_copy_window_seconds: round1(avg_copy_window_seconds),
                last_seen_secs: accumulator.last_seen_secs,
                watch_spec,
                wallet_health: None,
                reasons,
                cautions,
                examples: accumulator.examples,
            })
        })
        .collect::<Vec<_>>();

    candidates.sort_by(|left, right| {
        right
            .score
            .total_cmp(&left.score)
            .then_with(|| right.qualified_trades.cmp(&left.qualified_trades))
            .then_with(|| right.avg_tape_move.total_cmp(&left.avg_tape_move))
    });
    candidates.truncate(candidate_limit);
    candidates
}

fn apply_wallet_vetting(
    client: &PolymarketDataClient,
    report: &mut TapeRecruitmentReport,
    config: &TapeRecruitmentConfig,
) {
    let rejected_wallets = config
        .rejected_wallets
        .iter()
        .map(|wallet| wallet.trim().to_lowercase())
        .filter(|wallet| !wallet.is_empty())
        .collect::<HashSet<_>>();
    let mut accepted = Vec::new();
    let mut rejected = Vec::new();
    let mut rejection_warning_count = 0usize;

    let candidates = std::mem::take(&mut report.candidates);

    for mut candidate in candidates {
        if rejected_wallets.contains(&candidate.wallet.to_lowercase()) {
            let reasons = vec!["wallet is on the recruitment reject list".to_owned()];
            push_rejection_warning(report, &candidate, &reasons, &mut rejection_warning_count);
            rejected.push(candidate_rejection(candidate, None, reasons));
            continue;
        }

        match load_candidate_wallet_health(
            client,
            &candidate.wallet,
            config,
            report.generated_at_secs,
        ) {
            Ok(health) => {
                let reasons = wallet_health_rejection_reasons(&health, config);
                if reasons.is_empty() {
                    append_wallet_health_notes(&mut candidate, &health, config);
                    candidate.wallet_health = Some(health);
                    accepted.push(candidate);
                } else {
                    push_rejection_warning(
                        report,
                        &candidate,
                        &reasons,
                        &mut rejection_warning_count,
                    );
                    rejected.push(candidate_rejection(candidate, Some(health), reasons));
                }
            }
            Err(error) => {
                let reasons = vec![format!("wallet health unavailable: {error}")];
                push_rejection_warning(report, &candidate, &reasons, &mut rejection_warning_count);
                rejected.push(candidate_rejection(candidate, None, reasons));
            }
        }

        if config.candidate_vet_pause_ms > 0 {
            sleep(Duration::from_millis(config.candidate_vet_pause_ms));
        }
    }

    if !rejected.is_empty() {
        report.warnings.push(format!(
            "wallet vetting rejected {} tape candidate(s); inspect rejected_candidates in JSON for details",
            rejected.len()
        ));
    }
    if accepted.is_empty() && !rejected.is_empty() {
        report.warnings.push(
            "all tape candidates were rejected by wallet health checks; keep collecting tape or relax thresholds only for diagnostics"
                .to_owned(),
        );
    }

    report.candidates = accepted;
    report.rejected_candidates = rejected;
}

fn push_rejection_warning(
    report: &mut TapeRecruitmentReport,
    candidate: &TapeEmployeeCandidate,
    reasons: &[String],
    warning_count: &mut usize,
) {
    if *warning_count >= 8 {
        return;
    }

    report.warnings.push(format!(
        "rejected {} {}: {}",
        candidate.domain,
        candidate.wallet,
        reasons.join("; ")
    ));
    *warning_count += 1;
}

fn candidate_rejection(
    candidate: TapeEmployeeCandidate,
    wallet_health: Option<TapeCandidateWalletHealth>,
    reasons: Vec<String>,
) -> TapeCandidateRejection {
    TapeCandidateRejection {
        wallet: candidate.wallet,
        name: candidate.name,
        pseudonym: candidate.pseudonym,
        domain: candidate.domain,
        score: candidate.score,
        evaluated_trades: candidate.evaluated_trades,
        qualified_trades: candidate.qualified_trades,
        wallet_health,
        reasons,
    }
}

fn load_candidate_wallet_health(
    client: &PolymarketDataClient,
    wallet: &str,
    config: &TapeRecruitmentConfig,
    now_secs: u64,
) -> Result<TapeCandidateWalletHealth, PolymarketError> {
    let closed_positions = load_closed_positions_for_wallet(client, wallet, config)?;
    let current_positions = client.positions(wallet, 50, 0)?;
    let metrics = derive_metrics(
        &closed_positions,
        &current_positions,
        now_secs,
        config.recent_window_days,
    );
    let two_sided_condition_ratio = closed_two_sided_condition_ratio(&closed_positions);

    Ok(wallet_health_from_metrics(
        &metrics,
        two_sided_condition_ratio,
    ))
}

fn load_closed_positions_for_wallet(
    client: &PolymarketDataClient,
    wallet: &str,
    config: &TapeRecruitmentConfig,
) -> Result<Vec<ClosedPosition>, PolymarketError> {
    let mut positions = Vec::new();

    for page in 0..config.closed_position_pages {
        let offset = page * config.closed_position_page_size;
        let page_positions =
            client.closed_positions(wallet, config.closed_position_page_size, offset)?;
        let page_len = page_positions.len();
        positions.extend(page_positions);

        if page_len < config.closed_position_page_size {
            break;
        }
    }

    Ok(positions)
}

fn wallet_health_from_metrics(
    metrics: &SimpleWalletMetrics,
    two_sided_condition_ratio: f64,
) -> TapeCandidateWalletHealth {
    TapeCandidateWalletHealth {
        closed_positions: metrics.closed_positions,
        realized_pnl_usd: metrics.realized_pnl_usd,
        invested_usd: metrics.invested_usd,
        realized_roi: metrics.realized_roi,
        recent_window_days: metrics.recent_window_days,
        recent_closed_positions: metrics.recent_closed_positions,
        recent_pnl_usd: metrics.recent_pnl_usd,
        recent_roi: metrics.recent_roi,
        current_positions: metrics.current_positions,
        current_cash_pnl_usd: metrics.current_cash_pnl_usd,
        current_loss_usd: metrics.current_loss_usd,
        current_initial_value_usd: metrics.current_initial_value_usd,
        current_loss_ratio: metrics.current_loss_ratio,
        worst_current_position_pnl_usd: metrics.worst_current_position_pnl_usd,
        max_drawdown_ratio: metrics.max_drawdown_ratio,
        recent_loss_streak: metrics.recent_loss_streak,
        two_sided_condition_ratio: round4(two_sided_condition_ratio),
    }
}

fn wallet_health_rejection_reasons(
    health: &TapeCandidateWalletHealth,
    config: &TapeRecruitmentConfig,
) -> Vec<String> {
    let mut reasons = Vec::new();

    if health.closed_positions < config.min_wallet_closed_positions {
        reasons.push(format!(
            "wallet sample too thin: {} closed positions",
            health.closed_positions
        ));
    }
    if health.realized_pnl_usd < config.min_wallet_realized_pnl_usd {
        reasons.push(format!(
            "wallet realized pnl below threshold: ${:.2}",
            health.realized_pnl_usd
        ));
    }
    if health.realized_roi < config.min_wallet_realized_roi {
        reasons.push(format!(
            "wallet realized roi below threshold: {:.1}%",
            health.realized_roi * 100.0
        ));
    }
    if health.recent_closed_positions >= config.min_recent_closed_positions_for_health
        && health.recent_pnl_usd < config.min_wallet_recent_pnl_usd
    {
        reasons.push(format!(
            "recent {}d pnl below threshold: ${:.2}",
            health.recent_window_days, health.recent_pnl_usd
        ));
    }
    if health.recent_closed_positions >= config.min_recent_closed_positions_for_health
        && health.recent_roi < config.min_wallet_recent_roi
    {
        reasons.push(format!(
            "recent {}d roi below threshold: {:.1}%",
            health.recent_window_days,
            health.recent_roi * 100.0
        ));
    }
    if health.current_loss_usd > config.max_wallet_current_loss_usd {
        reasons.push(format!(
            "current/open loss too large: ${:.2}",
            health.current_loss_usd
        ));
    }
    if health.current_loss_ratio > config.max_wallet_current_loss_ratio {
        reasons.push(format!(
            "current/open loss ratio too high: {:.1}%",
            health.current_loss_ratio * 100.0
        ));
    }
    if health.closed_positions >= 8
        && health.two_sided_condition_ratio > config.max_wallet_two_sided_condition_ratio
    {
        reasons.push(format!(
            "two-sided closed-position footprint too high: {:.1}%",
            health.two_sided_condition_ratio * 100.0
        ));
    }

    reasons
}

fn append_wallet_health_notes(
    candidate: &mut TapeEmployeeCandidate,
    health: &TapeCandidateWalletHealth,
    config: &TapeRecruitmentConfig,
) {
    candidate.reasons.push(format!(
        "wallet health passed: pnl=${:.2}, roi={:.1}%, recent_{}d_pnl=${:.2}, current_loss=${:.2}",
        health.realized_pnl_usd,
        health.realized_roi * 100.0,
        health.recent_window_days,
        health.recent_pnl_usd,
        health.current_loss_usd
    ));

    if health.recent_closed_positions < config.min_recent_closed_positions_for_health {
        candidate.cautions.push(format!(
            "recent wallet sample is thin: {} closed positions in {}d",
            health.recent_closed_positions, health.recent_window_days
        ));
    }
    if health.current_loss_usd > 0.0 {
        candidate.cautions.push(format!(
            "wallet has current/open loss ${:.2}; keep trial size small",
            health.current_loss_usd
        ));
    }
    if health.two_sided_condition_ratio > 0.0 {
        candidate.cautions.push(format!(
            "wallet has some two-sided closed markets ({:.1}% of sampled conditions)",
            health.two_sided_condition_ratio * 100.0
        ));
    }
}

fn closed_two_sided_condition_ratio(positions: &[ClosedPosition]) -> f64 {
    let mut outcomes_by_condition: HashMap<String, HashSet<String>> = HashMap::new();

    for position in positions {
        let Some(condition_key) = position_condition_key(position) else {
            continue;
        };
        let Some(outcome_key) = position_outcome_key(position) else {
            continue;
        };
        outcomes_by_condition
            .entry(condition_key)
            .or_default()
            .insert(outcome_key);
    }

    if outcomes_by_condition.is_empty() {
        return 0.0;
    }

    let two_sided = outcomes_by_condition
        .values()
        .filter(|outcomes| outcomes.len() >= 2)
        .count();
    two_sided as f64 / outcomes_by_condition.len() as f64
}

fn position_condition_key(position: &ClosedPosition) -> Option<String> {
    position
        .condition_id
        .as_deref()
        .or(position.event_slug.as_deref())
        .or(position.slug.as_deref())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_lowercase())
}

fn position_outcome_key(position: &ClosedPosition) -> Option<String> {
    if let Some(index) = position.outcome_index {
        return Some(format!("index:{index}"));
    }

    position
        .outcome
        .as_deref()
        .or(position.asset.as_deref())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_lowercase())
}

#[derive(Debug, Clone, Copy)]
struct LaterTape {
    price: f64,
    seconds_after: u64,
    trade_count: usize,
}

fn find_later_tape(
    trade: &UserTrade,
    trades: &[UserTrade],
    config: &TapeRecruitmentConfig,
) -> Option<LaterTape> {
    let timestamp = trade.timestamp?;
    let end = timestamp.saturating_add(config.lookahead_seconds);
    let mut latest_timestamp = None;
    let mut latest_price = None;
    let mut trade_count = 0;

    for later in trades {
        if later.condition_id != trade.condition_id || later.asset != trade.asset {
            continue;
        }

        let Some(later_timestamp) = later.timestamp else {
            continue;
        };
        if later_timestamp <= timestamp || later_timestamp > end {
            continue;
        }

        let Some(price) = later.price else {
            continue;
        };

        trade_count += 1;
        if latest_timestamp
            .map(|current| later_timestamp >= current)
            .unwrap_or(true)
        {
            latest_timestamp = Some(later_timestamp);
            latest_price = Some(price);
        }
    }

    if trade_count < config.min_later_trades {
        return None;
    }

    Some(LaterTape {
        price: latest_price?,
        seconds_after: latest_timestamp?.saturating_sub(timestamp),
        trade_count,
    })
}

fn searchable_trade_text(trade: &UserTrade) -> String {
    [
        trade.title.as_deref().unwrap_or(""),
        trade.slug.as_deref().unwrap_or(""),
        trade.event_slug.as_deref().unwrap_or(""),
        trade.outcome.as_deref().unwrap_or(""),
    ]
    .join(" ")
    .to_lowercase()
}

fn matches_any_keyword(text: &str, keywords: &[String]) -> bool {
    keywords
        .iter()
        .map(|keyword| keyword.trim().to_lowercase())
        .filter(|keyword| !keyword.is_empty())
        .any(|keyword| keyword_matches_text(text, &keyword))
}

fn keyword_matches_text(text: &str, keyword: &str) -> bool {
    if keyword.chars().any(char::is_whitespace) {
        return text.contains(keyword);
    }

    let mut search_start = 0;
    while let Some(relative_index) = text[search_start..].find(keyword) {
        let start = search_start + relative_index;
        let end = start + keyword.len();

        if is_keyword_boundary(text, start, end) {
            return true;
        }

        search_start = end;
    }

    false
}

fn is_keyword_boundary(text: &str, start: usize, end: usize) -> bool {
    let before = text[..start].chars().next_back();
    let after = text[end..].chars().next();

    !before.map(is_keyword_char).unwrap_or(false) && !after.map(is_keyword_char).unwrap_or(false)
}

fn is_keyword_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric()
}

fn default_exclude_keywords() -> Vec<String> {
    ["up or down", "updown", "5m", "10m", "15m"]
        .iter()
        .map(|keyword| (*keyword).to_owned())
        .collect()
}

fn trade_quality_score(
    price: f64,
    notional_usd: f64,
    tape_move: f64,
    seconds_after: u64,
    config: &TapeRecruitmentConfig,
) -> f64 {
    let move_score = clamp01((tape_move - config.min_tape_move) / 0.08);
    let entry_score =
        1.0 - clamp01((price - 0.45) / (config.max_entry_price - 0.45).max(0.01)) * 0.45;
    let notional_score = notional_fit_score(notional_usd, config);
    let window_score = clamp01(seconds_after as f64 / config.lookahead_seconds.max(1) as f64);

    ((move_score * 0.50) + (entry_score * 0.20) + (notional_score * 0.15) + (window_score * 0.15))
        * 100.0
}

fn notional_fit_score(notional_usd: f64, config: &TapeRecruitmentConfig) -> f64 {
    if notional_usd < config.min_source_notional_usd {
        return 0.0;
    }

    if (10.0..=250.0).contains(&notional_usd) {
        return 1.0;
    }

    if notional_usd < 10.0 {
        return 0.50 + (notional_usd / 10.0 * 0.50);
    }

    1.0 - clamp01((notional_usd - 250.0) / (config.max_source_notional_usd - 250.0).max(1.0)) * 0.55
}

fn build_watch_spec(
    wallet: &str,
    label: Option<&str>,
    domain: &RecruitmentDomain,
    qualified_trades: usize,
    median_notional: f64,
    avg_copy_window_seconds: f64,
) -> String {
    let label = sanitize_label(label.unwrap_or(wallet));
    let poll_seconds = suggested_poll_seconds(qualified_trades, avg_copy_window_seconds);
    let min_notional = suggested_min_notional(median_notional);
    format!(
        "{}:{}:{}:{}:{}:{:.2}",
        wallet,
        label,
        domain.name,
        domain.keywords.join("|"),
        poll_seconds,
        min_notional
    )
}

fn suggested_poll_seconds(qualified_trades: usize, avg_copy_window_seconds: f64) -> u64 {
    if qualified_trades >= 5 && avg_copy_window_seconds <= 300.0 {
        30
    } else if qualified_trades >= 3 {
        60
    } else {
        120
    }
}

fn suggested_min_notional(median_notional: f64) -> f64 {
    if median_notional <= 0.0 {
        return 10.0;
    }

    round2((median_notional * 0.50).clamp(5.0, 100.0))
}

fn sanitize_label(value: &str) -> String {
    let cleaned = value
        .chars()
        .map(|ch| if ch == ':' { '-' } else { ch })
        .collect::<String>();
    let cleaned = cleaned.trim();

    if cleaned.is_empty() {
        "candidate".to_owned()
    } else {
        cleaned.to_owned()
    }
}

fn build_candidate_reasons(
    accumulator: &CandidateAccumulator,
    positive_move_rate: f64,
    avg_tape_move: f64,
    avg_copy_window_seconds: f64,
) -> Vec<String> {
    vec![
        format!(
            "{} qualified tape signals from {} evaluated domain buys",
            accumulator.qualified_trades, accumulator.evaluated_trades
        ),
        format!(
            "positive move rate {:.1}%, avg post-trade move {:.2}c",
            positive_move_rate * 100.0,
            avg_tape_move * 100.0
        ),
        format!(
            "avg observed copy window {:.0}s before latest same-outcome tape",
            avg_copy_window_seconds
        ),
    ]
}

fn build_candidate_cautions(
    accumulator: &CandidateAccumulator,
    positive_move_rate: f64,
    avg_entry_price: f64,
    median_notional: f64,
) -> Vec<String> {
    let mut cautions = Vec::new();

    if accumulator.evaluated_trades < 5 {
        cautions.push(
            "sample is still thin; keep as trial candidate until observed across days".to_owned(),
        );
    }
    if positive_move_rate < 0.60 {
        cautions
            .push("mixed tape movement; do not treat every BUY as directional signal".to_owned());
    }
    if avg_entry_price > 0.65 {
        cautions.push(
            "average entry is near the upper copyable range; watch for late-certainty behavior"
                .to_owned(),
        );
    }
    if median_notional > 300.0 {
        cautions.push("median source notional is above small-copy sweet spot; check whether entries are still fillable".to_owned());
    }

    cautions
}

fn clean_optional_text(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn max_option(left: Option<u64>, right: Option<u64>) -> Option<u64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.max(right)),
        (Some(left), None) => Some(left),
        (None, Some(right)) => Some(right),
        (None, None) => None,
    }
}

fn average(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }

    values.iter().sum::<f64>() / values.len() as f64
}

fn average_u64(values: &[u64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }

    values.iter().sum::<u64>() as f64 / values.len() as f64
}

fn median(mut values: Vec<f64>) -> f64 {
    if values.is_empty() {
        return 0.0;
    }

    values.sort_by(|left, right| left.total_cmp(right));
    let mid = values.len() / 2;

    if values.len() % 2 == 0 {
        (values[mid - 1] + values[mid]) / 2.0
    } else {
        values[mid]
    }
}

fn clamp01(value: f64) -> f64 {
    value.max(0.0).min(1.0)
}

fn round1(value: f64) -> f64 {
    (value * 10.0).round() / 10.0
}

fn round2(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

fn round4(value: f64) -> f64 {
    (value * 10_000.0).round() / 10_000.0
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trade(
        wallet: &str,
        side: &str,
        price: f64,
        timestamp: u64,
        title: &str,
        tx: &str,
    ) -> UserTrade {
        UserTrade {
            proxy_wallet: wallet.to_owned(),
            side: side.to_owned(),
            asset: "asset-1".to_owned(),
            condition_id: "condition-1".to_owned(),
            size: Some(100.0),
            price: Some(price),
            timestamp: Some(timestamp),
            title: Some(title.to_owned()),
            slug: Some("hong-kong-temperature-test".to_owned()),
            event_slug: Some("hong-kong-temperature-test".to_owned()),
            outcome: Some("Yes".to_owned()),
            outcome_index: Some(0),
            name: Some("Weather Scout".to_owned()),
            pseudonym: Some("Weather Scout".to_owned()),
            transaction_hash: Some(tx.to_owned()),
        }
    }

    fn closed_position(condition_id: &str, outcome_index: u32) -> ClosedPosition {
        ClosedPosition {
            proxy_wallet: "0xworker".to_owned(),
            asset: Some(format!("asset-{condition_id}-{outcome_index}")),
            condition_id: Some(condition_id.to_owned()),
            avg_price: Some(0.45),
            total_bought: Some(100.0),
            realized_pnl: Some(10.0),
            cur_price: Some(1.0),
            timestamp: Some(1_000),
            title: Some("test market".to_owned()),
            slug: Some(condition_id.to_owned()),
            event_slug: Some(condition_id.to_owned()),
            outcome: Some(if outcome_index == 0 { "Yes" } else { "No" }.to_owned()),
            outcome_index: Some(outcome_index),
            opposite_outcome: None,
            opposite_asset: None,
            end_date: None,
        }
    }

    fn healthy_wallet() -> TapeCandidateWalletHealth {
        TapeCandidateWalletHealth {
            closed_positions: 12,
            realized_pnl_usd: 500.0,
            invested_usd: 5_000.0,
            realized_roi: 0.10,
            recent_window_days: 30,
            recent_closed_positions: 4,
            recent_pnl_usd: 120.0,
            recent_roi: 0.06,
            current_positions: 3,
            current_cash_pnl_usd: 0.0,
            current_loss_usd: 0.0,
            current_initial_value_usd: 1_000.0,
            current_loss_ratio: 0.0,
            worst_current_position_pnl_usd: 0.0,
            max_drawdown_ratio: 0.05,
            recent_loss_streak: 0,
            two_sided_condition_ratio: 0.0,
        }
    }

    #[test]
    fn tape_recruitment_promotes_repeated_followable_buys() {
        let mut config = TapeRecruitmentConfig {
            domains: vec![RecruitmentDomain::new("WEATHER", &["temperature"])],
            min_candidate_score: 0.0,
            min_candidate_trades: 2,
            ..TapeRecruitmentConfig::default()
        };
        config.exclude_keywords = Vec::new();

        let trades = vec![
            trade(
                "0xworker",
                "BUY",
                0.34,
                1_000,
                "Hong Kong temperature reaches 31C",
                "0x1",
            ),
            trade(
                "0xlater",
                "BUY",
                0.38,
                1_080,
                "Hong Kong temperature reaches 31C",
                "0x2",
            ),
            trade(
                "0xworker",
                "BUY",
                0.42,
                2_000,
                "Hong Kong temperature reaches 31C",
                "0x3",
            ),
            trade(
                "0xlater",
                "SELL",
                0.47,
                2_100,
                "Hong Kong temperature reaches 31C",
                "0x4",
            ),
        ];

        let report = analyze_tape_trades(&trades, &config, 3_000);

        assert_eq!(report.candidates.len(), 1);
        let candidate = &report.candidates[0];
        assert_eq!(candidate.wallet, "0xworker");
        assert_eq!(candidate.domain, "WEATHER");
        assert_eq!(candidate.qualified_trades, 2);
        assert!(candidate.avg_tape_move > 0.04);
        assert!(candidate.watch_spec.contains(":WEATHER:"));
    }

    #[test]
    fn tape_recruitment_rejects_instant_unfollowable_move() {
        let config = TapeRecruitmentConfig {
            domains: vec![RecruitmentDomain::new("WEATHER", &["temperature"])],
            min_candidate_score: 0.0,
            min_candidate_trades: 1,
            min_copy_window_seconds: 20,
            exclude_keywords: Vec::new(),
            ..TapeRecruitmentConfig::default()
        };
        let trades = vec![
            trade(
                "0xworker",
                "BUY",
                0.34,
                1_000,
                "Hong Kong temperature reaches 31C",
                "0x1",
            ),
            trade(
                "0xlater",
                "BUY",
                0.40,
                1_005,
                "Hong Kong temperature reaches 31C",
                "0x2",
            ),
        ];

        let report = analyze_tape_trades(&trades, &config, 3_000);

        assert!(report.candidates.is_empty());
        assert_eq!(report.evaluated_trades, 1);
        assert_eq!(report.qualified_trades, 0);
    }

    #[test]
    fn keyword_matching_does_not_match_short_terms_inside_words() {
        assert!(!matches_any_keyword(
            "georgia vs. bahrain over under",
            &["ai".to_owned()]
        ));
        assert!(!matches_any_keyword(
            "haiti vs. peru over under",
            &["ai".to_owned()]
        ));
        assert!(matches_any_keyword(
            "will openai release a new model?",
            &["ai".to_owned(), "openai".to_owned()]
        ));
    }

    #[test]
    fn wallet_health_rejects_large_current_loss() {
        let config = TapeRecruitmentConfig::default();
        let mut health = healthy_wallet();
        health.current_loss_usd = 260_970.02;
        health.current_loss_ratio = 0.6383;
        health.worst_current_position_pnl_usd = -40_258.81;

        let reasons = wallet_health_rejection_reasons(&health, &config);

        assert!(reasons
            .iter()
            .any(|reason| reason.contains("current/open loss too large")));
        assert!(reasons
            .iter()
            .any(|reason| reason.contains("current/open loss ratio too high")));
    }

    #[test]
    fn wallet_health_rejects_two_sided_closed_footprint() {
        let config = TapeRecruitmentConfig::default();
        let mut health = healthy_wallet();
        health.two_sided_condition_ratio = 0.50;

        let reasons = wallet_health_rejection_reasons(&health, &config);

        assert!(reasons
            .iter()
            .any(|reason| reason.contains("two-sided closed-position footprint")));
    }

    #[test]
    fn two_sided_condition_ratio_detects_same_market_both_outcomes() {
        let positions = vec![
            closed_position("condition-a", 0),
            closed_position("condition-a", 1),
            closed_position("condition-b", 0),
        ];

        let ratio = closed_two_sided_condition_ratio(&positions);

        assert_eq!(ratio, 0.5);
    }
}
