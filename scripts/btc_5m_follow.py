#!/usr/bin/env python3
"""Paper-first BTC 5m Polymarket UP/DOWN follower.

The script uses Binance BTCUSDT 5m candles for trend, Gamma for BTC 5m
market metadata/results, CLOB order books for best asks, and the existing
Polymarket executor only when live trading is explicitly enabled.
"""

from __future__ import annotations

import argparse
import html
import json
import math
import os
import re
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request
from dataclasses import asdict, dataclass, field
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Dict, List, Optional, Tuple

try:
    from zoneinfo import ZoneInfo
except ImportError:  # pragma: no cover
    ZoneInfo = None  # type: ignore


ROOT = Path(__file__).resolve().parents[1]
DEFAULT_CONFIG_PATH = ROOT / "config" / "btc_5m_follow.json"
DEFAULT_STATE_PATH = ROOT / "state" / "btc_5m_follow_state.json"
DEFAULT_LOG_PATH = ROOT / "logs" / "btc_5m_follow_decisions.jsonl"
DEFAULT_EXECUTOR_PATH = ROOT / "scripts" / "polymarket_executor.sh"
BTC_5M_SLUG_RE = re.compile(r"^btc-updown-5m-(\d+)$")
UP = "UP"
DOWN = "DOWN"
NO_TREND = "NO_TREND"
UNKNOWN = "UNKNOWN"
OFFICIAL_RESULT_SOURCE = "official_polymarket"
COMPUTED_RESULT_SOURCE = "computed_price_to_beat"
COMPUTED_RESULT_CHECK_INTERVAL_SECONDS = 60
COMPUTED_RESULT_STATE_LIMIT = 500
DEFAULT_SHADOW_STRATEGIES = [
    "base_v1",
    "delta_filter_v2",
    "price60_v3",
    "current_side_price60_v4",
    "fast_c_v5",
    "anti_previous_result_v6",
    "anti_previous_result_v7",
    "anti_previous_result_v8",
    "last_minute_favorite_v13",
]


@dataclass
class Config:
    enabled: bool = False
    mode: str = "paper"
    fixed_amount_usdc: float = 1.0
    min_entry_price: float = 0.0
    max_entry_price: float = 0.51
    hard_max_entry_price: float = 0.52
    max_previous_result_latency_seconds: Optional[int] = None
    main_strategy: str = "trend_follow_v1"
    trend_deadband_pct: float = 0.00075
    trend_lookback_bars: int = 6
    warmup_hours: float = 3.0
    confirm_streak: int = 1
    max_consecutive_losses: int = 2
    entry_delay_seconds: int = 5
    latest_entry_seconds_before_close: int = 90
    trade_up: bool = False
    trade_down: bool = True
    max_trades_per_day: int = 80
    max_daily_loss_usdc: float = 50.0
    poll_seconds: int = 5
    timezone: str = "Asia/Shanghai"
    state_path: str = str(DEFAULT_STATE_PATH.relative_to(ROOT))
    log_path: str = str(DEFAULT_LOG_PATH.relative_to(ROOT))
    binance_base_url: str = "https://api.binance.com"
    gamma_api_base: str = "https://gamma-api.polymarket.com"
    clob_api_base: str = "https://clob.polymarket.com"
    polymarket_web_base: str = "https://polymarket.com"
    polymarket_page_locale: str = "zh"
    polymarket_proxy_url: Optional[str] = "http://127.0.0.1:7890"
    binance_proxy_url: Optional[str] = "http://127.0.0.1:7890"
    executor_path: str = str(DEFAULT_EXECUTOR_PATH.relative_to(ROOT))
    http_timeout_seconds: int = 12
    http_retries: int = 2
    settlement_check_delay_seconds: int = 20
    page_price_missing_retry_seconds: int = 2
    shadow_strategies: List[str] = field(default_factory=lambda: list(DEFAULT_SHADOW_STRATEGIES))

    @classmethod
    def load(cls, path: Optional[Path]) -> "Config":
        config = cls()
        if path is None:
            path = DEFAULT_CONFIG_PATH if DEFAULT_CONFIG_PATH.exists() else None
        if path is not None and path.exists():
            payload = json.loads(path.read_text())
            if not isinstance(payload, dict):
                raise ValueError(f"config must be a JSON object: {path}")
            known = set(cls.__dataclass_fields__.keys())
            for key, value in payload.items():
                if key in known:
                    setattr(config, key, value)
        config.validate()
        return config

    def validate(self) -> None:
        self.mode = str(self.mode).lower()
        if self.mode not in {"paper", "live"}:
            raise ValueError("mode must be paper or live")
        if self.fixed_amount_usdc <= 0:
            raise ValueError("fixed_amount_usdc must be positive")
        if not 0 <= self.min_entry_price < 1:
            raise ValueError("min_entry_price must be between 0 and 1")
        if not 0 < self.max_entry_price < 1:
            raise ValueError("max_entry_price must be between 0 and 1")
        if not 0 < self.hard_max_entry_price < 1:
            raise ValueError("hard_max_entry_price must be between 0 and 1")
        if self.min_entry_price >= self.max_entry_price:
            raise ValueError("min_entry_price must be below max_entry_price")
        if self.max_entry_price > self.hard_max_entry_price:
            raise ValueError("max_entry_price cannot exceed hard_max_entry_price")
        if self.max_previous_result_latency_seconds is not None:
            self.max_previous_result_latency_seconds = max(
                0,
                int(self.max_previous_result_latency_seconds),
            )
        self.trend_lookback_bars = max(1, int(self.trend_lookback_bars))
        self.confirm_streak = max(1, int(self.confirm_streak))
        self.max_consecutive_losses = max(1, int(self.max_consecutive_losses))
        self.entry_delay_seconds = max(0, int(self.entry_delay_seconds))
        self.latest_entry_seconds_before_close = max(0, int(self.latest_entry_seconds_before_close))
        self.max_trades_per_day = max(0, int(self.max_trades_per_day))
        self.poll_seconds = max(1, int(self.poll_seconds))
        self.http_timeout_seconds = max(1, int(self.http_timeout_seconds))
        self.http_retries = max(0, int(self.http_retries))
        self.page_price_missing_retry_seconds = max(
            1,
            int(self.page_price_missing_retry_seconds),
        )
        if isinstance(self.shadow_strategies, str):
            self.shadow_strategies = [
                item.strip()
                for item in self.shadow_strategies.split(",")
                if item.strip()
            ]
        elif not isinstance(self.shadow_strategies, list):
            self.shadow_strategies = []
        self.main_strategy = str(self.main_strategy or "trend_follow_v1").strip()
        self.shadow_strategies = [
            str(item).strip()
            for item in self.shadow_strategies
            if str(item).strip()
        ]

    def state_file(self) -> Path:
        return resolve_path(self.state_path)

    def log_file(self) -> Path:
        return resolve_path(self.log_path)

    def executor_file(self) -> Path:
        return resolve_path(self.executor_path)


@dataclass
class Candle:
    open_time_ms: int
    open: float
    high: float
    low: float
    close: float
    close_time_ms: int


@dataclass
class Market:
    slug: str
    start_ts: int
    end_ts: int
    up_token_id: Optional[str]
    down_token_id: Optional[str]
    price_to_beat: Optional[float]
    final_price: Optional[float]
    raw: Dict[str, Any] = field(repr=False)

    @property
    def start_iso(self) -> str:
        return iso_utc(self.start_ts)

    @property
    def end_iso(self) -> str:
        return iso_utc(self.end_ts)

    def token_for(self, outcome: str) -> Optional[str]:
        if outcome == UP:
            return self.up_token_id
        if outcome == DOWN:
            return self.down_token_id
        return None


@dataclass
class State:
    schema_version: int = 1
    current_trend: str = NO_TREND
    previous_trend: str = NO_TREND
    pause_until_trend_turn: bool = False
    consecutive_losses: int = 0
    last_seen_market_slug: str = ""
    last_decision_market_slug: str = ""
    last_decision_window_start: Optional[int] = None
    last_resolved_market_slug: str = ""
    last_resolved_result: str = UNKNOWN
    last_previous_result_observed_market_slug: str = ""
    last_research_observation_market_slug: str = ""
    last_late_observation_market_slug: str = ""
    last_trade_market_slug: str = ""
    daily_pnl_usdc: float = 0.0
    daily_trade_count: int = 0
    daily_date: str = ""
    trend_candidate: str = NO_TREND
    trend_candidate_streak: int = 0
    open_trades: List[Dict[str, Any]] = field(default_factory=list)
    shadow_open_trades: List[Dict[str, Any]] = field(default_factory=list)
    computed_results: Dict[str, Dict[str, Any]] = field(default_factory=dict)

    @classmethod
    def load(cls, path: Path) -> "State":
        if not path.exists():
            return cls()
        payload = json.loads(path.read_text())
        if not isinstance(payload, dict):
            raise ValueError(f"state must be a JSON object: {path}")
        state = cls()
        for key in cls.__dataclass_fields__.keys():
            if key in payload:
                setattr(state, key, payload[key])
        return state

    def save(self, path: Path) -> None:
        path.parent.mkdir(parents=True, exist_ok=True)
        data = json.dumps(asdict(self), ensure_ascii=False, indent=2, sort_keys=True)
        with tempfile.NamedTemporaryFile(
            "w",
            encoding="utf-8",
            dir=str(path.parent),
            delete=False,
        ) as handle:
            handle.write(data)
            handle.write("\n")
            tmp_name = handle.name
        os.replace(tmp_name, path)


