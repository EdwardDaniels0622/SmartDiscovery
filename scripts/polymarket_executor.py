#!/usr/bin/env python3
"""External Polymarket CLOB executor for WeatherHK auto-copy.

Reads one JSON request from stdin and writes one JSON result to stdout.
Secrets are read from .env / environment and are never printed.
"""

from __future__ import annotations

import argparse
import json
import os
import sys
import time
from decimal import ROUND_FLOOR, Decimal, InvalidOperation
from pathlib import Path
from typing import Any, Callable, Dict, Optional, TypeVar


ROOT = Path(__file__).resolve().parents[1]
ENV_FILE = ROOT / ".env"
COLLATERAL_TOKEN_SCALE = Decimal("1000000")
CONDITIONAL_TOKEN_SCALE = Decimal("1000000")
MIN_CONDITIONAL_ORDER_SIZE = 5.0
MIN_BUY_ORDER_AMOUNT_USD = 1.0
DEFAULT_BUY_BALANCE_BUFFER_USD = 0.05
DEFAULT_EXIT_SELL_FLOOR_PRICE = 0.001
DEFAULT_EXECUTOR_RETRIES = 2
DEFAULT_EXECUTOR_RETRY_BACKOFF_SECONDS = 0.4
RETRYABLE_ERROR_MARKERS = (
    "request exception",
    "status_code=none",
    "timed out",
    "timeout",
    "connection reset",
    "connection aborted",
    "connection error",
    "ssl",
    "remote end closed",
    "temporarily unavailable",
    "temporary failure",
    "max retries exceeded",
)
NON_RETRYABLE_ERROR_MARKERS = (
    "trading restricted",
    "geoblock",
    "not enough balance",
    "allowance",
    "lower than the minimum",
    "invalid token",
    "invalid. size",
    "no orders found to match",
    "no orderbook",
    "orderbook",
    "does not exist",
    "fully filled or killed",
)
T = TypeVar("T")
RETRY_NOTES: list[str] = []


def load_env_file(path: Path = ENV_FILE) -> None:
    if not path.exists():
        return

    for raw_line in path.read_text().splitlines():
        line = raw_line.strip()
        if not line or line.startswith("#") or "=" not in line:
            continue
        if line.startswith("export "):
            line = line[len("export ") :].strip()
        key, value = line.split("=", 1)
        key = key.strip()
        value = parse_env_value(value.strip())
        if key and key not in os.environ:
            os.environ[key] = value


def parse_env_value(value: str) -> str:
    if len(value) >= 2 and value[0] == value[-1] == '"':
        return (
            value[1:-1]
            .replace("\\n", "\n")
            .replace("\\r", "\r")
            .replace("\\t", "\t")
            .replace('\\"', '"')
            .replace("\\\\", "\\")
        )
    if len(value) >= 2 and value[0] == value[-1] == "'":
        return value[1:-1]

    for index, ch in enumerate(value):
        if ch == "#" and (index == 0 or value[index - 1].isspace()):
            return value[:index].strip()
    return value.strip()


def configure_proxy_env() -> None:
    proxy = env("POLYMARKET_PROXY_URL") or env("HTTPS_PROXY") or env("HTTP_PROXY")
    if not proxy:
        return
    os.environ.setdefault("HTTPS_PROXY", proxy)
    os.environ.setdefault("HTTP_PROXY", proxy)
    os.environ.setdefault("ALL_PROXY", proxy)


def env(key: str, default: Optional[str] = None) -> Optional[str]:
    value = os.environ.get(key)
    if value is None or value.strip() == "":
        return default
    return value.strip()


def require_env(key: str) -> str:
    value = env(key)
    if not value:
        raise RuntimeError(f"missing required env var {key}")
    return value


def env_int(key: str, default: int) -> int:
    try:
        return int(env(key, str(default)) or default)
    except (TypeError, ValueError):
        return default


def env_float(key: str, default: float) -> float:
    try:
        return float(env(key, str(default)) or default)
    except (TypeError, ValueError):
        return default


def executor_retries() -> int:
    return max(0, min(5, env_int("POLYMARKET_EXECUTOR_RETRIES", DEFAULT_EXECUTOR_RETRIES)))


def executor_retry_backoff_seconds() -> float:
    return max(
        0.0,
        min(
            5.0,
            env_float(
                "POLYMARKET_EXECUTOR_RETRY_BACKOFF_SECONDS",
                DEFAULT_EXECUTOR_RETRY_BACKOFF_SECONDS,
            ),
        ),
    )


def exit_sell_floor_price() -> float:
    return max(
        0.001,
        min(
            0.99,
            env_float(
                "POLYMARKET_FORCE_SELL_FLOOR_PRICE",
                DEFAULT_EXIT_SELL_FLOOR_PRICE,
            ),
        ),
    )


def buy_balance_buffer_usd() -> float:
    return max(
        0.0,
        min(
            1.0,
            env_float("POLYMARKET_BUY_BALANCE_BUFFER_USD", DEFAULT_BUY_BALANCE_BUFFER_USD),
        ),
    )


