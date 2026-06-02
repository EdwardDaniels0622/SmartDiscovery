use crate::{
    discovery::WalletEvaluation,
    polymarket::{PolymarketDataClient, UserTrade},
    profile::{
        build_employee_profile, CopySignalLevel, EmployeeProfile, StrategyArchetype,
        TradeProfileSignal,
    },
    telegram::TelegramNotifier,
};
use serde::Serialize;
use std::{
    collections::{HashMap, HashSet},
    io::{self, Write},
    thread::sleep,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[derive(Debug, Clone, Serialize)]
pub struct WatchedEmployee {
    pub wallet: String,
    pub name: Option<String>,
    pub domain: String,
    pub keywords: Vec<String>,
    pub poll_seconds: Option<u64>,
    pub min_notional_usd: Option<f64>,
}

impl WatchedEmployee {
    pub fn from_evaluation(evaluation: &WalletEvaluation) -> Self {
        Self {
            wallet: evaluation.wallet.clone(),
            name: evaluation.user_name.clone(),
            domain: evaluation.category.clone(),
            keywords: default_keywords_for_domain(&evaluation.category),
            poll_seconds: None,
            min_notional_usd: None,
        }
    }

    pub fn parse(spec: &str) -> Result<Self, String> {
        let parts = spec.split(':').collect::<Vec<_>>();
        let wallet = parts
            .first()
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
            .ok_or_else(|| "employee spec must start with a wallet".to_owned())?;
        let name = parts
            .get(1)
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
            .map(|value| value.to_owned());
        let domain = parts
            .get(2)
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
            .unwrap_or("CUSTOM")
            .to_uppercase();
        let keywords = parts
            .get(3)
            .map(|value| parse_keywords(value))
            .filter(|keywords| !keywords.is_empty())
            .unwrap_or_else(|| default_keywords_for_domain(&domain));
        let poll_seconds = parse_employee_poll_seconds(parts.get(4).copied())?;
        let min_notional_usd = parse_employee_min_notional(parts.get(5).copied())?;

        Ok(Self {
            wallet: wallet.to_owned(),
            name,
            domain,
            keywords,
            poll_seconds,
            min_notional_usd,
        })
    }

    pub fn label(&self) -> String {
        self.name
            .as_ref()
            .map(|name| format!("{name} ({})", self.wallet))
            .unwrap_or_else(|| self.wallet.clone())
    }

    pub fn poll_interval_seconds(&self, fallback_seconds: u64) -> u64 {
        self.poll_seconds
            .filter(|seconds| *seconds > 0)
            .unwrap_or_else(|| fallback_seconds.max(1))
    }

    pub fn min_notional_usd(&self, fallback_usd: f64) -> f64 {
        self.min_notional_usd
            .filter(|value| *value > 0.0)
            .unwrap_or(fallback_usd)
    }
}

#[derive(Debug, Clone)]
pub struct WatchRules {
    pub poll_seconds: u64,
    pub heartbeat_seconds: u64,
    pub trade_limit: usize,
    pub profile_trade_limit: usize,
    pub profile_closed_pages: usize,
    pub profile_closed_page_size: usize,
    pub profiles_enabled: bool,
    pub min_notional_usd: f64,
    pub max_entry_price: f64,
    pub follow_price_buffer: f64,
    pub iterations: Option<usize>,
}

impl Default for WatchRules {
    fn default() -> Self {
        Self {
            poll_seconds: 10,
            heartbeat_seconds: 3_600,
            trade_limit: 20,
            profile_trade_limit: 100,
            profile_closed_pages: 2,
            profile_closed_page_size: 50,
            profiles_enabled: true,
            min_notional_usd: 100.0,
            max_entry_price: 0.75,
            follow_price_buffer: 0.05,
            iterations: None,
        }
    }
}

pub struct WatchOutcome {
    pub polls_completed: usize,
    pub alerts_sent: usize,
    pub employees: usize,
    pub heartbeats_sent: usize,
    pub employee_polls_completed: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct EmployeeActivity {
    pub wallet: String,
    pub name: Option<String>,
    pub domain: String,
    pub matched_buys: usize,
    pub matched_buys_7d: usize,
    pub matched_buys_30d: usize,
    pub last_matched_buy_age_days: Option<u64>,
    pub avg_gap_hours: Option<f64>,
    pub median_gap_hours: Option<f64>,
    pub frequency: FrequencyTier,
}

#[derive(Debug, Clone)]
pub struct EmployeeProfileSnapshot {
    pub profile: EmployeeProfile,
    pub trades: Vec<UserTrade>,
}

#[derive(Debug, Clone, Copy)]
enum SellAction {
    TakeProfit,
    StopLoss,
    Trim,
    Unknown,
}

impl SellAction {
    fn label(self) -> &'static str {
        match self {
            Self::TakeProfit => "止盈减仓",
            Self::StopLoss => "止损撤退",
            Self::Trim => "调仓减仓",
            Self::Unknown => "未知卖出",
        }
    }

    fn guidance(self) -> &'static str {
        match self {
            Self::TakeProfit => {
                "如果此前跟随该仓位，建议检查是否同步止盈；如果还没买，不建议在员工开始减仓后追高。"
            }
            Self::StopLoss => {
                "如果此前跟随该方向，应重新检查市场信息；员工止损时，原买入信号可能已经失效。"
            }
            Self::Trim => {
                "这更像仓位管理或部分减仓，建议观察后续是否继续卖出，再决定是否同步降低风险。"
            }
            Self::Unknown => {
                "无法确认该员工此前成本，可能是旧仓、做市或调仓；不要把这笔单独理解成明确反向信号。"
            }
        }
    }
}