class JsonHttpClient:
    def __init__(self, timeout_seconds: int, retries: int, proxy_url: Optional[str] = None):
        self.timeout_seconds = timeout_seconds
        self.retries = retries
        self.proxy_url = normalize_proxy_url(proxy_url)

    def get_json(self, url: str, params: Optional[Dict[str, Any]] = None) -> Any:
        if params:
            query = urllib.parse.urlencode(
                {key: value for key, value in params.items() if value is not None}
            )
            separator = "&" if "?" in url else "?"
            url = f"{url}{separator}{query}"

        opener = self._opener()
        request = urllib.request.Request(
            url,
            headers={
                "Accept": "application/json",
                "User-Agent": "smart-wallet-discovery-btc-5m-follow/0.1",
            },
        )
        last_error: Optional[BaseException] = None
        for attempt in range(self.retries + 1):
            try:
                with opener.open(request, timeout=self.timeout_seconds) as response:
                    body = response.read()
                return json.loads(body)
            except (urllib.error.URLError, TimeoutError, json.JSONDecodeError) as error:
                last_error = error
                if attempt >= self.retries:
                    break
                time.sleep(0.5 * (2**attempt))
        raise RuntimeError(f"GET {url} failed: {last_error}") from last_error

    def get_text(self, url: str, params: Optional[Dict[str, Any]] = None) -> str:
        if params:
            query = urllib.parse.urlencode(
                {key: value for key, value in params.items() if value is not None}
            )
            separator = "&" if "?" in url else "?"
            url = f"{url}{separator}{query}"

        opener = self._opener()
        request = urllib.request.Request(
            url,
            headers={
                "Accept": "text/html,application/xhtml+xml,application/json",
                "User-Agent": "Mozilla/5.0 (compatible; smart-wallet-discovery-btc-5m-follow/0.1)",
            },
        )
        last_error: Optional[BaseException] = None
        for attempt in range(self.retries + 1):
            try:
                with opener.open(request, timeout=self.timeout_seconds) as response:
                    body = response.read()
                    charset = response.headers.get_content_charset() or "utf-8"
                return body.decode(charset, errors="replace")
            except (urllib.error.URLError, TimeoutError, UnicodeDecodeError) as error:
                last_error = error
                if attempt >= self.retries:
                    break
                time.sleep(0.5 * (2**attempt))
        raise RuntimeError(f"GET {url} failed: {last_error}") from last_error

    def _opener(self) -> urllib.request.OpenerDirector:
        if not self.proxy_url:
            return urllib.request.build_opener(urllib.request.ProxyHandler({}))
        return urllib.request.build_opener(
            urllib.request.ProxyHandler(
                {
                    "http": self.proxy_url,
                    "https": self.proxy_url,
                }
            )
        )


class BinanceClient:
    def __init__(self, config: Config):
        self.config = config
        proxy = (
            config.binance_proxy_url
            or os.environ.get("BINANCE_PROXY_URL")
            or os.environ.get("HTTPS_PROXY")
            or os.environ.get("HTTP_PROXY")
        )
        self.http = JsonHttpClient(
            config.http_timeout_seconds,
            config.http_retries,
            proxy,
        )

    def recent_5m_candles(self, now_ts: int) -> List[Candle]:
        bars_for_warmup = int(math.ceil(float(self.config.warmup_hours) * 60 / 5))
        limit = max(bars_for_warmup + self.config.trend_lookback_bars + 3, 12)
        limit = min(limit, 1000)
        payload = self.http.get_json(
            f"{self.config.binance_base_url.rstrip('/')}/api/v3/klines",
            {
                "symbol": "BTCUSDT",
                "interval": "5m",
                "limit": limit,
            },
        )
        if not isinstance(payload, list):
            raise RuntimeError("Binance klines response was not a list")

        now_ms = now_ts * 1000
        candles: List[Candle] = []
        for row in payload:
            if not isinstance(row, list) or len(row) < 7:
                continue
            candle = Candle(
                open_time_ms=int(row[0]),
                open=float(row[1]),
                high=float(row[2]),
                low=float(row[3]),
                close=float(row[4]),
                close_time_ms=int(row[6]),
            )
            if candle.close_time_ms <= now_ms:
                candles.append(candle)
        return candles

    def current_price(self) -> Optional[float]:
        try:
            payload = self.http.get_json(
                f"{self.config.binance_base_url.rstrip('/')}/api/v3/ticker/price",
                {"symbol": "BTCUSDT"},
            )
        except RuntimeError:
            return None
        if not isinstance(payload, dict):
            return None
        return parse_float(payload.get("price"))


class PolymarketClient:
    def __init__(self, config: Config):
        self.config = config
        proxy = (
            config.polymarket_proxy_url
            or os.environ.get("POLYMARKET_PROXY_URL")
            or os.environ.get("HTTPS_PROXY")
            or os.environ.get("HTTP_PROXY")
        )
        self.http = JsonHttpClient(
            config.http_timeout_seconds,
            config.http_retries,
            proxy,
        )
        self.page_price_cache: Dict[str, Dict[str, Any]] = {}

    def current_btc_5m_market(self, now_ts: int) -> Optional[Market]:
        start_ts = floor_5m(now_ts)
        direct = self.market_by_slug(slug_for_start(start_ts))
        if direct is not None:
            return direct
        return self.scan_current_market(now_ts)

    def market_by_slug(self, slug: str) -> Optional[Market]:
        try:
            event_payload = self.http.get_json(
                f"{self.config.gamma_api_base.rstrip('/')}/events/slug/{urllib.parse.quote(slug)}"
            )
            market = market_from_event_payload(event_payload, slug)
            if market:
                self.enrich_market_from_page(market)
                return market
        except RuntimeError:
            pass

        try:
            payload = self.http.get_json(
                f"{self.config.gamma_api_base.rstrip('/')}/markets",
                {"slug": slug},
            )
        except RuntimeError:
            payload = None

        items: List[Any] = []
        if isinstance(payload, list):
            items = payload
        elif isinstance(payload, dict):
            items = payload.get("markets") or payload.get("data") or []
        for item in items:
            if isinstance(item, dict) and item.get("slug") == slug:
                market = parse_market(item)
                if market:
                    self.enrich_market_from_page(market)
                    return market
        return None

    def enrich_market_from_page(self, market: Market) -> None:
        now_ts = int(time.time())
        needs_open = market.price_to_beat is None
        needs_final = market.end_ts <= now_ts and market.final_price is None
        if not needs_open and not needs_final:
            return

        cached = self.page_price_cache.get(market.slug)
        if cached:
            fetched_at = int(cached.get("fetched_at") or 0)
            has_open = cached.get("price_to_beat") is not None
            has_final = cached.get("final_price") is not None
            if now_ts - fetched_at < 15 and (has_open or not needs_open) and (has_final or not needs_final):
                apply_market_page_price_data(market, cached)
                return
            if (
                now_ts - fetched_at < self.config.page_price_missing_retry_seconds
                and not has_open
                and needs_open
            ):
                return

        data = self.market_page_price_data(market)
        data["fetched_at"] = now_ts
        self.page_price_cache[market.slug] = data
        apply_market_page_price_data(market, data)

    def market_page_price_data(self, market: Market) -> Dict[str, Any]:
        locale = str(self.config.polymarket_page_locale or "").strip("/")
        base = self.config.polymarket_web_base.rstrip("/")
        path = f"/{locale}/event/{market.slug}" if locale else f"/event/{market.slug}"
        url = f"{base}{path}"
        try:
            html_text = self.http.get_text(url)
        except RuntimeError:
            return {}
        data = extract_market_price_data_from_html(html_text, market)
        if data:
            data["page_url"] = url
        return data

    def scan_current_market(self, now_ts: int) -> Optional[Market]:
        payload = self.http.get_json(
            f"{self.config.gamma_api_base.rstrip('/')}/events",
            {
                "limit": 30,
                "active": "true",
                "closed": "false",
                "order": "endDate",
                "ascending": "true",
                "tag_slug": "crypto",
            },
        )
        events = payload if isinstance(payload, list) else payload.get("data", [])
        candidates: List[Market] = []
        for event in events:
            if not isinstance(event, dict):
                continue
            if not is_btc_5m_event(event):
                continue
            market = market_from_event_payload(event, str(event.get("slug") or ""))
            if market:
                candidates.append(market)
        candidates.sort(key=lambda item: item.start_ts)
        for market in candidates:
            if market.start_ts <= now_ts < market.end_ts:
                return market
        return None

    def previous_result(self, current_market: Market) -> Tuple[str, Optional[Market]]:
        previous = self.market_by_slug(slug_for_start(current_market.start_ts - 300))
        if previous is None:
            return UNKNOWN, None
        return resolved_result(previous.raw), previous

    def best_ask(self, token_id: str) -> Optional[float]:
        try:
            payload = self.http.get_json(
                f"{self.config.clob_api_base.rstrip('/')}/book",
                {"token_id": token_id},
            )
        except RuntimeError:
            return None
        if isinstance(payload, dict) and payload.get("error"):
            return None
        asks = payload.get("asks") if isinstance(payload, dict) else None
        prices = []
        if isinstance(asks, list):
            for level in asks:
                price = first_present(level, "price", "p")
                try:
                    prices.append(float(price))
                except (TypeError, ValueError):
                    continue
        return min(prices) if prices else None


def resolve_path(value: str) -> Path:
    path = Path(value)
    return path if path.is_absolute() else ROOT / path


def normalize_proxy_url(value: Optional[str]) -> Optional[str]:
    if value is None:
        return None
    text = str(value).strip()
    if not text or text.lower() in {"none", "off", "direct", "false", "0"}:
        return None
    return text


def floor_5m(timestamp: int) -> int:
    return timestamp - (timestamp % 300)


def slug_for_start(start_ts: int) -> str:
    return f"btc-updown-5m-{start_ts}"


def iso_utc(timestamp: int) -> str:
    return datetime.fromtimestamp(timestamp, tz=timezone.utc).isoformat().replace("+00:00", "Z")


def parse_iso_timestamp(value: Any) -> Optional[int]:
    if not value:
        return None
    if isinstance(value, (int, float)):
        return int(value)
    text = str(value).strip()
    if not text:
        return None
    if text.endswith("Z"):
        text = text[:-1] + "+00:00"
    try:
        parsed = datetime.fromisoformat(text)
    except ValueError:
        return None
    if parsed.tzinfo is None:
        parsed = parsed.replace(tzinfo=timezone.utc)
    return int(parsed.timestamp())


def parse_jsonish_list(value: Any) -> List[Any]:
    if isinstance(value, list):
        return value
    if value is None:
        return []
    if isinstance(value, str):
        try:
            parsed = json.loads(value)
            return parsed if isinstance(parsed, list) else []
        except json.JSONDecodeError:
            return []
    return []


def parse_jsonish_dict(value: Any) -> Dict[str, Any]:
    if isinstance(value, dict):
        return value
    if value is None:
        return {}
    if isinstance(value, str):
        try:
            parsed = json.loads(value)
            return parsed if isinstance(parsed, dict) else {}
        except json.JSONDecodeError:
            return {}
    return {}


def parse_float(value: Any) -> Optional[float]:
    if value is None:
        return None
    if isinstance(value, (int, float)) and not isinstance(value, bool):
        if math.isfinite(float(value)):
            return float(value)
        return None
    if isinstance(value, str):
        text = value.strip().replace("$", "").replace(",", "")
        if not text:
            return None
        try:
            parsed = float(text)
        except ValueError:
            return None
        return parsed if math.isfinite(parsed) else None
    return None