def short_error(error: Exception, limit: int = 180) -> str:
    text = " ".join(str(error).split())
    if len(text) <= limit:
        return text
    return text[: limit - 3] + "..."


def is_retryable_error(error: Exception) -> bool:
    text = str(error).lower()
    if any(marker in text for marker in NON_RETRYABLE_ERROR_MARKERS):
        return False
    return any(marker in text for marker in RETRYABLE_ERROR_MARKERS)


def is_insufficient_buying_power_error(error: Exception) -> bool:
    text = str(error).lower()
    return any(
        marker in text
        for marker in [
            "not enough balance",
            "insufficient balance",
            "insufficient funds",
            "allowance",
        ]
    )


def with_retries(label: str, fn: Callable[[], T]) -> T:
    retries = executor_retries()
    backoff = executor_retry_backoff_seconds()
    failures = []
    for attempt in range(retries + 1):
        try:
            result = fn()
            if attempt > 0:
                RETRY_NOTES.append(f"{label} succeeded after {attempt} retry(s)")
            return result
        except Exception as error:
            if attempt >= retries or not is_retryable_error(error):
                if failures:
                    joined = " | ".join(failures)
                    raise RuntimeError(
                        f"{label} failed after {len(failures)} retry(s): {joined}; final: {short_error(error)}"
                    ) from error
                raise
            failures.append(f"attempt {attempt + 1}: {short_error(error)}")
            time.sleep(backoff * (2**attempt))

    raise RuntimeError(f"{label} failed unexpectedly")


def message_with_retry_notes(message: str) -> str:
    if not RETRY_NOTES:
        return message
    return f"{message}; retries: {' | '.join(RETRY_NOTES)}"


def import_sdk():
    from py_clob_client_v2.client import ClobClient
    from py_clob_client_v2.clob_types import (
        ApiCreds,
        AssetType,
        BalanceAllowanceParams,
        MarketOrderArgsV2,
        OrderArgsV2,
        OrderPayload,
        OrderType,
    )

    return {
        "ClobClient": ClobClient,
        "ApiCreds": ApiCreds,
        "AssetType": AssetType,
        "BalanceAllowanceParams": BalanceAllowanceParams,
        "MarketOrderArgsV2": MarketOrderArgsV2,
        "OrderArgsV2": OrderArgsV2,
        "OrderPayload": OrderPayload,
        "OrderType": OrderType,
    }


def build_client() -> Any:
    sdk = import_sdk()
    ClobClient = sdk["ClobClient"]
    ApiCreds = sdk["ApiCreds"]

    host = env("POLYMARKET_CLOB_HOST", "https://clob.polymarket.com")
    key = require_env("POLYMARKET_PRIVATE_KEY")
    chain_id = int(env("POLYMARKET_CHAIN_ID", "137"))
    signature_type = int(env("POLYMARKET_SIGNATURE_TYPE", "2"))
    funder = require_env("POLYMARKET_FUNDER_ADDRESS")

    client = ClobClient(
        host,
        key=key,
        chain_id=chain_id,
        signature_type=signature_type,
        funder=funder,
    )

    api_key = env("POLYMARKET_CLOB_API_KEY")
    api_secret = env("POLYMARKET_CLOB_SECRET")
    api_passphrase = env("POLYMARKET_CLOB_PASSPHRASE")
    if api_key and api_secret and api_passphrase:
        client.set_api_creds(ApiCreds(api_key, api_secret, api_passphrase))
    else:
        auth_mode = (env("POLYMARKET_CLOB_AUTH_MODE", "derive") or "derive").lower()
        if auth_mode == "create-or-derive":
            creds = client.create_or_derive_api_key()
        elif auth_mode == "create":
            creds = client.create_api_key()
        else:
            creds = client.derive_api_key()
        client.set_api_creds(creds)

    return client


def best_price(client: Any, token_id: str, side: str) -> Optional[float]:
    book = with_retries("get orderbook", lambda: client.get_order_book(token_id))
    levels = getattr(book, "asks" if side.upper() == "BUY" else "bids", None)
    if levels is None and isinstance(book, dict):
        levels = book.get("asks" if side.upper() == "BUY" else "bids")
    if not levels:
        return None

    prices = []
    for level in levels:
        price = get_any(level, "price", "p")
        try:
            prices.append(float(price))
        except (TypeError, ValueError):
            pass
    if not prices:
        return None

    return min(prices) if side.upper() == "BUY" else max(prices)


def market_tick_size(client: Any, token_id: str) -> float:
    raw_tick = with_retries("get tick size", lambda: client.get_tick_size(token_id))
    try:
        tick = float(raw_tick)
    except (TypeError, ValueError) as error:
        raise ValueError(f"invalid market tick size: {raw_tick}") from error
    if tick <= 0.0 or tick >= 1.0:
        raise ValueError(f"invalid market tick size: {raw_tick}")
    return tick


