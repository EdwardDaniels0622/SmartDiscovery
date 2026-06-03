use crate::{monitor::WatchedEmployee, polymarket::UserTrade};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    env, fmt, fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{SystemTime, UNIX_EPOCH},
};

const WEATHERHK_WALLET: &str = "0x488c725253fc21c7a9ca812030dc2f6343f98c1c";
const DEFAULT_STATE_PATH: &str = "logs/weatherhk_autocopy_state.json";
const STATE_LOG_LIMIT: usize = 500;
const PROCESSED_TRADE_LIMIT: usize = 1_000;
const MIN_COPY_AMOUNT_USD: f64 = 1.0;
const SOURCE_POSITION_RECONCILE_GRACE_SECONDS: u64 = 60;

#[derive(Debug, Clone)]
pub struct AutoCopyConfig {
    pub enabled: bool,
    pub mode: AutoCopyMode,
    pub source_wallet: String,
    pub source_name: String,
    pub domain: String,
    pub state_path: PathBuf,
    pub executor_command: Option<String>,
    pub max_single_copy_usd: f64,
    pub max_market_exposure_usd: f64,
    pub max_daily_spend_usd: f64,
    pub max_daily_loss_usd: f64,
    pub max_chase_pct: f64,
    pub passive_offset_pct: f64,
    pub max_chase_delta: f64,
    pub passive_offset: f64,
    pub buy_take_enabled: bool,
    pub min_buy_source_notional_usd: f64,
    pub skip_buy_price_at_or_above: f64,
    pub skip_buy_price_at_or_below: f64,
    pub min_sell_sync_notional_usd: f64,
    pub passive_order_ttl_seconds: u64,
    pub startup_backfill_seconds: u64,
    pub pending_sync_seconds: u64,
    pub default_sell_fraction: f64,
    pub clear_sell_notional_usd: f64,
}

impl AutoCopyConfig {
    pub fn weatherhk_default() -> Self {
        Self {
            enabled: env_bool("WEATHERHK_AUTO_COPY_ENABLED", false),
            mode: env_string("WEATHERHK_AUTO_COPY_MODE")
                .as_deref()
                .map(AutoCopyMode::parse)
                .transpose()
                .unwrap_or_else(|error| {
                    eprintln!("{error}; falling back to dry-run");
                    Some(AutoCopyMode::DryRun)
                })
                .unwrap_or(AutoCopyMode::DryRun),
            source_wallet: env_string("WEATHERHK_SOURCE_WALLET")
                .unwrap_or_else(|| WEATHERHK_WALLET.to_owned()),
            source_name: env_string("WEATHERHK_SOURCE_NAME")
                .unwrap_or_else(|| "WeatherHK".to_owned()),
            domain: env_string("WEATHERHK_DOMAIN").unwrap_or_else(|| "WEATHER".to_owned()),
            state_path: PathBuf::from(
                env_string("WEATHERHK_STATE_PATH").unwrap_or_else(|| DEFAULT_STATE_PATH.to_owned()),
            ),
            executor_command: env_string("WEATHERHK_AUTO_COPY_EXEC"),
            max_single_copy_usd: env_f64("WEATHERHK_MAX_SINGLE_COPY_USD", 10.0),
            max_market_exposure_usd: env_f64("WEATHERHK_MAX_MARKET_EXPOSURE_USD", 50.0),
            max_daily_spend_usd: env_f64("WEATHERHK_MAX_DAILY_SPEND_USD", 200.0),
            max_daily_loss_usd: env_f64("WEATHERHK_MAX_DAILY_LOSS_USD", 50.0),
            max_chase_pct: env_f64("WEATHERHK_MAX_CHASE_PCT", 0.15),
            passive_offset_pct: env_f64("WEATHERHK_PASSIVE_OFFSET_PCT", 0.05),
            max_chase_delta: env_f64("WEATHERHK_MAX_CHASE_DELTA", 0.03),
            passive_offset: env_f64("WEATHERHK_PASSIVE_OFFSET", 0.02),
            buy_take_enabled: env_bool("WEATHERHK_BUY_TAKE_ENABLED", false),
            min_buy_source_notional_usd: env_f64("WEATHERHK_MIN_BUY_SOURCE_NOTIONAL_USD", 1.0),
            skip_buy_price_at_or_above: env_f64("WEATHERHK_SKIP_BUY_PRICE_AT_OR_ABOVE", 0.98),
            skip_buy_price_at_or_below: env_f64("WEATHERHK_SKIP_BUY_PRICE_AT_OR_BELOW", 0.005),
            min_sell_sync_notional_usd: env_f64("WEATHERHK_MIN_SELL_SYNC_NOTIONAL_USD", 1.0),
            passive_order_ttl_seconds: env_u64("WEATHERHK_PASSIVE_TTL_SECONDS", 0),
            startup_backfill_seconds: env_u64("WEATHERHK_STARTUP_BACKFILL_SECONDS", 1_800),
            pending_sync_seconds: env_u64("WEATHERHK_PENDING_SYNC_SECONDS", 30),
            default_sell_fraction: env_f64("WEATHERHK_SELL_FRACTION", 0.50),
            clear_sell_notional_usd: env_f64("WEATHERHK_CLEAR_SELL_NOTIONAL_USD", 60.0),
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if !self.enabled {
            return Ok(());
        }

        if self.mode == AutoCopyMode::LiveExternal
            && self
                .executor_command
                .as_deref()
                .map(str::trim)
                .filter(|command| !command.is_empty())
                .is_none()
        {
            return Err(
                "WeatherHK live-external mode requires --weatherhk-auto-copy-exec".to_owned(),
            );
        }

        if self.max_single_copy_usd < MIN_COPY_AMOUNT_USD {
            return Err(format!(
                "WeatherHK max single copy must be at least ${MIN_COPY_AMOUNT_USD:.2}"
            ));
        }
        if self.max_market_exposure_usd < MIN_COPY_AMOUNT_USD {
            return Err(format!(
                "WeatherHK market exposure cap must be at least ${MIN_COPY_AMOUNT_USD:.2}"
            ));
        }
        if self.max_daily_spend_usd < MIN_COPY_AMOUNT_USD {
            return Err(format!(
                "WeatherHK daily spend cap must be at least ${MIN_COPY_AMOUNT_USD:.2}"
            ));
        }
        if !(0.0..=1.0).contains(&self.max_chase_pct) {
            return Err("WeatherHK max chase pct must be between 0 and 1".to_owned());
        }
        if !(0.0..=1.0).contains(&self.passive_offset_pct) {
            return Err("WeatherHK passive offset pct must be between 0 and 1".to_owned());
        }
        if !(0.0..=0.25).contains(&self.max_chase_delta) {
            return Err("WeatherHK max chase delta must be between 0 and 0.25".to_owned());
        }
        if !(0.0..=0.25).contains(&self.passive_offset) {
            return Err("WeatherHK passive offset must be between 0 and 0.25".to_owned());
        }
        if self.min_buy_source_notional_usd < 0.0 {
            return Err("WeatherHK min buy source notional must be >= 0".to_owned());
        }
        if !(0.01..=1.0).contains(&self.skip_buy_price_at_or_above) {
            return Err("WeatherHK skip buy price must be between 0.01 and 1.0".to_owned());
        }
        if !(0.0..=1.0).contains(&self.skip_buy_price_at_or_below) {
            return Err(
                "WeatherHK low-price skip buy threshold must be between 0 and 1.0".to_owned(),
            );
        }
        if self.min_sell_sync_notional_usd < 0.0 {
            return Err("WeatherHK min sell sync notional must be >= 0".to_owned());
        }
        if !(0.0..=1.0).contains(&self.default_sell_fraction) || self.default_sell_fraction <= 0.0 {
            return Err("WeatherHK sell fraction must be greater than 0 and <= 1".to_owned());
        }

        Ok(())
    }
}

impl Default for AutoCopyConfig {
    fn default() -> Self {
        Self::weatherhk_default()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AutoCopyMode {
    DryRun,
    LiveExternal,
}

impl AutoCopyMode {
    pub fn parse(value: &str) -> Result<Self, String> {
        match value.trim().to_lowercase().as_str() {
            "dry-run" | "dryrun" | "dry" => Ok(Self::DryRun),
            "live-external" | "external" | "live" => Ok(Self::LiveExternal),
            _ => Err(format!(
                "invalid WeatherHK auto-copy mode: {value}; use dry-run or live-external"
            )),
        }
    }

    pub fn label_for_display(self) -> &'static str {
        self.label()
    }

    fn label(self) -> &'static str {
        match self {
            Self::DryRun => "dry-run",
            Self::LiveExternal => "live-external",
        }
    }
}

#[derive(Debug)]
pub struct AutoCopyEngine {
    config: AutoCopyConfig,
    state: AutoCopyState,
}

impl AutoCopyEngine {
    pub fn new(config: AutoCopyConfig) -> Result<Self, AutoCopyError> {
        let mut state = AutoCopyState::load(&config.state_path)?;
        state.reset_day_if_needed(now_secs());

        Ok(Self { config, state })
    }

