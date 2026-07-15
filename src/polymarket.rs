use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use std::{error::Error, fmt, process::Command};

const DEFAULT_DATA_API_BASE: &str = "https://data-api.polymarket.com";
const DEFAULT_PROXY_URL: &str = "http://127.0.0.1:7890";
const ACTIVITY_CONNECT_TIMEOUT_SECONDS: u64 = 3;
const ACTIVITY_MAX_TIME_SECONDS: u64 = 6;
const ACTIVITY_FALLBACK_CONNECT_TIMEOUT_SECONDS: u64 = 2;
const ACTIVITY_FALLBACK_MAX_TIME_SECONDS: u64 = 4;

#[derive(Debug, Clone)]
pub struct PolymarketDataClient {
    base_url: String,
    proxy_url: Option<String>,
}

impl PolymarketDataClient {
    pub fn new() -> Self {
        let base_url = std::env::var("POLYMARKET_DATA_API_BASE")
            .unwrap_or_else(|_| DEFAULT_DATA_API_BASE.to_owned());
        let proxy_url = std::env::var("POLYMARKET_PROXY_URL")
            .ok()
            .and_then(normalize_proxy_url)
            .or_else(|| Some(DEFAULT_PROXY_URL.to_owned()));

        Self {
            base_url,
            proxy_url,
        }
    }