def price_for_tick(price: float, tick: float) -> float:
    tick_decimal = Decimal(str(tick))
    price_decimal = Decimal(str(price))
    minimum = tick_decimal
    maximum = Decimal("1") - tick_decimal
    clamped = max(minimum, min(maximum, price_decimal))
    steps = (clamped / tick_decimal).to_integral_value(rounding=ROUND_FLOOR)
    return float(max(minimum, min(maximum, steps * tick_decimal)))


def get_any(obj: Any, *keys: str) -> Any:
    for key in keys:
        if isinstance(obj, dict) and key in obj:
            return obj[key]
        if hasattr(obj, key):
            return getattr(obj, key)
    return None


def collateral_balance_allowance(client: Any) -> Optional[Dict[str, Any]]:
    sdk = import_sdk()
    AssetType = sdk["AssetType"]
    BalanceAllowanceParams = sdk["BalanceAllowanceParams"]
    signature_type = int(env("POLYMARKET_SIGNATURE_TYPE", "2"))
    try:
        response = with_retries(
            "get collateral balance",
            lambda: client.get_balance_allowance(
                BalanceAllowanceParams(
                    asset_type=AssetType.COLLATERAL,
                    signature_type=signature_type,
                )
            ),
        )
    except Exception:
        return None
    return response if isinstance(response, dict) else public_object_dict(response)


def collateral_amount_usd(value: Any) -> Optional[float]:
    if value is None:
        return None
    try:
        amount = Decimal(str(value))
    except (InvalidOperation, ValueError):
        return None
    if amount < 0:
        return 0.0
    if amount > Decimal("10000"):
        amount = amount / COLLATERAL_TOKEN_SCALE
    return float(amount)


def collateral_available_usd(client: Any) -> Optional[float]:
    payload = collateral_balance_allowance(client)
    if payload is None:
        return None
    balance = collateral_amount_usd(first_present(payload, "balance"))
    allowance = collateral_amount_usd(first_present(payload, "allowance"))
    candidates = [
        value for value in [balance, allowance] if value is not None and value >= 0.0
    ]
    if not candidates:
        return None
    return min(candidates)


def reduce_buy_amount_to_available_balance(
    client: Any,
    requested_amount: float,
) -> tuple[Optional[float], str]:
    available = collateral_available_usd(client)
    if available is None:
        return requested_amount, ""

    spendable = max(0.0, available - buy_balance_buffer_usd())
    if spendable + 0.000001 >= requested_amount:
        return requested_amount, ""
    if spendable + 0.000001 < MIN_BUY_ORDER_AMOUNT_USD:
        return (
            None,
            "available USDC/allowance "
            f"{available:.4f}U leaves {spendable:.4f}U after buffer, below "
            f"the {MIN_BUY_ORDER_AMOUNT_USD:.2f}U minimum; cannot place a smaller buy",
        )
    return (
        spendable,
        f"requested {requested_amount:.4f}U but available USDC/allowance is "
        f"{available:.4f}U; reduced to {spendable:.4f}U and switched to post-only",
    )


def current_ask_message(current_ask: Optional[float]) -> str:
    if current_ask is None:
        return "current best ask unavailable"
    return f"current best ask {current_ask:.4f}"


def post_market_order_with_retries(client: Any, order_args: Any, order_type: Any) -> Any:
    signed_order = with_retries(
        "create market order",
        lambda: client.create_market_order(order_args),
    )
    return with_retries(
        "post market order",
        lambda: client.post_order(signed_order, order_type, False),
    )


def post_limit_order_with_retries(
    client: Any,
    order_args: Any,
    order_type: Any,
    post_only: bool,
) -> Any:
    signed_order = with_retries(
        "create order",
        lambda: client.create_order(order_args),
    )
    return with_retries(
        "post order",
        lambda: client.post_order(signed_order, order_type, post_only),
    )


def handle_auth_check(client: Any) -> Dict[str, Any]:
    sdk = import_sdk()
    AssetType = sdk["AssetType"]
    BalanceAllowanceParams = sdk["BalanceAllowanceParams"]

    result: Dict[str, Any] = {
        "status": "ok",
        "message": "CLOB auth initialized",
    }
    try:
        signature_type = int(env("POLYMARKET_SIGNATURE_TYPE", "2"))
        balance = client.get_balance_allowance(
            BalanceAllowanceParams(
                asset_type=AssetType.COLLATERAL,
                signature_type=signature_type,
            )
        )
        result["balance_allowance_available"] = True
        result["balance_allowance_summary"] = summarize_public(balance)
    except Exception as error:  # noqa: BLE001
        result["balance_allowance_available"] = False
        result["message"] = f"CLOB auth initialized, balance check failed: {error}"
    return result


def handle_request(client: Any, request: Dict[str, Any]) -> Dict[str, Any]:
    action = str(request.get("action", "")).lower()
    order = request.get("order") or {}

    if action == "buy":
        return handle_buy(client, order)
    if action == "sell":
        return handle_sell(client, order)
    if action == "cancel":
        return handle_cancel(client, order)
    if action == "sync":
        return handle_sync(client, order)
    if action == "auth-check":
        return handle_auth_check(client)

    return failed(f"unsupported action: {action}")