    pub fn config(&self) -> &AutoCopyConfig {
        &self.config
    }

    pub fn should_backfill_startup_trade(
        &self,
        employee: &WatchedEmployee,
        trade: &UserTrade,
        now_secs: u64,
    ) -> bool {
        if self.config.startup_backfill_seconds == 0 {
            return false;
        }

        if !self.should_handle(employee, trade) {
            return false;
        }

        let Some(timestamp) = trade.timestamp else {
            return false;
        };

        now_secs.saturating_sub(timestamp) <= self.config.startup_backfill_seconds
    }

    pub fn handle_tick(&mut self) -> Vec<AutoCopyReport> {
        let now = now_secs();
        self.state.reset_day_if_needed(now);

        let mut reports = Vec::new();
        reports.extend(self.sync_pending_orders(now));
        reports.extend(self.cancel_expired_orders(now));
        self.persist_if_needed(&mut reports);
        reports
    }

    pub fn has_pending_buy_orders(&self) -> bool {
        self.state
            .pending_orders
            .iter()
            .any(|order| order.side == "BUY")
    }

    pub fn cancel_pending_buys_absent_from_source_positions(
        &mut self,
        source_assets: &HashSet<String>,
    ) -> Vec<AutoCopyReport> {
        let now = now_secs();
        let orders = self
            .state
            .pending_orders
            .iter()
            .filter(|order| {
                should_cancel_pending_buy_absent_from_source_position(order, source_assets, now)
            })
            .cloned()
            .collect::<Vec<_>>();

        let mut reports = Vec::new();
        for order in orders {
            reports.extend(self.cancel_pending_order(
                &order,
                "WeatherHK 当前已不持有该 outcome，取消未成交跟单买单",
                now,
            ));
        }

        self.persist_if_needed(&mut reports);
        reports
    }

    pub fn handle_trade(
        &mut self,
        employee: &WatchedEmployee,
        trade: &UserTrade,
    ) -> Vec<AutoCopyReport> {
        let now = now_secs();
        self.state.reset_day_if_needed(now);

        if !self.should_handle(employee, trade) {
            return Vec::new();
        }

        let mut reports = Vec::new();
        reports.extend(self.handle_tick());

        let source_key = source_trade_key(trade);
        if self.state.has_processed_source_trade(&source_key) {
            return reports;
        }
        self.state.remember_processed_source_trade(source_key);

        let side = trade.side.to_uppercase();
        let trade_reports = if side == "BUY" {
            self.handle_buy(trade, now)
        } else if side == "SELL" {
            self.handle_sell(trade, now)
        } else {
            vec![self.skip_report(
                "未知方向",
                trade,
                format!("不支持的 WeatherHK 交易方向: {}", trade.side),
            )]
        };

        for report in trade_reports {
            self.state.append_log(&report);
            reports.push(report);
        }

        self.persist_if_needed(&mut reports);
        reports
    }

    fn should_handle(&self, employee: &WatchedEmployee, trade: &UserTrade) -> bool {
        if !self.config.enabled {
            return false;
        }

        if !same_wallet(&employee.wallet, &self.config.source_wallet) {
            return false;
        }

        if !employee.domain.eq_ignore_ascii_case(&self.config.domain) {
            return false;
        }

        matches_employee_keywords(employee, trade)
    }

    fn handle_buy(&mut self, trade: &UserTrade, now: u64) -> Vec<AutoCopyReport> {
        let Some(source_price) = trade.price.filter(|price| *price > 0.0 && *price < 1.0) else {
            return vec![self.skip_report("缺少价格", trade, "WeatherHK BUY 缺少有效成交价格。")];
        };
        let Some(source_size) = trade.size.filter(|size| *size > 0.0) else {
            return vec![self.skip_report("缺少数量", trade, "WeatherHK BUY 缺少有效成交数量。")];
        };

        let source_notional = source_price * source_size;
        if should_skip_small_buy(source_notional, self.config.min_buy_source_notional_usd) {
            return vec![self.skip_report(
                "买入金额过小",
                trade,
                format!(
                    "WeatherHK BUY 金额 {:.2}U，低于自动跟单阈值 {:.2}U；视为试探/碎片成交，不跟单。",
                    source_notional,
                    self.config.min_buy_source_notional_usd
                ),
            )];
        }

        if should_skip_near_zero_buy(source_price, self.config.skip_buy_price_at_or_below) {
            return vec![self.skip_report(
                "近零低价尾部",
                trade,
                format!(
                    "WeatherHK BUY 价格 {:.2}c，达到/低于低价跳过线 {:.2}c；这种近零赔率单可能是极小概率尾部或挂单噪音，不适合自动跟。",
                    source_price * 100.0,
                    self.config.skip_buy_price_at_or_below * 100.0
                ),
            )];
        }

        if should_skip_low_edge_buy(source_price, self.config.skip_buy_price_at_or_above) {
            return vec![self.skip_report(
                "高概率低收益",
                trade,
                format!(
                    "WeatherHK BUY 价格 {:.2}c，达到/超过高价跳过线 {:.2}c；历史画像显示 98c+/99c 高概率单胜率高但收益空间极薄，不适合自动跟。",
                    source_price * 100.0,
                    self.config.skip_buy_price_at_or_above * 100.0
                ),
            )];
        }

        let mut copy_amount =
            copy_amount_for_source_notional(source_notional).min(self.config.max_single_copy_usd);
        let key = position_key(trade);
        let market_exposure = self.state.market_exposure_usd(&key);
        let daily_reserved = self.state.daily_reserved_buy_usd();
        let remaining_market = self.config.max_market_exposure_usd - market_exposure;
        let remaining_daily =
            self.config.max_daily_spend_usd - self.state.daily_spend_usd - daily_reserved;
        copy_amount = copy_amount.min(remaining_market).min(remaining_daily);

        if self.state.daily_loss_usd() >= self.config.max_daily_loss_usd {
            return vec![self.skip_report(
                "今日亏损上限",
                trade,
                format!(
                    "今日已实现亏损 ${:.2}，达到上限 ${:.2}。",
                    self.state.daily_loss_usd(),
                    self.config.max_daily_loss_usd
                ),
            )];
        }

        if copy_amount < MIN_COPY_AMOUNT_USD {
            return vec![self.skip_report(
                "额度不足",
                trade,
                format!(
                    "按分档应跟 ${:.2}，但该市场/今日剩余额度只剩 ${:.2}/${:.2}。",
                    copy_amount_for_source_notional(source_notional)
                        .min(self.config.max_single_copy_usd),
                    remaining_market.max(0.0),
                    remaining_daily.max(0.0)
                ),
            )];
        }

        let direct_limit_price = price_with_capped_upside(
            source_price,
            self.config.max_chase_pct,
            self.config.max_chase_delta,
        );
        let passive_limit_price = price_with_capped_upside(
            source_price,
            self.config.passive_offset_pct,
            self.config.passive_offset,
        )
        .min(direct_limit_price);
        let request = AutoCopyExecutionRequest::buy(
            self.config.mode,
            self.config.source_name.clone(),
            trade,
            copy_amount,
            direct_limit_price,
            passive_limit_price,
            self.config.buy_take_enabled,
            self.config.passive_order_ttl_seconds,
        );
        let execution = self.execute_request(&request);
        let mut report = self.report_from_execution(
            "BUY",
            trade,
            source_notional,
            copy_amount,
            direct_limit_price,
            Some(passive_limit_price),
            &execution,
        );

        self.apply_buy_execution(
            &key,
            trade,
            copy_amount,
            passive_limit_price,
            now,
            &execution,
        );
        report.market_exposure_after_usd = self.state.market_exposure_usd(&key);
        report.daily_spend_after_usd =
            self.state.daily_spend_usd + self.state.daily_reserved_buy_usd();
        vec![report]
    }