def extract_price_to_beat(payload: Dict[str, Any]) -> Optional[float]:
    metadata = parse_jsonish_dict(payload.get("eventMetadata") or payload.get("metadata"))
    sources = [payload, metadata]
    for source in sources:
        for key in (
            "priceToBeat",
            "price_to_beat",
            "targetPrice",
            "target_price",
            "openPrice",
            "open_price",
        ):
            parsed = parse_float(source.get(key))
            if parsed is not None:
                return parsed
    return None


def extract_final_price(payload: Dict[str, Any]) -> Optional[float]:
    metadata = parse_jsonish_dict(payload.get("eventMetadata") or payload.get("metadata"))
    sources = [payload, metadata]
    for source in sources:
        for key in (
            "finalPrice",
            "final_price",
            "closePrice",
            "close_price",
            "settlementPrice",
            "settlement_price",
        ):
            parsed = parse_float(source.get(key))
            if parsed is not None:
                return parsed
    return None


def parse_market(payload: Dict[str, Any]) -> Optional[Market]:
    slug = str(payload.get("slug") or payload.get("market_slug") or "")
    if not slug:
        return None
    slug_start = parse_slug_start(slug)
    start_ts = slug_start or parse_iso_timestamp(
        payload.get("eventStartTime")
        or payload.get("startTime")
        or payload.get("startDate")
        or payload.get("game_start_time")
    )
    end_ts = parse_iso_timestamp(payload.get("endDate") or payload.get("end_date_iso"))
    if start_ts is None:
        return None
    if end_ts is None or end_ts <= start_ts:
        end_ts = start_ts + 300

    outcomes = [normalize_outcome(value) for value in parse_jsonish_list(payload.get("outcomes"))]
    token_ids = [str(value) for value in parse_jsonish_list(payload.get("clobTokenIds"))]

    tokens = payload.get("tokens")
    if isinstance(tokens, list) and tokens:
        outcomes = [normalize_outcome(item.get("outcome")) for item in tokens if isinstance(item, dict)]
        token_ids = [str(item.get("token_id") or item.get("tokenId") or "") for item in tokens if isinstance(item, dict)]

    up_token_id = token_for_outcome(outcomes, token_ids, UP)
    down_token_id = token_for_outcome(outcomes, token_ids, DOWN)
    return Market(
        slug=slug,
        start_ts=start_ts,
        end_ts=end_ts,
        up_token_id=up_token_id,
        down_token_id=down_token_id,
        price_to_beat=extract_price_to_beat(payload),
        final_price=extract_final_price(payload),
        raw=payload,
    )


def market_from_event_payload(payload: Any, expected_slug: str) -> Optional[Market]:
    if not isinstance(payload, dict):
        return None
    markets = payload.get("markets")
    if isinstance(markets, list):
        for item in markets:
            if not isinstance(item, dict):
                continue
            if expected_slug and item.get("slug") != expected_slug:
                continue
            merged = dict(item)
            if "eventStartTime" not in merged:
                merged["eventStartTime"] = payload.get("startTime")
            if "eventMetadata" not in merged and payload.get("eventMetadata") is not None:
                merged["eventMetadata"] = payload.get("eventMetadata")
            market = parse_market(merged)
            if market:
                market.raw = merged
                return market
    if expected_slug and payload.get("slug") != expected_slug:
        return None
    return parse_market(payload)


def next_data_from_html(html_text: str) -> Optional[Dict[str, Any]]:
    match = re.search(
        r"<script[^>]*id=[\"']__NEXT_DATA__[\"'][^>]*>(.*?)</script>",
        html_text,
        flags=re.IGNORECASE | re.DOTALL,
    )
    if not match:
        return None
    text = html.unescape(match.group(1))
    try:
        payload = json.loads(text)
    except json.JSONDecodeError:
        return None
    return payload if isinstance(payload, dict) else None


def dehydrated_queries(payload: Dict[str, Any]) -> List[Dict[str, Any]]:
    queries = (
        payload.get("props", {})
        .get("pageProps", {})
        .get("dehydratedState", {})
        .get("queries", [])
    )
    return [query for query in queries if isinstance(query, dict)] if isinstance(queries, list) else []


def app_router_flight_strings(html_text: str) -> List[str]:
    strings: List[str] = []
    for match in re.finditer(
        r"self\.__next_f\.push\((\[.*?\])\)</script>",
        html_text,
        flags=re.IGNORECASE | re.DOTALL,
    ):
        try:
            values = json.loads(match.group(1))
        except json.JSONDecodeError:
            continue
        if not isinstance(values, list):
            continue
        strings.extend(value for value in values if isinstance(value, str))
    return strings


def raw_json_value_after_key(text: str, key: str, start: int = 0) -> Tuple[Optional[Any], int]:
    key_text = json.dumps(key)
    key_index = text.find(key_text, start)
    if key_index < 0:
        return None, -1
    colon_index = text.find(":", key_index + len(key_text))
    if colon_index < 0:
        return None, -1
    value_start = colon_index + 1
    while value_start < len(text) and text[value_start].isspace():
        value_start += 1
    try:
        value, offset = json.JSONDecoder().raw_decode(text[value_start:])
    except json.JSONDecodeError:
        return None, key_index + len(key_text)
    return value, value_start + offset


def app_router_dehydrated_queries(html_text: str) -> List[Dict[str, Any]]:
    queries: List[Dict[str, Any]] = []
    for chunk in app_router_flight_strings(html_text):
        search_from = 0
        while True:
            value, next_index = raw_json_value_after_key(chunk, "dehydratedState", search_from)
            if next_index < 0:
                break
            search_from = next_index
            if not isinstance(value, dict):
                continue
            chunk_queries = value.get("queries")
            if isinstance(chunk_queries, list):
                queries.extend(query for query in chunk_queries if isinstance(query, dict))
    return queries


def dehydrated_queries_from_html(html_text: str) -> List[Dict[str, Any]]:
    payload = next_data_from_html(html_text)
    queries: List[Dict[str, Any]] = dehydrated_queries(payload) if payload else []
    queries.extend(app_router_dehydrated_queries(html_text))
    return queries


def market_time_matches(value: Any, expected_ts: int) -> bool:
    parsed = parse_iso_timestamp(value)
    return parsed == expected_ts if parsed is not None else False


def result_from_price_delta(delta: Optional[float]) -> str:
    if delta is None:
        return UNKNOWN
    if delta > 0:
        return UP
    if delta < 0:
        return DOWN
    return UNKNOWN


def extract_market_price_data_from_queries(
    queries: List[Dict[str, Any]],
    market: Market,
) -> Dict[str, Any]:
    found: Dict[str, Any] = {}
    for query in queries:
        state = query.get("state")
        data = state.get("data") if isinstance(state, dict) else None
        if not isinstance(data, dict):
            continue

        open_price = parse_float(data.get("openPrice"))
        close_price = parse_float(data.get("closePrice"))
        if open_price is not None:
            key_text = json.dumps(
                query.get("queryKey") or query.get("queryHash") or "",
                ensure_ascii=False,
            )
            key_matches = market.start_iso in key_text or market.end_iso in key_text
            if key_matches or not found.get("price_to_beat"):
                found.update(
                    {
                        "price_to_beat": open_price,
                        "final_price": close_price,
                        "page_price_source": "polymarket_next_data_crypto_price",
                    }
                )

        results = data.get("results")
        if not isinstance(results, list):
            nested = data.get("data")
            if isinstance(nested, dict):
                results = nested.get("results")
        if not isinstance(results, list):
            continue
        for item in results:
            if not isinstance(item, dict):
                continue
            if not market_time_matches(item.get("startTime"), market.start_ts):
                continue
            item_open = parse_float(item.get("openPrice"))
            item_close = parse_float(item.get("closePrice"))
            outcome = normalize_outcome(item.get("outcome"))
            found.update(
                {
                    "price_to_beat": item_open if item_open is not None else found.get("price_to_beat"),
                    "final_price": item_close if item_close is not None else found.get("final_price"),
                    "page_result": outcome if outcome in {UP, DOWN} else UNKNOWN,
                    "page_price_source": "polymarket_next_data_past_results",
                }
            )
            return found
    return found


def extract_market_price_data_from_next_data(
    payload: Dict[str, Any],
    market: Market,
) -> Dict[str, Any]:
    return extract_market_price_data_from_queries(dehydrated_queries(payload), market)


def extract_market_price_data_from_html(html_text: str, market: Market) -> Dict[str, Any]:
    return extract_market_price_data_from_queries(dehydrated_queries_from_html(html_text), market)


def apply_market_page_price_data(market: Market, data: Dict[str, Any]) -> None:
    price_to_beat = parse_float(data.get("price_to_beat"))
    final_price = parse_float(data.get("final_price"))
    if market.price_to_beat is None and price_to_beat is not None:
        market.price_to_beat = price_to_beat
    if market.final_price is None and final_price is not None:
        market.final_price = final_price
    if data:
        market.raw["pagePriceData"] = {
            key: value
            for key, value in data.items()
            if key
            in {
                "price_to_beat",
                "final_price",
                "page_result",
                "page_price_source",
                "page_url",
                "fetched_at",
            }
        }


def parse_slug_start(slug: str) -> Optional[int]:
    match = BTC_5M_SLUG_RE.match(slug)
    return int(match.group(1)) if match else None


def normalize_outcome(value: Any) -> str:
    text = str(value or "").strip().upper()
    if text == "UP":
        return UP
    if text == "DOWN":
        return DOWN
    return text


def token_for_outcome(outcomes: List[str], token_ids: List[str], outcome: str) -> Optional[str]:
    for index, value in enumerate(outcomes):
        if value == outcome and index < len(token_ids) and token_ids[index]:
            return token_ids[index]
    if outcome == UP and token_ids:
        return token_ids[0]
    if outcome == DOWN and len(token_ids) >= 2:
        return token_ids[1]
    return None


def is_btc_5m_event(event: Dict[str, Any]) -> bool:
    slug = str(event.get("slug") or "")
    if BTC_5M_SLUG_RE.match(slug):
        return True
    if event.get("seriesSlug") == "btc-up-or-down-5m":
        return True
    title = str(event.get("title") or "")
    return "Bitcoin Up or Down" in title and "5m" in str(event).lower()


