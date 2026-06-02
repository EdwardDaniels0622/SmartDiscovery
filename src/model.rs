use std::fmt;

pub type TimestampMs = u64;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct WalletId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MarketId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AssetId(pub String);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutcomeSide {
    Yes,
    No,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiquidityRole {
    Maker,
    Taker,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WalletFlag {
    InsufficientSample,
    HighLateEntryRate,
    HighHedgeRate,
    MakerLikeFlow,
    LowLiquidityReplicability,
    Inactive,
}

#[derive(Debug, Clone)]
pub struct WalletTrade {
    pub wallet: WalletId,
    pub market_id: MarketId,
    pub asset_id: AssetId,
    pub side: OutcomeSide,
    pub price: f64,
    pub size_usd: f64,
    pub timestamp_ms: TimestampMs,
    pub role: LiquidityRole,
}

#[derive(Debug, Clone)]
pub struct MarketSnapshot {
    pub market_id: MarketId,
    pub category: Option<String>,
    pub resolved: bool,
    pub winning_side: Option<OutcomeSide>,
    pub best_bid: Option<f64>,
    pub best_ask: Option<f64>,
    pub current_price: Option<f64>,
    pub spread: Option<f64>,
    pub depth_to_one_cent_usd: Option<f64>,
    pub depth_to_three_cents_usd: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct WalletCandidate {
    pub wallet: WalletId,
    pub display_name: Option<String>,
    pub source_rank: Option<u32>,
    pub source_pnl_usd: Option<f64>,
    pub source_volume_usd: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct WalletMetrics {
    pub wallet: WalletId,
    pub total_trades: u32,
    pub resolved_markets: u32,
    pub realized_pnl_usd: f64,
    pub realized_roi: f64,
    pub max_drawdown: f64,
    pub positive_month_ratio: f64,
    pub median_entry_price: f64,
    pub late_entry_ratio: f64,
    pub hedge_market_ratio: f64,
    pub maker_like_ratio: f64,
    pub avg_clv_1h: f64,
    pub avg_clv_24h: f64,
    pub copyable_trade_ratio: f64,
    pub liquidity_replicability: f64,
    pub recency_score: f64,
    pub category_focus_score: f64,
}

#[derive(Debug, Clone)]
pub struct DiscoveryConfig {
    pub min_trades: u32,
    pub min_resolved_markets: u32,
    pub max_late_entry_ratio: f64,
    pub max_hedge_market_ratio: f64,
    pub max_maker_like_ratio: f64,
    pub min_liquidity_replicability: f64,
    pub min_recency_score: f64,
}

impl Default for DiscoveryConfig {
    fn default() -> Self {
        Self {
            min_trades: 50,
            min_resolved_markets: 30,
            max_late_entry_ratio: 0.30,
            max_hedge_market_ratio: 0.20,
            max_maker_like_ratio: 0.35,
            min_liquidity_replicability: 0.45,
            min_recency_score: 0.25,
        }
    }
}

#[derive(Debug, Clone)]
pub struct WalletScore {
    pub wallet: WalletId,
    pub total: f64,
    pub profitability: f64,
    pub consistency: f64,
    pub entry_edge: f64,
    pub low_hedge_behavior: f64,
    pub liquidity_replicability: f64,
    pub recency: f64,
    pub category_focus: f64,
    pub flags: Vec<WalletFlag>,
}

#[derive(Debug, Clone)]
pub struct DiscoverySignal {
    pub source_wallet: WalletId,
    pub market_id: MarketId,
    pub asset_id: AssetId,
    pub side: OutcomeSide,
    pub observed_price: f64,
    pub max_follow_price: f64,
    pub confidence: f64,
    pub suggested_budget_usd: f64,
    pub expires_at_ms: TimestampMs,
    pub reasons: Vec<String>,
}

impl fmt::Display for WalletId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
