use crate::model::{
    DiscoverySignal, MarketId, MarketSnapshot, WalletCandidate, WalletId, WalletMetrics,
    WalletTrade,
};

#[derive(Debug, Clone)]
pub enum DataError {
    Unavailable(String),
    InvalidResponse(String),
    RateLimited,
}

pub type DataResult<T> = Result<T, DataError>;

pub trait LeaderboardSource {
    fn load_candidates(
        &self,
        category: Option<&str>,
        limit: usize,
    ) -> DataResult<Vec<WalletCandidate>>;
}

pub trait WalletActivitySource {
    fn load_wallet_trades(&self, wallet: &WalletId) -> DataResult<Vec<WalletTrade>>;
}

pub trait MarketDataSource {
    fn load_market_snapshot(&self, market: &MarketId) -> DataResult<MarketSnapshot>;
}

pub trait MetricsStore {
    fn save_wallet_metrics(&self, metrics: &WalletMetrics) -> DataResult<()>;
    fn load_wallet_metrics(&self, wallet: &WalletId) -> DataResult<Option<WalletMetrics>>;
}

pub trait SignalSink {
    fn publish_discovery_signal(&self, signal: &DiscoverySignal) -> DataResult<()>;
}

pub struct StdoutSignalSink;

impl SignalSink for StdoutSignalSink {
    fn publish_discovery_signal(&self, signal: &DiscoverySignal) -> DataResult<()> {
        println!(
            "signal wallet={} market={} side={:?} price={:.3} max_follow={:.3} confidence={:.2} budget=${:.2}",
            signal.source_wallet,
            signal.market_id.0,
            signal.side,
            signal.observed_price,
            signal.max_follow_price,
            signal.confidence,
            signal.suggested_budget_usd
        );
        Ok(())
    }
}