    fn handle_sell(&mut self, trade: &UserTrade, now: u64) -> Vec<AutoCopyReport> {
        let key = position_key(trade);
        let Some(source_price) = trade.price.filter(|price| *price > 0.0 && *price < 1.0) else {
            return vec![self.skip_report(
                "缺少价格",
                trade,
                "WeatherHK SELL 缺少有效成交价格；不取消挂单、不卖仓位。",
            )];
        };
        let source_notional = trade
            .size
            .filter(|size| *size > 0.0)
            .map(|size| source_price * size)
            .unwrap_or(0.0);
        let mut reports = self.cancel_pending_for_key(&key, "WeatherHK 已卖出/减仓", now);
        if should_skip_dust_sell(source_notional, self.config.min_sell_sync_notional_usd) {
            reports.push(self.skip_report(
                "卖出金额过小",
                trade,
                format!(
                    "WeatherHK SELL 金额 {:.2}U，低于同步卖出阈值 {:.2}U；若有同 outcome 未成交买单已先取消，不同步卖出仓位。",
                    source_notional,
                    self.config.min_sell_sync_notional_usd
                ),
            ));
            return reports;
        }

        let Some(position) = self.state.position(&key).cloned() else {
            reports.push(self.skip_report(
                "无对应仓位",
                trade,
                "我方没有此前跟随的同 market/outcome 仓位；只记录 WeatherHK SELL，不下单。",
            ));
            return reports;
        };

        if position.size_shares <= 0.0 {
            reports.push(self.skip_report(
                "无有效仓位",
                trade,
                "我方同 market/outcome 仓位数量为 0；只记录 WeatherHK SELL。",
            ));
            return reports;
        }

        let sell_fraction = if source_notional >= self.config.clear_sell_notional_usd {
            1.0
        } else {
            self.config.default_sell_fraction
        };
        let sell_size = position.size_shares * sell_fraction.min(1.0);
        let min_sell_price = price_with_capped_downside(
            source_price,
            self.config.max_chase_pct,
            self.config.max_chase_delta,
        );
        let request = AutoCopyExecutionRequest::sell(
            self.config.mode,
            self.config.source_name.clone(),
            trade,
            sell_size,
            min_sell_price,
        );
        let execution = self.execute_request(&request);
        let mut report = self.report_from_execution(
            "SELL",
            trade,
            source_notional,
            sell_size * source_price,
            min_sell_price,
            None,
            &execution,
        );

        self.apply_sell_execution(&key, sell_size, source_price, &execution, &mut report);
        report.market_exposure_after_usd = self.state.market_exposure_usd(&key);
        report.daily_spend_after_usd =
            self.state.daily_spend_usd + self.state.daily_reserved_buy_usd();
        reports.push(report);
        reports
    }

    fn sync_pending_orders(&mut self, now: u64) -> Vec<AutoCopyReport> {
        if self.config.mode != AutoCopyMode::LiveExternal {
            return Vec::new();
        }

        let order_ids = self
            .state
            .pending_orders
            .iter()
            .filter(|order| {
                now.saturating_sub(order.last_sync_at_secs) >= self.config.pending_sync_seconds
            })
            .map(|order| order.local_order_id.clone())
            .collect::<Vec<_>>();
        let mut reports = Vec::new();

        for local_order_id in order_ids {
            let Some(order) = self.state.pending_order(&local_order_id).cloned() else {
                continue;
            };
            let request = AutoCopyExecutionRequest::sync(
                self.config.mode,
                self.config.source_name.clone(),
                &order,
            );
            let execution = self.execute_request(&request);
            let mut report = self.report_for_pending_execution("SYNC", &order, &execution);
            let should_notify = should_report_pending_sync(&order, &execution);

            self.apply_pending_sync(&order.local_order_id, &execution, &mut report, now);
            report.market_exposure_after_usd = self.state.market_exposure_usd(&order.position_key);
            report.daily_spend_after_usd =
                self.state.daily_spend_usd + self.state.daily_reserved_buy_usd();
            if should_notify {
                reports.push(report);
            }
        }

        reports
    }

    fn cancel_expired_orders(&mut self, now: u64) -> Vec<AutoCopyReport> {
        if self.config.passive_order_ttl_seconds == 0 {
            return Vec::new();
        }

        let expired = self
            .state
            .pending_orders
            .iter()
            .filter(|order| now >= order.expires_at_secs)
            .map(|order| order.local_order_id.clone())
            .collect::<Vec<_>>();
        let mut reports = Vec::new();

        for local_order_id in expired {
            let Some(order) = self.state.pending_order(&local_order_id).cloned() else {
                continue;
            };
            reports.extend(self.cancel_pending_order(&order, "挂单 TTL 到期", now));
        }

        reports
    }

    fn cancel_pending_for_key(&mut self, key: &str, reason: &str, now: u64) -> Vec<AutoCopyReport> {
        let orders = self
            .state
            .pending_orders
            .iter()
            .filter(|order| order.position_key == key)
            .cloned()
            .collect::<Vec<_>>();
        let mut reports = Vec::new();

        for order in orders {
            reports.extend(self.cancel_pending_order(&order, reason, now));
        }

        reports
    }

    fn cancel_pending_order(
        &mut self,
        order: &PendingCopyOrder,
        reason: &str,
        now: u64,
    ) -> Vec<AutoCopyReport> {
        let request = AutoCopyExecutionRequest::cancel(
            self.config.mode,
            self.config.source_name.clone(),
            order,
            reason,
        );
        let execution = self.execute_request(&request);
        let mut report = self.report_for_pending_execution("CANCEL", order, &execution);
        report.reason = reason.to_owned();

        if execution.status.is_successful_cancel() {
            self.state.remove_pending_order(&order.local_order_id);
        } else if let Some(existing) = self.state.pending_order_mut(&order.local_order_id) {
            existing.last_sync_at_secs = now;
        }
        report.market_exposure_after_usd = self.state.market_exposure_usd(&order.position_key);
        report.daily_spend_after_usd =
            self.state.daily_spend_usd + self.state.daily_reserved_buy_usd();

        self.state.append_log(&report);
        vec![report]
    }

    fn execute_request(&self, request: &AutoCopyExecutionRequest) -> ExecutionResult {
        match self.config.mode {
            AutoCopyMode::DryRun => ExecutionResult {
                status: ExecutionStatus::DryRun,
                order_id: None,
                order_price: None,
                filled_amount_usd: None,
                filled_size: None,
                filled_price: None,
                realized_pnl_usd: None,
                message: Some("dry-run: 未真实下单，只生成执行请求。".to_owned()),
            },
            AutoCopyMode::LiveExternal => self.execute_external(request),
        }
    }

    fn execute_external(&self, request: &AutoCopyExecutionRequest) -> ExecutionResult {
        let Some(command) = self
            .config
            .executor_command
            .as_deref()
            .map(str::trim)
            .filter(|command| !command.is_empty())
        else {
            return ExecutionResult::failed(
                "live-external mode enabled but executor command is missing",
            );
        };

        let request_json = match serde_json::to_vec(request) {
            Ok(json) => json,
            Err(error) => {
                return ExecutionResult::failed(format!(
                    "failed to encode executor request: {error}"
                ))
            }
        };
        let mut child = match Command::new("sh")
            .arg("-c")
            .arg(command)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(child) => child,
            Err(error) => {
                return ExecutionResult::failed(format!("failed to start executor: {error}"))
            }
        };

        if let Some(stdin) = child.stdin.as_mut() {
            if let Err(error) = stdin.write_all(&request_json) {
                return ExecutionResult::failed(format!(
                    "failed to write executor request: {error}"
                ));
            }
        }

        let output = match child.wait_with_output() {
            Ok(output) => output,
            Err(error) => {
                return ExecutionResult::failed(format!("failed to wait for executor: {error}"))
            }
        };

        if !output.status.success() {
            return ExecutionResult::failed(format!(
                "executor exited with code {:?}: {}",
                output.status.code(),
                String::from_utf8_lossy(&output.stderr)
            ));
        }

        if output.stdout.is_empty() {
            return ExecutionResult {
                status: ExecutionStatus::Submitted,
                order_id: None,
                order_price: None,
                filled_amount_usd: None,
                filled_size: None,
                filled_price: None,
                realized_pnl_usd: None,
                message: Some("executor returned empty success output".to_owned()),
            };
        }

        match serde_json::from_slice::<ExternalExecutionResult>(&output.stdout) {
            Ok(result) => result.into(),
            Err(error) => ExecutionResult::failed(format!(
                "executor output is not valid JSON: {error}; stdout={}",
                String::from_utf8_lossy(&output.stdout)
            )),
        }
    }

    fn apply_buy_execution(
        &mut self,
        key: &str,
        trade: &UserTrade,
        copy_amount: f64,
        passive_limit_price: f64,
        now: u64,
        execution: &ExecutionResult,
    ) {
        match execution.status {
            ExecutionStatus::Filled => {
                let filled_amount = execution.filled_amount_usd.unwrap_or(copy_amount);
                let filled_price = execution.filled_price.unwrap_or(passive_limit_price);
                let filled_size = execution
                    .filled_size
                    .unwrap_or_else(|| shares_for_amount(filled_amount, filled_price));
                self.state
                    .apply_buy_fill(key, trade, filled_amount, filled_size);
                self.state.daily_spend_usd += filled_amount;
            }
            ExecutionStatus::Pending | ExecutionStatus::Submitted => {
                let limit_price = execution.order_price.unwrap_or(passive_limit_price);
                self.state.pending_orders.push(PendingCopyOrder {
                    local_order_id: local_order_id(trade, now),
                    external_order_id: execution.order_id.clone(),
                    position_key: key.to_owned(),
                    side: "BUY".to_owned(),
                    market_title: trade.title.clone(),
                    outcome: trade.outcome.clone(),
                    asset: trade.asset.clone(),
                    condition_id: trade.condition_id.clone(),
                    copy_amount_usd: copy_amount,
                    limit_price,
                    filled_amount_usd: execution.filled_amount_usd.unwrap_or(0.0),
                    filled_size: execution.filled_size.unwrap_or(0.0),
                    created_at_secs: now,
                    expires_at_secs: pending_expires_at(now, self.config.passive_order_ttl_seconds),
                    last_sync_at_secs: now,
                    source_trade_key: source_trade_key(trade),
                });
            }
            ExecutionStatus::DryRun
            | ExecutionStatus::Cancelled
            | ExecutionStatus::Skipped
            | ExecutionStatus::Failed => {}
        }
    }