    pub fn with_base_url(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            proxy_url: Some(DEFAULT_PROXY_URL.to_owned()),
        }
    }

    pub fn with_proxy_url(mut self, proxy_url: Option<String>) -> Self {
        self.proxy_url = proxy_url.and_then(normalize_proxy_url);
        self
    }

    pub fn leaderboard(
        &self,
        category: &str,
        time_period: &str,
        order_by: &str,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<LeaderboardEntry>, PolymarketError> {
        let url = format!("{}/v1/leaderboard", self.base_url);
        let query = [
            ("category", category.to_owned()),
            ("timePeriod", time_period.to_owned()),
            ("orderBy", order_by.to_owned()),
            ("limit", limit.clamp(1, 50).to_string()),
            ("offset", offset.to_string()),
        ];

        self.get_json(&url, &query)
    }

    pub fn closed_positions(
        &self,
        user: &str,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<ClosedPosition>, PolymarketError> {
        let url = format!("{}/closed-positions", self.base_url);
        let query = [
            ("user", user.to_owned()),
            ("limit", limit.clamp(1, 50).to_string()),
            ("offset", offset.to_string()),
            ("sortBy", "TIMESTAMP".to_owned()),
            ("sortDirection", "DESC".to_owned()),
        ];

        self.get_json(&url, &query)
    }

    pub fn closed_positions_history(
        &self,
        user: &str,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<ClosedPosition>, PolymarketError> {
        let url = format!("{}/closed-positions", self.base_url);
        let query = [
            ("user", user.to_owned()),
            ("limit", limit.clamp(1, 50).to_string()),
            ("offset", offset.to_string()),
            ("sortBy", "TIMESTAMP".to_owned()),
            ("sortDirection", "DESC".to_owned()),
        ];

        self.get_json_with_timeouts(&url, &query, 20, 60)
    }

    pub fn positions(
        &self,
        user: &str,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<CurrentPosition>, PolymarketError> {
        let url = format!("{}/positions", self.base_url);
        let query = [
            ("user", user.to_owned()),
            ("limit", limit.clamp(1, 50).to_string()),
            ("offset", offset.to_string()),
            ("sortBy", "CASHPNL".to_owned()),
            ("sortDirection", "ASC".to_owned()),
        ];

        self.get_json(&url, &query)
    }

    pub fn positions_fast(
        &self,
        user: &str,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<CurrentPosition>, PolymarketError> {
        let url = format!("{}/positions", self.base_url);
        let query = [
            ("user", user.to_owned()),
            ("limit", limit.clamp(1, 50).to_string()),
            ("offset", offset.to_string()),
            ("sortBy", "CASHPNL".to_owned()),
            ("sortDirection", "ASC".to_owned()),
        ];

        self.get_activity_json_with_failover(&url, &query)
    }

    pub fn positions_history(
        &self,
        user: &str,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<CurrentPosition>, PolymarketError> {
        let url = format!("{}/positions", self.base_url);
        let query = [
            ("user", user.to_owned()),
            ("limit", limit.clamp(1, 50).to_string()),
            ("offset", offset.to_string()),
            ("sortBy", "CASHPNL".to_owned()),
            ("sortDirection", "ASC".to_owned()),
        ];

        self.get_json_with_timeouts(&url, &query, 20, 60)
    }

    pub fn trades(
        &self,
        user: &str,
        limit: usize,
        offset: usize,
        side: Option<&str>,
    ) -> Result<Vec<UserTrade>, PolymarketError> {
        let url = format!("{}/trades", self.base_url);
        let limit = limit.clamp(1, 100).to_string();
        let offset = offset.to_string();
        let mut query = vec![
            ("user", user.to_owned()),
            ("limit", limit),
            ("offset", offset),
        ];

        if let Some(side) = side {
            query.push(("side", side.to_owned()));
        }

        self.get_json(&url, &query)
    }

    pub fn trades_history(
        &self,
        user: &str,
        limit: usize,
        offset: usize,
        side: Option<&str>,
    ) -> Result<Vec<UserTrade>, PolymarketError> {
        let url = format!("{}/trades", self.base_url);
        let mut query = vec![
            ("user", user.to_owned()),
            ("limit", limit.clamp(1, 100).to_string()),
            ("offset", offset.to_string()),
        ];
        if let Some(side) = side {
            query.push(("side", side.to_owned()));
        }

        self.get_json_with_timeouts(&url, &query, 20, 60)
    }

    pub fn global_trades(
        &self,
        limit: usize,
        offset: usize,
        side: Option<&str>,
    ) -> Result<Vec<UserTrade>, PolymarketError> {
        let url = format!("{}/trades", self.base_url);
        let limit = limit.clamp(1, 100).to_string();
        let offset = offset.to_string();
        let mut query = vec![("limit", limit), ("offset", offset)];

        if let Some(side) = side {
            query.push(("side", side.to_owned()));
        }

        self.get_json(&url, &query)
    }

    pub fn activity(
        &self,
        user: &str,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<UserTrade>, PolymarketError> {
        let url = format!("{}/activity", self.base_url);
        let query = [
            ("user", user.to_owned()),
            ("limit", limit.clamp(1, 100).to_string()),
            ("offset", offset.to_string()),
        ];

        self.get_json_with_timeouts(&url, &query, 4, 10)
    }

    pub fn activity_history(
        &self,
        user: &str,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<UserTrade>, PolymarketError> {
        let url = format!("{}/activity", self.base_url);
        let query = [
            ("user", user.to_owned()),
            ("limit", limit.clamp(1, 100).to_string()),
            ("offset", offset.to_string()),
        ];

        self.get_json_with_timeouts(&url, &query, 20, 60)
    }

    fn get_json<T>(&self, url: &str, query: &[(&str, String)]) -> Result<T, PolymarketError>
    where
        T: for<'de> Deserialize<'de>,
    {
        self.get_json_with_timeouts(url, query, 8, 20)
    }

    fn get_json_with_timeouts<T>(
        &self,
        url: &str,
        query: &[(&str, String)],
        connect_timeout_seconds: u64,
        max_time_seconds: u64,
    ) -> Result<T, PolymarketError>
    where
        T: for<'de> Deserialize<'de>,
    {
        self.get_json_with_timeouts_via(
            url,
            query,
            connect_timeout_seconds,
            max_time_seconds,
            self.proxy_url.as_deref(),
        )
    }

    fn get_activity_json_with_failover<T>(
        &self,
        url: &str,
        query: &[(&str, String)],
    ) -> Result<T, PolymarketError>
    where
        T: for<'de> Deserialize<'de>,
    {
        match self.get_json_with_timeouts_via(
            url,
            query,
            ACTIVITY_CONNECT_TIMEOUT_SECONDS,
            ACTIVITY_MAX_TIME_SECONDS,
            self.proxy_url.as_deref(),
        ) {
            Ok(value) => Ok(value),
            Err(primary_error) => {
                if !is_retryable_hot_path_error(&primary_error) {
                    return Err(primary_error);
                }
                let Some(fallback_proxy_url) = self.activity_fallback_proxy_url() else {
                    return Err(primary_error);
                };
                if fallback_proxy_url.as_deref() == self.proxy_url.as_deref() {
                    return Err(primary_error);
                }

                self.get_json_with_timeouts_via(
                    url,
                    query,
                    ACTIVITY_FALLBACK_CONNECT_TIMEOUT_SECONDS,
                    ACTIVITY_FALLBACK_MAX_TIME_SECONDS,
                    fallback_proxy_url.as_deref(),
                )
                .map_err(|fallback_error| PolymarketError::Failover {
                    primary: Box::new(primary_error),
                    fallback_route: route_label(fallback_proxy_url.as_deref()),
                    fallback: Box::new(fallback_error),
                })
            }
        }
    }

    fn activity_fallback_proxy_url(&self) -> Option<Option<String>> {
        if let Ok(value) = std::env::var("POLYMARKET_ACTIVITY_FALLBACK_PROXY_URL") {
            return Some(normalize_proxy_url(value));
        }

        self.proxy_url.as_ref().map(|_| None)
    }

    fn get_json_with_timeouts_via<T>(
        &self,
        url: &str,
        query: &[(&str, String)],
        connect_timeout_seconds: u64,
        max_time_seconds: u64,
        proxy_url: Option<&str>,
    ) -> Result<T, PolymarketError>
    where
        T: for<'de> Deserialize<'de>,
    {
        let mut command = Command::new("curl");
        command.args([
            "--fail",
            "--silent",
            "--show-error",
            "--location",
            "--connect-timeout",
            &connect_timeout_seconds.to_string(),
            "--max-time",
            &max_time_seconds.to_string(),
            "--get",
            url,
        ]);

        if let Some(proxy_url) = proxy_url {
            command.args(["--proxy", proxy_url]);
        }

        for (key, value) in query {
            command.args(["--data-urlencode", &format!("{key}={value}")]);
        }

        let output = command.output().map_err(PolymarketError::Command)?;

        if !output.status.success() {
            return Err(PolymarketError::Status {
                code: output.status.code(),
                stderr: String::from_utf8_lossy(&output.stderr).to_string(),
            });
        }

        serde_json::from_slice::<T>(&output.stdout).map_err(PolymarketError::Json)
    }
}

impl Default for PolymarketDataClient {
    fn default() -> Self {
        Self::new()
    }
}

fn normalize_proxy_url(proxy_url: String) -> Option<String> {
    let proxy_url = proxy_url.trim();

    if proxy_url.is_empty()
        || proxy_url.eq_ignore_ascii_case("none")
        || proxy_url.eq_ignore_ascii_case("off")
        || proxy_url.eq_ignore_ascii_case("direct")
    {
        None
    } else {
        Some(proxy_url.to_owned())
    }
}

fn is_retryable_hot_path_error(error: &PolymarketError) -> bool {
    match error {
        PolymarketError::Command(_) => true,
        PolymarketError::Status { code, stderr } => {
            if *code == Some(22) && stderr.contains("400") {
                return false;
            }
            true
        }
        PolymarketError::Json(_) => false,
        PolymarketError::Failover { .. } => true,
    }
}

fn route_label(proxy_url: Option<&str>) -> String {
    proxy_url
        .map(|url| format!("proxy {url}"))
        .unwrap_or_else(|| "direct".to_owned())
}

#[derive(Debug)]
pub enum PolymarketError {
    Command(std::io::Error),
    Status {
        code: Option<i32>,
        stderr: String,
    },
    Json(serde_json::Error),
    Failover {
        primary: Box<PolymarketError>,
        fallback_route: String,
        fallback: Box<PolymarketError>,
    },
}

impl fmt::Display for PolymarketError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Command(error) => write!(f, "failed to run curl: {error}"),
            Self::Status { code, stderr } => {
                write!(f, "curl exited with code {code:?}: {stderr}")
            }
            Self::Json(error) => write!(f, "failed to parse Polymarket JSON: {error}"),
            Self::Failover {
                primary,
                fallback_route,
                fallback,
            } => write!(
                f,
                "primary /activity route failed: {primary}; fallback route {fallback_route} failed: {fallback}"
            ),
        }
    }
}

impl Error for PolymarketError {}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct LeaderboardEntry {
    #[serde(default, deserialize_with = "optional_string")]
    pub rank: Option<String>,
    #[serde(rename = "proxyWallet")]
    pub proxy_wallet: String,
    #[serde(rename = "userName", default)]
    pub user_name: Option<String>,
    #[serde(default, deserialize_with = "optional_f64")]
    pub vol: Option<f64>,
    #[serde(default, deserialize_with = "optional_f64")]
    pub pnl: Option<f64>,
    #[serde(rename = "xUsername", default)]
    pub x_username: Option<String>,
    #[serde(rename = "verifiedBadge", default)]
    pub verified_badge: Option<bool>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ClosedPosition {
    #[serde(rename = "proxyWallet")]
    pub proxy_wallet: String,
    #[serde(default)]
    pub asset: Option<String>,
    #[serde(rename = "conditionId", default)]
    pub condition_id: Option<String>,
    #[serde(rename = "avgPrice", default, deserialize_with = "optional_f64")]
    pub avg_price: Option<f64>,
    #[serde(rename = "totalBought", default, deserialize_with = "optional_f64")]
    pub total_bought: Option<f64>,
    #[serde(rename = "realizedPnl", default, deserialize_with = "optional_f64")]
    pub realized_pnl: Option<f64>,
    #[serde(rename = "curPrice", default, deserialize_with = "optional_f64")]
    pub cur_price: Option<f64>,
    #[serde(default, deserialize_with = "optional_u64")]
    pub timestamp: Option<u64>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub slug: Option<String>,
    #[serde(rename = "eventSlug", default)]
    pub event_slug: Option<String>,
    #[serde(default)]
    pub outcome: Option<String>,
    #[serde(rename = "outcomeIndex", default, deserialize_with = "optional_u32")]
    pub outcome_index: Option<u32>,
    #[serde(rename = "oppositeOutcome", default)]
    pub opposite_outcome: Option<String>,
    #[serde(rename = "oppositeAsset", default)]
    pub opposite_asset: Option<String>,
    #[serde(rename = "endDate", default)]
    pub end_date: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CurrentPosition {
    #[serde(rename = "proxyWallet")]
    pub proxy_wallet: String,
    #[serde(default)]
    pub asset: Option<String>,
    #[serde(rename = "conditionId", default)]
    pub condition_id: Option<String>,
    #[serde(default, deserialize_with = "optional_f64")]
    pub size: Option<f64>,
    #[serde(rename = "avgPrice", default, deserialize_with = "optional_f64")]
    pub avg_price: Option<f64>,
    #[serde(rename = "initialValue", default, deserialize_with = "optional_f64")]
    pub initial_value: Option<f64>,
    #[serde(rename = "currentValue", default, deserialize_with = "optional_f64")]
    pub current_value: Option<f64>,
    #[serde(rename = "cashPnl", default, deserialize_with = "optional_f64")]
    pub cash_pnl: Option<f64>,
    #[serde(rename = "percentPnl", default, deserialize_with = "optional_f64")]
    pub percent_pnl: Option<f64>,
    #[serde(rename = "totalBought", default, deserialize_with = "optional_f64")]
    pub total_bought: Option<f64>,
    #[serde(rename = "realizedPnl", default, deserialize_with = "optional_f64")]
    pub realized_pnl: Option<f64>,
    #[serde(rename = "curPrice", default, deserialize_with = "optional_f64")]
    pub cur_price: Option<f64>,
    #[serde(default)]
    pub redeemable: Option<bool>,
    #[serde(default)]
    pub mergeable: Option<bool>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub slug: Option<String>,
    #[serde(rename = "eventSlug", default)]
    pub event_slug: Option<String>,
    #[serde(default)]
    pub outcome: Option<String>,
    #[serde(rename = "outcomeIndex", default, deserialize_with = "optional_u32")]
    pub outcome_index: Option<u32>,
    #[serde(rename = "oppositeOutcome", default)]
    pub opposite_outcome: Option<String>,
    #[serde(rename = "endDate", default)]
    pub end_date: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct UserTrade {
    #[serde(rename = "proxyWallet")]
    pub proxy_wallet: String,
    pub side: String,
    pub asset: String,
    #[serde(rename = "conditionId")]
    pub condition_id: String,
    #[serde(default, deserialize_with = "optional_f64")]
    pub size: Option<f64>,
    #[serde(default, deserialize_with = "optional_f64")]
    pub price: Option<f64>,
    #[serde(default, deserialize_with = "optional_u64")]
    pub timestamp: Option<u64>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub slug: Option<String>,
    #[serde(rename = "eventSlug", default)]
    pub event_slug: Option<String>,
    #[serde(default)]
    pub outcome: Option<String>,
    #[serde(rename = "outcomeIndex", default, deserialize_with = "optional_u32")]
    pub outcome_index: Option<u32>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub pseudonym: Option<String>,
    #[serde(rename = "transactionHash", default)]
    pub transaction_hash: Option<String>,
}

fn optional_f64<'de, D>(deserializer: D) -> Result<Option<f64>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<Value>::deserialize(deserializer)?;
    Ok(match value {
        Some(Value::Number(number)) => number.as_f64(),
        Some(Value::String(text)) => text.parse::<f64>().ok(),
        _ => None,
    })
}

fn optional_u64<'de, D>(deserializer: D) -> Result<Option<u64>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<Value>::deserialize(deserializer)?;
    Ok(match value {
        Some(Value::Number(number)) => number.as_u64(),
        Some(Value::String(text)) => text.parse::<u64>().ok(),
        _ => None,
    })
}

fn optional_u32<'de, D>(deserializer: D) -> Result<Option<u32>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<Value>::deserialize(deserializer)?;
    Ok(match value {
        Some(Value::Number(number)) => number.as_u64().and_then(|value| value.try_into().ok()),
        Some(Value::String(text)) => text.parse::<u32>().ok(),
        _ => None,
    })
}

fn optional_string<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<Value>::deserialize(deserializer)?;
    Ok(match value {
        Some(Value::String(text)) => Some(text),
        Some(Value::Number(number)) => Some(number.to_string()),
        Some(Value::Bool(value)) => Some(value.to_string()),
        _ => None,
    })
}
