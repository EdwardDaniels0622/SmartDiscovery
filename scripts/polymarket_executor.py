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
from pathlib import Path
from typing import Any, Dict, Optional


ROOT = Path(__file__).resolve().parents[1]
ENV_FILE = ROOT / ".env"


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
    book = client.get_order_book(token_id)
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


def get_any(obj: Any, *keys: str) -> Any:
    for key in keys:
        if isinstance(obj, dict) and key in obj:
            return obj[key]
        if hasattr(obj, key):
            return getattr(obj, key)
    return None


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
    take_enabled = bool_value(order.get("take_enabled"), False)
    current_ask = best_price(client, token_id, "BUY")

    if take_enabled and current_ask is not None and current_ask <= direct_limit:
        args = MarketOrderArgsV2(
            token_id=token_id,
            amount=amount,
            side="BUY",
            price=direct_limit,
            order_type=OrderType.FOK,
        )
        response = client.create_and_post_market_order(args, order_type=OrderType.FOK)
        return normalize_order_response(
            response,
            default_status="filled",
            order_price=current_ask,
            message=f"best ask {current_ask:.4f} <= direct limit {direct_limit:.4f}; sent FOK buy",
        )

    if not take_enabled and current_ask is not None and current_ask <= passive_limit:
        return {
            "status": "skipped",
            "order_id": None,
            "order_price": passive_limit,
            "filled_amount_usd": None,
            "filled_size": None,
            "filled_price": None,
            "realized_pnl_usd": None,
            "message": (
                "passive-only mode: best ask "
                f"{current_ask:.4f} <= passive bid {passive_limit:.4f}; "
                "skipped instead of taking liquidity"
            ),
        }

    size = amount / passive_limit
    args = OrderArgsV2(
        token_id=token_id,
        price=passive_limit,
        size=size,
        side="BUY",
    )
    response = client.create_and_post_order(
        args,
        order_type=OrderType.GTC,
        post_only=True,
    )
    return normalize_order_response(
        response,
        default_status="pending",
        order_price=passive_limit,
        message=(
            ("take disabled; " if not take_enabled else "best ask unavailable or above direct limit; ")
            + f"placed post-only buy at {passive_limit:.4f}"
        ),
    )


def handle_sell(client: Any, order: Dict[str, Any]) -> Dict[str, Any]:
    sdk = import_sdk()
    OrderArgsV2 = sdk["OrderArgsV2"]
    OrderType = sdk["OrderType"]

    token_id = required_order(order, "asset")
    size = positive_float(required_order(order, "size_shares"), "size_shares")
    min_sell_price = clamp_price(float(required_order(order, "min_sell_price")))
    args = OrderArgsV2(
        token_id=token_id,
        price=min_sell_price,
        size=size,
        side="SELL",
    )
    response = client.create_and_post_order(
        args,
        order_type=OrderType.GTC,
        post_only=False,
    )
    return normalize_order_response(
        response,
        default_status="pending",
        order_price=min_sell_price,
        message=f"placed sell at or above {min_sell_price:.4f}",
    )


def handle_cancel(client: Any, order: Dict[str, Any]) -> Dict[str, Any]:
    sdk = import_sdk()
    OrderPayload = sdk["OrderPayload"]

    order_id = required_order(order, "order_id")
    response = client.cancel_order(OrderPayload(orderID=order_id))
    return normalize_order_response(
        response,
        default_status="cancelled",
        order_id=order_id,
        message=f"cancel requested for {order_id}",
    )


def handle_sync(client: Any, order: Dict[str, Any]) -> Dict[str, Any]:
    order_id = required_order(order, "order_id")
    response = client.get_order(order_id)
    return normalize_order_response(
        response,
        default_status="pending",
        order_id=order_id,
        message=f"synced order {order_id}",
    )


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
    if any(value in text for value in ["matched", "filled", "complete", "executed"]):
        return "filled"
    if any(value in text for value in ["cancel"]):
        return "cancelled"
    if any(value in text for value in ["live", "open", "pending", "unmatched", "submitted"]):
        return "pending"
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


def failed(message: str) -> Dict[str, Any]:
    return {
        "status": "failed",
        "order_id": None,
        "filled_amount_usd": None,
        "filled_size": None,
        "filled_price": None,
        "realized_pnl_usd": None,
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