def handle_buy(client: Any, order: Dict[str, Any]) -> Dict[str, Any]:
    sdk = import_sdk()
    MarketOrderArgsV2 = sdk["MarketOrderArgsV2"]
    OrderArgsV2 = sdk["OrderArgsV2"]
    OrderType = sdk["OrderType"]

    token_id = required_order(order, "asset")
    amount = positive_float(required_order(order, "copy_amount_usd"), "copy_amount_usd")
    direct_limit = clamp_price(float(required_order(order, "direct_limit_price")))
    passive_limit = clamp_price(float(required_order(order, "passive_limit_price")))
    original_passive_limit = passive_limit
    take_enabled = bool_value(order.get("take_enabled"), False)
    current_ask = best_price(client, token_id, "BUY")
    balance_note = ""

    adjusted_amount, adjustment_note = reduce_buy_amount_to_available_balance(client, amount)
    if adjusted_amount is None:
        return skipped(adjustment_note)
    if adjusted_amount + 0.000001 < amount:
        amount = adjusted_amount
        take_enabled = False
        balance_note = f"; {adjustment_note}"

    if take_enabled and current_ask is not None and current_ask <= direct_limit:
        args = MarketOrderArgsV2(
            token_id=token_id,
            amount=amount,
            side="BUY",
            price=direct_limit,
            order_type=OrderType.FOK,
        )
        try:
            response = post_market_order_with_retries(client, args, OrderType.FOK)
        except Exception as exc:
            if not is_insufficient_buying_power_error(exc):
                raise
            adjusted_amount, adjustment_note = reduce_buy_amount_to_available_balance(
                client, amount
            )
            if adjusted_amount is None:
                return skipped(
                    f"FOK buy failed due insufficient balance/allowance; {adjustment_note}"
                )
            if adjusted_amount + 0.000001 >= amount:
                return failed(
                    "FOK buy failed due insufficient balance/allowance, but a lower safe "
                    f"amount could not be computed: {exc}"
                )
            amount = adjusted_amount
            take_enabled = False
            balance_note = (
                "; FOK buy failed due insufficient balance/allowance; "
                f"{adjustment_note}"
            )
        else:
            return normalize_order_response(
                response,
                default_status="filled",
                order_price=current_ask,
                message=message_with_retry_notes(
                    f"best ask {current_ask:.4f} <= direct limit {direct_limit:.4f}; sent FOK buy"
                ),
            )

    passive_adjustment = ""
    if not take_enabled and current_ask is not None and current_ask <= passive_limit:
        tick_size = market_tick_size(client, token_id)
        adjusted_limit = price_for_tick(current_ask - tick_size, tick_size)
        if adjusted_limit >= current_ask:
            return skipped(
                "passive-only mode: no tick-valid bid exists below the current best ask"
            )
        passive_adjustment = (
            f"; capped below best ask {current_ask:.4f} at {adjusted_limit:.4f} "
            "to avoid taking liquidity"
        )
        passive_limit = min(passive_limit, adjusted_limit)
        amount *= passive_limit / original_passive_limit
        if amount < MIN_BUY_ORDER_AMOUNT_USD:
            return skipped(
                "passive-only price adjustment keeps the requested share count but reduces "
                f"the order to {amount:.4f}U, below the {MIN_BUY_ORDER_AMOUNT_USD:.2f}U minimum"
                f"{balance_note}"
            )

    size = amount / passive_limit
    args = OrderArgsV2(
        token_id=token_id,
        price=passive_limit,
        size=size,
        side="BUY",
    )
    try:
        response = post_limit_order_with_retries(client, args, OrderType.GTC, True)
    except Exception as exc:
        if not is_insufficient_buying_power_error(exc):
            raise
        adjusted_amount, adjustment_note = reduce_buy_amount_to_available_balance(client, amount)
        if adjusted_amount is None:
            return skipped(
                f"post-only buy failed due insufficient balance/allowance; {adjustment_note}"
            )
        if adjusted_amount + 0.000001 >= amount:
            return failed(
                "post-only buy failed due insufficient balance/allowance, but a lower safe "
                f"amount could not be computed: {exc}"
            )
        amount = adjusted_amount
        size = amount / passive_limit
        balance_note = (
            "; post-only buy first attempt failed due insufficient balance/allowance; "
            f"{adjustment_note}"
        )
        args = OrderArgsV2(
            token_id=token_id,
            price=passive_limit,
            size=size,
            side="BUY",
        )
        response = post_limit_order_with_retries(client, args, OrderType.GTC, True)
    result = normalize_order_response(
        response,
        default_status="pending",
        order_price=passive_limit,
        message=message_with_retry_notes(
            (
                "take disabled; "
                if not take_enabled
                else "best ask unavailable or above direct limit; "
            )
            + f"placed post-only buy at {passive_limit:.4f}; "
            + current_ask_message(current_ask)
            + f"{balance_note}{passive_adjustment}"
        ),
    )
    result["target_size_shares"] = size
    filled_amount = result.get("filled_amount_usd")
    if filled_amount is None and result.get("filled_size") is not None:
        filled_amount = float(result["filled_size"]) * passive_limit
        result["filled_amount_usd"] = filled_amount
    if (
        result.get("status") == "filled"
        and filled_amount is not None
        and float(filled_amount) + 0.000001 < amount
    ):
        result["status"] = "pending"
        result["message"] = message_with_retry_notes(
            f"post-only buy partially filled at {passive_limit:.4f}; remainder stays open"
        )
    return result


