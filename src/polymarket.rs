use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use std::{error::Error, fmt, process::Command};

const DEFAULT_DATA_API_BASE: &str = "https://data-api.polymarket.com";
const DEFAULT_PROXY_URL: &str = "http://127.0.0.1:7890";

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

        self.get_json(&url, &query)
    }

    fn get_json<T>(&self, url: &str, query: &[(&str, String)]) -> Result<T, PolymarketError>
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
            "8",
            "--max-time",
            "20",
            "--get",
            url,
        ]);

        if let Some(proxy_url) = &self.proxy_url {
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

#[derive(Debug)]
pub enum PolymarketError {
    Command(std::io::Error),
    Status { code: Option<i32>, stderr: String },
    Json(serde_json::Error),
}

impl fmt::Display for PolymarketError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Command(error) => write!(f, "failed to run curl: {error}"),
            Self::Status { code, stderr } => {
                write!(f, "curl exited with code {code:?}: {stderr}")
            }
            Self::Json(error) => write!(f, "failed to parse Polymarket JSON: {error}"),
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