def resolved_result(market_payload: Dict[str, Any]) -> str:
    tokens = market_payload.get("tokens")
    if isinstance(tokens, list):
        for token in tokens:
            if not isinstance(token, dict) or not token.get("winner"):
                continue
            outcome = normalize_outcome(token.get("outcome"))
            if outcome in {UP, DOWN}:
                return outcome

    outcomes = [normalize_outcome(value) for value in parse_jsonish_list(market_payload.get("outcomes"))]
    prices = parse_jsonish_list(market_payload.get("outcomePrices"))
    if len(outcomes) >= 2 and len(prices) >= 2:
        parsed_prices = []
        for price in prices:
            try:
                parsed_prices.append(float(price))
            except (TypeError, ValueError):
                parsed_prices.append(float("nan"))
        if len(parsed_prices) >= len(outcomes):
            best_index = max(range(len(outcomes)), key=lambda index: parsed_prices[index])
            best_price = parsed_prices[best_index]
            if best_price >= 0.999 or bool(market_payload.get("closed")):
                outcome = outcomes[best_index]
                if outcome in {UP, DOWN}:
                    return outcome
    return UNKNOWN


def first_present(payload: Any, *keys: str) -> Any:
    if isinstance(payload, dict):
        for key in keys:
            value = payload.get(key)
            if value is not None:
                return value
    return None