def handle_sell(client: Any, order: Dict[str, Any]) -> Dict[str, Any]:
    sdk = import_sdk()
    MarketOrderArgsV2 = sdk["MarketOrderArgsV2"]
    OrderArgsV2 = sdk["OrderArgsV2"]
    OrderType = sdk["OrderType"]

    token_id = required_order(order, "asset")
    actual_balance = conditional_token_balance(client, token_id)
    if actual_balance <= 0.0:
        return skipped(
            "actual CLOB token balance is zero; local state should be reconciled",
            actual_balance_shares=actual_balance,
            target_size_shares=0.0,
        )

    raw_fraction = order.get("sell_fraction")
    if raw_fraction is not None and raw_fraction != "":
        sell_fraction = max(0.0, min(1.0, float(raw_fraction)))
        target_size = actual_balance if sell_fraction >= 0.999999 else actual_balance * sell_fraction
    else:
        requested_size = positive_float(required_order(order, "size_shares"), "size_shares")
        target_size = min(requested_size, actual_balance)

    if target_size <= 0.0:
        return skipped(
            "requested sell target is zero after clamping to actual CLOB token balance",
            actual_balance_shares=actual_balance,
            target_size_shares=target_size,
        )

    size = target_size
    if size < MIN_CONDITIONAL_ORDER_SIZE:
        if actual_balance < MIN_CONDITIONAL_ORDER_SIZE:
            return skipped(
                f"actual CLOB token balance {actual_balance:.6f} is below minimum sell size "
                f"{MIN_CONDITIONAL_ORDER_SIZE:.2f}; local state should be reconciled as dust",
                actual_balance_shares=actual_balance,
                target_size_shares=target_size,
            )
        return skipped(
            f"proportional sell target {target_size:.6f} shares is below exchange minimum "
            f"{MIN_CONDITIONAL_ORDER_SIZE:.2f}; accumulate future source SELL before submitting",
            actual_balance_shares=actual_balance,
            target_size_shares=target_size,
        )

    tick_size = market_tick_size(client, token_id)
    raw_min_sell_price = order.get("min_sell_price")
    requested_floor = (
        float(raw_min_sell_price)
        if raw_min_sell_price is not None and raw_min_sell_price != ""
        else exit_sell_floor_price()
    )
    sell_floor_price = price_for_tick(requested_floor, tick_size)
    rounding_note = ""

    force_market_sell = bool_value(order.get("force_market_sell"), False)
    lock_profit = bool_value(order.get("lock_profit"), False)
    raw_passive_limit_price = order.get("passive_limit_price")

    def place_protective_gtc_after_no_match(reason: str, fresh_balance: Optional[float]) -> Dict[str, Any]:
        balance = fresh_balance
        if balance is None:
            balance = safe_conditional_token_balance(client, token_id)
        sell_size = min(size, balance if balance is not None else actual_balance)
        if sell_size < MIN_CONDITIONAL_ORDER_SIZE:
            return skipped(
                f"{reason}; fresh CLOB balance {sell_size:.6f} is below minimum sell size "
                f"{MIN_CONDITIONAL_ORDER_SIZE:.2f}",
                actual_balance_shares=balance if balance is not None else actual_balance,
                target_size_shares=size,
            )

        args = OrderArgsV2(
            token_id=token_id,
            price=sell_floor_price,
            size=sell_size,
            side="SELL",
        )
        try:
            response = post_limit_order_with_retries(client, args, OrderType.GTC, False)
        except Exception as exc:
            return failed(
                f"{reason}; protective GTC sell also failed: {exc}",
                actual_balance_shares=balance if balance is not None else actual_balance,
                target_size_shares=sell_size,
            )

        result = normalize_order_response(
            response,
            default_status="pending",
            order_price=sell_floor_price,
            message=message_with_retry_notes(
                f"{reason}; placed protective GTC limit sell at {sell_floor_price:.4f} "
                f"after FAK found no matching bids (tick {tick_size:g})"
            ),
        )
        result["target_size_shares"] = sell_size
        post_balance = safe_conditional_token_balance(client, token_id)
        if post_balance is not None:
            result["actual_balance_shares"] = post_balance
            sold_size = max(0.0, min(sell_size, actual_balance - post_balance))
            if sold_size > 0.0:
                result["filled_size"] = sold_size
                if result.get("filled_price") is None:
                    result["filled_price"] = sell_floor_price
                result["filled_amount_usd"] = sold_size * float(result["filled_price"])
                if sold_size + 0.000001 >= sell_size:
                    result["status"] = "filled"
                else:
                    result["status"] = "pending"
                    result["message"] = (
                        str(result.get("message") or "")
                        + f"; partial protective GTC fill {sold_size:.6f}/{sell_size:.6f} shares, "
                        + f"remaining {sell_size - sold_size:.6f} shares"
                    )
        fill_missing_sell_details(result)
        return result

    if (
        not force_market_sell
        and raw_passive_limit_price is not None
        and raw_passive_limit_price != ""
    ):
        passive_limit = price_for_tick(float(raw_passive_limit_price), tick_size)
        args = OrderArgsV2(
            token_id=token_id,
            price=passive_limit,
            size=size,
            side="SELL",
        )
        try:
            response = post_limit_order_with_retries(client, args, OrderType.GTC, False)
        except Exception as exc:
            post_balance = safe_conditional_token_balance(client, token_id)
            return failed(
                str(exc),
                actual_balance_shares=(
                    post_balance if post_balance is not None else actual_balance
                ),
                target_size_shares=size,
            )

        result = normalize_order_response(
            response,
            default_status="pending",
            order_price=passive_limit,
            message=message_with_retry_notes(
                f"placed GTC limit sell at {passive_limit:.4f} for protected small source SELL "
                f"(tick {tick_size:g})"
            ),
        )
        result["target_size_shares"] = size
        post_balance = safe_conditional_token_balance(client, token_id)
        if post_balance is not None:
            result["actual_balance_shares"] = post_balance
            sold_size = max(0.0, min(size, actual_balance - post_balance))
            if sold_size > 0.0:
                result["filled_size"] = sold_size
                if result.get("filled_price") is None:
                    result["filled_price"] = passive_limit
                result["filled_amount_usd"] = sold_size * float(result["filled_price"])
                if sold_size + 0.000001 >= size:
                    result["status"] = "filled"
                else:
                    result["status"] = "pending"
                    result["message"] = (
                        str(result.get("message") or "")
                        + f"; partial GTC fill {sold_size:.6f}/{size:.6f} shares, "
                        + f"remaining {size - sold_size:.6f} shares"
                    )
        fill_missing_sell_details(result)
        return result

    if force_market_sell or raw_min_sell_price is not None:
        args = MarketOrderArgsV2(
            token_id=token_id,
            amount=size,
            side="SELL",
            price=sell_floor_price,
            order_type=OrderType.FAK,
        )
        try:
            response = post_market_order_with_retries(client, args, OrderType.FAK)
        except Exception as exc:
            message = str(exc)
            post_balance = safe_conditional_token_balance(client, token_id)
            if (
                "no orders found to match" in message.lower()
                and post_balance is not None
                and post_balance < MIN_CONDITIONAL_ORDER_SIZE
            ):
                return skipped(
                    "FAK sell found no matching bids, and a fresh CLOB balance check shows "
                    f"{post_balance:.6f} shares; local state should be reconciled as already exited",
                    actual_balance_shares=post_balance,
                    target_size_shares=size,
                )
            if "no orders found to match" in message.lower() and (
                lock_profit or sell_floor_price >= 0.90
            ):
                return place_protective_gtc_after_no_match(
                    "FAK sell found no matching bids while preserving a high-price/lock-profit exit",
                    post_balance,
                )
            return failed(
                message,
                actual_balance_shares=(
                    post_balance if post_balance is not None else actual_balance
                ),
                target_size_shares=size,
            )

        result = normalize_order_response(
            response,
            default_status="filled",
            order_price=None,
            message=message_with_retry_notes(
                f"sent {'market-exit' if force_market_sell else 'protected proportional'} "
                f"FAK sell with tick-valid floor {sell_floor_price:.4f} "
                f"(tick {tick_size:g}){rounding_note}"
            ),
        )
        result["target_size_shares"] = size
        post_balance = safe_conditional_token_balance(client, token_id)
        if post_balance is not None:
            result["actual_balance_shares"] = post_balance
            sold_size = max(0.0, min(size, actual_balance - post_balance))
            if sold_size > 0.0:
                result["status"] = "filled"
                result["filled_size"] = sold_size
                if sold_size + 0.000001 < size:
                    result["message"] = (
                        str(result.get("message") or "")
                        + f"; partial FAK fill {sold_size:.6f}/{size:.6f} shares, "
                        + f"remaining {size - sold_size:.6f} shares"
                    )
            elif result.get("status") == "filled":
                if lock_profit or sell_floor_price >= 0.90:
                    return place_protective_gtc_after_no_match(
                        "FAK sell observed no filled balance delta while preserving a high-price/lock-profit exit",
                        post_balance,
                    )
                result["status"] = "skipped"
                result["message"] = (
                    str(result.get("message") or "")
                    + "; no filled balance delta observed after FAK sell"
                )
        fill_missing_sell_details(result)
        return result

    current_bid = best_price(client, token_id, "SELL")
    if raw_min_sell_price is None or raw_min_sell_price == "":
        if current_bid is None:
            return failed(
                "current orderbook has no bids for this token; asks or historical trades may exist, "
                "but there is no executable buy liquidity for an immediate sell",
                actual_balance_shares=actual_balance,
                target_size_shares=size,
            )
        min_sell_price = price_for_tick(current_bid, tick_size)
        price_source = "current bid"
    else:
        min_sell_price = sell_floor_price
        price_source = "min_sell_price"
    args = OrderArgsV2(
        token_id=token_id,
        price=min_sell_price,
        size=size,
        side="SELL",
    )
    try:
        response = post_limit_order_with_retries(client, args, OrderType.FOK, False)
    except Exception as exc:
        return failed(
            str(exc),
            actual_balance_shares=actual_balance,
            target_size_shares=size,
        )
    result = normalize_order_response(
        response,
        default_status="filled",
        order_price=min_sell_price,
        message=message_with_retry_notes(
            f"sent FOK sell at or above {min_sell_price:.4f} ({price_source}, tick {tick_size:g})"
            f"{rounding_note}"
        ),
    )
    if result.get("status") == "filled" and result.get("filled_size") is None:
        result["filled_size"] = size
    result["target_size_shares"] = size
    post_balance = safe_conditional_token_balance(client, token_id)
    if post_balance is not None:
        result["actual_balance_shares"] = post_balance
        sold_size = max(0.0, min(size, actual_balance - post_balance))
        if sold_size > 0.0:
            result["status"] = "filled"
            result["filled_size"] = sold_size
        elif result.get("status") == "filled" and not result.get("filled_size"):
            result["status"] = "skipped"
            result["message"] = (
                str(result.get("message") or "")
                + "; no filled balance delta observed after FOK sell"
            )
    fill_missing_sell_details(result)
    return result