    fn apply_pending_sync(
        &mut self,
        local_order_id: &str,
        execution: &ExecutionResult,
        report: &mut AutoCopyReport,
        now: u64,
    ) {
        let Some(order) = self.state.pending_order(local_order_id).cloned() else {
            return;
        };

        if order.side == "BUY" {
            let filled_price = execution.filled_price.unwrap_or(order.limit_price);
            let filled_amount = execution
                .filled_amount_usd
                .or_else(|| execution.filled_size.map(|size| size * filled_price))
                .unwrap_or(order.filled_amount_usd);
            let filled_delta = (filled_amount - order.filled_amount_usd).max(0.0);
            if filled_delta > 0.0 {
                let filled_size_delta = execution
                    .filled_size
                    .map(|size| (size - order.filled_size).max(0.0))
                    .unwrap_or_else(|| shares_for_amount(filled_delta, filled_price));
                self.state.apply_buy_fill_from_order(
                    &order,
                    filled_delta,
                    filled_size_delta,
                    filled_price,
                );
                self.state.daily_spend_usd += filled_delta;
                report.copy_amount_usd = filled_delta;
                report.copy_price = Some(filled_price);
            }
        }

        match execution.status {
            ExecutionStatus::Filled | ExecutionStatus::Cancelled | ExecutionStatus::Skipped => {
                self.state.remove_pending_order(local_order_id);
            }
            ExecutionStatus::Pending
            | ExecutionStatus::Submitted
            | ExecutionStatus::DryRun
            | ExecutionStatus::Failed => {
                if let Some(existing) = self.state.pending_order_mut(local_order_id) {
                    existing.last_sync_at_secs = now;
                    let filled_price = execution.filled_price.unwrap_or(existing.limit_price);
                    if let Some(filled_amount) = execution
                        .filled_amount_usd
                        .or_else(|| execution.filled_size.map(|size| size * filled_price))
                    {
                        existing.filled_amount_usd = filled_amount;
                    }
                    if let Some(filled_size) = execution.filled_size {
                        existing.filled_size = filled_size;
                    }
                }
            }
        }
    }

    fn apply_sell_execution(
        &mut self,
        key: &str,
        requested_sell_size: f64,
        fallback_price: f64,
        execution: &ExecutionResult,
        report: &mut AutoCopyReport,
    ) {
        if !matches!(
            execution.status,
            ExecutionStatus::Filled | ExecutionStatus::DryRun
        ) {
            return;
        }

        if execution.status == ExecutionStatus::DryRun {
            return;
        }

        let Some(position) = self.state.position(key).cloned() else {
            return;
        };
        let sell_size = execution.filled_size.unwrap_or(requested_sell_size);
        let sell_price = execution.filled_price.unwrap_or(fallback_price);
        let filled_amount = execution
            .filled_amount_usd
            .unwrap_or_else(|| sell_size * sell_price);
        let avg_cost = position.avg_cost();
        let realized_pnl = execution
            .realized_pnl_usd
            .unwrap_or_else(|| filled_amount - sell_size * avg_cost);

        self.state
            .apply_sell_fill(key, sell_size, filled_amount, realized_pnl);
        self.state.daily_realized_pnl_usd += realized_pnl;
        report.realized_pnl_usd = Some(realized_pnl);
        report.copy_price = Some(sell_price);
        report.copy_amount_usd = filled_amount;
    }

    fn report_from_execution(
        &self,
        action: &str,
        trade: &UserTrade,
        source_notional: f64,
        copy_amount: f64,
        limit_price: f64,
        passive_limit_price: Option<f64>,
        execution: &ExecutionResult,
    ) -> AutoCopyReport {
        let title = trade.title.as_deref().unwrap_or("-");
        let outcome = trade.outcome.as_deref().unwrap_or("-");
        let source_price = trade.price.unwrap_or(0.0);
        let market_url = market_url(trade);
        let status = execution.status.label().to_owned();
        let copy_price = execution
            .filled_price
            .or(execution.order_price)
            .or(passive_limit_price)
            .or(Some(limit_price));
        let reason = match action {
            "BUY" => format!(
                "{source_notional:.2}U 落入 {} 档位；买入模式: {}；直接追价上限 {:.2}c（+{:.1}% 且最多 +{:.2}c），挂单 {:.2}c（+{:.1}% 且最多 +{:.2}c），{}。",
                tier_label(source_notional),
                if self.config.buy_take_enabled {
                    "允许 FOK 吃卖一"
                } else {
                    "只挂 post-only，不主动吃卖一"
                },
                limit_price * 100.0,
                self.config.max_chase_pct * 100.0,
                self.config.max_chase_delta * 100.0,
                passive_limit_price.unwrap_or(limit_price) * 100.0,
                self.config.passive_offset_pct * 100.0,
                self.config.passive_offset * 100.0,
                passive_ttl_label(self.config.passive_order_ttl_seconds)
            ),
            "SELL" => format!(
                "检测到 WeatherHK 卖出；先取消同 outcome 未成交买单，再按规则同步卖出我方已有仓位。最低卖价 {:.2}c。",
                limit_price * 100.0
            ),
            _ => "-".to_owned(),
        };
        let action_text = report_action_label(action, execution.status);
        let mut text = format!(
            "[WeatherHK 自动跟随{} / {}]\n\
状态: {}\n\
市场: {}\n\
方向: {}\n\
链接: {}\n\
WeatherHK: {} {:.2}U @ {:.2}c\n\
我方: {} {:.2}U @ {}\n\
原因: {}\n\
额度: 该市场敞口 {:.2}U / {:.2}U, 今日已用/预留 {:.2}U / {:.2}U",
            action_text,
            self.config.mode.label(),
            status,
            title,
            outcome,
            market_url,
            action_label(action),
            source_notional,
            source_price * 100.0,
            copy_action_label(action, execution.status),
            copy_amount,
            copy_price
                .map(|price| format!("{:.2}c", price * 100.0))
                .unwrap_or_else(|| "-".to_owned()),
            reason,
            self.state.market_exposure_usd(&position_key(trade)),
            self.config.max_market_exposure_usd,
            self.state.daily_spend_usd + self.state.daily_reserved_buy_usd(),
            self.config.max_daily_spend_usd,
        );

        if let Some(message) = &execution.message {
            text.push_str(&format!("\n执行器: {message}"));
        }
        if let Some(order_id) = &execution.order_id {
            text.push_str(&format!("\n订单: {order_id}"));
        }

        AutoCopyReport {
            action: action.to_owned(),
            status,
            reason,
            source_trade_key: source_trade_key(trade),
            market_title: trade.title.clone(),
            outcome: trade.outcome.clone(),
            position_key: position_key(trade),
            source_price: trade.price,
            source_notional_usd: source_notional,
            copy_amount_usd: copy_amount,
            copy_price,
            order_id: execution.order_id.clone(),
            realized_pnl_usd: execution.realized_pnl_usd,
            market_exposure_after_usd: self.state.market_exposure_usd(&position_key(trade)),
            daily_spend_after_usd: self.state.daily_spend_usd + self.state.daily_reserved_buy_usd(),
            created_at_secs: now_secs(),
            text,
        }
    }