#[derive(Debug, Clone)]
struct SellAnalysis {
    action: SellAction,
    avg_entry_price: Option<f64>,
    return_pct: Option<f64>,
    known_position_size: Option<f64>,
    sell_fraction: Option<f64>,
    level: CopySignalLevel,
    score: u8,
    reasons: Vec<String>,
    cautions: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum FrequencyTier {
    High,
    Medium,
    Low,
    Dormant,
}

pub fn analyze_employee_activity(
    client: &PolymarketDataClient,
    employees: &[WatchedEmployee],
    trade_limit: usize,
) -> Vec<EmployeeActivity> {
    let now = now_secs();

    employees
        .iter()
        .map(|employee| {
            let trades = client
                .trades(&employee.wallet, trade_limit, 0, Some("BUY"))
                .unwrap_or_else(|error| {
                    eprintln!("failed to load activity for {}: {error}", employee.label());
                    Vec::new()
                });
            analyze_employee_trades(employee, &trades, now)
        })
        .collect()
}

pub fn analyze_employee_trades(
    employee: &WatchedEmployee,
    trades: &[UserTrade],
    now_secs: u64,
) -> EmployeeActivity {
    let mut timestamps = trades
        .iter()
        .filter(|trade| trade.side.eq_ignore_ascii_case("BUY"))
        .filter(|trade| matches_keywords(employee, trade))
        .filter_map(|trade| trade.timestamp)
        .collect::<Vec<_>>();

    timestamps.sort_unstable();

    let matched_buys = timestamps.len();
    let matched_buys_7d = count_since(&timestamps, now_secs, 7);
    let matched_buys_30d = count_since(&timestamps, now_secs, 30);
    let last_matched_buy_age_days = timestamps
        .last()
        .map(|timestamp| now_secs.saturating_sub(*timestamp) / 86_400);
    let gaps = timestamps
        .windows(2)
        .map(|pair| pair[1].saturating_sub(pair[0]) as f64 / 3_600.0)
        .collect::<Vec<_>>();
    let avg_gap_hours = if gaps.is_empty() {
        None
    } else {
        Some(round2(gaps.iter().sum::<f64>() / gaps.len() as f64))
    };
    let median_gap_hours = median(gaps);
    let frequency = classify_frequency(matched_buys_7d, matched_buys_30d);

    EmployeeActivity {
        wallet: employee.wallet.clone(),
        name: employee.name.clone(),
        domain: employee.domain.clone(),
        matched_buys,
        matched_buys_7d,
        matched_buys_30d,
        last_matched_buy_age_days,
        avg_gap_hours,
        median_gap_hours,
        frequency,
    }
}

pub fn watch_employees(
    client: &PolymarketDataClient,
    employees: &[WatchedEmployee],
    rules: &WatchRules,
    telegram: Option<&TelegramNotifier>,
) -> WatchOutcome {
    let mut seen = HashSet::new();
    let mut polls_completed = 0;
    let mut employee_polls_completed = 0;
    let mut alerts_sent = 0;
    let mut heartbeats_sent = 0;
    let started_at = Instant::now();
    let mut next_heartbeat_at_secs =
        next_heartbeat_boundary_secs(now_secs(), rules.heartbeat_seconds);
    let mut last_alert_at: Option<u64> = None;
    let mut last_polled = vec![None; employees.len()];
    let mut seeded = vec![false; employees.len()];
    let max_polls = rules.iterations.unwrap_or(usize::MAX);
    let snapshots = load_employee_profile_snapshots(client, employees, rules);
    let profiles = snapshots
        .iter()
        .map(|snapshot| (snapshot.profile.wallet.clone(), snapshot.profile.clone()))
        .collect::<HashMap<_, _>>();
    let mut trade_histories = snapshots
        .into_iter()
        .map(|snapshot| (snapshot.profile.wallet.clone(), snapshot.trades))
        .collect::<HashMap<_, _>>();

    println!(
        "Watching {} employees, poll={}s, min_notional=${:.2}, max_entry={:.3}",
        employees.len(),
        rules.poll_seconds,
        rules.min_notional_usd,
        rules.max_entry_price
    );
    flush_stdout();

    while polls_completed < max_polls {
        polls_completed += 1;
        for (index, employee) in employees.iter().enumerate() {
            if !employee_is_due(employee, rules, last_polled[index]) {
                continue;
            }

            let seed_only = !seeded[index];
            employee_polls_completed += 1;
            let trades = match client.trades(&employee.wallet, rules.trade_limit, 0, None) {
                Ok(trades) => {
                    last_polled[index] = Some(Instant::now());
                    trades
                }
                Err(error) => {
                    last_polled[index] = Some(Instant::now());
                    eprintln!("failed to load trades for {}: {error}", employee.label());
                    continue;
                }
            };

            let mut newest_first = trades;
            newest_first.sort_by_key(|trade| trade.timestamp.unwrap_or(0));

            for trade in newest_first {
                let key = trade_key(&trade);
                if seen.contains(&key) {
                    continue;
                }
                seen.insert(key);

                let history = trade_histories.entry(employee.wallet.clone()).or_default();

                if !seed_only {
                    let profile = profiles.get(&employee.wallet);
                    let alert = if trade.side.eq_ignore_ascii_case("SELL") {
                        build_sell_alert(employee, &trade, rules, profile, history)
                    } else {
                        build_alert(employee, &trade, rules, profile)
                    };

                    if let Some(alert) = alert {
                        println!("{alert}");
                        flush_stdout();
                        if let Some(telegram) = telegram {
                            if let Err(error) = telegram.send_message(&alert) {
                                eprintln!("failed to send Telegram alert: {error}");
                            }
                        }
                        alerts_sent += 1;
                        last_alert_at = Some(now_secs());
                    }
                }

                remember_trade(
                    history,
                    trade,
                    rules.profile_trade_limit.max(rules.trade_limit),
                );
            }

            seeded[index] = true;
        }

        if should_send_heartbeat(
            rules,
            &mut next_heartbeat_at_secs,
            polls_completed,
            max_polls,
        ) {
            let heartbeat = build_heartbeat(
                employees,
                rules,
                polls_completed,
                employee_polls_completed,
                alerts_sent,
                started_at.elapsed().as_secs(),
                last_alert_at,
            );
            println!("{heartbeat}");
            flush_stdout();
            if let Some(telegram) = telegram {
                if let Err(error) = telegram.send_message(&heartbeat) {
                    eprintln!("failed to send Telegram heartbeat: {error}");
                }
            }
            heartbeats_sent += 1;
        }

        if polls_completed < max_polls {
            sleep(Duration::from_secs(rules.poll_seconds.max(1)));
        }
    }

    WatchOutcome {
        polls_completed,
        alerts_sent,
        employees: employees.len(),
        heartbeats_sent,
        employee_polls_completed,
    }
}

pub fn load_employee_profiles(
    client: &PolymarketDataClient,
    employees: &[WatchedEmployee],
    rules: &WatchRules,
) -> Vec<EmployeeProfile> {
    load_employee_profile_snapshots(client, employees, rules)
        .into_iter()
        .map(|snapshot| snapshot.profile)
        .collect()
}

pub fn load_employee_profile_snapshots(
    client: &PolymarketDataClient,
    employees: &[WatchedEmployee],
    rules: &WatchRules,
) -> Vec<EmployeeProfileSnapshot> {
    if !rules.profiles_enabled {
        return Vec::new();
    }

    employees
        .iter()
        .filter_map(|employee| {
            let trades = match client.trades(&employee.wallet, rules.profile_trade_limit, 0, None) {
                Ok(trades) => trades,
                Err(error) => {
                    eprintln!(
                        "failed to load profile trades for {}: {error}",
                        employee.label()
                    );
                    Vec::new()
                }
            };
            let closed_positions = load_profile_closed_positions(client, employee, rules);

            if trades.is_empty() && closed_positions.is_empty() {
                eprintln!(
                    "profile unavailable for {}: no trades or closed positions",
                    employee.label()
                );
                return None;
            }

            let profile = build_employee_profile(employee, &trades, &closed_positions, now_secs());
            println!(
                "Profile {} score={} strategies={} closed={} matched_trades={} suspected_mm={}",
                employee.label(),
                profile.copy_trade_score,
                profile.strategy_labels().join(","),
                profile.closed_positions,
                profile.matched_trades,
                profile.suspected_market_making,
            );
            flush_stdout();

            Some(EmployeeProfileSnapshot { profile, trades })
        })
        .collect()
}

fn load_profile_closed_positions(
    client: &PolymarketDataClient,
    employee: &WatchedEmployee,
    rules: &WatchRules,
) -> Vec<crate::polymarket::ClosedPosition> {
    let mut positions = Vec::new();

    for page in 0..rules.profile_closed_pages {
        let offset = page * rules.profile_closed_page_size;
        let page_positions =
            match client.closed_positions(&employee.wallet, rules.profile_closed_page_size, offset)
            {
                Ok(page_positions) => page_positions,
                Err(error) => {
                    eprintln!(
                        "failed to load profile closed positions for {}: {error}",
                        employee.label()
                    );
                    break;
                }
            };
        let page_len = page_positions.len();
        positions.extend(page_positions);

        if page_len < rules.profile_closed_page_size {
            break;
        }
    }

    positions
}

fn flush_stdout() {
    let _ = io::stdout().flush();
}

fn should_send_heartbeat(
    rules: &WatchRules,
    next_heartbeat_at_secs: &mut u64,
    polls_completed: usize,
    max_polls: usize,
) -> bool {
    if rules.heartbeat_seconds == 0 {
        return false;
    }

    if polls_completed == 1 && max_polls == 1 {
        return false;
    }

    let current_time_secs = now_secs();
    if current_time_secs >= *next_heartbeat_at_secs {
        *next_heartbeat_at_secs =
            next_heartbeat_boundary_secs(current_time_secs, rules.heartbeat_seconds);
        true
    } else {
        false
    }
}

fn next_heartbeat_boundary_secs(now_secs: u64, interval_secs: u64) -> u64 {
    if interval_secs == 0 {
        u64::MAX
    } else {
        let interval_secs = interval_secs.max(1);
        ((now_secs / interval_secs) + 1) * interval_secs
    }
}

fn build_heartbeat(
    employees: &[WatchedEmployee],
    rules: &WatchRules,
    polls_completed: usize,
    employee_polls_completed: usize,
    alerts_sent: usize,
    uptime_seconds: u64,
    last_alert_at: Option<u64>,
) -> String {
    let last_alert = last_alert_at
        .map(|timestamp| format!("{}s ago", now_secs().saturating_sub(timestamp)))
        .unwrap_or_else(|| "none".to_owned());
    let domains = employees
        .iter()
        .map(|employee| employee.domain.as_str())
        .collect::<HashSet<_>>()
        .len();

    format!(
        "Polymarket watcher heartbeat\n\
status: running\n\
uptime: {}\n\
employees: {}\n\
domains: {}\n\
polls: {}\n\
employee_polls: {}\n\
alerts: {}\n\
last_alert: {}\n\
rules: poll={}s, heartbeat={}s, min_notional=${:.2}, max_entry={:.3}\n\
note: no action is normal when no new employee BUY/SELL passes the filters.",
        format_duration(uptime_seconds),
        employees.len(),
        domains,
        polls_completed,
        employee_polls_completed,
        alerts_sent,
        last_alert,
        rules.poll_seconds,
        rules.heartbeat_seconds,
        rules.min_notional_usd,
        rules.max_entry_price,
    )
}

fn employee_is_due(
    employee: &WatchedEmployee,
    rules: &WatchRules,
    last_polled: Option<Instant>,
) -> bool {
    match last_polled {
        Some(last_polled) => {
            last_polled.elapsed().as_secs() >= employee.poll_interval_seconds(rules.poll_seconds)
        }
        None => true,
    }
}

fn format_duration(seconds: u64) -> String {
    let hours = seconds / 3_600;
    let minutes = (seconds % 3_600) / 60;
    let secs = seconds % 60;

    format!("{hours}h {minutes}m {secs}s")
}

pub fn build_alert(
    employee: &WatchedEmployee,
    trade: &UserTrade,
    rules: &WatchRules,
    profile: Option<&EmployeeProfile>,
) -> Option<String> {
    if !trade.side.eq_ignore_ascii_case("BUY") {
        return None;
    }

    if !matches_keywords(employee, trade) {
        return None;
    }

    let price = trade.price?;
    let size = trade.size?;
    let notional = price * size;
    let preferred_min_notional_usd = employee.min_notional_usd(rules.min_notional_usd);
    let below_preferred_notional = notional < preferred_min_notional_usd;

    if price > rules.max_entry_price || notional < buy_dust_floor_usd() {
        return None;
    }

    let title = trade.title.as_deref().unwrap_or("-");
    let slug = trade.slug.as_deref().unwrap_or("-");
    let outcome = trade.outcome.as_deref().unwrap_or("-");
    let tx = trade.transaction_hash.as_deref().unwrap_or("-");
    let timestamp = trade.timestamp.unwrap_or_default();
    let age_seconds = now_secs().saturating_sub(timestamp);
    let max_follow_price = (price + rules.follow_price_buffer).min(0.99);
    let market_url = market_url(trade);
    let mut profile_signal = profile
        .map(|profile| profile.analyze_trade(trade))
        .unwrap_or_else(TradeProfileSignal::without_profile);

    if below_preferred_notional {
        profile_signal.cautions.push(format!(
            "金额 ${:.2} 低于参考阈值 ${:.2}，但已命中员工专业领域；按试探仓保留提醒，不再硬过滤。",
            notional, preferred_min_notional_usd
        ));
    }
    let profile_summary = profile
        .map(profile_summary)
        .unwrap_or_else(|| "策略画像: 暂无历史画像".to_owned());
    let analysis_lines = format_signal_lines(&profile_signal.reasons);
    let caution_lines = format_signal_lines(&profile_signal.cautions);

    Some(format!(
        "Polymarket 跟单提醒 [{}]\n\
一句话: {} 买入【{}】, 押注这个问题的答案是【{}】。\n\
问题: {}\n\
他买: {} @ {:.2}c / 隐含概率 {:.1}%\n\
可跟上限: {:.2}c\n\
金额: ${:.2} (size {:.4})\n\
距今: {}s\n\
链接: {}\n\
\n\
画像评分: {}/100\n\
{}\n\
分析:\n\
{}\n\
风险:\n\
{}\n\
跟单提示: {}\n\
\n\
员工: {}\n\
领域: {}\n\
钱包: {}\n\
slug: {}\n\
tx: {}\n\
\n\
手动检查: 你要跟的话就是买同一个 outcome【{}】；下单前确认当前卖一价、盘口深度、市场规则和价格漂移。",
        profile_signal.level.label(),
        employee.name.as_deref().unwrap_or("-"),
        outcome,
        outcome,
        title,
        outcome,
        price * 100.0,
        price * 100.0,
        max_follow_price * 100.0,
        notional,
        size,
        age_seconds,
        market_url,
        profile_signal.score,
        profile_summary,
        analysis_lines,
        caution_lines,
        profile_signal.level.guidance(),
        employee.name.as_deref().unwrap_or("-"),
        employee.domain,
        employee.wallet,
        slug,
        tx,
        outcome,
    ))
}

pub fn build_sell_alert(
    employee: &WatchedEmployee,
    trade: &UserTrade,
    rules: &WatchRules,
    profile: Option<&EmployeeProfile>,
    history: &[UserTrade],
) -> Option<String> {
    if !trade.side.eq_ignore_ascii_case("SELL") {
        return None;
    }

    if !matches_keywords(employee, trade) {
        return None;
    }

    let price = trade.price?;
    let size = trade.size?;
    let notional = price * size;
    let min_notional_usd = (employee.min_notional_usd(rules.min_notional_usd) * 0.5).max(5.0);

    if notional < min_notional_usd {
        return None;
    }

    let title = trade.title.as_deref().unwrap_or("-");
    let slug = trade.slug.as_deref().unwrap_or("-");
    let outcome = trade.outcome.as_deref().unwrap_or("-");
    let tx = trade.transaction_hash.as_deref().unwrap_or("-");
    let timestamp = trade.timestamp.unwrap_or_default();
    let age_seconds = now_secs().saturating_sub(timestamp);
    let market_url = market_url(trade);
    let analysis = analyze_sell_trade(profile, history, trade);
    let profile_summary = profile
        .map(profile_summary)
        .unwrap_or_else(|| "策略画像: 暂无历史画像".to_owned());
    let analysis_lines = format_signal_lines(&analysis.reasons);
    let caution_lines = format_signal_lines(&analysis.cautions);

    Some(format!(
        "Polymarket 员工卖出提醒 [{} / {}]\n\
一句话: {} 卖出【{}】仓位。\n\
问题: {}\n\
他卖: {} @ {:.2}c / 隐含概率 {:.1}%\n\
卖出金额: ${:.2} (size {:.4})\n\
估算成本: {}\n\
估算收益: {}\n\
卖出比例: {}\n\
距今: {}s\n\
链接: {}\n\
\n\
卖出评分: {}/100\n\
{}\n\
分析:\n\
{}\n\
风险:\n\
{}\n\
跟单提示: {}\n\
\n\
员工: {}\n\
领域: {}\n\
钱包: {}\n\
slug: {}\n\
tx: {}\n\
\n\
手动检查: 如果此前跟了同一个 outcome【{}】，现在应重点检查是否同步止盈/止损、盘口深度和该员工是否继续卖出。",
        analysis.level.label(),
        analysis.action.label(),
        employee.name.as_deref().unwrap_or("-"),
        outcome,
        title,
        outcome,
        price * 100.0,
        price * 100.0,
        notional,
        size,
        format_optional_price(analysis.avg_entry_price),
        format_optional_pct(analysis.return_pct),
        format_sell_fraction(analysis.sell_fraction, analysis.known_position_size),
        age_seconds,
        market_url,
        analysis.score,
        profile_summary,
        analysis_lines,
        caution_lines,
        analysis.action.guidance(),
        employee.name.as_deref().unwrap_or("-"),
        employee.domain,
        employee.wallet,
        slug,
        tx,
        outcome,
    ))
}

fn analyze_sell_trade(
    profile: Option<&EmployeeProfile>,
    history: &[UserTrade],
    sell: &UserTrade,
) -> SellAnalysis {
    let price = sell.price.unwrap_or(0.0);
    let size = sell.size.unwrap_or(0.0);
    let (known_position_size, known_position_cost) = estimate_known_position(history, sell);
    let avg_entry_price = if known_position_size > 0.0 {
        Some(known_position_cost / known_position_size)
    } else {
        None
    };
    let return_pct = avg_entry_price
        .filter(|entry| *entry > 0.0)
        .map(|entry| (price - entry) / entry);
    let sell_fraction = if known_position_size > 0.0 {
        Some((size / known_position_size).min(9.99))
    } else {
        None
    };
    let action = match return_pct {
        Some(value) if value >= 0.10 => SellAction::TakeProfit,
        Some(value) if value <= -0.10 => SellAction::StopLoss,
        Some(_) => SellAction::Trim,
        None => SellAction::Unknown,
    };
    let mut score: i32 = match action {
        SellAction::TakeProfit | SellAction::StopLoss => 65,
        SellAction::Trim => 55,
        SellAction::Unknown => 35,
    };
    let mut reasons = Vec::new();
    let mut cautions = Vec::new();

    match avg_entry_price {
        Some(entry) => {
            reasons.push(format!(
                "找到同 outcome 的已知持仓，估算成本 {:.2}c，当前卖出 {:.2}c。",
                entry * 100.0,
                price * 100.0
            ));
        }
        None => {
            cautions
                .push("近期历史里没有找到可匹配的买入成本，可能是旧仓、做市或调仓。".to_owned());
        }
    }

    if let Some(return_pct) = return_pct {
        if return_pct >= 0.10 {
            score += 10;
            reasons.push(format!(
                "卖出价高于估算成本 {:.1}%，更像止盈。",
                return_pct * 100.0
            ));
        } else if return_pct <= -0.10 {
            score += 15;
            reasons.push(format!(
                "卖出价低于估算成本 {:.1}%，更像止损/撤退。",
                return_pct * 100.0
            ));
        } else {
            reasons.push(format!(
                "卖出价接近估算成本，收益约 {:.1}%，更像调仓或降低风险。",
                return_pct * 100.0
            ));
        }
    }

    if let Some(fraction) = sell_fraction {
        if fraction >= 0.75 {
            score += 10;
            reasons.push(format!(
                "本次卖出约占已知持仓 {:.1}%，接近清仓。",
                fraction * 100.0
            ));
        } else if fraction >= 0.25 {
            score += 5;
            reasons.push(format!(
                "本次卖出约占已知持仓 {:.1}%，属于明显减仓。",
                fraction * 100.0
            ));
        } else {
            cautions.push(format!(
                "本次卖出约占已知持仓 {:.1}%，更像小幅减仓。",
                fraction * 100.0
            ));
        }
    }

    if let Some(profile) = profile {
        let notional = price * size;
        if profile.large_trade_threshold_usd > 0.0 && notional >= profile.large_trade_threshold_usd
        {
            score += 8;
            reasons.push(format!(
                "卖出金额 ${:.2} 高于该员工历史 P80 ${:.2}。",
                notional, profile.large_trade_threshold_usd
            ));
        }

        if profile.suspected_market_making {
            score -= 30;
            cautions.push("该员工画像疑似做市/价差型，单笔 SELL 需要降级理解。".to_owned());
        }

        if profile
            .strategy_archetypes
            .contains(&StrategyArchetype::EarlyExitTrader)
        {
            reasons.push("该员工画像带有提前卖出型标签，SELL 本身是重要跟踪信号。".to_owned());
        }

        if profile
            .strategy_archetypes
            .contains(&StrategyArchetype::ShortTermOperator)
        {
            cautions.push("该员工画像带有短线操作型标签，买入信号有效期可能较短。".to_owned());
        }
    }

    let score = score.clamp(0, 100) as u8;

    SellAnalysis {
        action,
        avg_entry_price,
        return_pct,
        known_position_size: if known_position_size > 0.0 {
            Some(known_position_size)
        } else {
            None
        },
        sell_fraction,
        level: copy_signal_level(score),
        score,
        reasons,
        cautions,
    }
}

fn estimate_known_position(history: &[UserTrade], target: &UserTrade) -> (f64, f64) {
    let target_key = trade_position_key(target);
    let target_timestamp = target.timestamp.unwrap_or(u64::MAX);
    let target_trade_key = trade_key(target);
    let mut ordered = history
        .iter()
        .filter(|trade| trade_position_key(trade) == target_key)
        .filter(|trade| trade_key(trade) != target_trade_key)
        .filter(|trade| trade.timestamp.unwrap_or(0) <= target_timestamp)
        .collect::<Vec<_>>();

    ordered.sort_by_key(|trade| trade.timestamp.unwrap_or(0));

    let mut position_size = 0.0;
    let mut position_cost = 0.0;

    for trade in ordered {
        let Some(price) = trade.price else {
            continue;
        };
        let Some(size) = trade.size else {
            continue;
        };

        if price <= 0.0 || size <= 0.0 {
            continue;
        }

        if trade.side.eq_ignore_ascii_case("BUY") {
            position_size += size;
            position_cost += price * size;
            continue;
        }

        if !trade.side.eq_ignore_ascii_case("SELL") || position_size <= 0.0 {
            continue;
        }

        let matched_size = size.min(position_size);
        let avg_cost = position_cost / position_size;
        position_size -= matched_size;
        position_cost -= avg_cost * matched_size;

        if position_size <= 0.000_001 {
            position_size = 0.0;
            position_cost = 0.0;
        }
    }

    (position_size, position_cost)
}

fn profile_summary(profile: &EmployeeProfile) -> String {
    let strategies = profile.strategy_labels();
    let strategy_text = if strategies.is_empty() {
        "-".to_owned()
    } else {
        strategies.join(",")
    };
    let best_subcategory = profile
        .best_subcategories
        .first()
        .map(|metric| {
            format!(
                "{} ROI {:.1}% PnL ${:.2}",
                metric.name,
                metric.roi * 100.0,
                metric.profit_usd
            )
        })
        .unwrap_or_else(|| "-".to_owned());
    let best_price_band = profile
        .best_price_bands
        .first()
        .map(|metric| format!("{} ROI {:.1}%", metric.band, metric.roi * 100.0))
        .unwrap_or_else(|| "-".to_owned());

    format!(
        "策略画像: {} | 员工分 {} | ROI {:.1}% | P80 ${:.2} | 净仓变化 {:.1}% | SELL占比 {:.1}% | 快进快出 {:.1}% | 做市嫌疑 {}\n\
优势: 子领域 {} | 价格区间 {}",
        strategy_text,
        profile.copy_trade_score,
        profile.realized_roi * 100.0,
        profile.large_trade_threshold_usd,
        profile.net_position_change_ratio * 100.0,
        profile.sell_notional_ratio * 100.0,
        profile.quick_flip_ratio * 100.0,
        if profile.suspected_market_making {
            "是"
        } else {
            "否"
        },
        best_subcategory,
        best_price_band,
    )
}

fn format_signal_lines(lines: &[String]) -> String {
    if lines.is_empty() {
        return "- 无".to_owned();
    }

    lines
        .iter()
        .map(|line| format!("- {line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn format_optional_price(price: Option<f64>) -> String {
    price
        .map(|price| format!("{:.2}c", price * 100.0))
        .unwrap_or_else(|| "未知".to_owned())
}

fn buy_dust_floor_usd() -> f64 {
    5.0
}

fn format_optional_pct(value: Option<f64>) -> String {
    value
        .map(|value| format!("{:+.1}%", value * 100.0))
        .unwrap_or_else(|| "未知".to_owned())
}

fn format_sell_fraction(fraction: Option<f64>, known_position_size: Option<f64>) -> String {
    match (fraction, known_position_size) {
        (Some(fraction), Some(position_size)) => {
            format!(
                "约 {:.1}% 已知持仓 (known size {:.4})",
                fraction * 100.0,
                position_size
            )
        }
        _ => "未知".to_owned(),
    }
}

fn copy_signal_level(score: u8) -> CopySignalLevel {
    match score {
        80..=100 => CopySignalLevel::Strong,
        60..=79 => CopySignalLevel::Normal,
        40..=59 => CopySignalLevel::Watch,
        _ => CopySignalLevel::LowPriority,
    }
}

fn remember_trade(history: &mut Vec<UserTrade>, trade: UserTrade, max_history: usize) {
    let key = trade_key(&trade);
    if history.iter().any(|existing| trade_key(existing) == key) {
        return;
    }

    history.push(trade);
    history.sort_by_key(|trade| std::cmp::Reverse(trade.timestamp.unwrap_or(0)));
    history.truncate(max_history.max(20));
}

fn trade_position_key(trade: &UserTrade) -> String {
    format!("{}:{}", trade.condition_id, trade.asset)
}

fn parse_employee_min_notional(value: Option<&str>) -> Result<Option<f64>, String> {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };

    let min_notional = value
        .parse::<f64>()
        .map_err(|_| format!("employee min notional must be a positive number: {value}"))?;
    if min_notional <= 0.0 {
        return Err("employee min notional must be greater than zero".to_owned());
    }

    Ok(Some(min_notional))
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

fn matches_keywords(employee: &WatchedEmployee, trade: &UserTrade) -> bool {
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

fn count_since(timestamps: &[u64], now_secs: u64, days: u64) -> usize {
    let window = days.saturating_mul(86_400);

    timestamps
        .iter()
        .filter(|timestamp| now_secs.saturating_sub(**timestamp) <= window)
        .count()
}

fn classify_frequency(matched_buys_7d: usize, matched_buys_30d: usize) -> FrequencyTier {
    if matched_buys_7d >= 5 || matched_buys_30d >= 15 {
        FrequencyTier::High
    } else if matched_buys_30d >= 3 {
        FrequencyTier::Medium
    } else if matched_buys_30d > 0 {
        FrequencyTier::Low
    } else {
        FrequencyTier::Dormant
    }
}

fn median(mut values: Vec<f64>) -> Option<f64> {
    if values.is_empty() {
        return None;
    }

    values.sort_by(|left, right| left.total_cmp(right));
    let mid = values.len() / 2;

    if values.len() % 2 == 0 {
        Some(round2((values[mid - 1] + values[mid]) / 2.0))
    } else {
        Some(round2(values[mid]))
    }
}

fn round2(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

fn trade_key(trade: &UserTrade) -> String {
    trade.transaction_hash.clone().unwrap_or_else(|| {
        format!(
            "{}:{}:{}:{:.8}:{:.8}",
            trade.proxy_wallet,
            trade.asset,
            trade.timestamp.unwrap_or_default(),
            trade.price.unwrap_or_default(),
            trade.size.unwrap_or_default()
        )
    })
}

pub fn default_keywords_for_domain(domain: &str) -> Vec<String> {
    match domain.to_uppercase().as_str() {
        "TECH" => parse_keywords("ai,gemini,llm,model,arena,openai,anthropic,xai,grok,google"),
        "CRYPTO" => parse_keywords("bitcoin,btc,ethereum,eth,solana,sol,crypto"),
        "SPORTS" => parse_keywords("nba,nhl,nfl,mlb,ufc,soccer,tennis,championship"),
        "POLITICS" => parse_keywords("trump,biden,election,senate,house,president,china"),
        "FINANCE" | "ECONOMICS" => parse_keywords("fed,rates,cpi,gdp,oil,wti,gold,stock"),
        "WEATHER" => parse_keywords("weather,temperature,hurricane,storm,rain,snow"),
        "CULTURE" | "MENTIONS" => parse_keywords("twitter,x,post,tweet,album,movie,celebrity"),
        _ => Vec::new(),
    }
}

fn parse_keywords(value: &str) -> Vec<String> {
    value
        .split([',', '|'])
        .map(|keyword| keyword.trim().to_lowercase())
        .filter(|keyword| !keyword.is_empty())
        .collect()
}

fn parse_employee_poll_seconds(value: Option<&str>) -> Result<Option<u64>, String> {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };

    let poll_seconds = value
        .parse::<u64>()
        .map_err(|_| format!("employee poll interval must be a positive integer: {value}"))?;
    if poll_seconds == 0 {
        return Err("employee poll interval must be greater than zero".to_owned());
    }

    Ok(Some(poll_seconds))
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

    fn trade(title: &str, price: f64, size: f64) -> UserTrade {
        UserTrade {
            proxy_wallet: "0xemployee".to_owned(),
            side: "BUY".to_owned(),
            asset: "asset".to_owned(),
            condition_id: "condition".to_owned(),
            size: Some(size),
            price: Some(price),
            timestamp: Some(now_secs()),
            title: Some(title.to_owned()),
            slug: Some(title.to_lowercase().replace(' ', "-")),
            event_slug: None,
            outcome: Some("Yes".to_owned()),
            outcome_index: Some(0),
            name: Some("employee".to_owned()),
            pseudonym: None,
            transaction_hash: Some("0xtx".to_owned()),
        }
    }

    #[test]
    fn ai_employee_alerts_on_cheap_relevant_trade() {
        let employee = WatchedEmployee::parse("0xemployee:worker:TECH:gemini|ai").unwrap();
        let rules = WatchRules::default();
        let alert = build_alert(
            &employee,
            &trade("Will Gemini 4 release by June?", 0.24, 1_000.0),
            &rules,
            None,
        );

        assert!(alert.is_some());
    }

    #[test]
    fn employee_spec_can_set_poll_interval() {
        let employee = WatchedEmployee::parse("0xemployee:worker:TECH:gemini|ai:120:10").unwrap();

        assert_eq!(employee.poll_seconds, Some(120));
        assert_eq!(employee.poll_interval_seconds(10), 120);
        assert_eq!(employee.min_notional_usd, Some(10.0));
        assert_eq!(employee.min_notional_usd(100.0), 10.0);
    }

    #[test]
    fn heartbeat_targets_next_wall_clock_boundary() {
        assert_eq!(
            next_heartbeat_boundary_secs(20 * 3_600 + 59 * 60, 3_600),
            21 * 3_600
        );
        assert_eq!(next_heartbeat_boundary_secs(21 * 3_600, 3_600), 22 * 3_600);
        assert_eq!(
            next_heartbeat_boundary_secs(21 * 3_600 + 29 * 60, 1_800),
            21 * 3_600 + 30 * 60
        );
        assert_eq!(next_heartbeat_boundary_secs(21 * 3_600, 0), u64::MAX);
    }

    #[test]
    fn ai_employee_skips_expensive_or_irrelevant_trade() {
        let employee = WatchedEmployee::parse("0xemployee:worker:TECH:gemini|ai").unwrap();
        let rules = WatchRules::default();

        assert!(build_alert(
            &employee,
            &trade("Will Gemini 4 release by June?", 0.92, 1_000.0),
            &rules,
            None,
        )
        .is_none());
        assert!(build_alert(
            &employee,
            &trade("Bitcoin Up or Down", 0.24, 1_000.0),
            &rules,
            None,
        )
        .is_none());
    }

    #[test]
    fn relevant_small_buy_is_observation_not_filtered() {
        let employee = WatchedEmployee::parse("0xemployee:worker:TECH:gemini|ai").unwrap();
        let rules = WatchRules::default();
        let alert = build_alert(
            &employee,
            &trade("Will Gemini 4 release by June?", 0.10, 60.0),
            &rules,
            None,
        )
        .unwrap();

        assert!(alert.contains("试探仓保留提醒"));
    }

    #[test]
    fn sell_alert_classifies_known_profitable_exit() {
        let employee = WatchedEmployee::parse("0xemployee:worker:TECH:gemini|ai").unwrap();
        let rules = WatchRules::default();
        let now = now_secs();
        let mut buy = trade("Will Gemini 4 release by June?", 0.20, 1_000.0);
        buy.timestamp = Some(now - 3_600);
        buy.transaction_hash = Some("0xbuy".to_owned());

        let mut sell = trade("Will Gemini 4 release by June?", 0.35, 500.0);
        sell.side = "SELL".to_owned();
        sell.timestamp = Some(now);
        sell.transaction_hash = Some("0xsell".to_owned());

        let alert = build_sell_alert(&employee, &sell, &rules, None, &[buy]).unwrap();

        assert!(alert.contains("止盈减仓"));
        assert!(alert.contains("+75.0%"));
    }

    #[test]
    fn activity_frequency_classifies_matched_domain_buys() {
        let employee = WatchedEmployee::parse("0xemployee:worker:TECH:gemini|ai").unwrap();
        let now = now_secs();
        let mut trades = Vec::new();

        for days_ago in [1, 2, 3, 5, 6] {
            let mut trade = trade("Will Gemini 4 release by June?", 0.24, 1_000.0);
            trade.timestamp = Some(now - (days_ago * 86_400));
            trades.push(trade);
        }

        let mut irrelevant = trade("Bitcoin Up or Down", 0.24, 1_000.0);
        irrelevant.timestamp = Some(now - 86_400);
        trades.push(irrelevant);

        let activity = analyze_employee_trades(&employee, &trades, now);

        assert_eq!(activity.matched_buys_7d, 5);
        assert_eq!(activity.frequency, FrequencyTier::High);
    }
}