def handle_cancel(client: Any, order: Dict[str, Any]) -> Dict[str, Any]:
    sdk = import_sdk()
    OrderPayload = sdk["OrderPayload"]

    order_id = required_order(order, "order_id")
    passive_limit = float(required_order(order, "passive_limit_price"))
    with_retries("cancel order", lambda: client.cancel_order(OrderPayload(orderID=order_id)))
    response = with_retries("sync cancelled order", lambda: client.get_order(order_id))
    result = normalize_order_response(
        response,
        default_status="cancelled",
        order_price=passive_limit,
        order_id=order_id,
        message=message_with_retry_notes(
            f"cancelled order {order_id} and reconciled its final matched size"
        ),
    )
    if result.get("filled_amount_usd") is None and result.get("filled_size") is not None:
        result["filled_amount_usd"] = float(result["filled_size"]) * passive_limit
    return result


def handle_sync(client: Any, order: Dict[str, Any]) -> Dict[str, Any]:
    order_id = required_order(order, "order_id")
    response = with_retries("sync order", lambda: client.get_order(order_id))
    passive_limit = float(required_order(order, "passive_limit_price"))
    copy_amount = positive_float(required_order(order, "copy_amount_usd"), "copy_amount_usd")
    result = normalize_order_response(
        response,
        default_status="pending",
        order_price=passive_limit,
        order_id=order_id,
        message=message_with_retry_notes(f"synced order {order_id}"),
    )
    filled_amount = result.get("filled_amount_usd")
    if filled_amount is None and result.get("filled_size") is not None:
        filled_amount = float(result["filled_size"]) * passive_limit
        result["filled_amount_usd"] = filled_amount
    if (
        result.get("status") == "filled"
        and filled_amount is not None
        and float(filled_amount) + 0.000001 < copy_amount
    ):
        result["status"] = "pending"
        result["message"] = message_with_retry_notes(
            f"synced partially filled order {order_id}"
        )
    return result