    fn report_for_pending_execution(
        &self,
        action: &str,
        order: &PendingCopyOrder,
        execution: &ExecutionResult,
    ) -> AutoCopyReport {
        let copy_price = execution
            .filled_price
            .or(execution.order_price)
            .or(Some(order.limit_price));
        let copy_amount = execution.filled_amount_usd.unwrap_or(order.copy_amount_usd);
        let status = execution.status.label().to_owned();
        let mut text = format!(
            "[WeatherHK 挂单{} / {}]\n\
状态: {}\n\
市场: {}\n\
方向: {}\n\
原挂单: {:.2}U @ {:.2}c\n\
订单: {}\n\
原因: {}",
            pending_action_label(action),
            self.config.mode.label(),
            status,
            order.market_title.as_deref().unwrap_or("-"),
            order.outcome.as_deref().unwrap_or("-"),
            order.copy_amount_usd,
            order.limit_price * 100.0,
            order
                .external_order_id
                .as_deref()
                .unwrap_or(&order.local_order_id),
            execution
                .message
                .as_deref()
                .unwrap_or("同步/撤单请求已提交。"),
        );

        if let Some(filled_price) = execution.filled_price {
            text.push_str(&format!("\n成交价: {:.2}c", filled_price * 100.0));
        }
        if let Some(filled_amount) = execution.filled_amount_usd {
            text.push_str(&format!("\n成交金额: {:.2}U", filled_amount));
        }

        AutoCopyReport {
            action: action.to_owned(),
            status,
            reason: execution
                .message
                .clone()
                .unwrap_or_else(|| "pending order lifecycle update".to_owned()),
            source_trade_key: order.source_trade_key.clone(),
            market_title: order.market_title.clone(),
            outcome: order.outcome.clone(),
            position_key: order.position_key.clone(),
            source_price: None,
            source_notional_usd: 0.0,
            copy_amount_usd: copy_amount,
            copy_price,
            order_id: order.external_order_id.clone(),
            realized_pnl_usd: execution.realized_pnl_usd,
            market_exposure_after_usd: self.state.market_exposure_usd(&order.position_key),
            daily_spend_after_usd: self.state.daily_spend_usd + self.state.daily_reserved_buy_usd(),
            created_at_secs: now_secs(),
            text,
        }
    }

    fn skip_report(
        &self,
        title: &str,
        trade: &UserTrade,
        reason: impl Into<String>,
    ) -> AutoCopyReport {
        let reason = reason.into();
        let source_price = trade.price.unwrap_or(0.0);
        let source_notional = trade
            .size
            .filter(|size| *size > 0.0)
            .map(|size| source_price * size)
            .unwrap_or(0.0);
        let text = format!(
            "[WeatherHK 自动跟随跳过 / {}]\n\
原因: {}\n\
市场: {}\n\
方向: {}\n\
WeatherHK: {} {:.2}U @ {:.2}c",
            self.config.mode.label(),
            reason,
            trade.title.as_deref().unwrap_or("-"),
            trade.outcome.as_deref().unwrap_or("-"),
            trade.side,
            source_notional,
            source_price * 100.0,
        );

        AutoCopyReport {
            action: format!("SKIP:{title}"),
            status: "skipped".to_owned(),
            reason,
            source_trade_key: source_trade_key(trade),
            market_title: trade.title.clone(),
            outcome: trade.outcome.clone(),
            position_key: position_key(trade),
            source_price: trade.price,
            source_notional_usd: source_notional,
            copy_amount_usd: 0.0,
            copy_price: None,
            order_id: None,
            realized_pnl_usd: None,
            market_exposure_after_usd: self.state.market_exposure_usd(&position_key(trade)),
            daily_spend_after_usd: self.state.daily_spend_usd + self.state.daily_reserved_buy_usd(),
            created_at_secs: now_secs(),
            text,
        }
    }