def trend_context(config: Config, candles: List[Candle], ret_30m: Optional[float]) -> Dict[str, Any]:
    lookback = config.trend_lookback_bars
    if len(candles) <= lookback:
        return {
            "binance_latest_candle_close_time": "",
            "binance_latest_close": None,
            "binance_lookback_candle_close_time": "",
            "binance_lookback_close": None,
            "trend_lookback_bars": lookback,
            "trend_deadband_pct": config.trend_deadband_pct,
            "ret_30m": round(ret_30m, 8) if ret_30m is not None else None,
        }
    latest = candles[-1]
    base = candles[-1 - lookback]
    return {
        "binance_latest_candle_open_time": iso_utc(latest.open_time_ms // 1000),
        "binance_latest_candle_close_time": iso_utc(latest.close_time_ms // 1000),
        "binance_latest_open": latest.open,
        "binance_latest_high": latest.high,
        "binance_latest_low": latest.low,
        "binance_latest_close": latest.close,
        "binance_lookback_candle_open_time": iso_utc(base.open_time_ms // 1000),
        "binance_lookback_candle_close_time": iso_utc(base.close_time_ms // 1000),
        "binance_lookback_close": base.close,
        "trend_lookback_bars": lookback,
        "trend_deadband_pct": config.trend_deadband_pct,
        "ret_30m": round(ret_30m, 8) if ret_30m is not None else None,
    }


def compute_trend(config: Config, state: State, candles: List[Candle]) -> Tuple[str, Optional[float]]:
    lookback = config.trend_lookback_bars
    if len(candles) <= lookback:
        return state.current_trend or NO_TREND, None

    latest_close = candles[-1].close
    base_close = candles[-1 - lookback].close
    if base_close <= 0:
        return state.current_trend or NO_TREND, None
    ret_30m = latest_close / base_close - 1.0
    raw_signal: Optional[str]
    if ret_30m > config.trend_deadband_pct:
        raw_signal = UP
    elif ret_30m < -config.trend_deadband_pct:
        raw_signal = DOWN
    else:
        raw_signal = None

    if raw_signal is None:
        return state.current_trend or NO_TREND, ret_30m

    if raw_signal == state.current_trend:
        state.trend_candidate = raw_signal
        state.trend_candidate_streak = config.confirm_streak
        return state.current_trend, ret_30m

    if raw_signal == state.trend_candidate:
        state.trend_candidate_streak += 1
    else:
        state.trend_candidate = raw_signal
        state.trend_candidate_streak = 1

    if state.trend_candidate_streak >= config.confirm_streak:
        return raw_signal, ret_30m
    return state.current_trend or NO_TREND, ret_30m


def apply_trend(config: Config, state: State, new_trend: str) -> None:
    old_trend = state.current_trend or NO_TREND
    state.previous_trend = old_trend
    state.current_trend = new_trend or NO_TREND
    if old_trend in {UP, DOWN} and state.current_trend in {UP, DOWN} and old_trend != state.current_trend:
        state.pause_until_trend_turn = False
        state.consecutive_losses = 0
        state.trend_candidate = state.current_trend
        state.trend_candidate_streak = config.confirm_streak


def local_date(config: Config, now_ts: int) -> str:
    if ZoneInfo is None:
        tz = timezone.utc
    else:
        try:
            tz = ZoneInfo(config.timezone)
        except Exception:
            tz = timezone.utc
    return datetime.fromtimestamp(now_ts, tz=tz).date().isoformat()


def reset_daily_if_needed(config: Config, state: State, now_ts: int) -> None:
    today = local_date(config, now_ts)
    if state.daily_date != today:
        state.daily_date = today
        state.daily_pnl_usdc = 0.0
        state.daily_trade_count = 0


def computed_result_record(state: State, slug: str) -> Optional[Dict[str, Any]]:
    record = normalized_computed_results(state).get(slug)
    return record if isinstance(record, dict) else None


def computed_result_for_market(state: State, slug: str) -> Tuple[str, Optional[Dict[str, Any]]]:
    record = computed_result_record(state, slug)
    result = str(record.get("computed_result") or UNKNOWN) if record else UNKNOWN
    if result in {UP, DOWN}:
        return result, record
    return UNKNOWN, record


def resolve_market_for_settlement(
    state: State,
    polymarket: PolymarketClient,
    slug: str,
) -> Tuple[str, str, Optional[Dict[str, Any]]]:
    computed_result, record = computed_result_for_market(state, slug)
    if computed_result in {UP, DOWN}:
        return computed_result, COMPUTED_RESULT_SOURCE, record
    market = polymarket.market_by_slug(slug) if slug else None
    official_result = resolved_result(market.raw) if market else UNKNOWN
    if official_result in {UP, DOWN}:
        return official_result, OFFICIAL_RESULT_SOURCE, record
    return UNKNOWN, "", record


def settle_open_trades(
    config: Config,
    state: State,
    polymarket: PolymarketClient,
    log_path: Path,
    now_ts: int,
) -> None:
    remaining: List[Dict[str, Any]] = []
    for trade in state.open_trades:
        try:
            end_ts = int(trade.get("market_end_ts") or 0)
        except (TypeError, ValueError):
            end_ts = 0
        if end_ts + config.settlement_check_delay_seconds > now_ts:
            remaining.append(trade)
            continue

        slug = str(trade.get("market_slug") or "")
        result, result_source, computed_record = resolve_market_for_settlement(
            state,
            polymarket,
            slug,
        )
        if result not in {UP, DOWN}:
            remaining.append(trade)
            continue

        selected = str(trade.get("selected_outcome") or "")
        amount = float(trade.get("amount_usdc") or 0.0)
        entry_price = float(trade.get("entry_price") or 0.0)
        shares = float(trade.get("shares") or 0.0)
        pnl = shares - amount if selected == result else -amount
        state.daily_pnl_usdc += pnl
        state.last_resolved_market_slug = slug
        state.last_resolved_result = result
        if pnl < 0:
            state.consecutive_losses += 1
        else:
            state.consecutive_losses = 0
        if state.consecutive_losses >= config.max_consecutive_losses:
            state.pause_until_trend_turn = True

        append_jsonl(
            log_path,
            {
                "event_type": "settlement",
                "timestamp": iso_utc(now_ts),
                "market_slug": slug,
                "market_start": trade.get("market_start"),
                "market_end": trade.get("market_end"),
                "trend": trade.get("trend"),
                "ret_30m": trade.get("ret_30m"),
                "previous_result": trade.get("previous_result"),
                "action": "SETTLE",
                "selected_outcome": selected,
                "best_ask": entry_price,
                "entry_price": entry_price,
                "shares": shares,
                "fixed_amount_usdc": amount,
                "mode": trade.get("mode"),
                "main_strategy": trade.get("main_strategy"),
                "order_id": trade.get("order_id"),
                "decision_reason": trade.get("decision_reason"),
                "previous_result_source": trade.get("previous_result_source"),
                "official_previous_result": trade.get("official_previous_result"),
                "computed_previous_result": trade.get("computed_previous_result"),
                "previous_price_to_beat": trade.get("previous_price_to_beat"),
                "previous_final_price": trade.get("previous_final_price"),
                "current_price_to_beat": trade.get("current_price_to_beat"),
                "computed_price_delta": trade.get("computed_price_delta"),
                "computed_result_latency_seconds": trade.get("computed_result_latency_seconds"),
                "settlement_result_source": result_source,
                "settlement_computed_at": computed_record.get("computed_at") if computed_record else "",
                "result": "WIN" if pnl > 0 else "LOSS",
                "resolved_result": result,
                "pnl_usdc": round(pnl, 8),
                "consecutive_losses": state.consecutive_losses,
                "daily_pnl_usdc": round(state.daily_pnl_usdc, 8),
                "daily_trade_count": state.daily_trade_count,
                "pause_until_trend_turn": state.pause_until_trend_turn,
            },
        )
    state.open_trades = remaining


def settle_shadow_open_trades(
    config: Config,
    state: State,
    polymarket: PolymarketClient,
    log_path: Path,
    now_ts: int,
) -> None:
    remaining: List[Dict[str, Any]] = []
    for trade in state.shadow_open_trades:
        try:
            end_ts = int(trade.get("market_end_ts") or 0)
        except (TypeError, ValueError):
            end_ts = 0
        if end_ts + config.settlement_check_delay_seconds > now_ts:
            remaining.append(trade)
            continue

        slug = str(trade.get("market_slug") or "")
        result, result_source, computed_record = resolve_market_for_settlement(
            state,
            polymarket,
            slug,
        )
        if result not in {UP, DOWN}:
            remaining.append(trade)
            continue

        selected = str(trade.get("selected_outcome") or "")
        amount = float(trade.get("amount_usdc") or 0.0)
        entry_price = float(trade.get("entry_price") or 0.0)
        shares = float(trade.get("shares") or 0.0)
        pnl = shares - amount if selected == result else -amount
        append_jsonl(
            log_path,
            {
                "event_type": "shadow_settlement",
                "timestamp": iso_utc(now_ts),
                "strategy_name": trade.get("strategy_name"),
                "market_slug": slug,
                "market_start": trade.get("market_start"),
                "market_end": trade.get("market_end"),
                "trend": trade.get("trend"),
                "ret_30m": trade.get("ret_30m"),
                "previous_result": trade.get("previous_result"),
                "selected_outcome": selected,
                "entry_price": entry_price,
                "shares": shares,
                "fixed_amount_usdc": amount,
                "previous_result_source": trade.get("previous_result_source"),
                "computed_previous_result": trade.get("computed_previous_result"),
                "previous_price_to_beat": trade.get("previous_price_to_beat"),
                "previous_final_price": trade.get("previous_final_price"),
                "current_price_to_beat": trade.get("current_price_to_beat"),
                "computed_price_delta": trade.get("computed_price_delta"),
                "computed_result_latency_seconds": trade.get("computed_result_latency_seconds"),
                "binance_live_price": trade.get("binance_live_price"),
                "current_price_delta_to_target": trade.get("current_price_delta_to_target"),
                "current_price_side": trade.get("current_price_side"),
                "settlement_result_source": result_source,
                "settlement_computed_at": computed_record.get("computed_at") if computed_record else "",
                "result": "WIN" if pnl > 0 else "LOSS",
                "resolved_result": result,
                "pnl_usdc": round(pnl, 8),
            },
        )
    state.shadow_open_trades = remaining


def build_decision(
    config: Config,
    state: State,
    market: Optional[Market],
    previous_result: str,
    previous_result_source: str,
    official_previous_result: str,
    computed_result: Dict[str, Any],
    previous_market: Optional[Market],
    up_best_ask: Optional[float],
    down_best_ask: Optional[float],
    live_btc_price: Optional[float],
    trend_data: Dict[str, Any],
    now_ts: int,
) -> Dict[str, Any]:
    main_strategy_name = str(config.main_strategy or "trend_follow_v1")
    main_strategy = shadow_strategy_config(main_strategy_name)
    if main_strategy_name == "anti_previous_result_v12":
        selected_outcome = opposite_outcome(previous_result)
    else:
        selected_outcome = state.current_trend if state.current_trend in {UP, DOWN} else None
    action = "SKIP"
    reason = "SKIP_MARKET_NOT_FOUND"
    token_id = market.token_for(selected_outcome) if market and selected_outcome else None
    best_ask = up_best_ask if selected_outcome == UP else down_best_ask if selected_outcome == DOWN else None
    previous_result_latency_seconds = (
        now_ts - previous_market.end_ts
        if previous_market and previous_result in {UP, DOWN}
        else None
    )

    if market is None:
        reason = "SKIP_MARKET_NOT_FOUND"
    elif now_ts - market.start_ts < config.entry_delay_seconds:
        reason = "SKIP_ENTRY_DELAY"
    elif market.end_ts - now_ts <= config.latest_entry_seconds_before_close:
        reason = "SKIP_TOO_LATE_TO_ENTER"
    elif (
        main_strategy_name == "anti_previous_result_v12"
        and main_strategy.get("max_seconds_after_market_open") is not None
        and now_ts - market.start_ts > float(main_strategy["max_seconds_after_market_open"])
    ):
        reason = "SKIP_ENTRY_TOO_LATE_FOR_STRATEGY"
    elif main_strategy_name != "anti_previous_result_v12" and state.current_trend not in {UP, DOWN}:
        reason = "SKIP_NO_TREND"
    elif state.daily_pnl_usdc <= -abs(config.max_daily_loss_usdc):
        reason = "SKIP_DAILY_LOSS_LIMIT"
    elif state.daily_trade_count >= config.max_trades_per_day:
        reason = "SKIP_DAILY_TRADE_LIMIT"
    elif state.pause_until_trend_turn:
        reason = "SKIP_PAUSED_AFTER_LOSSES"
    elif selected_outcome == UP and not config.trade_up:
        reason = "SKIP_DIRECTION_DISABLED"
    elif selected_outcome == DOWN and not config.trade_down:
        reason = "SKIP_DIRECTION_DISABLED"
    elif previous_result == UNKNOWN:
        reason = "SKIP_PREVIOUS_RESULT_UNKNOWN"
    elif main_strategy_name != "anti_previous_result_v12" and previous_result != state.current_trend:
        reason = "SKIP_PREVIOUS_RESULT_AGAINST_TREND"
    elif (
        config.max_previous_result_latency_seconds is not None
        and (
            previous_result_latency_seconds is None
            or previous_result_latency_seconds > config.max_previous_result_latency_seconds
        )
    ):
        reason = "SKIP_PREVIOUS_RESULT_TOO_LATE"
    elif token_id is None:
        reason = "SKIP_MARKET_NOT_FOUND"
    elif best_ask is None:
        reason = "SKIP_PRICE_UNAVAILABLE"
    elif best_ask <= config.min_entry_price:
        reason = "SKIP_PRICE_TOO_LOW"
    elif best_ask > config.hard_max_entry_price:
        reason = "SKIP_PRICE_TOO_HIGH"
    elif best_ask > config.max_entry_price:
        reason = "SKIP_PRICE_TOO_HIGH"
    elif config.mode == "live" and not config.enabled:
        reason = "SKIP_LIVE_DISABLED"
    else:
        action = "BUY_UP" if selected_outcome == UP else "BUY_DOWN"
        reason = "ENTER_UP" if selected_outcome == UP else "ENTER_DOWN"

    current_price_delta_to_target = (
        live_btc_price - market.price_to_beat
        if market and live_btc_price is not None and market.price_to_beat is not None
        else None
    )
    current_price_side = result_from_price_delta(current_price_delta_to_target)
    decision = {
        "event_type": "decision",
        "timestamp": iso_utc(now_ts),
        "market_slug": market.slug if market else "",
        "market_start": market.start_iso if market else "",
        "market_end": market.end_iso if market else "",
        "market_start_ts": market.start_ts if market else None,
        "market_end_ts": market.end_ts if market else None,
        "seconds_after_market_open": now_ts - market.start_ts if market else None,
        "seconds_before_market_close": market.end_ts - now_ts if market else None,
        "up_token_id": market.up_token_id if market else None,
        "down_token_id": market.down_token_id if market else None,
        "current_price_to_beat": rounded_or_none(market.price_to_beat) if market else None,
        "previous_market_slug": previous_market.slug if previous_market else "",
        "previous_market_start": previous_market.start_iso if previous_market else "",
        "previous_market_end": previous_market.end_iso if previous_market else "",
        "previous_result_available_at": iso_utc(now_ts) if previous_result in {UP, DOWN} else "",
        "previous_result_latency_seconds": previous_result_latency_seconds,
        "previous_result_source": previous_result_source,
        "official_previous_result": official_previous_result,
        "official_previous_result_available_at": (
            iso_utc(now_ts) if official_previous_result in {UP, DOWN} else ""
        ),
        "official_previous_result_latency_seconds": (
            now_ts - previous_market.end_ts
            if previous_market and official_previous_result in {UP, DOWN}
            else None
        ),
        "computed_previous_result": computed_result.get("computed_previous_result", UNKNOWN),
        "computed_result_source": computed_result.get("computed_result_source", COMPUTED_RESULT_SOURCE),
        "computed_result_rule": computed_result.get("computed_result_rule"),
        "computed_result_market_slug": computed_result.get("computed_result_market_slug", ""),
        "computed_result_next_market_slug": computed_result.get("computed_result_next_market_slug", ""),
        "previous_price_to_beat": computed_result.get("previous_price_to_beat"),
        "previous_final_price": computed_result.get("previous_final_price"),
        "computed_price_delta": computed_result.get("computed_price_delta"),
        "computed_result_available_at": computed_result.get("computed_result_available_at", ""),
        "computed_result_latency_seconds": computed_result.get("computed_result_latency_seconds"),
        "binance_live_price": rounded_or_none(live_btc_price),
        "current_price_delta_to_target": rounded_or_none(current_price_delta_to_target),
        "current_price_side": current_price_side,
        "current_page_price_source": (
            market.raw.get("pagePriceData", {}).get("page_price_source")
            if market and isinstance(market.raw.get("pagePriceData"), dict)
            else ""
        ),
        "previous_page_price_source": (
            previous_market.raw.get("pagePriceData", {}).get("page_price_source")
            if previous_market and isinstance(previous_market.raw.get("pagePriceData"), dict)
            else ""
        ),
        "trend": state.current_trend,
        "previous_trend": state.previous_trend,
        **trend_data,
        "previous_result": previous_result,
        "action": action,
        "selected_outcome": selected_outcome,
        "token_id": token_id,
        "best_ask": best_ask,
        "up_best_ask": up_best_ask,
        "down_best_ask": down_best_ask,
        "fixed_amount_usdc": config.fixed_amount_usdc,
        "min_entry_price": config.min_entry_price,
        "max_entry_price": config.max_entry_price,
        "hard_max_entry_price": config.hard_max_entry_price,
        "max_previous_result_latency_seconds": config.max_previous_result_latency_seconds,
        "main_strategy": main_strategy_name,
        "mode": config.mode,
        "enabled": config.enabled,
        "order_id": None,
        "order_status": None,
        "decision_reason": reason,
        "result": "PENDING" if action.startswith("BUY_") else "",
        "pnl_usdc": None,
        "consecutive_losses": state.consecutive_losses,
        "daily_pnl_usdc": round(state.daily_pnl_usdc, 8),
        "daily_trade_count": state.daily_trade_count,
        "pause_until_trend_turn": state.pause_until_trend_turn,
    }
    decision["shadow_strategies"] = build_shadow_strategies(config, decision)
    return decision


def shadow_strategy_config(name: str) -> Dict[str, Any]:
    configs = {
        "base_v1": {
            "side_mode": "decision",
            "min_entry_price": 0.0,
            "max_entry_price": 0.51,
            "max_seconds_after_market_open": None,
            "min_previous_result_latency_seconds": None,
            "max_previous_result_latency_seconds": None,
            "min_abs_computed_delta": None,
            "excluded_price_ranges": [],
            "require_current_price_side": False,
        },
        "delta_filter_v2": {
            "side_mode": "decision",
            "min_entry_price": 0.0,
            "max_entry_price": 0.51,
            "max_seconds_after_market_open": None,
            "min_previous_result_latency_seconds": None,
            "max_previous_result_latency_seconds": None,
            "min_abs_computed_delta": 5.0,
            "excluded_price_ranges": [(0.31, 0.40)],
            "require_current_price_side": False,
        },
        "price60_v3": {
            "side_mode": "decision",
            "min_entry_price": 0.0,
            "max_entry_price": 0.60,
            "max_seconds_after_market_open": None,
            "min_previous_result_latency_seconds": None,
            "max_previous_result_latency_seconds": None,
            "min_abs_computed_delta": None,
            "excluded_price_ranges": [],
            "require_current_price_side": False,
        },
        "current_side_price60_v4": {
            "side_mode": "decision",
            "min_entry_price": 0.0,
            "max_entry_price": 0.60,
            "max_seconds_after_market_open": None,
            "min_previous_result_latency_seconds": None,
            "max_previous_result_latency_seconds": None,
            "min_abs_computed_delta": None,
            "excluded_price_ranges": [],
            "require_current_price_side": True,
        },
        "fast_c_v5": {
            "side_mode": "decision",
            "min_entry_price": 0.35,
            "max_entry_price": 0.70,
            "max_seconds_after_market_open": None,
            "min_previous_result_latency_seconds": None,
            "max_previous_result_latency_seconds": 25,
            "min_abs_computed_delta": None,
            "excluded_price_ranges": [],
            "require_current_price_side": False,
        },
        "anti_previous_result_v6": {
            "side_mode": "anti_previous_result",
            "min_entry_price": 0.40,
            "max_entry_price": 0.70,
            "max_seconds_after_market_open": 60,
            "min_previous_result_latency_seconds": None,
            "max_previous_result_latency_seconds": 60,
            "min_abs_computed_delta": None,
            "excluded_price_ranges": [],
            "require_current_price_side": False,
        },
        "anti_previous_result_v7": {
            "side_mode": "anti_previous_result",
            "min_entry_price": 0.45,
            "max_entry_price": 0.60,
            "max_seconds_after_market_open": 60,
            "min_previous_result_latency_seconds": None,
            "max_previous_result_latency_seconds": 60,
            "min_abs_computed_delta": None,
            "excluded_price_ranges": [],
            "require_current_price_side": False,
        },
        "anti_previous_result_v8": {
            "side_mode": "anti_previous_result",
            "min_entry_price": 0.45,
            "max_entry_price": 0.60,
            "max_seconds_after_market_open": 60,
            "min_seconds_before_market_close": None,
            "max_seconds_before_market_close": None,
            "min_previous_result_latency_seconds": 30,
            "max_previous_result_latency_seconds": 60,
            "require_previous_result": True,
            "min_abs_computed_delta": None,
            "excluded_price_ranges": [],
            "require_current_price_side": False,
        },
        "anti_previous_result_v12": {
            "side_mode": "anti_previous_result",
            "min_entry_price": 0.50,
            "max_entry_price": 0.70,
            "max_seconds_after_market_open": 90,
            "min_seconds_before_market_close": None,
            "max_seconds_before_market_close": None,
            "min_previous_result_latency_seconds": None,
            "max_previous_result_latency_seconds": 60,
            "require_previous_result": True,
            "min_abs_computed_delta": None,
            "excluded_price_ranges": [],
            "require_current_price_side": False,
        },
        "last_minute_favorite_v13": {
            "side_mode": "favorite",
            "min_entry_price": 0.70,
            "max_entry_price": 0.98,
            "max_seconds_after_market_open": None,
            "min_seconds_before_market_close": 0,
            "max_seconds_before_market_close": 60,
            "min_previous_result_latency_seconds": None,
            "max_previous_result_latency_seconds": None,
            "require_previous_result": False,
            "min_abs_computed_delta": None,
            "excluded_price_ranges": [],
            "require_current_price_side": False,
        },
    }
    return configs.get(name, configs["base_v1"])


def price_in_excluded_range(price: float, ranges: List[Tuple[float, float]]) -> bool:
    return any(low <= price <= high for low, high in ranges)


def opposite_outcome(outcome: str) -> str:
    if outcome == UP:
        return DOWN
    if outcome == DOWN:
        return UP
    return ""


def shadow_selected_outcome(strategy: Dict[str, Any], decision: Dict[str, Any]) -> str:
    mode = str(strategy.get("side_mode") or "decision")
    if mode == "anti_previous_result":
        return opposite_outcome(str(decision.get("previous_result") or ""))
    if mode == "previous_result":
        previous_result = str(decision.get("previous_result") or "")
        return previous_result if previous_result in {UP, DOWN} else ""
    if mode == "current_price_side":
        current_price_side = str(decision.get("current_price_side") or "")
        return current_price_side if current_price_side in {UP, DOWN} else ""
    if mode == "anti_current_price_side":
        return opposite_outcome(str(decision.get("current_price_side") or ""))
    if mode == "favorite":
        up_best_ask = parse_float(decision.get("up_best_ask"))
        down_best_ask = parse_float(decision.get("down_best_ask"))
        if up_best_ask is None or down_best_ask is None:
            return ""
        if up_best_ask > down_best_ask:
            return UP
        if down_best_ask > up_best_ask:
            return DOWN
        return ""
    return str(decision.get("selected_outcome") or "")


def shadow_token_id(decision: Dict[str, Any], selected_outcome: str) -> Optional[str]:
    if selected_outcome == UP:
        return str(decision.get("up_token_id") or "") or None
    if selected_outcome == DOWN:
        return str(decision.get("down_token_id") or "") or None
    return None


def shadow_best_ask(decision: Dict[str, Any], selected_outcome: str) -> Optional[float]:
    if selected_outcome == UP:
        return parse_float(decision.get("up_best_ask"))
    if selected_outcome == DOWN:
        return parse_float(decision.get("down_best_ask"))
    return None


def build_shadow_strategies(config: Config, decision: Dict[str, Any]) -> List[Dict[str, Any]]:
    shadows: List[Dict[str, Any]] = []
    for name in config.shadow_strategies:
        strategy = shadow_strategy_config(name)
        action = "SHADOW_SKIP"
        reason = "SHADOW_SKIP_MARKET_NOT_FOUND"
        selected_outcome = shadow_selected_outcome(strategy, decision)
        token_id = shadow_token_id(decision, selected_outcome)
        best_ask = shadow_best_ask(decision, selected_outcome)
        computed_delta = parse_float(decision.get("computed_price_delta"))
        previous_result_latency_seconds = parse_float(
            decision.get("previous_result_latency_seconds")
        )
        min_latency = strategy.get("min_previous_result_latency_seconds")
        max_latency = strategy.get("max_previous_result_latency_seconds")
        max_seconds_after_open = strategy.get("max_seconds_after_market_open")
        min_seconds_before_close = strategy.get("min_seconds_before_market_close")
        max_seconds_before_close = strategy.get("max_seconds_before_market_close")
        require_previous_result = bool(strategy.get("require_previous_result", True))
        min_entry_price = float(strategy.get("min_entry_price") or 0.0)
        seconds_after_open = int(decision.get("seconds_after_market_open") or 0)
        seconds_before_close = int(decision.get("seconds_before_market_close") or 0)

        if not decision.get("market_slug"):
            reason = "SHADOW_SKIP_MARKET_NOT_FOUND"
        elif seconds_after_open < config.entry_delay_seconds:
            reason = "SHADOW_SKIP_ENTRY_DELAY"
        elif seconds_before_close <= 0:
            reason = "SHADOW_SKIP_TOO_LATE_TO_ENTER"
        elif (
            min_seconds_before_close is not None
            and seconds_before_close <= float(min_seconds_before_close)
        ):
            reason = "SHADOW_SKIP_TOO_LATE_TO_ENTER"
        elif (
            max_seconds_before_close is not None
            and seconds_before_close > float(max_seconds_before_close)
        ):
            reason = "SHADOW_SKIP_TOO_EARLY_FOR_STRATEGY"
        elif (
            max_seconds_before_close is None
            and seconds_before_close <= config.latest_entry_seconds_before_close
        ):
            reason = "SHADOW_SKIP_TOO_LATE_TO_ENTER"
        elif (
            max_seconds_after_open is not None
            and seconds_after_open > float(max_seconds_after_open)
        ):
            reason = "SHADOW_SKIP_ENTRY_TOO_LATE_FOR_STRATEGY"
        elif (
            strategy.get("side_mode") == "decision"
            and decision.get("trend") not in {UP, DOWN}
        ):
            reason = "SHADOW_SKIP_NO_TREND"
        elif selected_outcome not in {UP, DOWN}:
            reason = "SHADOW_SKIP_NO_OUTCOME"
        elif require_previous_result and decision.get("previous_result") not in {UP, DOWN}:
            reason = "SHADOW_SKIP_PREVIOUS_RESULT_UNKNOWN"
        elif (
            strategy.get("side_mode") == "decision"
            and decision.get("previous_result") != decision.get("trend")
        ):
            reason = "SHADOW_SKIP_PREVIOUS_RESULT_AGAINST_TREND"
        elif max_latency is not None and (
            previous_result_latency_seconds is None
            or previous_result_latency_seconds > float(max_latency)
        ):
            reason = "SHADOW_SKIP_PREVIOUS_RESULT_TOO_LATE"
        elif min_latency is not None and (
            previous_result_latency_seconds is None
            or previous_result_latency_seconds <= float(min_latency)
        ):
            reason = "SHADOW_SKIP_PREVIOUS_RESULT_TOO_EARLY"
        elif token_id is None:
            reason = "SHADOW_SKIP_MARKET_NOT_FOUND"
        elif best_ask is None:
            reason = "SHADOW_SKIP_PRICE_UNAVAILABLE"
        elif best_ask <= min_entry_price:
            reason = "SHADOW_SKIP_PRICE_TOO_LOW"
        elif best_ask > float(strategy["max_entry_price"]):
            reason = "SHADOW_SKIP_PRICE_TOO_HIGH"
        elif price_in_excluded_range(best_ask, strategy["excluded_price_ranges"]):
            reason = "SHADOW_SKIP_PRICE_RANGE_FILTER"
        elif (
            strategy["min_abs_computed_delta"] is not None
            and (computed_delta is None or abs(computed_delta) < float(strategy["min_abs_computed_delta"]))
        ):
            reason = "SHADOW_SKIP_TINY_TARGET_DELTA"
        elif (
            strategy["require_current_price_side"]
            and decision.get("current_price_side") != selected_outcome
        ):
            reason = "SHADOW_SKIP_CURRENT_PRICE_AGAINST_OR_UNKNOWN"
        else:
            action = "SHADOW_BUY_UP" if selected_outcome == UP else "SHADOW_BUY_DOWN"
            reason = "SHADOW_ENTER_UP" if selected_outcome == UP else "SHADOW_ENTER_DOWN"

        shadow = {
            "strategy_name": name,
            "action": action,
            "decision_reason": reason,
            "selected_outcome": selected_outcome,
            "best_ask": best_ask,
            "entry_price": best_ask if action.startswith("SHADOW_BUY_") else None,
            "shares": (
                float(config.fixed_amount_usdc) / best_ask
                if action.startswith("SHADOW_BUY_") and best_ask
                else None
            ),
            "fixed_amount_usdc": config.fixed_amount_usdc,
            "side_mode": strategy.get("side_mode") or "decision",
            "min_entry_price": min_entry_price,
            "max_entry_price": strategy["max_entry_price"],
            "max_seconds_after_market_open": max_seconds_after_open,
            "min_seconds_before_market_close": min_seconds_before_close,
            "max_seconds_before_market_close": max_seconds_before_close,
            "min_previous_result_latency_seconds": min_latency,
            "max_previous_result_latency_seconds": max_latency,
            "require_previous_result": require_previous_result,
            "min_abs_computed_delta": strategy["min_abs_computed_delta"],
            "require_current_price_side": strategy["require_current_price_side"],
        }
        shadows.append(shadow)
    return shadows


def execute_decision(config: Config, decision: Dict[str, Any], now_ts: int) -> Dict[str, Any]:
    action = str(decision.get("action") or "")
    if not action.startswith("BUY_"):
        return decision

    best_ask = float(decision["best_ask"])
    amount = float(decision["fixed_amount_usdc"])
    shares = amount / best_ask
    decision["entry_price"] = best_ask
    decision["shares"] = shares

    if config.mode == "paper":
        decision["order_id"] = f"paper-{decision['market_slug']}-{decision['selected_outcome']}"
        decision["order_status"] = "filled"
        decision["execution_message"] = "paper fill at current best ask"
        return decision

    request = {
        "schema_version": 1,
        "mode": "live-external",
        "action": "buy",
        "source_name": "BTC5mFollow",
        "source_trade": None,
        "order": {
            "asset": decision["token_id"],
            "copy_amount_usd": amount,
            "direct_limit_price": best_ask,
            "passive_limit_price": best_ask,
            "take_enabled": True,
        },
    }
    executor = config.executor_file()
    completed = subprocess.run(
        [str(executor)],
        input=json.dumps(request, separators=(",", ":")),
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        cwd=str(ROOT),
        check=False,
    )
    stdout = completed.stdout.strip()
    try:
        result = json.loads(stdout) if stdout else {}
    except json.JSONDecodeError:
        result = {"status": "failed", "message": stdout or completed.stderr.strip()}

    decision["order_status"] = result.get("status") or "failed"
    decision["order_id"] = result.get("order_id")
    decision["executor_result"] = public_executor_result(result)
    if result.get("filled_price"):
        decision["entry_price"] = float(result["filled_price"])
    elif result.get("order_price"):
        decision["entry_price"] = float(result["order_price"])
    if result.get("filled_size"):
        decision["shares"] = float(result["filled_size"])
    elif decision.get("entry_price"):
        decision["shares"] = amount / float(decision["entry_price"])
    if completed.returncode != 0 and decision["order_status"] != "failed":
        decision["order_status"] = "failed"
    return decision


def public_executor_result(result: Dict[str, Any]) -> Dict[str, Any]:
    allowed = {
        "status",
        "order_id",
        "order_price",
        "filled_amount_usd",
        "filled_size",
        "filled_price",
        "message",
    }
    return {key: value for key, value in result.items() if key in allowed}


def remember_trade(config: Config, state: State, decision: Dict[str, Any]) -> None:
    action = str(decision.get("action") or "")
    status = str(decision.get("order_status") or "")
    if not action.startswith("BUY_") or status in {"failed", "skipped", "cancelled"}:
        return
    market_slug = str(decision.get("market_slug") or "")
    if not market_slug:
        return
    state.last_trade_market_slug = market_slug
    state.daily_trade_count += 1
    state.open_trades.append(
        {
            "market_slug": market_slug,
            "market_start": decision.get("market_start"),
            "market_end": decision.get("market_end"),
            "market_start_ts": decision.get("market_start_ts"),
            "market_end_ts": decision.get("market_end_ts"),
            "selected_outcome": decision.get("selected_outcome"),
            "amount_usdc": float(decision.get("fixed_amount_usdc") or config.fixed_amount_usdc),
            "entry_price": float(decision.get("entry_price") or decision.get("best_ask") or 0.0),
            "shares": float(decision.get("shares") or 0.0),
            "mode": decision.get("mode"),
            "main_strategy": decision.get("main_strategy"),
            "order_id": decision.get("order_id"),
            "decision_reason": decision.get("decision_reason"),
            "trend": decision.get("trend"),
            "ret_30m": decision.get("ret_30m"),
            "previous_result": decision.get("previous_result"),
            "previous_result_source": decision.get("previous_result_source"),
            "official_previous_result": decision.get("official_previous_result"),
            "computed_previous_result": decision.get("computed_previous_result"),
            "previous_price_to_beat": decision.get("previous_price_to_beat"),
            "previous_final_price": decision.get("previous_final_price"),
            "current_price_to_beat": decision.get("current_price_to_beat"),
            "computed_price_delta": decision.get("computed_price_delta"),
            "computed_result_latency_seconds": decision.get("computed_result_latency_seconds"),
        }
    )


def remember_shadow_trades(config: Config, state: State, decision: Dict[str, Any]) -> None:
    market_slug = str(decision.get("market_slug") or "")
    if not market_slug:
        return
    existing = {
        (str(item.get("market_slug") or ""), str(item.get("strategy_name") or ""))
        for item in state.shadow_open_trades
    }
    for shadow in decision.get("shadow_strategies") or []:
        if not isinstance(shadow, dict):
            continue
        action = str(shadow.get("action") or "")
        strategy_name = str(shadow.get("strategy_name") or "")
        if not action.startswith("SHADOW_BUY_") or not strategy_name:
            continue
        key = (market_slug, strategy_name)
        if key in existing:
            continue
        best_ask = parse_float(shadow.get("entry_price") or shadow.get("best_ask"))
        if best_ask is None or best_ask <= 0:
            continue
        existing.add(key)
        state.shadow_open_trades.append(
            {
                "strategy_name": strategy_name,
                "market_slug": market_slug,
                "market_start": decision.get("market_start"),
                "market_end": decision.get("market_end"),
                "market_start_ts": decision.get("market_start_ts"),
                "market_end_ts": decision.get("market_end_ts"),
                "selected_outcome": shadow.get("selected_outcome"),
                "amount_usdc": float(shadow.get("fixed_amount_usdc") or config.fixed_amount_usdc),
                "entry_price": best_ask,
                "shares": float(shadow.get("shares") or 0.0),
                "decision_reason": shadow.get("decision_reason"),
                "trend": decision.get("trend"),
                "ret_30m": decision.get("ret_30m"),
                "previous_result": decision.get("previous_result"),
                "previous_result_source": decision.get("previous_result_source"),
                "computed_previous_result": decision.get("computed_previous_result"),
                "previous_price_to_beat": decision.get("previous_price_to_beat"),
                "previous_final_price": decision.get("previous_final_price"),
                "current_price_to_beat": decision.get("current_price_to_beat"),
                "computed_price_delta": decision.get("computed_price_delta"),
                "computed_result_latency_seconds": decision.get("computed_result_latency_seconds"),
                "binance_live_price": decision.get("binance_live_price"),
                "current_price_delta_to_target": decision.get("current_price_delta_to_target"),
                "current_price_side": decision.get("current_price_side"),
            }
        )


def append_jsonl(path: Path, payload: Dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("a", encoding="utf-8") as handle:
        handle.write(json.dumps(payload, ensure_ascii=False, sort_keys=True))
        handle.write("\n")


def log_previous_result_observed(
    state: State,
    log_path: Path,
    current_market: Market,
    previous_market: Optional[Market],
    previous_result: str,
    now_ts: int,
) -> None:
    if previous_market is None or previous_result not in {UP, DOWN}:
        return
    if state.last_previous_result_observed_market_slug == previous_market.slug:
        return
    state.last_previous_result_observed_market_slug = previous_market.slug
    append_jsonl(
        log_path,
        {
            "event_type": "previous_result_observed",
            "timestamp": iso_utc(now_ts),
            "current_market_slug": current_market.slug,
            "current_market_start": current_market.start_iso,
            "previous_market_slug": previous_market.slug,
            "previous_market_start": previous_market.start_iso,
            "previous_market_end": previous_market.end_iso,
            "previous_result": previous_result,
            "previous_result_latency_seconds": now_ts - previous_market.end_ts,
        },
    )


def normalized_computed_results(state: State) -> Dict[str, Dict[str, Any]]:
    if not isinstance(state.computed_results, dict):
        state.computed_results = {}
    return state.computed_results


def rounded_or_none(value: Optional[float], digits: int = 8) -> Optional[float]:
    return round(float(value), digits) if value is not None else None


def computed_previous_result_payload(
    current_market: Market,
    previous_market: Optional[Market],
    now_ts: int,
) -> Dict[str, Any]:
    previous_price = previous_market.price_to_beat if previous_market else None
    previous_final = previous_market.final_price if previous_market else None
    current_price = current_market.price_to_beat
    payload: Dict[str, Any] = {
        "computed_previous_result": UNKNOWN,
        "computed_result_source": COMPUTED_RESULT_SOURCE,
        "computed_result_rule": "",
        "computed_result_market_slug": previous_market.slug if previous_market else "",
        "computed_result_next_market_slug": current_market.slug,
        "previous_price_to_beat": rounded_or_none(previous_price),
        "previous_final_price": rounded_or_none(previous_final),
        "current_price_to_beat": rounded_or_none(current_price),
        "computed_price_delta": None,
        "computed_result_available_at": "",
        "computed_result_latency_seconds": None,
    }
    if previous_market is not None:
        payload["computed_result_latency_seconds"] = now_ts - previous_market.end_ts
    if previous_market is None or previous_price is None:
        return payload

    if previous_final is not None:
        delta = previous_final - previous_price
        rule = "previous_final_price - previous_price_to_beat"
    elif current_price is not None:
        delta = current_price - previous_price
        rule = "current_price_to_beat - previous_price_to_beat"
    else:
        return payload
    result = result_from_price_delta(delta)
    payload.update(
        {
            "computed_previous_result": result,
            "computed_result_rule": rule,
            "computed_price_delta": rounded_or_none(delta),
            "computed_result_available_at": iso_utc(now_ts) if result in {UP, DOWN} else "",
        }
    )
    return payload


def record_computed_previous_result(
    state: State,
    log_path: Path,
    current_market: Market,
    previous_market: Optional[Market],
    now_ts: int,
) -> Dict[str, Any]:
    payload = computed_previous_result_payload(current_market, previous_market, now_ts)
    result = str(payload.get("computed_previous_result") or UNKNOWN)
    if previous_market is None or result not in {UP, DOWN}:
        return payload

    results = normalized_computed_results(state)
    existing = results.get(previous_market.slug)
    record = dict(existing) if isinstance(existing, dict) else {}
    is_new = not record
    record.update(
        {
            "market_slug": previous_market.slug,
            "market_start": previous_market.start_iso,
            "market_end": previous_market.end_iso,
            "market_start_ts": previous_market.start_ts,
            "market_end_ts": previous_market.end_ts,
            "next_market_slug": current_market.slug,
            "next_market_start": current_market.start_iso,
            "next_market_start_ts": current_market.start_ts,
            "previous_price_to_beat": payload.get("previous_price_to_beat"),
            "previous_final_price": payload.get("previous_final_price"),
            "current_price_to_beat": payload.get("current_price_to_beat"),
            "computed_price_delta": payload.get("computed_price_delta"),
            "computed_result": result,
            "computed_result_source": COMPUTED_RESULT_SOURCE,
            "computed_result_rule": payload.get("computed_result_rule"),
            "computed_at": iso_utc(now_ts),
            "computed_latency_seconds": payload.get("computed_result_latency_seconds"),
        }
    )
    results[previous_market.slug] = record
    prune_computed_results(state)

    if is_new:
        append_jsonl(
            log_path,
            {
                "event_type": "computed_previous_result",
                "timestamp": iso_utc(now_ts),
                **record,
            },
        )
    return payload


def prune_computed_results(state: State) -> None:
    results = normalized_computed_results(state)
    if len(results) <= COMPUTED_RESULT_STATE_LIMIT:
        return
    ordered = sorted(
        results.items(),
        key=lambda item: int(item[1].get("market_start_ts") or 0)
        if isinstance(item[1], dict)
        else 0,
    )
    for slug, _record in ordered[: -COMPUTED_RESULT_STATE_LIMIT]:
        results.pop(slug, None)


def verify_computed_results(
    config: Config,
    state: State,
    polymarket: PolymarketClient,
    log_path: Path,
    now_ts: int,
) -> None:
    results = normalized_computed_results(state)
    checked = 0
    ordered = sorted(
        results.items(),
        key=lambda item: int(item[1].get("market_start_ts") or 0)
        if isinstance(item[1], dict)
        else 0,
    )
    for slug, record in ordered:
        if checked >= 5:
            break
        if not isinstance(record, dict):
            continue
        if record.get("official_result") in {UP, DOWN}:
            continue
        try:
            end_ts = int(record.get("market_end_ts") or 0)
            last_check_ts = int(record.get("last_official_check_ts") or 0)
        except (TypeError, ValueError):
            continue
        if end_ts + config.settlement_check_delay_seconds > now_ts:
            continue
        if now_ts - last_check_ts < COMPUTED_RESULT_CHECK_INTERVAL_SECONDS:
            continue

        record["last_official_check_ts"] = now_ts
        record["last_official_check_at"] = iso_utc(now_ts)
        market = polymarket.market_by_slug(slug)
        official_result = resolved_result(market.raw) if market else UNKNOWN
        checked += 1
        if official_result not in {UP, DOWN}:
            continue

        computed_result = str(record.get("computed_result") or UNKNOWN)
        record.update(
            {
                "official_result": official_result,
                "official_checked_at": iso_utc(now_ts),
                "official_result_latency_seconds": now_ts - end_ts,
                "match": computed_result == official_result,
            }
        )
        append_jsonl(
            log_path,
            {
                "event_type": "computed_result_verified",
                "timestamp": iso_utc(now_ts),
                **record,
            },
        )


def should_record_decision(state: State, market: Optional[Market], now_ts: int) -> bool:
    if market is not None:
        return state.last_decision_market_slug != market.slug
    window_start = floor_5m(now_ts)
    return state.last_decision_window_start != window_start


def mark_decision_recorded(state: State, market: Optional[Market], now_ts: int) -> None:
    state.last_decision_window_start = floor_5m(now_ts)
    if market is not None:
        state.last_decision_market_slug = market.slug


def should_record_research_observation(
    state: State,
    market: Optional[Market],
    previous_result: str,
) -> bool:
    if market is None or previous_result not in {UP, DOWN}:
        return False
    return state.last_research_observation_market_slug != market.slug


def mark_research_observation_recorded(state: State, market: Optional[Market]) -> None:
    if market is not None:
        state.last_research_observation_market_slug = market.slug


def should_record_late_observation(state: State, market: Optional[Market], now_ts: int) -> bool:
    if market is None:
        return False
    seconds_before_close = market.end_ts - now_ts
    if seconds_before_close <= 0 or seconds_before_close > 60:
        return False
    return state.last_late_observation_market_slug != market.slug


def mark_late_observation_recorded(state: State, market: Optional[Market]) -> None:
    if market is not None:
        state.last_late_observation_market_slug = market.slug


def should_retry_decision_later(decision: Dict[str, Any]) -> bool:
    return decision.get("decision_reason") in {
        "SKIP_PREVIOUS_RESULT_UNKNOWN",
        "SKIP_PRICE_UNAVAILABLE",
    }


def run_cycle(
    config: Config,
    state: State,
    binance: BinanceClient,
    polymarket: PolymarketClient,
    now_ts: int,
) -> Optional[Dict[str, Any]]:
    reset_daily_if_needed(config, state, now_ts)

    candles = binance.recent_5m_candles(now_ts)
    trend, ret_30m = compute_trend(config, state, candles)
    trend_data = trend_context(config, candles, ret_30m)
    apply_trend(config, state, trend)

    log_path = config.log_file()

    market = polymarket.current_btc_5m_market(now_ts)
    if market is not None:
        state.last_seen_market_slug = market.slug

    previous_result = UNKNOWN
    previous_result_source = ""
    official_previous_result = UNKNOWN
    computed_result: Dict[str, Any] = {}
    previous_market: Optional[Market] = None
    if market is not None:
        official_previous_result, previous_market = polymarket.previous_result(market)
        log_previous_result_observed(
            state,
            log_path,
            market,
            previous_market,
            official_previous_result,
            now_ts,
        )
        computed_result = record_computed_previous_result(
            state,
            log_path,
            market,
            previous_market,
            now_ts,
        )
        computed_previous_result = str(
            computed_result.get("computed_previous_result") or UNKNOWN
        )
        if official_previous_result in {UP, DOWN}:
            previous_result = official_previous_result
            previous_result_source = OFFICIAL_RESULT_SOURCE
        elif computed_previous_result in {UP, DOWN}:
            previous_result = computed_previous_result
            previous_result_source = COMPUTED_RESULT_SOURCE

    settle_open_trades(config, state, polymarket, log_path, now_ts)
    settle_shadow_open_trades(config, state, polymarket, log_path, now_ts)
    verify_computed_results(config, state, polymarket, log_path, now_ts)

    if market is not None and now_ts - market.start_ts < config.entry_delay_seconds:
        return None
    record_main_decision = should_record_decision(state, market, now_ts)
    record_research_observation = should_record_research_observation(
        state,
        market,
        previous_result,
    )
    record_late_observation = should_record_late_observation(state, market, now_ts)
    if not record_main_decision and not record_research_observation and not record_late_observation:
        return None

    up_best_ask: Optional[float] = None
    down_best_ask: Optional[float] = None
    if market is not None:
        if market.up_token_id:
            up_best_ask = polymarket.best_ask(market.up_token_id)
        if market.down_token_id:
            down_best_ask = polymarket.best_ask(market.down_token_id)

    live_btc_price = binance.current_price()
    decision = build_decision(
        config,
        state,
        market,
        previous_result,
        previous_result_source,
        official_previous_result,
        computed_result,
        previous_market,
        up_best_ask,
        down_best_ask,
        live_btc_price,
        trend_data,
        now_ts,
    )
    if record_main_decision:
        decision = execute_decision(config, decision, now_ts)
        append_jsonl(log_path, decision)
        remember_trade(config, state, decision)
        remember_shadow_trades(config, state, decision)
        if record_research_observation:
            mark_research_observation_recorded(state, market)
        if not should_retry_decision_later(decision):
            mark_decision_recorded(state, market, now_ts)
        return decision

    observation = dict(decision)
    if record_research_observation:
        observation["event_type"] = "research_observation"
        observation["research_reason"] = "PREVIOUS_RESULT_KNOWN_AFTER_MAIN_DECISION"
    else:
        observation["event_type"] = "late_window_observation"
        observation["research_reason"] = "LAST_MINUTE_SHADOW_WINDOW"
    observation["main_decision_already_recorded"] = True
    append_jsonl(log_path, observation)
    remember_shadow_trades(config, state, observation)
    if record_research_observation:
        mark_research_observation_recorded(state, market)
    if record_late_observation:
        mark_late_observation_recorded(state, market)
    return observation


def write_default_config(path: Path) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(asdict(Config()), indent=2, ensure_ascii=False) + "\n")


def parse_now(value: Optional[str]) -> int:
    if not value:
        return int(time.time())
    text = value.strip()
    if text.isdigit():
        return int(text)
    parsed = parse_iso_timestamp(text)
    if parsed is None:
        raise ValueError(f"invalid --now value: {value}")
    return parsed


def print_decision(decision: Optional[Dict[str, Any]], now_ts: int) -> None:
    if decision is None:
        print(json.dumps({"timestamp": iso_utc(now_ts), "status": "waiting"}, ensure_ascii=False))
    else:
        print(json.dumps(decision, ensure_ascii=False, sort_keys=True))


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--config", type=Path, default=None)
    parser.add_argument("--once", action="store_true", help="run one polling cycle")
    parser.add_argument("--dry-run", action="store_true", help="force paper execution for this run")
    parser.add_argument("--now", default=None, help="override current time as unix seconds or ISO timestamp")
    parser.add_argument("--print-state", action="store_true")
    parser.add_argument("--write-default-config", type=Path, default=None)
    args = parser.parse_args()

    if args.write_default_config:
        write_default_config(args.write_default_config)
        print(f"wrote {args.write_default_config}")
        return 0

    config = Config.load(args.config)
    if args.dry_run:
        config.mode = "paper"
        config.enabled = False

    state_path = config.state_file()
    state = State.load(state_path)
    if args.print_state:
        print(json.dumps(asdict(state), ensure_ascii=False, indent=2, sort_keys=True))
        return 0

    binance = BinanceClient(config)
    polymarket = PolymarketClient(config)

    while True:
        now_ts = parse_now(args.now)
        try:
            decision = run_cycle(config, state, binance, polymarket, now_ts)
            state.save(state_path)
            if args.once:
                print_decision(decision, now_ts)
                return 0
        except KeyboardInterrupt:
            state.save(state_path)
            raise
        except Exception as error:  # noqa: BLE001
            state.save(state_path)
            message = {"timestamp": iso_utc(int(time.time())), "status": "error", "error": str(error)}
            print(json.dumps(message, ensure_ascii=False), file=sys.stderr)
            if args.once:
                return 1

        time.sleep(config.poll_seconds)


if __name__ == "__main__":
    raise SystemExit(main())