def required_order(order: Dict[str, Any], key: str) -> Any:
    value = order.get(key)
    if value is None or value == "":
        raise RuntimeError(f"missing order.{key}")
    return value


def positive_float(value: Any, label: str) -> float:
    number = float(value)
    if number <= 0:
        raise RuntimeError(f"{label} must be positive")
    return number


def bool_value(value: Any, default: bool) -> bool:
    if value is None:
        return default
    if isinstance(value, bool):
        return value
    text = str(value).strip().lower()
    if text in {"1", "true", "yes", "on"}:
        return True
    if text in {"0", "false", "no", "off"}:
        return False
    return default


def clamp_price(price: float) -> float:
    return max(0.01, min(0.99, price))


def normalize_order_response(
    response: Any,
    *,
    default_status: str,
    order_price: Optional[float] = None,
    order_id: Optional[str] = None,
    message: str,
) -> Dict[str, Any]:
    payload = response if isinstance(response, dict) else public_object_dict(response)
    success = payload.get("success")
    if success is False:
        return failed(str(payload.get("errorMsg") or payload.get("error") or payload))

    resolved_order_id = (
        order_id
        or first_present(payload, "orderID", "orderId", "order_id", "id", "hash")
        or first_present(payload.get("order") or {}, "orderID", "orderId", "order_id", "id", "hash")
    )
    raw_status = str(first_present(payload, "status", "state") or default_status).lower()
    status = map_order_status(raw_status, default_status)

    return {
        "status": status,
        "order_id": resolved_order_id,
        "order_price": order_price,
        "filled_amount_usd": first_float(payload, "filled_amount_usd", "filledAmount", "matchedAmount"),
        "filled_size": first_float(payload, "filled_size", "filledSize", "size_matched"),
        "filled_price": first_float(payload, "filled_price", "averagePrice", "avgPrice"),
        "realized_pnl_usd": None,
        "message": message,
    }