    fn persist_if_needed(&self, reports: &mut Vec<AutoCopyReport>) {
        if reports.is_empty() {
            return;
        }

        if let Err(error) = self.state.save(&self.config.state_path) {
            reports.push(AutoCopyReport::system(format!(
                "[WeatherHK 自动跟随状态保存失败]\n原因: {error}"
            )));
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutoCopyState {
    pub day_bucket: u64,
    pub daily_spend_usd: f64,
    pub daily_realized_pnl_usd: f64,
    pub positions: Vec<CopyPosition>,
    pub pending_orders: Vec<PendingCopyOrder>,
    pub processed_source_trades: Vec<String>,
    pub logs: Vec<AutoCopyLog>,
}

impl Default for AutoCopyState {
    fn default() -> Self {
        Self {
            day_bucket: now_secs() / 86_400,
            daily_spend_usd: 0.0,
            daily_realized_pnl_usd: 0.0,
            positions: Vec::new(),
            pending_orders: Vec::new(),
            processed_source_trades: Vec::new(),
            logs: Vec::new(),
        }
    }
}

impl AutoCopyState {
    fn load(path: &Path) -> Result<Self, AutoCopyError> {
        if !path.exists() {
            return Ok(Self::default());
        }

        let bytes = fs::read(path).map_err(AutoCopyError::Io)?;
        serde_json::from_slice(&bytes).map_err(AutoCopyError::Json)
    }

    fn save(&self, path: &Path) -> Result<(), AutoCopyError> {
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent).map_err(AutoCopyError::Io)?;
        }

        let json = serde_json::to_vec_pretty(self).map_err(AutoCopyError::Json)?;
        fs::write(path, json).map_err(AutoCopyError::Io)
    }

    fn reset_day_if_needed(&mut self, now: u64) {
        let current_day = now / 86_400;
        if self.day_bucket != current_day {
            self.day_bucket = current_day;
            self.daily_spend_usd = 0.0;
            self.daily_realized_pnl_usd = 0.0;
        }
    }

    fn daily_loss_usd(&self) -> f64 {
        (-self.daily_realized_pnl_usd).max(0.0)
    }

    fn daily_reserved_buy_usd(&self) -> f64 {
        self.pending_orders
            .iter()
            .filter(|order| order.side == "BUY")
            .map(|order| (order.copy_amount_usd - order.filled_amount_usd).max(0.0))
            .sum()
    }

    fn market_exposure_usd(&self, key: &str) -> f64 {
        let position_exposure = self
            .positions
            .iter()
            .find(|position| position.position_key == key)
            .map(|position| position.cost_usd.max(0.0))
            .unwrap_or(0.0);
        let pending_exposure = self
            .pending_orders
            .iter()
            .filter(|order| order.position_key == key && order.side == "BUY")
            .map(|order| (order.copy_amount_usd - order.filled_amount_usd).max(0.0))
            .sum::<f64>();

        position_exposure + pending_exposure
    }

    fn has_processed_source_trade(&self, source_key: &str) -> bool {
        self.processed_source_trades
            .iter()
            .any(|key| key == source_key)
    }

    fn remember_processed_source_trade(&mut self, source_key: String) {
        if !self.has_processed_source_trade(&source_key) {
            self.processed_source_trades.push(source_key);
            if self.processed_source_trades.len() > PROCESSED_TRADE_LIMIT {
                let excess = self.processed_source_trades.len() - PROCESSED_TRADE_LIMIT;
                self.processed_source_trades.drain(0..excess);
            }
        }
    }

    fn position(&self, key: &str) -> Option<&CopyPosition> {
        self.positions
            .iter()
            .find(|position| position.position_key == key)
    }

    fn position_mut(&mut self, key: &str) -> Option<&mut CopyPosition> {
        self.positions
            .iter_mut()
            .find(|position| position.position_key == key)
    }

    fn pending_order(&self, local_order_id: &str) -> Option<&PendingCopyOrder> {
        self.pending_orders
            .iter()
            .find(|order| order.local_order_id == local_order_id)
    }

    fn pending_order_mut(&mut self, local_order_id: &str) -> Option<&mut PendingCopyOrder> {
        self.pending_orders
            .iter_mut()
            .find(|order| order.local_order_id == local_order_id)
    }

    fn remove_pending_order(&mut self, local_order_id: &str) {
        self.pending_orders
            .retain(|order| order.local_order_id != local_order_id);
    }

    fn apply_buy_fill(
        &mut self,
        key: &str,
        trade: &UserTrade,
        filled_amount_usd: f64,
        filled_size: f64,
    ) {
        if filled_amount_usd <= 0.0 || filled_size <= 0.0 {
            return;
        }

        if let Some(position) = self.position_mut(key) {
            position.size_shares += filled_size;
            position.cost_usd += filled_amount_usd;
            position.updated_at_secs = now_secs();
            return;
        }

        self.positions.push(CopyPosition {
            position_key: key.to_owned(),
            market_title: trade.title.clone(),
            outcome: trade.outcome.clone(),
            asset: trade.asset.clone(),
            condition_id: trade.condition_id.clone(),
            size_shares: filled_size,
            cost_usd: filled_amount_usd,
            realized_pnl_usd: 0.0,
            updated_at_secs: now_secs(),
        });
    }

    fn apply_buy_fill_from_order(
        &mut self,
        order: &PendingCopyOrder,
        filled_amount_usd: f64,
        filled_size: f64,
        filled_price: f64,
    ) {
        let synthetic_trade = UserTrade {
            proxy_wallet: WEATHERHK_WALLET.to_owned(),
            side: "BUY".to_owned(),
            asset: order.asset.clone(),
            condition_id: order.condition_id.clone(),
            size: Some(filled_size),
            price: Some(filled_price),
            timestamp: Some(now_secs()),
            title: order.market_title.clone(),
            slug: None,
            event_slug: None,
            outcome: order.outcome.clone(),
            outcome_index: None,
            name: None,
            pseudonym: None,
            transaction_hash: None,
        };
        self.apply_buy_fill(
            &order.position_key,
            &synthetic_trade,
            filled_amount_usd,
            filled_size,
        );
    }

    fn apply_sell_fill(
        &mut self,
        key: &str,
        sell_size: f64,
        _filled_amount_usd: f64,
        realized_pnl_usd: f64,
    ) {
        let Some(position) = self.position_mut(key) else {
            return;
        };
        if sell_size <= 0.0 || position.size_shares <= 0.0 {
            return;
        }

        let actual_sell_size = sell_size.min(position.size_shares);
        let avg_cost = position.avg_cost();
        position.size_shares -= actual_sell_size;
        position.cost_usd -= actual_sell_size * avg_cost;
        position.realized_pnl_usd += realized_pnl_usd;
        position.updated_at_secs = now_secs();

        if position.size_shares <= 0.000_001 {
            position.size_shares = 0.0;
            position.cost_usd = 0.0;
        }
    }

    fn append_log(&mut self, report: &AutoCopyReport) {
        self.logs.push(AutoCopyLog {
            created_at_secs: report.created_at_secs,
            action: report.action.clone(),
            status: report.status.clone(),
            reason: report.reason.clone(),
            source_trade_key: report.source_trade_key.clone(),
            position_key: report.position_key.clone(),
            source_notional_usd: report.source_notional_usd,
            copy_amount_usd: report.copy_amount_usd,
            copy_price: report.copy_price,
            order_id: report.order_id.clone(),
            realized_pnl_usd: report.realized_pnl_usd,
        });

        if self.logs.len() > STATE_LOG_LIMIT {
            let excess = self.logs.len() - STATE_LOG_LIMIT;
            self.logs.drain(0..excess);
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CopyPosition {
    pub position_key: String,
    pub market_title: Option<String>,
    pub outcome: Option<String>,
    pub asset: String,
    pub condition_id: String,
    pub size_shares: f64,
    pub cost_usd: f64,
    pub realized_pnl_usd: f64,
    pub updated_at_secs: u64,
}

impl CopyPosition {
    fn avg_cost(&self) -> f64 {
        if self.size_shares > 0.0 {
            self.cost_usd / self.size_shares
        } else {
            0.0
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingCopyOrder {
    pub local_order_id: String,
    pub external_order_id: Option<String>,
    pub position_key: String,
    pub side: String,
    pub market_title: Option<String>,
    pub outcome: Option<String>,
    pub asset: String,
    pub condition_id: String,
    pub copy_amount_usd: f64,
    pub limit_price: f64,
    pub filled_amount_usd: f64,
    pub filled_size: f64,
    pub created_at_secs: u64,
    pub expires_at_secs: u64,
    pub last_sync_at_secs: u64,
    pub source_trade_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutoCopyLog {
    pub created_at_secs: u64,
    pub action: String,
    pub status: String,
    pub reason: String,
    pub source_trade_key: String,
    pub position_key: String,
    pub source_notional_usd: f64,
    pub copy_amount_usd: f64,
    pub copy_price: Option<f64>,
    pub order_id: Option<String>,
    pub realized_pnl_usd: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct AutoCopyReport {
    pub action: String,
    pub status: String,
    pub reason: String,
    pub source_trade_key: String,
    pub market_title: Option<String>,
    pub outcome: Option<String>,
    pub position_key: String,
    pub source_price: Option<f64>,
    pub source_notional_usd: f64,
    pub copy_amount_usd: f64,
    pub copy_price: Option<f64>,
    pub order_id: Option<String>,
    pub realized_pnl_usd: Option<f64>,
    pub market_exposure_after_usd: f64,
    pub daily_spend_after_usd: f64,
    pub created_at_secs: u64,
    pub text: String,
}

impl AutoCopyReport {
    fn system(text: String) -> Self {
        Self {
            action: "SYSTEM".to_owned(),
            status: "failed".to_owned(),
            reason: text.clone(),
            source_trade_key: "-".to_owned(),
            market_title: None,
            outcome: None,
            position_key: "-".to_owned(),
            source_price: None,
            source_notional_usd: 0.0,
            copy_amount_usd: 0.0,
            copy_price: None,
            order_id: None,
            realized_pnl_usd: None,
            market_exposure_after_usd: 0.0,
            daily_spend_after_usd: 0.0,
            created_at_secs: now_secs(),
            text,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
struct AutoCopyExecutionRequest {
    schema_version: u8,
    mode: AutoCopyMode,
    action: String,
    source_name: String,
    source_trade: Option<SourceTradeSnapshot>,
    order: CopyOrderIntent,
}

impl AutoCopyExecutionRequest {
    fn buy(
        mode: AutoCopyMode,
        source_name: String,
        trade: &UserTrade,
        copy_amount_usd: f64,
        direct_limit_price: f64,
        passive_limit_price: f64,
        take_enabled: bool,
        ttl_seconds: u64,
    ) -> Self {
        Self {
            schema_version: 1,
            mode,
            action: "buy".to_owned(),
            source_name,
            source_trade: Some(SourceTradeSnapshot::from_trade(trade)),
            order: CopyOrderIntent {
                position_key: position_key(trade),
                side: "BUY".to_owned(),
                asset: trade.asset.clone(),
                condition_id: trade.condition_id.clone(),
                copy_amount_usd: Some(copy_amount_usd),
                size_shares: None,
                source_price: trade.price,
                direct_limit_price: Some(direct_limit_price),
                passive_limit_price: Some(passive_limit_price),
                take_enabled: Some(take_enabled),
                min_sell_price: None,
                ttl_seconds: Some(ttl_seconds),
                order_id: None,
                reason: Some(if take_enabled {
                    "If current ask <= direct_limit_price, buy immediately; otherwise place passive limit order at passive_limit_price and keep it until ttl/cancel."
                } else {
                    "Passive-only mode: never take the current ask; place post-only at passive_limit_price unless that would cross the current ask."
                }
                .to_owned()),
            },
        }
    }

    fn sell(
        mode: AutoCopyMode,
        source_name: String,
        trade: &UserTrade,
        size_shares: f64,
        min_sell_price: f64,
    ) -> Self {
        Self {
            schema_version: 1,
            mode,
            action: "sell".to_owned(),
            source_name,
            source_trade: Some(SourceTradeSnapshot::from_trade(trade)),
            order: CopyOrderIntent {
                position_key: position_key(trade),
                side: "SELL".to_owned(),
                asset: trade.asset.clone(),
                condition_id: trade.condition_id.clone(),
                copy_amount_usd: None,
                size_shares: Some(size_shares),
                source_price: trade.price,
                direct_limit_price: None,
                passive_limit_price: None,
                take_enabled: None,
                min_sell_price: Some(min_sell_price),
                ttl_seconds: None,
                order_id: None,
                reason: Some(
                    "WeatherHK sold this market/outcome; cancel pending buys first, then sell this size at or above min_sell_price."
                        .to_owned(),
                ),
            },
        }
    }

    fn cancel(
        mode: AutoCopyMode,
        source_name: String,
        order: &PendingCopyOrder,
        reason: &str,
    ) -> Self {
        Self {
            schema_version: 1,
            mode,
            action: "cancel".to_owned(),
            source_name,
            source_trade: None,
            order: CopyOrderIntent::from_pending(order, Some(reason.to_owned())),
        }
    }

    fn sync(mode: AutoCopyMode, source_name: String, order: &PendingCopyOrder) -> Self {
        Self {
            schema_version: 1,
            mode,
            action: "sync".to_owned(),
            source_name,
            source_trade: None,
            order: CopyOrderIntent::from_pending(
                order,
                Some("Report whether this pending order is filled, partially filled, pending, or cancelled.".to_owned()),
            ),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
struct SourceTradeSnapshot {
    source_trade_key: String,
    proxy_wallet: String,
    side: String,
    asset: String,
    condition_id: String,
    price: Option<f64>,
    size: Option<f64>,
    notional_usd: Option<f64>,
    timestamp: Option<u64>,
    title: Option<String>,
    slug: Option<String>,
    event_slug: Option<String>,
    outcome: Option<String>,
    transaction_hash: Option<String>,
}

impl SourceTradeSnapshot {
    fn from_trade(trade: &UserTrade) -> Self {
        Self {
            source_trade_key: source_trade_key(trade),
            proxy_wallet: trade.proxy_wallet.clone(),
            side: trade.side.clone(),
            asset: trade.asset.clone(),
            condition_id: trade.condition_id.clone(),
            price: trade.price,
            size: trade.size,
            notional_usd: trade
                .price
                .zip(trade.size)
                .map(|(price, size)| price * size),
            timestamp: trade.timestamp,
            title: trade.title.clone(),
            slug: trade.slug.clone(),
            event_slug: trade.event_slug.clone(),
            outcome: trade.outcome.clone(),
            transaction_hash: trade.transaction_hash.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
struct CopyOrderIntent {
    position_key: String,
    side: String,
    asset: String,
    condition_id: String,
    copy_amount_usd: Option<f64>,
    size_shares: Option<f64>,
    source_price: Option<f64>,
    direct_limit_price: Option<f64>,
    passive_limit_price: Option<f64>,
    take_enabled: Option<bool>,
    min_sell_price: Option<f64>,
    ttl_seconds: Option<u64>,
    order_id: Option<String>,
    reason: Option<String>,
}

impl CopyOrderIntent {
    fn from_pending(order: &PendingCopyOrder, reason: Option<String>) -> Self {
        Self {
            position_key: order.position_key.clone(),
            side: order.side.clone(),
            asset: order.asset.clone(),
            condition_id: order.condition_id.clone(),
            copy_amount_usd: Some(order.copy_amount_usd),
            size_shares: None,
            source_price: None,
            direct_limit_price: None,
            passive_limit_price: Some(order.limit_price),
            take_enabled: None,
            min_sell_price: None,
            ttl_seconds: Some(order_remaining_ttl(order.expires_at_secs)),
            order_id: order.external_order_id.clone(),
            reason,
        }
    }
}

fn order_remaining_ttl(expires_at_secs: u64) -> u64 {
    if expires_at_secs == 0 {
        0
    } else {
        expires_at_secs.saturating_sub(now_secs())
    }
}

#[derive(Debug, Clone, Deserialize)]
struct ExternalExecutionResult {
    status: Option<String>,
    order_id: Option<String>,
    order_price: Option<f64>,
    limit_price: Option<f64>,
    filled_amount_usd: Option<f64>,
    filled_size: Option<f64>,
    filled_price: Option<f64>,
    realized_pnl_usd: Option<f64>,
    message: Option<String>,
}

impl From<ExternalExecutionResult> for ExecutionResult {
    fn from(value: ExternalExecutionResult) -> Self {
        let status = value
            .status
            .as_deref()
            .map(ExecutionStatus::parse)
            .unwrap_or(ExecutionStatus::Submitted);
        Self {
            status,
            order_id: value.order_id,
            order_price: value.order_price.or(value.limit_price),
            filled_amount_usd: value.filled_amount_usd,
            filled_size: value.filled_size,
            filled_price: value.filled_price,
            realized_pnl_usd: value.realized_pnl_usd,
            message: value.message,
        }
    }
}

#[derive(Debug, Clone)]
struct ExecutionResult {
    status: ExecutionStatus,
    order_id: Option<String>,
    order_price: Option<f64>,
    filled_amount_usd: Option<f64>,
    filled_size: Option<f64>,
    filled_price: Option<f64>,
    realized_pnl_usd: Option<f64>,
    message: Option<String>,
}

impl ExecutionResult {
    fn failed(message: impl Into<String>) -> Self {
        Self {
            status: ExecutionStatus::Failed,
            order_id: None,
            order_price: None,
            filled_amount_usd: None,
            filled_size: None,
            filled_price: None,
            realized_pnl_usd: None,
            message: Some(message.into()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExecutionStatus {
    DryRun,
    Submitted,
    Pending,
    Filled,
    Cancelled,
    Skipped,
    Failed,
}

impl ExecutionStatus {
    fn parse(value: &str) -> Self {
        match value.trim().to_lowercase().as_str() {
            "dry-run" | "dryrun" => Self::DryRun,
            "submitted" | "submit" | "placed" => Self::Submitted,
            "pending" | "open" | "partial" | "partially-filled" => Self::Pending,
            "filled" | "done" | "success" => Self::Filled,
            "cancelled" | "canceled" | "cancelled-by-user" => Self::Cancelled,
            "skipped" | "skip" => Self::Skipped,
            "failed" | "error" => Self::Failed,
            _ => Self::Submitted,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::DryRun => "dry-run",
            Self::Submitted => "submitted",
            Self::Pending => "pending",
            Self::Filled => "filled",
            Self::Cancelled => "cancelled",
            Self::Skipped => "skipped",
            Self::Failed => "failed",
        }
    }

    fn is_successful_cancel(self) -> bool {
        matches!(
            self,
            Self::DryRun | Self::Submitted | Self::Pending | Self::Cancelled | Self::Skipped
        )
    }
}

#[derive(Debug)]
pub enum AutoCopyError {
    Io(std::io::Error),
    Json(serde_json::Error),
}

impl fmt::Display for AutoCopyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "I/O error: {error}"),
            Self::Json(error) => write!(f, "JSON error: {error}"),
        }
    }
}

impl std::error::Error for AutoCopyError {}

fn copy_amount_for_source_notional(source_notional: f64) -> f64 {
    match source_notional {
        value if value < 10.0 => 1.0,
        value if value < 30.0 => 2.0,
        value if value < 60.0 => 3.0,
        value if value < 100.0 => 5.0,
        value if value < 200.0 => 8.0,
        _ => 10.0,
    }
}

fn env_string(key: &str) -> Option<String> {
    env::var(key)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn env_bool(key: &str, default: bool) -> bool {
    match env_string(key)
        .map(|value| value.to_ascii_lowercase())
        .as_deref()
    {
        Some("1" | "true" | "yes" | "on") => true,
        Some("0" | "false" | "no" | "off") => false,
        Some(value) => {
            eprintln!("invalid bool for {key}: {value}; using {default}");
            default
        }
        None => default,
    }
}

fn env_f64(key: &str, default: f64) -> f64 {
    env_string(key)
        .and_then(|value| match value.parse::<f64>() {
            Ok(parsed) => Some(parsed),
            Err(error) => {
                eprintln!("invalid number for {key}: {error}; using {default}");
                None
            }
        })
        .unwrap_or(default)
}

fn env_u64(key: &str, default: u64) -> u64 {
    env_string(key)
        .and_then(|value| match value.parse::<u64>() {
            Ok(parsed) => Some(parsed),
            Err(error) => {
                eprintln!("invalid integer for {key}: {error}; using {default}");
                None
            }
        })
        .unwrap_or(default)
}

fn tier_label(source_notional: f64) -> &'static str {
    match source_notional {
        value if value < 10.0 => "<10U => 1U",
        value if value < 30.0 => "10-30U => 2U",
        value if value < 60.0 => "30-60U => 3U",
        value if value < 100.0 => "60-100U => 5U",
        value if value < 200.0 => "100-200U => 8U",
        _ => ">=200U => 10U",
    }
}

fn shares_for_amount(amount_usd: f64, price: f64) -> f64 {
    if price > 0.0 {
        amount_usd / price
    } else {
        0.0
    }
}

fn same_wallet(left: &str, right: &str) -> bool {
    left.trim().eq_ignore_ascii_case(right.trim())
}

fn matches_employee_keywords(employee: &WatchedEmployee, trade: &UserTrade) -> bool {
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

fn position_key(trade: &UserTrade) -> String {
    format!("{}:{}", trade.condition_id, trade.asset)
}

fn source_trade_key(trade: &UserTrade) -> String {
    if let Some(tx) = trade
        .transaction_hash
        .as_deref()
        .map(str::trim)
        .filter(|tx| !tx.is_empty())
    {
        return format!(
            "{}:{}:{}:{:.8}:{:.8}",
            tx,
            trade.condition_id,
            trade.asset,
            trade.price.unwrap_or(0.0),
            trade.size.unwrap_or(0.0)
        );
    }

    format!(
        "{}:{}:{}:{}:{:.8}:{:.8}",
        trade.timestamp.unwrap_or(0),
        trade.side,
        trade.condition_id,
        trade.asset,
        trade.price.unwrap_or(0.0),
        trade.size.unwrap_or(0.0)
    )
}

fn local_order_id(trade: &UserTrade, now: u64) -> String {
    format!("weatherhk:{}:{}:{}", now, trade.condition_id, trade.asset)
}

fn clamp_price(price: f64) -> f64 {
    price.clamp(0.01, 0.99)
}

fn price_with_capped_upside(source_price: f64, pct: f64, absolute_cap: f64) -> f64 {
    let delta = (source_price * pct).min(absolute_cap);
    clamp_price(source_price + delta)
}

fn price_with_capped_downside(source_price: f64, pct: f64, absolute_cap: f64) -> f64 {
    let delta = (source_price * pct).min(absolute_cap);
    clamp_price(source_price - delta)
}

fn should_skip_low_edge_buy(source_price: f64, skip_price_at_or_above: f64) -> bool {
    source_price >= skip_price_at_or_above
}

fn should_skip_near_zero_buy(source_price: f64, skip_price_at_or_below: f64) -> bool {
    skip_price_at_or_below > 0.0 && source_price <= skip_price_at_or_below
}

fn should_skip_small_buy(source_notional: f64, min_buy_source_notional_usd: f64) -> bool {
    min_buy_source_notional_usd > 0.0 && source_notional < min_buy_source_notional_usd
}

fn should_skip_dust_sell(source_notional: f64, min_sell_sync_notional_usd: f64) -> bool {
    min_sell_sync_notional_usd > 0.0 && source_notional < min_sell_sync_notional_usd
}

fn should_report_pending_sync(order: &PendingCopyOrder, execution: &ExecutionResult) -> bool {
    match execution.status {
        ExecutionStatus::Filled
        | ExecutionStatus::Cancelled
        | ExecutionStatus::Skipped
        | ExecutionStatus::Failed => true,
        ExecutionStatus::Pending | ExecutionStatus::Submitted => {
            let filled_price = execution.filled_price.unwrap_or(order.limit_price);
            let filled_amount = execution
                .filled_amount_usd
                .or_else(|| execution.filled_size.map(|size| size * filled_price))
                .unwrap_or(order.filled_amount_usd);
            filled_amount > order.filled_amount_usd + 0.000001
        }
        ExecutionStatus::DryRun => false,
    }
}

fn should_cancel_pending_buy_absent_from_source_position(
    order: &PendingCopyOrder,
    source_assets: &HashSet<String>,
    now: u64,
) -> bool {
    order.side == "BUY"
        && now.saturating_sub(order.created_at_secs) >= SOURCE_POSITION_RECONCILE_GRACE_SECONDS
        && !source_assets.contains(&order.asset)
}

fn pending_expires_at(now: u64, ttl_seconds: u64) -> u64 {
    if ttl_seconds == 0 {
        0
    } else {
        now.saturating_add(ttl_seconds)
    }
}

fn passive_ttl_label(ttl_seconds: u64) -> String {
    if ttl_seconds == 0 {
        "无 TTL，WeatherHK 未卖出则继续挂单".to_owned()
    } else {
        format!("TTL {ttl_seconds}s")
    }
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

fn action_label(action: &str) -> &'static str {
    match action {
        "BUY" => "买入",
        "SELL" => "卖出",
        _ => "操作",
    }
}

fn report_action_label(action: &str, status: ExecutionStatus) -> &'static str {
    match status {
        ExecutionStatus::Skipped | ExecutionStatus::Failed => "跳过",
        _ => action_label(action),
    }
}

fn copy_action_label(action: &str, status: ExecutionStatus) -> &'static str {
    match (action, status) {
        ("BUY", ExecutionStatus::Pending | ExecutionStatus::Submitted) => "挂单/提交",
        ("BUY", ExecutionStatus::Skipped | ExecutionStatus::Failed) => "跳过",
        ("BUY", _) => "买入",
        ("SELL", ExecutionStatus::Skipped | ExecutionStatus::Failed) => "跳过",
        ("SELL", _) => "卖出",
        _ => "操作",
    }
}

fn pending_action_label(action: &str) -> &'static str {
    match action {
        "SYNC" => "同步",
        "CANCEL" => "取消",
        _ => "更新",
    }
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

    #[test]
    fn copy_amount_uses_weatherhk_tiers() {
        assert_eq!(copy_amount_for_source_notional(9.99), 1.0);
        assert_eq!(copy_amount_for_source_notional(10.0), 2.0);
        assert_eq!(copy_amount_for_source_notional(29.99), 2.0);
        assert_eq!(copy_amount_for_source_notional(30.0), 3.0);
        assert_eq!(copy_amount_for_source_notional(60.0), 5.0);
        assert_eq!(copy_amount_for_source_notional(100.0), 8.0);
        assert_eq!(copy_amount_for_source_notional(200.0), 10.0);
    }

    #[test]
    fn chase_limits_use_pct_with_absolute_cap() {
        assert_near(price_with_capped_upside(0.064, 0.15, 0.03), 0.0736);
        assert_near(price_with_capped_upside(0.50, 0.15, 0.03), 0.53);
        assert_near(price_with_capped_downside(0.064, 0.15, 0.03), 0.0544);
        assert_near(price_with_capped_downside(0.50, 0.15, 0.03), 0.47);
    }

    #[test]
    fn low_edge_high_probability_buys_are_skipped() {
        assert!(!should_skip_low_edge_buy(0.979, 0.98));
        assert!(should_skip_low_edge_buy(0.98, 0.98));
        assert!(should_skip_low_edge_buy(0.99, 0.98));
    }

    #[test]
    fn near_zero_tail_buys_are_skipped() {
        assert!(should_skip_near_zero_buy(0.004, 0.005));
        assert!(should_skip_near_zero_buy(0.005, 0.005));
        assert!(!should_skip_near_zero_buy(0.006, 0.005));
        assert!(!should_skip_near_zero_buy(0.005, 0.0));
    }

    #[test]
    fn small_source_buys_do_not_trigger_copy() {
        assert!(should_skip_small_buy(0.99, 1.0));
        assert!(!should_skip_small_buy(1.0, 1.0));
        assert!(!should_skip_small_buy(0.01, 0.0));
    }

    #[test]
    fn dust_sells_do_not_trigger_sync() {
        assert!(should_skip_dust_sell(0.99, 1.0));
        assert!(!should_skip_dust_sell(1.0, 1.0));
        assert!(!should_skip_dust_sell(0.01, 0.0));
    }

    #[test]
    fn routine_pending_sync_does_not_notify() {
        let order = PendingCopyOrder {
            local_order_id: "local".to_owned(),
            external_order_id: Some("external".to_owned()),
            position_key: "market:asset".to_owned(),
            side: "BUY".to_owned(),
            market_title: None,
            outcome: None,
            asset: "asset".to_owned(),
            condition_id: "market".to_owned(),
            copy_amount_usd: 2.0,
            limit_price: 0.05,
            filled_amount_usd: 0.0,
            filled_size: 0.0,
            created_at_secs: 0,
            expires_at_secs: 0,
            last_sync_at_secs: 0,
            source_trade_key: "source".to_owned(),
        };
        let pending = ExecutionResult {
            status: ExecutionStatus::Pending,
            order_id: Some("external".to_owned()),
            order_price: Some(0.05),
            filled_amount_usd: None,
            filled_size: None,
            filled_price: None,
            realized_pnl_usd: None,
            message: Some("still pending".to_owned()),
        };
        let partial = ExecutionResult {
            filled_amount_usd: Some(0.5),
            ..pending.clone()
        };
        let cancelled = ExecutionResult {
            status: ExecutionStatus::Cancelled,
            ..pending.clone()
        };

        assert!(!should_report_pending_sync(&order, &pending));
        assert!(should_report_pending_sync(&order, &partial));
        assert!(should_report_pending_sync(&order, &cancelled));
    }

    #[test]
    fn missing_source_position_cancels_old_pending_buy_only() {
        let mut source_assets = HashSet::new();
        source_assets.insert("held".to_owned());
        let old_missing = PendingCopyOrder {
            local_order_id: "old-missing".to_owned(),
            external_order_id: None,
            position_key: "market:missing".to_owned(),
            side: "BUY".to_owned(),
            market_title: None,
            outcome: None,
            asset: "missing".to_owned(),
            condition_id: "market".to_owned(),
            copy_amount_usd: 1.0,
            limit_price: 0.05,
            filled_amount_usd: 0.0,
            filled_size: 0.0,
            created_at_secs: 100,
            expires_at_secs: 0,
            last_sync_at_secs: 100,
            source_trade_key: "source".to_owned(),
        };
        let new_missing = PendingCopyOrder {
            created_at_secs: 150,
            ..old_missing.clone()
        };
        let old_held = PendingCopyOrder {
            asset: "held".to_owned(),
            ..old_missing.clone()
        };

        assert!(should_cancel_pending_buy_absent_from_source_position(
            &old_missing,
            &source_assets,
            200
        ));
        assert!(!should_cancel_pending_buy_absent_from_source_position(
            &new_missing,
            &source_assets,
            200
        ));
        assert!(!should_cancel_pending_buy_absent_from_source_position(
            &old_held,
            &source_assets,
            200
        ));
    }

    #[test]
    fn zero_ttl_means_no_expiry() {
        assert_eq!(pending_expires_at(100, 0), 0);
        assert_eq!(pending_expires_at(100, 300), 400);
        assert_eq!(order_remaining_ttl(0), 0);
    }

    #[test]
    fn state_counts_pending_buy_as_reserved_budget() {
        let state = AutoCopyState {
            pending_orders: vec![PendingCopyOrder {
                local_order_id: "local".to_owned(),
                external_order_id: None,
                position_key: "market:asset".to_owned(),
                side: "BUY".to_owned(),
                market_title: None,
                outcome: None,
                asset: "asset".to_owned(),
                condition_id: "market".to_owned(),
                copy_amount_usd: 5.0,
                limit_price: 0.43,
                filled_amount_usd: 2.0,
                filled_size: 4.0,
                created_at_secs: 1,
                expires_at_secs: 2,
                last_sync_at_secs: 1,
                source_trade_key: "source".to_owned(),
            }],
            ..AutoCopyState::default()
        };

        assert!((state.daily_reserved_buy_usd() - 3.0).abs() < f64::EPSILON);
        assert!((state.market_exposure_usd("market:asset") - 3.0).abs() < f64::EPSILON);
    }

    fn assert_near(left: f64, right: f64) {
        assert!(
            (left - right).abs() < 0.000_000_1,
            "expected {left} to be near {right}"
        );
    }
}
