use crate::model::{DiscoveryConfig, WalletFlag, WalletId, WalletMetrics, WalletScore};

pub fn score_wallet(metrics: &WalletMetrics, config: &DiscoveryConfig) -> WalletScore {
    let profitability = score_profitability(metrics);
    let consistency = score_consistency(metrics);
    let entry_edge = score_entry_edge(metrics);
    let low_hedge_behavior =
        1.0 - clamp01(metrics.hedge_market_ratio / config.max_hedge_market_ratio);
    let liquidity_replicability = clamp01(metrics.liquidity_replicability);
    let recency = clamp01(metrics.recency_score);
    let category_focus = clamp01(metrics.category_focus_score);

    let flags = collect_flags(metrics, config);
    let penalty = if flags.contains(&WalletFlag::InsufficientSample) {
        0.55
    } else {
        1.0
    };

    let total = penalty
        * ((profitability * 0.25)
            + (consistency * 0.15)
            + (entry_edge * 0.22)
            + (low_hedge_behavior * 0.12)
            + (liquidity_replicability * 0.12)
            + (recency * 0.08)
            + (category_focus * 0.06));

    WalletScore {
        wallet: WalletId(metrics.wallet.0.clone()),
        total: round4(total),
        profitability: round4(profitability),
        consistency: round4(consistency),
        entry_edge: round4(entry_edge),
        low_hedge_behavior: round4(low_hedge_behavior),
        liquidity_replicability: round4(liquidity_replicability),
        recency: round4(recency),
        category_focus: round4(category_focus),
        flags,
    }
}

fn score_profitability(metrics: &WalletMetrics) -> f64 {
    let roi_component = clamp01((metrics.realized_roi + 0.20) / 1.20);
    let drawdown_component = 1.0 - clamp01(metrics.max_drawdown / 0.80);
    (roi_component * 0.70) + (drawdown_component * 0.30)
}

fn score_consistency(metrics: &WalletMetrics) -> f64 {
    let month_component = clamp01(metrics.positive_month_ratio);
    let sample_component = clamp01(metrics.resolved_markets as f64 / 80.0);
    (month_component * 0.70) + (sample_component * 0.30)
}

fn score_entry_edge(metrics: &WalletMetrics) -> f64 {
    let clv_component = clamp01((metrics.avg_clv_24h + 0.05) / 0.15);
    let early_entry_component = 1.0 - clamp01(metrics.late_entry_ratio / 0.50);
    let median_entry_component = 1.0 - clamp01((metrics.median_entry_price - 0.55) / 0.40);
    (clv_component * 0.50) + (early_entry_component * 0.30) + (median_entry_component * 0.20)
}

fn collect_flags(metrics: &WalletMetrics, config: &DiscoveryConfig) -> Vec<WalletFlag> {
    let mut flags = Vec::new();

    if metrics.total_trades < config.min_trades
        || metrics.resolved_markets < config.min_resolved_markets
    {
        flags.push(WalletFlag::InsufficientSample);
    }

    if metrics.late_entry_ratio > config.max_late_entry_ratio {
        flags.push(WalletFlag::HighLateEntryRate);
    }

    if metrics.hedge_market_ratio > config.max_hedge_market_ratio {
        flags.push(WalletFlag::HighHedgeRate);
    }

    if metrics.maker_like_ratio > config.max_maker_like_ratio {
        flags.push(WalletFlag::MakerLikeFlow);
    }

    if metrics.liquidity_replicability < config.min_liquidity_replicability {
        flags.push(WalletFlag::LowLiquidityReplicability);
    }

    if metrics.recency_score < config.min_recency_score {
        flags.push(WalletFlag::Inactive);
    }

    flags
}

fn clamp01(value: f64) -> f64 {
    value.max(0.0).min(1.0)
}

fn round4(value: f64) -> f64 {
    (value * 10_000.0).round() / 10_000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_metrics(wallet: &str) -> WalletMetrics {
        WalletMetrics {
            wallet: WalletId(wallet.to_owned()),
            total_trades: 120,
            resolved_markets: 65,
            realized_pnl_usd: 12_500.0,
            realized_roi: 0.62,
            max_drawdown: 0.18,
            positive_month_ratio: 0.76,
            median_entry_price: 0.57,
            late_entry_ratio: 0.11,
            hedge_market_ratio: 0.08,
            maker_like_ratio: 0.12,
            avg_clv_1h: 0.018,
            avg_clv_24h: 0.045,
            copyable_trade_ratio: 0.64,
            liquidity_replicability: 0.72,
            recency_score: 0.88,
            category_focus_score: 0.70,
        }
    }

    #[test]
    fn good_wallet_scores_high_without_flags() {
        let score = score_wallet(&base_metrics("0xsmart"), &DiscoveryConfig::default());

        assert!(score.total > 0.70);
        assert!(score.flags.is_empty());
    }

    #[test]
    fn thin_sample_is_penalized() {
        let mut metrics = base_metrics("0xthin");
        metrics.total_trades = 8;
        metrics.resolved_markets = 4;

        let score = score_wallet(&metrics, &DiscoveryConfig::default());

        assert!(score.total < 0.50);
        assert!(score.flags.contains(&WalletFlag::InsufficientSample));
    }

    #[test]
    fn hedged_maker_like_wallet_is_flagged() {
        let mut metrics = base_metrics("0xmaker");
        metrics.hedge_market_ratio = 0.44;
        metrics.maker_like_ratio = 0.55;

        let score = score_wallet(&metrics, &DiscoveryConfig::default());

        assert!(score.flags.contains(&WalletFlag::HighHedgeRate));
        assert!(score.flags.contains(&WalletFlag::MakerLikeFlow));
    }
}