def map_order_status(raw_status: str, default_status: str) -> str:
    text = raw_status.lower()
    if any(value in text for value in ["cancel"]):
        return "cancelled"
    if any(value in text for value in ["live", "open", "pending", "unmatched", "submitted"]):
        return "pending"
    if any(value in text for value in ["matched", "filled", "complete", "executed"]):
        return "filled"
    if any(value in text for value in ["fail", "reject", "error"]):
        return "failed"
    return default_status


def first_present(payload: Any, *keys: str) -> Any:
    if not isinstance(payload, dict):
        return None
    for key in keys:
        value = payload.get(key)
        if value is not None:
            return value
    return None


def first_float(payload: Dict[str, Any], *keys: str) -> Optional[float]:
    value = first_present(payload, *keys)
    try:
        return float(value) if value is not None else None
    except (TypeError, ValueError):
        return None


def conditional_token_balance(client: Any, token_id: str) -> float:
    sdk = import_sdk()
    AssetType = sdk["AssetType"]
    BalanceAllowanceParams = sdk["BalanceAllowanceParams"]
    signature_type = int(env("POLYMARKET_SIGNATURE_TYPE", "2"))
    response = with_retries(
        "get conditional token balance",
        lambda: client.get_balance_allowance(
            BalanceAllowanceParams(
                asset_type=AssetType.CONDITIONAL,
                token_id=token_id,
                signature_type=signature_type,
            )
        ),
    )
    payload = response if isinstance(response, dict) else public_object_dict(response)
    raw_balance = first_present(payload, "balance")
    if raw_balance is None:
        return 0.0
    try:
        return float(Decimal(str(raw_balance)) / CONDITIONAL_TOKEN_SCALE)
    except (InvalidOperation, ValueError):
        return 0.0


def safe_conditional_token_balance(client: Any, token_id: str) -> Optional[float]:
    try:
        return conditional_token_balance(client, token_id)
    except Exception:
        return None


def fill_missing_sell_details(result: Dict[str, Any]) -> None:
    filled_size = result.get("filled_size")
    filled_amount = result.get("filled_amount_usd")
    filled_price = result.get("filled_price")
    try:
        if filled_price is None and filled_amount is not None and filled_size is not None:
            size = float(filled_size)
            if size > 0:
                result["filled_price"] = float(filled_amount) / size
        elif filled_amount is None and filled_price is not None and filled_size is not None:
            result["filled_amount_usd"] = float(filled_price) * float(filled_size)
    except (TypeError, ValueError):
        return


def public_object_dict(obj: Any) -> Dict[str, Any]:
    if isinstance(obj, dict):
        return obj
    if hasattr(obj, "__dict__"):
        return {
            key: value
            for key, value in vars(obj).items()
            if not key.startswith("_") and "secret" not in key.lower() and "key" not in key.lower()
        }
    return {"value": str(obj)}


def summarize_public(obj: Any) -> Dict[str, Any]:
    payload = public_object_dict(obj)
    summary: Dict[str, Any] = {}
    for key, value in payload.items():
        lower = key.lower()
        if any(secret_word in lower for secret_word in ["secret", "key", "passphrase", "token"]):
            continue
        if isinstance(value, (str, int, float, bool)) or value is None:
            summary[key] = value
    return summary


def skipped(
    message: str,
    actual_balance_shares: Optional[float] = None,
    target_size_shares: Optional[float] = None,
) -> Dict[str, Any]:
    return {
        "status": "skipped",
        "order_id": None,
        "filled_amount_usd": None,
        "filled_size": None,
        "filled_price": None,
        "realized_pnl_usd": None,
        "actual_balance_shares": actual_balance_shares,
        "target_size_shares": target_size_shares,
        "message": message,
    }


def failed(
    message: str,
    actual_balance_shares: Optional[float] = None,
    target_size_shares: Optional[float] = None,
) -> Dict[str, Any]:
    return {
        "status": "failed",
        "order_id": None,
        "filled_amount_usd": None,
        "filled_size": None,
        "filled_price": None,
        "realized_pnl_usd": None,
        "actual_balance_shares": actual_balance_shares,
        "target_size_shares": target_size_shares,
        "message": message,
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--auth-check", action="store_true")
    args = parser.parse_args()

    load_env_file()
    configure_proxy_env()

    try:
        client = build_client()
        if args.auth_check:
            result = handle_auth_check(client)
        else:
            request = json.load(sys.stdin)
            result = handle_request(client, request)
    except Exception as error:  # noqa: BLE001
        result = failed(str(error))

    print(json.dumps(result, separators=(",", ":"), ensure_ascii=False))
    return 0 if result.get("status") != "failed" else 1


if __name__ == "__main__":
    raise SystemExit(main())
