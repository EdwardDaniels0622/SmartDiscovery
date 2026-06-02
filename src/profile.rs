use crate::{
    monitor::WatchedEmployee,
    polymarket::{ClosedPosition, UserTrade},
};
use serde::Serialize;
use std::collections::{BTreeMap, HashMap, VecDeque};

#[derive(Debug, Clone, Serialize)]
pub struct EmployeeProfile {
    pub wallet: String,
    pub name: Option<String>,
    pub domain: String,
    pub keywords: Vec<String>,
    pub generated_at_secs: u64,
    pub closed_positions: usize,
    pub realized_pnl_usd: f64,
    pub invested_usd: f64,
    pub realized_roi: f64,
    pub win_position_ratio: f64,
    pub top_5_profit_share: f64,
    pub top_10_profit_share: f64,
    pub total_trades: usize,
    pub matched_trades: usize,
    pub buy_trades: usize,
    pub sell_trades: usize,
    pub total_notional_usd: f64,
    pub sell_notional_usd: f64,
    pub sell_notional_ratio: f64,
    pub avg_trade_size_usd: f64,
    pub median_trade_size_usd: f64,
    pub large_trade_threshold_usd: f64,
    pub very_large_trade_threshold_usd: f64,
    pub buy_sell_ratio: f64,
    pub net_position_change_ratio: f64,
    pub matched_sell_trades: usize,
    pub avg_holding_hours: Option<f64>,
    pub quick_flip_ratio: f64,
    pub take_profit_sell_ratio: f64,
    pub stop_loss_sell_ratio: f64,
    pub repeated_market_ratio: f64,
    pub suspected_market_making: bool,
    pub best_subcategories: Vec<SubcategoryMetric>,
    pub worst_subcategories: Vec<SubcategoryMetric>,
    pub best_price_bands: Vec<PriceBandMetric>,
    pub worst_price_bands: Vec<PriceBandMetric>,
    pub strategy_archetypes: Vec<StrategyArchetype>,
    pub copy_trade_score: u8,
    pub copy_trade_notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SubcategoryMetric {
    pub name: String,
    pub closed_positions: usize,
    pub trade_count: usize,
    pub profit_usd: f64,
    pub invested_usd: f64,
    pub roi: f64,
    pub win_rate: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct PriceBandMetric {
    pub band: String,
    pub closed_positions: usize,
    pub trade_count: usize,
    pub profit_usd: f64,
    pub invested_usd: f64,
    pub roi: f64,
    pub win_rate: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum StrategyArchetype {
    HeavyStakeSignal,
    StableSmallStake,
    LongshotVolatile,
    HighProbabilityLowReturn,
    HighFrequencyMarketMaking,
    UnstableConcentrated,
    EarlyExitTrader,
    ShortTermOperator,
}

impl StrategyArchetype {
    pub fn label(self) -> &'static str {
        match self {
            Self::HeavyStakeSignal => "重仓信号型",
            Self::StableSmallStake => "稳定小额型",
            Self::LongshotVolatile => "冷门赔率型",
            Self::HighProbabilityLowReturn => "高概率低收益型",
            Self::HighFrequencyMarketMaking => "高频做市/套利型",
            Self::UnstableConcentrated => "不可稳定跟单型",
            Self::EarlyExitTrader => "提前卖出型",
            Self::ShortTermOperator => "短线操作型",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum CopySignalLevel {
    Strong,
    Normal,
    Watch,
    LowPriority,
}

impl CopySignalLevel {
    pub fn label(self) -> &'static str {
        match self {
            Self::Strong => "强提醒",
            Self::Normal => "普通提醒",
            Self::Watch => "观察提醒",
            Self::LowPriority => "低优先级",
        }
    }

    pub fn guidance(self) -> &'static str {
        match self {
            Self::Strong => {
                "该交易较符合员工历史优势模式，可作为较强人工跟单信号；下单前仍要检查盘口深度和价格漂移。"
            }
            Self::Normal => {
                "该交易有参考价值，但信号强度不是顶格；适合结合盘口和市场规则再判断。"
            }
            Self::Watch => {
                "该交易更适合观察，不建议只凭这一笔直接跟；重点看后续是否连续净加仓。"
            }
            Self::LowPriority => {
                "不建议把这笔单独当成强方向信号；它可能只是小额试探、调仓或价差交易。"
            }
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct TradeProfileSignal {
    pub profile_available: bool,
    pub level: CopySignalLevel,
    pub score: u8,
    pub reasons: Vec<String>,
    pub cautions: Vec<String>,
}

impl TradeProfileSignal {
    pub fn without_profile() -> Self {
        Self {
            profile_available: false,
            level: CopySignalLevel::Normal,
            score: 55,
            reasons: vec!["暂无足够历史画像，本次只通过基础关键词、价格和金额过滤。".to_owned()],
            cautions: vec!["建议先手动查看该员工近期同市场是否连续加仓。".to_owned()],
        }
    }
}

#[derive(Debug, Default)]
struct AggregateMetric {
    closed_positions: usize,
    trade_count: usize,
    profit_usd: f64,
    invested_usd: f64,
    wins: usize,
}

#[derive(Debug, Default)]
struct ExitBehaviorMetrics {
    sell_notional_usd: f64,
    sell_notional_ratio: f64,
    matched_sell_trades: usize,
    avg_holding_hours: Option<f64>,
    quick_flip_ratio: f64,
    take_profit_sell_ratio: f64,
    stop_loss_sell_ratio: f64,
}

#[derive(Debug, Clone)]
struct PositionLot {
    remaining_size: f64,
    price: f64,
    timestamp: u64,
}

pub fn build_employee_profile(
    employee: &WatchedEmployee,
    trades: &[UserTrade],
    closed_positions: &[ClosedPosition],
    now_secs: u64,
) -> EmployeeProfile {
    let keyword_matched_positions = closed_positions
        .iter()
        .filter(|position| position_matches_employee(employee, position))
        .cloned()
        .collect::<Vec<_>>();
    let profile_positions = if keyword_matched_positions.is_empty() {
        closed_positions.to_vec()
    } else {
        keyword_matched_positions
    };
    let matched_trades = trades
        .iter()
        .filter(|trade| trade_matches_employee(employee, trade))
        .collect::<Vec<_>>();
    let trade_amounts = matched_trades
        .iter()
        .filter_map(|trade| trade_notional(trade))
        .collect::<Vec<_>>();
    let buy_trades = matched_trades
        .iter()
        .filter(|trade| trade.side.eq_ignore_ascii_case("BUY"))
        .count();
    let sell_trades = matched_trades
        .iter()
        .filter(|trade| trade.side.eq_ignore_ascii_case("SELL"))
        .count();
    let total_notional_usd = trade_amounts.iter().sum::<f64>();
    let exit_behavior = exit_behavior_metrics(&matched_trades);
    let avg_trade_size_usd = if trade_amounts.is_empty() {
        0.0
    } else {
        total_notional_usd / trade_amounts.len() as f64
    };
    let median_trade_size_usd = percentile(trade_amounts.clone(), 0.50);
    let large_trade_threshold_usd = percentile(trade_amounts.clone(), 0.80);
    let very_large_trade_threshold_usd = percentile(trade_amounts.clone(), 0.95);
    let signed_notional = matched_trades
        .iter()
        .filter_map(|trade| {
            trade_notional(trade).map(|notional| {
                if trade.side.eq_ignore_ascii_case("SELL") {
                    -notional
                } else {
                    notional
                }
            })
        })
        .sum::<f64>();
    let net_position_change_ratio = if total_notional_usd > 0.0 {
        signed_notional.abs() / total_notional_usd
    } else {
        0.0
    };
    let buy_sell_ratio = match sell_trades {
        0 => buy_trades as f64,
        sells => buy_trades as f64 / sells as f64,
    };
    let repeated_market_ratio = repeated_market_ratio(&matched_trades);
    let suspected_market_making = suspected_market_making(
        matched_trades.len(),
        buy_trades,
        sell_trades,
        net_position_change_ratio,
        repeated_market_ratio,
        median_trade_size_usd,
        avg_trade_size_usd,
    );

    let realized_pnl_usd = profile_positions
        .iter()
        .map(|position| position.realized_pnl.unwrap_or(0.0))
        .sum::<f64>();
    let invested_usd = profile_positions
        .iter()
        .map(|position| position.total_bought.unwrap_or(0.0).max(0.0))
        .sum::<f64>();
    let realized_roi = if invested_usd > 0.0 {
        realized_pnl_usd / invested_usd
    } else {
        0.0
    };
    let win_position_ratio = if profile_positions.is_empty() {
        0.0
    } else {
        profile_positions
            .iter()
            .filter(|position| position.realized_pnl.unwrap_or(0.0) > 0.0)
            .count() as f64
            / profile_positions.len() as f64
    };
    let top_5_profit_share = top_profit_share(&profile_positions, 5);
    let top_10_profit_share = top_profit_share(&profile_positions, 10);

    let mut subcategory_metrics = subcategory_metrics(employee, trades, &profile_positions);
    let mut price_band_metrics = price_band_metrics(employee, trades, &profile_positions);

    sort_best_metrics(&mut subcategory_metrics);
    sort_best_price_metrics(&mut price_band_metrics);

    let best_subcategories = subcategory_metrics
        .iter()
        .take(3)
        .cloned()
        .collect::<Vec<_>>();
    let worst_subcategories = subcategory_metrics
        .iter()
        .rev()
        .take(3)
        .cloned()
        .collect::<Vec<_>>();
    let best_price_bands = price_band_metrics
        .iter()
        .take(3)
        .cloned()
        .collect::<Vec<_>>();
    let worst_price_bands = price_band_metrics
        .iter()
        .rev()
        .take(3)
        .cloned()
        .collect::<Vec<_>>();

    let strategy_archetypes = classify_strategy(
        &profile_positions,
        realized_roi,
        top_5_profit_share,
        median_trade_size_usd,
        large_trade_threshold_usd,
        suspected_market_making,
        &exit_behavior,
    );
    let copy_trade_score = profile_copy_score(
        profile_positions.len(),
        realized_roi,
        win_position_ratio,
        top_5_profit_share,
        suspected_market_making,
        &strategy_archetypes,
    );
    let copy_trade_notes = build_profile_notes(
        realized_roi,
        top_5_profit_share,
        median_trade_size_usd,
        suspected_market_making,
        &strategy_archetypes,
        &best_subcategories,
        &best_price_bands,
    );

    EmployeeProfile {
        wallet: employee.wallet.clone(),
        name: employee.name.clone(),
        domain: employee.domain.clone(),
        keywords: employee.keywords.clone(),
        generated_at_secs: now_secs,
        closed_positions: profile_positions.len(),
        realized_pnl_usd: round2(realized_pnl_usd),
        invested_usd: round2(invested_usd),
        realized_roi: round4(realized_roi),
        win_position_ratio: round4(win_position_ratio),
        top_5_profit_share: round4(top_5_profit_share),
        top_10_profit_share: round4(top_10_profit_share),
        total_trades: trades.len(),
        matched_trades: matched_trades.len(),
        buy_trades,
        sell_trades,
        total_notional_usd: round2(total_notional_usd),
        sell_notional_usd: round2(exit_behavior.sell_notional_usd),
        sell_notional_ratio: round4(exit_behavior.sell_notional_ratio),
        avg_trade_size_usd: round2(avg_trade_size_usd),
        median_trade_size_usd: round2(median_trade_size_usd),
        large_trade_threshold_usd: round2(large_trade_threshold_usd),
        very_large_trade_threshold_usd: round2(very_large_trade_threshold_usd),
        buy_sell_ratio: round4(buy_sell_ratio),
        net_position_change_ratio: round4(net_position_change_ratio),
        matched_sell_trades: exit_behavior.matched_sell_trades,
        avg_holding_hours: exit_behavior.avg_holding_hours.map(round2),
        quick_flip_ratio: round4(exit_behavior.quick_flip_ratio),
        take_profit_sell_ratio: round4(exit_behavior.take_profit_sell_ratio),
        stop_loss_sell_ratio: round4(exit_behavior.stop_loss_sell_ratio),
        repeated_market_ratio: round4(repeated_market_ratio),
        suspected_market_making,
        best_subcategories,
        worst_subcategories,
        best_price_bands,
        worst_price_bands,
        strategy_archetypes,
        copy_trade_score,
        copy_trade_notes,
    }
}

impl EmployeeProfile {
    pub fn analyze_trade(&self, trade: &UserTrade) -> TradeProfileSignal {
        let price = trade.price.unwrap_or(0.0);
        let notional = trade_notional(trade).unwrap_or(0.0);
        let subcategory = trade_subcategory_from_keywords(
            &self.keywords,
            &self.domain,
            trade_haystack(
                trade.title.as_deref(),
                trade.slug.as_deref(),
                trade.event_slug.as_deref(),
            ),
        );
        let price_band = price_band(price);
        let mut score: i32 = 50;
        let mut reasons = Vec::new();
        let mut cautions = Vec::new();

        if self.closed_positions >= 8 {
            score += 5;
            reasons.push(format!(
                "历史结算样本 {} 笔，ROI {:.1}%，胜率 {:.1}%。",
                self.closed_positions,
                self.realized_roi * 100.0,
                self.win_position_ratio * 100.0
            ));
        } else {
            score -= 10;
            cautions.push(format!(
                "历史结算样本只有 {} 笔，画像稳定性偏弱。",
                self.closed_positions
            ));
        }

        if self.realized_roi > 0.05 {
            score += 10;
        } else if self.closed_positions > 0 {
            score -= 10;
            cautions.push(format!(
                "该员工样本 ROI 只有 {:.1}%，不应只看排行榜名次。",
                self.realized_roi * 100.0
            ));
        }

        if notional >= self.very_large_trade_threshold_usd
            && self.very_large_trade_threshold_usd > 0.0
        {
            score += 20;
            reasons.push(format!(
                "本次金额 ${:.2} 达到其历史 P95 重仓区间。",
                notional
            ));
        } else if notional >= self.large_trade_threshold_usd && self.large_trade_threshold_usd > 0.0
        {
            score += 15;
            reasons.push(format!(
                "本次金额 ${:.2} 高于其历史 P80 阈值 ${:.2}。",
                notional, self.large_trade_threshold_usd
            ));
        } else if self.median_trade_size_usd > 0.0 && notional < self.median_trade_size_usd * 0.5 {
            reasons.push(format!(
                "本次金额 ${:.2} 低于其历史中位数 ${:.2}，更像机会试探仓，保留观察而不按金额降权。",
                notional, self.median_trade_size_usd
            ));
        }

        if metric_names(&self.best_subcategories).contains(&subcategory.as_str()) {
            score += 15;
            reasons.push(format!("当前子领域 `{subcategory}` 属于该员工历史优势区。"));
        } else if metric_names(&self.worst_subcategories).contains(&subcategory.as_str())
            && self.closed_positions >= 8
        {
            score -= 10;
            cautions.push(format!("当前子领域 `{subcategory}` 不是该员工历史强项。"));
        }

        if price_metric_names(&self.best_price_bands).contains(&price_band.as_str()) {
            score += 10;
            reasons.push(format!(
                "成交价 {:.2}c 落在历史表现较好的价格区间。",
                price * 100.0
            ));
        } else if price_metric_names(&self.worst_price_bands).contains(&price_band.as_str())
            && self.closed_positions >= 8
        {
            score -= 10;
            cautions.push(format!(
                "成交价 {:.2}c 落在该员工历史较弱的价格区间。",
                price * 100.0
            ));
        }

        if self
            .strategy_archetypes
            .contains(&StrategyArchetype::HeavyStakeSignal)
            && notional >= self.large_trade_threshold_usd
            && self.large_trade_threshold_usd > 0.0
        {
            score += 10;
            reasons.push("该员工历史上重仓交易更有参考价值，本次符合重仓模式。".to_owned());
        }

        if self
            .strategy_archetypes
            .contains(&StrategyArchetype::StableSmallStake)
            && notional < self.large_trade_threshold_usd.max(1.0)
        {
            score += 5;
            reasons.push("该员工偏稳定小额策略，本次金额没有明显脱离常规。".to_owned());
        }

        if self.suspected_market_making {
            score -= 30;
            cautions.push(
                "该员工历史买卖较双向、净仓位变化低，疑似做市/价差型；单笔不宜强跟。".to_owned(),
            );
        }

        if self
            .strategy_archetypes
            .contains(&StrategyArchetype::UnstableConcentrated)
        {
            score -= 15;
            cautions.push(format!(
                "盈利集中度高，前 5 笔盈利贡献 {:.1}%，跟单波动会更大。",
                self.top_5_profit_share * 100.0
            ));
        }

        if trade.side.eq_ignore_ascii_case("SELL") {
            score -= 25;
            cautions.push("这是一笔卖出/减仓，不应当作买入方向信号。".to_owned());
        }

        let score = score.clamp(0, 100) as u8;
        let level = signal_level(score);

        if reasons.is_empty() {
            reasons.push("该笔通过基础过滤，但没有命中特别强的画像加分项。".to_owned());
        }

        TradeProfileSignal {
            profile_available: true,
            level,
            score,
            reasons,
            cautions,
        }
    }

    pub fn strategy_labels(&self) -> Vec<&'static str> {
        self.strategy_archetypes
            .iter()
            .map(|strategy| strategy.label())
            .collect()
    }
}

fn subcategory_metrics(
    employee: &WatchedEmployee,
    trades: &[UserTrade],
    positions: &[ClosedPosition],
) -> Vec<SubcategoryMetric> {
    let mut aggregates: BTreeMap<String, AggregateMetric> = BTreeMap::new();

    for trade in trades
        .iter()
        .filter(|trade| trade_matches_employee(employee, trade))
    {
        let subcategory = trade_subcategory(employee, trade);
        aggregates.entry(subcategory).or_default().trade_count += 1;
    }

    for position in positions {
        let subcategory = position_subcategory(employee, position);
        let aggregate = aggregates.entry(subcategory).or_default();
        aggregate.closed_positions += 1;
        aggregate.profit_usd += position.realized_pnl.unwrap_or(0.0);
        aggregate.invested_usd += position.total_bought.unwrap_or(0.0).max(0.0);
        if position.realized_pnl.unwrap_or(0.0) > 0.0 {
            aggregate.wins += 1;
        }
    }

    aggregates
        .into_iter()
        .map(|(name, metric)| SubcategoryMetric {
            name,
            closed_positions: metric.closed_positions,
            trade_count: metric.trade_count,
            profit_usd: round2(metric.profit_usd),
            invested_usd: round2(metric.invested_usd),
            roi: round4(roi(metric.profit_usd, metric.invested_usd)),
            win_rate: win_rate(metric.wins, metric.closed_positions),
        })
        .collect()
}

fn price_band_metrics(
    employee: &WatchedEmployee,
    trades: &[UserTrade],
    positions: &[ClosedPosition],
) -> Vec<PriceBandMetric> {
    let mut aggregates: BTreeMap<String, AggregateMetric> = BTreeMap::new();

    for trade in trades
        .iter()
        .filter(|trade| trade_matches_employee(employee, trade))
    {
        let band = price_band(trade.price.unwrap_or(0.0));
        aggregates.entry(band).or_default().trade_count += 1;
    }

    for position in positions {
        let band = price_band(position.avg_price.unwrap_or(0.0));
        let aggregate = aggregates.entry(band).or_default();
        aggregate.closed_positions += 1;
        aggregate.profit_usd += position.realized_pnl.unwrap_or(0.0);
        aggregate.invested_usd += position.total_bought.unwrap_or(0.0).max(0.0);
        if position.realized_pnl.unwrap_or(0.0) > 0.0 {
            aggregate.wins += 1;
        }
    }

    aggregates
        .into_iter()
        .map(|(band, metric)| PriceBandMetric {
            band,
            closed_positions: metric.closed_positions,
            trade_count: metric.trade_count,
            profit_usd: round2(metric.profit_usd),
            invested_usd: round2(metric.invested_usd),
            roi: round4(roi(metric.profit_usd, metric.invested_usd)),
            win_rate: win_rate(metric.wins, metric.closed_positions),
        })
        .collect()
}

fn classify_strategy(
    positions: &[ClosedPosition],
    realized_roi: f64,
    top_5_profit_share: f64,
    median_trade_size_usd: f64,
    large_trade_threshold_usd: f64,
    suspected_market_making: bool,
    exit_behavior: &ExitBehaviorMetrics,
) -> Vec<StrategyArchetype> {
    let mut strategies = Vec::new();
    let position_amounts = positions
        .iter()
        .filter_map(|position| position.total_bought)
        .collect::<Vec<_>>();
    let large_position_threshold = percentile(position_amounts, 0.80);
    let (large_pnl, large_invested, large_positive_pnl) =
        position_bucket_pnl(positions, large_position_threshold, true);
    let (small_pnl, small_invested, _) =
        position_bucket_pnl(positions, large_position_threshold, false);
    let large_roi = roi(large_pnl, large_invested);
    let small_roi = roi(small_pnl, small_invested);
    let total_positive_pnl = positions
        .iter()
        .map(|position| position.realized_pnl.unwrap_or(0.0).max(0.0))
        .sum::<f64>();
    let large_profit_share = if total_positive_pnl > 0.0 {
        large_positive_pnl / total_positive_pnl
    } else {
        0.0
    };

    if suspected_market_making {
        strategies.push(StrategyArchetype::HighFrequencyMarketMaking);
    }

    if exit_behavior.matched_sell_trades >= 3 && exit_behavior.sell_notional_ratio >= 0.25 {
        strategies.push(StrategyArchetype::EarlyExitTrader);
    }

    if exit_behavior.matched_sell_trades >= 3
        && (exit_behavior.quick_flip_ratio >= 0.40
            || exit_behavior
                .avg_holding_hours
                .map(|hours| hours <= 24.0)
                .unwrap_or(false))
    {
        strategies.push(StrategyArchetype::ShortTermOperator);
    }

    if positions.len() >= 5 && large_roi > small_roi + 0.05 && large_profit_share >= 0.35 {
        strategies.push(StrategyArchetype::HeavyStakeSignal);
    }

    if median_trade_size_usd > 0.0
        && median_trade_size_usd <= 100.0
        && realized_roi > 0.03
        && !suspected_market_making
    {
        strategies.push(StrategyArchetype::StableSmallStake);
    }

    let median_entry_price = median_position_price(positions);
    if positions.len() >= 5 && median_entry_price > 0.0 && median_entry_price <= 0.35 {
        strategies.push(StrategyArchetype::LongshotVolatile);
    }

    if positions.len() >= 5 && median_entry_price >= 0.70 {
        strategies.push(StrategyArchetype::HighProbabilityLowReturn);
    }

    if top_5_profit_share >= 0.65 || (positions.len() < 8 && large_trade_threshold_usd == 0.0) {
        strategies.push(StrategyArchetype::UnstableConcentrated);
    }

    if strategies.is_empty() {
        strategies.push(StrategyArchetype::StableSmallStake);
    }

    strategies
}

fn exit_behavior_metrics(trades: &[&UserTrade]) -> ExitBehaviorMetrics {
    let mut ordered = trades.to_vec();
    ordered.sort_by_key(|trade| trade.timestamp.unwrap_or(0));

    let mut lots: HashMap<String, VecDeque<PositionLot>> = HashMap::new();
    let mut buy_notional = 0.0;
    let mut sell_notional = 0.0;
    let mut matched_sell_trades = 0;
    let mut matched_size = 0.0;
    let mut weighted_holding_hours = 0.0;
    let mut quick_flip_size = 0.0;
    let mut take_profit_size = 0.0;
    let mut stop_loss_size = 0.0;

    for trade in ordered {
        let Some(price) = trade.price else {
            continue;
        };
        let Some(size) = trade.size else {
            continue;
        };
        let Some(timestamp) = trade.timestamp else {
            continue;
        };

        if size <= 0.0 || price <= 0.0 {
            continue;
        }

        let key = position_key(trade);

        if trade.side.eq_ignore_ascii_case("BUY") {
            buy_notional += price * size;
            lots.entry(key).or_default().push_back(PositionLot {
                remaining_size: size,
                price,
                timestamp,
            });
            continue;
        }

        if !trade.side.eq_ignore_ascii_case("SELL") {
            continue;
        }

        sell_notional += price * size;

        let mut remaining_sell_size = size;
        let mut matched_this_trade = false;
        let Some(queue) = lots.get_mut(&key) else {
            continue;
        };

        while remaining_sell_size > 0.000_001 {
            let Some(front) = queue.front_mut() else {
                break;
            };

            let matched = remaining_sell_size.min(front.remaining_size);
            let holding_hours = timestamp.saturating_sub(front.timestamp) as f64 / 3_600.0;
            let return_pct = (price - front.price) / front.price;

            matched_size += matched;
            weighted_holding_hours += holding_hours * matched;
            if holding_hours <= 24.0 {
                quick_flip_size += matched;
            }
            if return_pct >= 0.10 {
                take_profit_size += matched;
            } else if return_pct <= -0.10 {
                stop_loss_size += matched;
            }

            front.remaining_size -= matched;
            remaining_sell_size -= matched;
            matched_this_trade = true;

            if queue
                .front()
                .map(|front| front.remaining_size <= 0.000_001)
                .unwrap_or(false)
            {
                queue.pop_front();
            }
        }

        if matched_this_trade {
            matched_sell_trades += 1;
        }
    }

    let avg_holding_hours = if matched_size > 0.0 {
        Some(weighted_holding_hours / matched_size)
    } else {
        None
    };

    ExitBehaviorMetrics {
        sell_notional_usd: sell_notional,
        sell_notional_ratio: if buy_notional > 0.0 {
            sell_notional / buy_notional
        } else {
            0.0
        },
        matched_sell_trades,
        avg_holding_hours,
        quick_flip_ratio: ratio(quick_flip_size, matched_size),
        take_profit_sell_ratio: ratio(take_profit_size, matched_size),
        stop_loss_sell_ratio: ratio(stop_loss_size, matched_size),
    }
}

fn build_profile_notes(
    realized_roi: f64,
    top_5_profit_share: f64,
    median_trade_size_usd: f64,
    suspected_market_making: bool,
    strategies: &[StrategyArchetype],
    best_subcategories: &[SubcategoryMetric],
    best_price_bands: &[PriceBandMetric],
) -> Vec<String> {
    let mut notes = Vec::new();

    if let Some(best) = best_subcategories.first() {
        notes.push(format!(
            "优势子领域: {} ROI {:.1}% PnL ${:.2}",
            best.name,
            best.roi * 100.0,
            best.profit_usd
        ));
    }

    if let Some(best) = best_price_bands.first() {
        notes.push(format!(
            "优势价格区间: {} ROI {:.1}%",
            best.band,
            best.roi * 100.0
        ));
    }

    if strategies.contains(&StrategyArchetype::HeavyStakeSignal) {
        notes.push("历史重仓表现优于小额，重仓建仓更值得提高权重。".to_owned());
    }

    if suspected_market_making {
        notes.push("买卖双向且净仓位变化低，单笔成交更像价差/做市噪音。".to_owned());
    }

    if strategies.contains(&StrategyArchetype::EarlyExitTrader) {
        notes.push("该员工经常提前卖出，不一定等市场结算，SELL 提醒对跟单管理很关键。".to_owned());
    }

    if strategies.contains(&StrategyArchetype::ShortTermOperator) {
        notes.push("该员工存在短线快进快出特征，买入提醒的有效期应更短。".to_owned());
    }

    if top_5_profit_share >= 0.65 {
        notes.push(format!(
            "盈利集中度高，前 5 笔盈利贡献 {:.1}%。",
            top_5_profit_share * 100.0
        ));
    }

    if median_trade_size_usd > 0.0 {
        notes.push(format!(
            "历史匹配交易中位金额约 ${:.2}，整体 ROI {:.1}%。",
            median_trade_size_usd,
            realized_roi * 100.0
        ));
    }

    notes
}

fn profile_copy_score(
    closed_positions: usize,
    realized_roi: f64,
    win_position_ratio: f64,
    top_5_profit_share: f64,
    suspected_market_making: bool,
    strategies: &[StrategyArchetype],
) -> u8 {
    let mut score: i32 = 50;

    if closed_positions >= 20 {
        score += 10;
    } else if closed_positions >= 8 {
        score += 5;
    } else {
        score -= 15;
    }

    if realized_roi > 0.20 {
        score += 15;
    } else if realized_roi > 0.05 {
        score += 8;
    } else {
        score -= 10;
    }

    if win_position_ratio >= 0.60 {
        score += 10;
    } else if win_position_ratio < 0.45 && closed_positions >= 8 {
        score -= 10;
    }

    if strategies.contains(&StrategyArchetype::HeavyStakeSignal) {
        score += 8;
    }

    if suspected_market_making {
        score -= 30;
    }

    if top_5_profit_share >= 0.65 {
        score -= 15;
    }

    score.clamp(0, 100) as u8
}

fn suspected_market_making(
    matched_trades: usize,
    buy_trades: usize,
    sell_trades: usize,
    net_position_change_ratio: f64,
    repeated_market_ratio: f64,
    median_trade_size_usd: f64,
    avg_trade_size_usd: f64,
) -> bool {
    let two_sided = buy_trades >= 3 && sell_trades >= 3;
    let balanced_flow = if sell_trades > 0 {
        let ratio = buy_trades as f64 / sell_trades as f64;
        (0.40..=2.50).contains(&ratio)
    } else {
        false
    };
    let small_tickets = (median_trade_size_usd > 0.0 && median_trade_size_usd <= 75.0)
        || (avg_trade_size_usd > 0.0 && avg_trade_size_usd <= 100.0);

    matched_trades >= 10
        && two_sided
        && balanced_flow
        && net_position_change_ratio < 0.35
        && (repeated_market_ratio > 0.30 || small_tickets)
}

fn repeated_market_ratio(trades: &[&UserTrade]) -> f64 {
    if trades.is_empty() {
        return 0.0;
    }

    let mut counts: HashMap<&str, usize> = HashMap::new();
    for trade in trades {
        *counts.entry(trade.condition_id.as_str()).or_default() += 1;
    }

    let repeated = trades
        .iter()
        .filter(|trade| {
            counts
                .get(trade.condition_id.as_str())
                .copied()
                .unwrap_or(0)
                >= 3
        })
        .count();
    repeated as f64 / trades.len() as f64
}

fn top_profit_share(positions: &[ClosedPosition], top_n: usize) -> f64 {
    let mut profits = positions
        .iter()
        .map(|position| position.realized_pnl.unwrap_or(0.0).max(0.0))
        .filter(|profit| *profit > 0.0)
        .collect::<Vec<_>>();
    let total = profits.iter().sum::<f64>();

    if total <= 0.0 {
        return 0.0;
    }

    profits.sort_by(|left, right| right.total_cmp(left));
    profits.iter().take(top_n).sum::<f64>() / total
}

fn position_bucket_pnl(
    positions: &[ClosedPosition],
    threshold: f64,
    large_bucket: bool,
) -> (f64, f64, f64) {
    let mut pnl = 0.0;
    let mut invested = 0.0;
    let mut positive_pnl = 0.0;

    for position in positions {
        let bought = position.total_bought.unwrap_or(0.0).max(0.0);
        let is_large = threshold > 0.0 && bought >= threshold;

        if is_large == large_bucket {
            let realized = position.realized_pnl.unwrap_or(0.0);
            pnl += realized;
            invested += bought;
            positive_pnl += realized.max(0.0);
        }
    }

    (pnl, invested, positive_pnl)
}

fn position_key(trade: &UserTrade) -> String {
    format!("{}:{}", trade.condition_id, trade.asset)
}

fn median_position_price(positions: &[ClosedPosition]) -> f64 {
    percentile(
        positions
            .iter()
            .filter_map(|position| position.avg_price)
            .collect::<Vec<_>>(),
        0.50,
    )
}

fn trade_matches_employee(employee: &WatchedEmployee, trade: &UserTrade) -> bool {
    if employee.keywords.is_empty() {
        return true;
    }

    let haystack = trade_haystack(
        trade.title.as_deref(),
        trade.slug.as_deref(),
        trade.event_slug.as_deref(),
    );

    employee
        .keywords
        .iter()
        .any(|keyword| haystack.contains(&keyword.to_lowercase()))
}

fn position_matches_employee(employee: &WatchedEmployee, position: &ClosedPosition) -> bool {
    if employee.keywords.is_empty() {
        return true;
    }

    let haystack = trade_haystack(
        position.title.as_deref(),
        position.slug.as_deref(),
        position.event_slug.as_deref(),
    );

    employee
        .keywords
        .iter()
        .any(|keyword| haystack.contains(&keyword.to_lowercase()))
}

fn position_subcategory(employee: &WatchedEmployee, position: &ClosedPosition) -> String {
    let haystack = trade_haystack(
        position.title.as_deref(),
        position.slug.as_deref(),
        position.event_slug.as_deref(),
    );
    trade_subcategory_from_keywords(&employee.keywords, &employee.domain, haystack)
}

fn trade_subcategory(employee: &WatchedEmployee, trade: &UserTrade) -> String {
    let haystack = trade_haystack(
        trade.title.as_deref(),
        trade.slug.as_deref(),
        trade.event_slug.as_deref(),
    );
    trade_subcategory_from_keywords(&employee.keywords, &employee.domain, haystack)
}

fn trade_subcategory_from_keywords(keywords: &[String], domain: &str, haystack: String) -> String {
    keywords
        .iter()
        .find(|keyword| haystack.contains(&keyword.to_lowercase()))
        .cloned()
        .unwrap_or_else(|| format!("{}_other", domain.to_lowercase()))
}

fn trade_haystack(title: Option<&str>, slug: Option<&str>, event_slug: Option<&str>) -> String {
    format!(
        "{} {} {}",
        title.unwrap_or(""),
        slug.unwrap_or(""),
        event_slug.unwrap_or("")
    )
    .to_lowercase()
}

fn price_band(price: f64) -> String {
    match price {
        value if value < 0.20 => "0.00-0.20".to_owned(),
        value if value < 0.40 => "0.20-0.40".to_owned(),
        value if value < 0.60 => "0.40-0.60".to_owned(),
        value if value < 0.80 => "0.60-0.80".to_owned(),
        _ => "0.80-1.00".to_owned(),
    }
}

fn trade_notional(trade: &UserTrade) -> Option<f64> {
    Some(trade.price? * trade.size?)
}

fn sort_best_metrics(metrics: &mut [SubcategoryMetric]) {
    metrics.sort_by(|left, right| {
        right
            .roi
            .total_cmp(&left.roi)
            .then_with(|| right.profit_usd.total_cmp(&left.profit_usd))
            .then_with(|| right.closed_positions.cmp(&left.closed_positions))
    });
}

fn sort_best_price_metrics(metrics: &mut [PriceBandMetric]) {
    metrics.sort_by(|left, right| {
        right
            .roi
            .total_cmp(&left.roi)
            .then_with(|| right.profit_usd.total_cmp(&left.profit_usd))
            .then_with(|| right.closed_positions.cmp(&left.closed_positions))
    });
}

fn metric_names(metrics: &[SubcategoryMetric]) -> Vec<&str> {
    metrics
        .iter()
        .filter(|metric| metric.closed_positions > 0 || metric.trade_count > 0)
        .map(|metric| metric.name.as_str())
        .collect()
}

fn price_metric_names(metrics: &[PriceBandMetric]) -> Vec<&str> {
    metrics
        .iter()
        .filter(|metric| metric.closed_positions > 0 || metric.trade_count > 0)
        .map(|metric| metric.band.as_str())
        .collect()
}

fn signal_level(score: u8) -> CopySignalLevel {
    match score {
        80..=100 => CopySignalLevel::Strong,
        60..=79 => CopySignalLevel::Normal,
        40..=59 => CopySignalLevel::Watch,
        _ => CopySignalLevel::LowPriority,
    }
}

fn roi(profit: f64, invested: f64) -> f64 {
    if invested > 0.0 {
        profit / invested
    } else {
        0.0
    }
}

fn ratio(numerator: f64, denominator: f64) -> f64 {
    if denominator > 0.0 {
        numerator / denominator
    } else {
        0.0
    }
}

fn win_rate(wins: usize, count: usize) -> f64 {
    if count > 0 {
        round4(wins as f64 / count as f64)
    } else {
        0.0
    }
}

fn percentile(mut values: Vec<f64>, percentile: f64) -> f64 {
    values.retain(|value| value.is_finite());
    if values.is_empty() {
        return 0.0;
    }

    values.sort_by(|left, right| left.total_cmp(right));
    let rank = ((values.len() - 1) as f64 * percentile).round() as usize;
    values[rank.min(values.len() - 1)]
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

    fn employee() -> WatchedEmployee {
        WatchedEmployee::parse("0xemployee:surfandturf:SPORTS:nba|nfl|mlb").unwrap()
    }

    fn trade(side: &str, title: &str, condition_id: &str, price: f64, size: f64) -> UserTrade {
        UserTrade {
            proxy_wallet: "0xemployee".to_owned(),
            side: side.to_owned(),
            asset: "asset".to_owned(),
            condition_id: condition_id.to_owned(),
            size: Some(size),
            price: Some(price),
            timestamp: Some(1_800_000_000),
            title: Some(title.to_owned()),
            slug: Some(title.to_lowercase().replace(' ', "-")),
            event_slug: None,
            outcome: Some("Yes".to_owned()),
            outcome_index: Some(0),
            name: Some("employee".to_owned()),
            pseudonym: None,
            transaction_hash: Some(format!("0x{condition_id}{side}")),
        }
    }

    fn position(title: &str, avg_price: f64, bought: f64, pnl: f64) -> ClosedPosition {
        ClosedPosition {
            proxy_wallet: "0xemployee".to_owned(),
            asset: None,
            condition_id: None,
            avg_price: Some(avg_price),
            total_bought: Some(bought),
            realized_pnl: Some(pnl),
            cur_price: None,
            timestamp: Some(1_800_000_000),
            title: Some(title.to_owned()),
            slug: Some(title.to_lowercase().replace(' ', "-")),
            event_slug: None,
            outcome: Some("Yes".to_owned()),
            outcome_index: Some(0),
            opposite_outcome: None,
            opposite_asset: None,
            end_date: None,
        }
    }

    #[test]
    fn profile_marks_heavy_stake_signal() {
        let employee = employee();
        let trades = vec![
            trade("BUY", "NBA finals winner", "c1", 0.42, 3_000.0),
            trade("BUY", "NBA MVP", "c2", 0.35, 2_000.0),
            trade("BUY", "NFL champion", "c3", 0.50, 200.0),
        ];
        let positions = vec![
            position("NBA finals winner", 0.42, 1_200.0, 700.0),
            position("NBA MVP", 0.35, 900.0, 450.0),
            position("NFL champion", 0.50, 120.0, -20.0),
            position("MLB champion", 0.45, 100.0, 5.0),
            position("NBA playoffs", 0.40, 100.0, 10.0),
        ];

        let profile = build_employee_profile(&employee, &trades, &positions, 1_800_000_000);

        assert!(profile
            .strategy_archetypes
            .contains(&StrategyArchetype::HeavyStakeSignal));
        assert_eq!(profile.best_subcategories[0].name, "nba");
    }

    #[test]
    fn profile_flags_two_sided_small_flow_as_market_making() {
        let employee = employee();
        let mut trades = Vec::new();
        for index in 0..6 {
            trades.push(trade(
                "BUY",
                "NBA finals winner",
                "c1",
                0.45,
                50.0 + index as f64,
            ));
            trades.push(trade(
                "SELL",
                "NBA finals winner",
                "c1",
                0.46,
                48.0 + index as f64,
            ));
        }

        let profile = build_employee_profile(&employee, &trades, &[], 1_800_000_000);

        assert!(profile.suspected_market_making);
        assert!(profile
            .strategy_archetypes
            .contains(&StrategyArchetype::HighFrequencyMarketMaking));
    }

    #[test]
    fn profile_labels_short_term_early_exit_behavior() {
        let employee = employee();
        let mut trades = Vec::new();

        for index in 0..3 {
            let mut buy = trade(
                "BUY",
                "NBA finals winner",
                &format!("c{index}"),
                0.30,
                100.0,
            );
            buy.timestamp = Some(1_800_000_000 + (index * 10_000));
            trades.push(buy);

            let mut sell = trade(
                "SELL",
                "NBA finals winner",
                &format!("c{index}"),
                0.45,
                80.0,
            );
            sell.timestamp = Some(1_800_000_000 + (index * 10_000) + 3_600);
            trades.push(sell);
        }

        let profile = build_employee_profile(&employee, &trades, &[], 1_800_100_000);

        assert!(profile
            .strategy_archetypes
            .contains(&StrategyArchetype::EarlyExitTrader));
        assert!(profile
            .strategy_archetypes
            .contains(&StrategyArchetype::ShortTermOperator));
        assert!(profile.quick_flip_ratio > 0.0);
    }
}
