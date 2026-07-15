use crate::{
    autocopy::{AutoCopyConfig, AutoCopyEngine, ObservedSourcePosition},
    discovery::WalletEvaluation,
    polymarket::{PolymarketDataClient, UserTrade},
    profile::{
        build_employee_profile, CopySignalLevel, EmployeeProfile, StrategyArchetype,
        TradeProfileSignal,
    },
    telegram::TelegramNotifier,
};
use serde::Serialize;
use std::{
    collections::{HashMap, HashSet},
    io::{self, Write},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError},
        Arc,
    },
    thread::{self, sleep},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const AUTO_COPY_SOURCE_POSITION_RECONCILE_SECONDS: u64 = 30;
const AUTO_COPY_MAINTENANCE_STEP_SECONDS: u64 = 5;
const SOURCE_POSITION_POLL_SECONDS: u64 = 600;
const SOURCE_POSITION_MAX_AGE_SECONDS: u64 = 30;
const WATCH_EVENT_WAIT_MILLIS: u64 = 200;
const WATCH_METRICS_LOG_SECONDS: u64 = 60;
const TELEGRAM_QUEUE_CAPACITY: usize = 1_024;
const ACTIVITY_FAILURE_BACKOFF_FIRST_SECONDS: u64 = 3;
const ACTIVITY_FAILURE_BACKOFF_MAX_SECONDS: u64 = 5;
const ACTIVITY_DEGRADED_ALERT_FAILURES: u32 = 6;
const ACTIVITY_RECOVERY_PAGE_SIZE: usize = 100;
const ACTIVITY_RECOVERY_MAX_EXTRA_PAGES: usize = 3;
const OBSERVER_ALERT_COOLDOWN_SECONDS: u64 = 300;
const OBSERVER_ALERT_PRICE_MOVE: f64 = 0.03;

#[derive(Debug, Clone, Serialize)]
pub struct WatchedEmployee {
    pub wallet: String,
    pub name: Option<String>,
    pub domain: String,
    pub keywords: Vec<String>,
    pub poll_seconds: Option<u64>,
    pub min_notional_usd: Option<f64>,
}

impl WatchedEmployee {
    pub fn from_evaluation(evaluation: &WalletEvaluation) -> Self {
        Self {
            wallet: evaluation.wallet.clone(),
            name: evaluation.user_name.clone(),
            domain: evaluation.category.clone(),
            keywords: default_keywords_for_domain(&evaluation.category),
            poll_seconds: None,
            min_notional_usd: None,
        }
    }

    pub fn parse(spec: &str) -> Result<Self, String> {
        let parts = spec.split(':').collect::<Vec<_>>();
        let wallet = parts
            .first()
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
            .ok_or_else(|| "employee spec must start with a wallet".to_owned())?;
        let name = parts
            .get(1)
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
            .map(|value| value.to_owned());
        let domain = parts
            .get(2)
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
            .unwrap_or("CUSTOM")
            .to_uppercase();
        let keywords = parts
            .get(3)
            .map(|value| parse_keywords(value))
            .filter(|keywords| !keywords.is_empty())
            .unwrap_or_else(|| default_keywords_for_domain(&domain));
        let poll_seconds = parse_employee_poll_seconds(parts.get(4).copied())?;
        let min_notional_usd = parse_employee_min_notional(parts.get(5).copied())?;

        Ok(Self {
            wallet: wallet.to_owned(),
            name,
            domain,
            keywords,
            poll_seconds,
            min_notional_usd,
        })
    }

    pub fn label(&self) -> String {
        self.name
            .as_ref()
            .map(|name| format!("{name} ({})", self.wallet))
            .unwrap_or_else(|| self.wallet.clone())
    }

    pub fn poll_interval_seconds(&self, fallback_seconds: u64) -> u64 {
        self.poll_seconds
            .filter(|seconds| *seconds > 0)
            .unwrap_or_else(|| fallback_seconds.max(1))
    }

    pub fn min_notional_usd(&self, fallback_usd: f64) -> f64 {
        self.min_notional_usd
            .filter(|value| *value > 0.0)
            .unwrap_or(fallback_usd)
    }
}

#[derive(Debug, Clone)]
pub struct WatchRules {
    pub poll_seconds: u64,
    pub heartbeat_seconds: u64,
    pub trade_limit: usize,
    pub profile_trade_limit: usize,
    pub profile_closed_pages: usize,
    pub profile_closed_page_size: usize,
    pub profiles_enabled: bool,
    pub min_notional_usd: f64,
    pub max_entry_price: f64,
    pub follow_price_buffer: f64,
    pub auto_copy: Option<AutoCopyConfig>,
    pub iterations: Option<usize>,
}

impl Default for WatchRules {
    fn default() -> Self {
        Self {
            poll_seconds: 10,
            heartbeat_seconds: 3_600,
            trade_limit: 20,
            profile_trade_limit: 100,
            profile_closed_pages: 2,
            profile_closed_page_size: 50,
            profiles_enabled: true,
            min_notional_usd: 100.0,
            max_entry_price: 0.75,
            follow_price_buffer: 0.05,
            auto_copy: None,
            iterations: None,
        }
    }
}

pub struct WatchOutcome {
    pub polls_completed: usize,
    pub alerts_sent: usize,
    pub employees: usize,
    pub heartbeats_sent: usize,
    pub employee_polls_completed: usize,
}

#[derive(Default)]
struct ObserverAlertThrottle {
    last_sent: HashMap<String, ObserverAlertStamp>,
}

#[derive(Debug, Clone, Copy)]
struct ObserverAlertStamp {
    sent_at_secs: u64,
    price: f64,
}

#[derive(Debug, Clone)]
struct AggregatedActivityTrade {
    trade: UserTrade,
    fill_count: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct EmployeeActivity {
    pub wallet: String,
    pub name: Option<String>,
    pub domain: String,
    pub matched_buys: usize,
    pub matched_buys_7d: usize,
    pub matched_buys_30d: usize,
    pub last_matched_buy_age_days: Option<u64>,
    pub avg_gap_hours: Option<f64>,
    pub median_gap_hours: Option<f64>,
    pub frequency: FrequencyTier,
}

#[derive(Debug, Clone)]
pub struct EmployeeProfileSnapshot {
    pub profile: EmployeeProfile,
    pub trades: Vec<UserTrade>,
}

#[derive(Debug, Clone, Copy)]
enum SellAction {
    TakeProfit,
    StopLoss,
    Trim,
    Unknown,
}

impl SellAction {
    fn label(self) -> &'static str {
        match self {
            Self::TakeProfit => "止盈减仓",
            Self::StopLoss => "止损撤退",
            Self::Trim => "调仓减仓",
            Self::Unknown => "未知卖出",
        }
    }

    fn guidance(self) -> &'static str {
        match self {
            Self::TakeProfit => {
                "如果此前跟随该仓位，建议检查是否同步止盈；如果还没买，不建议在员工开始减仓后追高。"
            }
            Self::StopLoss => {
                "如果此前跟随该方向，应重新检查市场信息；员工止损时，原买入信号可能已经失效。"
            }
            Self::Trim => {
                "这更像仓位管理或部分减仓，建议观察后续是否继续卖出，再决定是否同步降低风险。"
            }
            Self::Unknown => {
                "无法确认该员工此前成本，可能是旧仓、做市或调仓；不要把这笔单独理解成明确反向信号。"
            }
        }
    }
}

#[derive(Debug, Clone)]
struct SellAnalysis {
    action: SellAction,
    avg_entry_price: Option<f64>,
    return_pct: Option<f64>,
    known_position_size: Option<f64>,
    sell_fraction: Option<f64>,
    level: CopySignalLevel,
    score: u8,
    reasons: Vec<String>,
    cautions: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum FrequencyTier {
    High,
    Medium,
    Low,
    Dormant,
}

pub fn analyze_employee_activity(
    client: &PolymarketDataClient,
    employees: &[WatchedEmployee],
    trade_limit: usize,
) -> Vec<EmployeeActivity> {
    let now = now_secs();

    employees
        .iter()
        .map(|employee| {
            let trades = client
                .activity(&employee.wallet, trade_limit, 0)
                .map(|trades| {
                    trades
                        .into_iter()
                        .filter(|trade| trade.side.eq_ignore_ascii_case("BUY"))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_else(|error| {
                    eprintln!("failed to load activity for {}: {error}", employee.label());
                    Vec::new()
                });
            analyze_employee_trades(employee, &trades, now)
        })
        .collect()
}

pub fn analyze_employee_trades(
    employee: &WatchedEmployee,
    trades: &[UserTrade],
    now_secs: u64,
) -> EmployeeActivity {
    let mut timestamps = trades
        .iter()
        .filter(|trade| trade.side.eq_ignore_ascii_case("BUY"))
        .filter(|trade| matches_keywords(employee, trade))
        .filter_map(|trade| trade.timestamp)
        .collect::<Vec<_>>();

    timestamps.sort_unstable();

    let matched_buys = timestamps.len();
    let matched_buys_7d = count_since(&timestamps, now_secs, 7);
    let matched_buys_30d = count_since(&timestamps, now_secs, 30);
    let last_matched_buy_age_days = timestamps
        .last()
        .map(|timestamp| now_secs.saturating_sub(*timestamp) / 86_400);
    let gaps = timestamps
        .windows(2)
        .map(|pair| pair[1].saturating_sub(pair[0]) as f64 / 3_600.0)
        .collect::<Vec<_>>();
    let avg_gap_hours = if gaps.is_empty() {
        None
    } else {
        Some(round2(gaps.iter().sum::<f64>() / gaps.len() as f64))
    };
    let median_gap_hours = median(gaps);
    let frequency = classify_frequency(matched_buys_7d, matched_buys_30d);

    EmployeeActivity {
        wallet: employee.wallet.clone(),
        name: employee.name.clone(),
        domain: employee.domain.clone(),
        matched_buys,
        matched_buys_7d,
        matched_buys_30d,
        last_matched_buy_age_days,
        avg_gap_hours,
        median_gap_hours,
        frequency,
    }
}

pub fn watch_employees(
    client: &PolymarketDataClient,
    employees: &[WatchedEmployee],
    rules: &WatchRules,
    telegram: Option<&TelegramNotifier>,
) -> WatchOutcome {
    let mut alerts_sent = 0;
    let mut heartbeats_sent = 0;
    let started_at = Instant::now();
    let mut next_heartbeat_at_secs =
        next_heartbeat_boundary_secs(now_secs(), rules.heartbeat_seconds);
    let mut last_alert_at: Option<u64> = None;
    let mut observer_alert_throttle = ObserverAlertThrottle::default();
    let max_polls = rules.iterations.unwrap_or(usize::MAX);
    let snapshots = load_employee_profile_snapshots(client, employees, rules);
    let profiles = snapshots
        .iter()
        .map(|snapshot| (snapshot.profile.wallet.clone(), snapshot.profile.clone()))
        .collect::<HashMap<_, _>>();
    let mut trade_histories = snapshots
        .into_iter()
        .map(|snapshot| (snapshot.profile.wallet.clone(), snapshot.trades))
        .collect::<HashMap<_, _>>();
    let mut auto_copy = rules
        .auto_copy
        .clone()
        .filter(|config| config.enabled)
        .and_then(|mut config| {
            if let Some(source_employee) = employees.iter().find(|employee| {
                employee
                    .wallet
                    .trim()
                    .eq_ignore_ascii_case(config.source_wallet.trim())
            }) {
                config.specialty_keywords = source_employee.keywords.clone();
            }
            let source_name = config.source_name.clone();
            match AutoCopyEngine::new(config) {
                Ok(engine) => {
                    println!(
                        "{} auto-copy enabled: mode={}, state={}",
                        engine.config().source_name,
                        engine.config().mode.label_for_display(),
                        engine.config().state_path.display()
                    );
                    Some(engine)
                }
                Err(error) => {
                    eprintln!(
                        "{} auto-copy disabled: failed to load state: {error}",
                        source_name
                    );
                    None
                }
            }
        });

    println!(
        "Watching {} employees, poll={}s, min_notional=${:.2}, max_entry={:.3}",
        employees.len(),
        rules.poll_seconds,
        rules.min_notional_usd,
        rules.max_entry_price
    );
    flush_stdout();

    let stop = Arc::new(AtomicBool::new(false));
    let poll_counters = Arc::new(ActivityPollCounters::default());
    let (activity_sender, activity_receiver) = mpsc::channel();
    let activity_thread = spawn_activity_poller(
        client.clone(),
        employees.to_vec(),
        rules.clone(),
        stop.clone(),
        poll_counters.clone(),
        activity_sender,
    );
    let telegram_sender = spawn_telegram_dispatcher(telegram);

    let (position_sender, position_receiver) = mpsc::channel();
    let source_position_thread = auto_copy.as_ref().map(|engine| {
        spawn_source_position_poller(
            client.clone(),
            engine.config().source_wallet.clone(),
            engine.config().source_name.clone(),
            stop.clone(),
            position_sender,
        )
    });

    let mut activity_finished = false;
    let mut latest_source_positions: Option<SourcePositionsSnapshot> = None;
    let mut last_auto_copy_position_reconcile: Option<Instant> = None;
    let mut last_maintenance_step: Option<Instant> = None;
    let mut has_received_activity = false;
    let mut last_metrics_log = Instant::now();
    let mut last_metrics_polls = 0;
    let mut last_metrics_employee_polls = 0;

    while !activity_finished {
        match activity_receiver.recv_timeout(Duration::from_millis(WATCH_EVENT_WAIT_MILLIS)) {
            Ok(ActivityPollMessage::Batch(batch)) => {
                process_activity_batch(
                    batch,
                    employees,
                    rules,
                    &profiles,
                    &mut trade_histories,
                    &mut auto_copy,
                    &mut observer_alert_throttle,
                    latest_source_positions.as_ref(),
                    telegram_sender.as_ref(),
                    &mut alerts_sent,
                    &mut last_alert_at,
                );
                has_received_activity = true;
            }
            Ok(ActivityPollMessage::Warning(message)) => {
                publish_message(&message, telegram_sender.as_ref());
                alerts_sent += 1;
                last_alert_at = Some(now_secs());
            }
            Ok(ActivityPollMessage::Finished) => activity_finished = true,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => activity_finished = true,
        }

        loop {
            match activity_receiver.try_recv() {
                Ok(ActivityPollMessage::Batch(batch)) => {
                    process_activity_batch(
                        batch,
                        employees,
                        rules,
                        &profiles,
                        &mut trade_histories,
                        &mut auto_copy,
                        &mut observer_alert_throttle,
                        latest_source_positions.as_ref(),
                        telegram_sender.as_ref(),
                        &mut alerts_sent,
                        &mut last_alert_at,
                    );
                    has_received_activity = true;
                }
                Ok(ActivityPollMessage::Warning(message)) => {
                    publish_message(&message, telegram_sender.as_ref());
                    alerts_sent += 1;
                    last_alert_at = Some(now_secs());
                }
                Ok(ActivityPollMessage::Finished) => {
                    activity_finished = true;
                    break;
                }
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            }
        }

        while let Ok(snapshot) = position_receiver.try_recv() {
            latest_source_positions = Some(snapshot);
        }

        let polls_completed = poll_counters.polls_completed.load(Ordering::Relaxed);
        let employee_polls_completed = poll_counters
            .employee_polls_completed
            .load(Ordering::Relaxed);
        if last_metrics_log.elapsed() >= Duration::from_secs(WATCH_METRICS_LOG_SECONDS) {
            println!(
                "Watcher hot-scan metrics: interval={}s polls={} employee_polls={} total_polls={} total_employee_polls={}",
                last_metrics_log.elapsed().as_secs(),
                polls_completed.saturating_sub(last_metrics_polls),
                employee_polls_completed.saturating_sub(last_metrics_employee_polls),
                polls_completed,
                employee_polls_completed
            );
            flush_stdout();
            last_metrics_log = Instant::now();
            last_metrics_polls = polls_completed;
            last_metrics_employee_polls = employee_polls_completed;
        }
        if should_send_heartbeat(
            rules,
            &mut next_heartbeat_at_secs,
            polls_completed,
            max_polls,
        ) {
            let heartbeat = build_heartbeat(
                employees,
                rules,
                polls_completed,
                employee_polls_completed,
                alerts_sent,
                started_at.elapsed().as_secs(),
                last_alert_at,
            );
            publish_message(&heartbeat, telegram_sender.as_ref());
            heartbeats_sent += 1;
        }

        let maintenance_due = has_received_activity
            && last_maintenance_step.map_or(true, |last| {
                last.elapsed() >= Duration::from_secs(AUTO_COPY_MAINTENANCE_STEP_SECONDS)
            });
        if maintenance_due {
            if let Some(engine) = auto_copy.as_mut() {
                let reconcile_due = engine.needs_source_position_reconcile()
                    && last_auto_copy_position_reconcile.map_or(true, |last| {
                        last.elapsed()
                            >= Duration::from_secs(AUTO_COPY_SOURCE_POSITION_RECONCILE_SECONDS)
                    });
                let reports = if reconcile_due {
                    latest_source_positions
                        .as_ref()
                        .filter(|snapshot| {
                            now_secs().saturating_sub(snapshot.observed_at_secs)
                                <= SOURCE_POSITION_MAX_AGE_SECONDS
                        })
                        .map(|snapshot| {
                            last_auto_copy_position_reconcile = Some(Instant::now());
                            engine.reconcile_absent_from_source_positions_step(&snapshot.positions)
                        })
                        .unwrap_or_default()
                } else {
                    engine.handle_maintenance_step()
                };
                for report in reports {
                    if report.should_notify() {
                        publish_message(&report.text, telegram_sender.as_ref());
                        alerts_sent += 1;
                        last_alert_at = Some(now_secs());
                    }
                }
            }
            last_maintenance_step = Some(Instant::now());
        }
    }

    stop.store(true, Ordering::Relaxed);
    let _ = activity_thread.join();
    drop(source_position_thread);

    WatchOutcome {
        polls_completed: poll_counters.polls_completed.load(Ordering::Relaxed),
        alerts_sent,
        employees: employees.len(),
        heartbeats_sent,
        employee_polls_completed: poll_counters
            .employee_polls_completed
            .load(Ordering::Relaxed),
    }
}

#[derive(Default)]
struct ActivityPollCounters {
    polls_completed: AtomicUsize,
    employee_polls_completed: AtomicUsize,
}

struct ActivityBatch {
    employee_index: usize,
    trades: Vec<UserTrade>,
    seed_only: bool,
    observed_at_secs: u64,
    request_elapsed_millis: u128,
}

enum ActivityPollMessage {
    Batch(ActivityBatch),
    Warning(String),
    Finished,
}

struct SourcePositionsSnapshot {
    positions: HashMap<String, ObservedSourcePosition>,
    observed_at_secs: u64,
}

#[derive(Default)]
struct ActivityRecoveryStats {
    pages_loaded: usize,
    extra_trades: usize,
    failed_page_offset: Option<usize>,
    failure_message: Option<String>,
}

fn spawn_activity_poller(
    client: PolymarketDataClient,
    employees: Vec<WatchedEmployee>,
    rules: WatchRules,
    stop: Arc<AtomicBool>,
    counters: Arc<ActivityPollCounters>,
    sender: mpsc::Sender<ActivityPollMessage>,
) -> thread::JoinHandle<()> {
    thread::Builder::new()
        .name("polymarket-activity-poller".to_owned())
        .spawn(move || {
            let mut seen = HashSet::new();
            let mut last_polled = vec![None; employees.len()];
            let mut activity_failures = vec![0_u32; employees.len()];
            let mut activity_degraded_alerted = vec![false; employees.len()];
            let mut seeded = vec![false; employees.len()];
            let max_polls = rules.iterations.unwrap_or(usize::MAX);

            while !stop.load(Ordering::Relaxed)
                && counters.polls_completed.load(Ordering::Relaxed) < max_polls
            {
                let loop_started = Instant::now();
                counters.polls_completed.fetch_add(1, Ordering::Relaxed);

                for (index, employee) in employees.iter().enumerate() {
                    if stop.load(Ordering::Relaxed) {
                        break;
                    }
                    if !employee_is_due(
                        employee,
                        &rules,
                        last_polled[index],
                        activity_failures[index],
                    ) {
                        continue;
                    }

                    counters
                        .employee_polls_completed
                        .fetch_add(1, Ordering::Relaxed);
                    let poll_started = Instant::now();
                    let (trades, recovery_stats) =
                        match client.activity(&employee.wallet, rules.trade_limit, 0) {
                            Ok(mut trades) => {
                                let failures_before_recovery = activity_failures[index];
                                let recovery_stats = if failures_before_recovery > 0 && seeded[index]
                                {
                                    let (recovered, stats) = load_activity_recovery_pages(
                                        &client,
                                        employee,
                                        &rules,
                                        failures_before_recovery,
                                    );
                                    trades.extend(recovered);
                                    Some(stats)
                                } else {
                                    None
                                };
                            last_polled[index] = Some(poll_started);
                            if activity_failures[index] > 0 && activity_degraded_alerted[index] {
                                let recovery_line =
                                    format_activity_recovery_line(recovery_stats.as_ref());
                                let _ = sender.send(ActivityPollMessage::Warning(format!(
                                    "[{} 数据轮询恢复]\n状态: recovered\n原因: /activity 已恢复，上一轮连续失败 {} 次；跟单检测恢复正常轮询。{}",
                                    employee.label(),
                                    activity_failures[index],
                                    recovery_line
                                )));
                            }
                            activity_failures[index] = 0;
                            activity_degraded_alerted[index] = false;
                            (trades, recovery_stats)
                        }
                        Err(error) => {
                            last_polled[index] = Some(poll_started);
                            activity_failures[index] = activity_failures[index].saturating_add(1);
                            let backoff_seconds = effective_employee_poll_interval_seconds(
                                employee,
                                &rules,
                                activity_failures[index],
                            );
                            eprintln!(
                                "failed to load trades for {}: {error}; backing off activity poll to {}s after {} consecutive failure(s)",
                                employee.label(),
                                backoff_seconds,
                                activity_failures[index]
                            );
                            if activity_failures[index] >= ACTIVITY_DEGRADED_ALERT_FAILURES
                                && !activity_degraded_alerted[index]
                            {
                                let _ = sender.send(ActivityPollMessage::Warning(format!(
                                    "[{} 数据轮询降级]\n状态: degraded\n原因: /activity 连续失败 {} 次，当前退避到 {} 秒；这期间 BUY/SELL 跟随会明显延迟，直到数据接口恢复。\n执行器: {error}",
                                    employee.label(),
                                    activity_failures[index],
                                    backoff_seconds
                                )));
                                activity_degraded_alerted[index] = true;
                            }
                            continue;
                        }
                    };

                    let seed_only = !seeded[index];
                    let mut unseen = trades
                        .into_iter()
                        .filter(|trade| seen.insert(trade_key(trade)))
                        .collect::<Vec<_>>();
                    unseen.sort_by_key(|trade| trade.timestamp.unwrap_or(0));
                    if let Some(stats) = recovery_stats.as_ref() {
                        if stats.pages_loaded > 0 || stats.failed_page_offset.is_some() {
                            println!(
                                "{} activity recovery: pages={} extra_trades={} failed_offset={:?}",
                                employee.label(),
                                stats.pages_loaded,
                                stats.extra_trades,
                                stats.failed_page_offset
                            );
                            flush_stdout();
                        }
                    }
                    seeded[index] = true;

                    if (seed_only || !unseen.is_empty())
                        && sender
                            .send(ActivityPollMessage::Batch(ActivityBatch {
                                employee_index: index,
                                trades: unseen,
                                seed_only,
                                observed_at_secs: now_secs(),
                                request_elapsed_millis: poll_started.elapsed().as_millis(),
                            }))
                            .is_err()
                    {
                        return;
                    }
                }

                if counters.polls_completed.load(Ordering::Relaxed) < max_polls {
                    let target_interval = Duration::from_secs(rules.poll_seconds.max(1));
                    if let Some(remaining) = target_interval.checked_sub(loop_started.elapsed()) {
                        sleep(remaining);
                    }
                }
            }

            let _ = sender.send(ActivityPollMessage::Finished);
        })
        .expect("failed to start activity polling thread")
}

fn spawn_source_position_poller(
    client: PolymarketDataClient,
    wallet: String,
    source_name: String,
    stop: Arc<AtomicBool>,
    sender: mpsc::Sender<SourcePositionsSnapshot>,
) -> thread::JoinHandle<()> {
    thread::Builder::new()
        .name("weatherhk-position-poller".to_owned())
        .spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                match load_source_positions(&client, &wallet) {
                    Ok(positions) => {
                        if sender
                            .send(SourcePositionsSnapshot {
                                positions,
                                observed_at_secs: now_secs(),
                            })
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(error) => {
                        eprintln!("failed to refresh {source_name} positions cache: {error}")
                    }
                }

                for _ in 0..SOURCE_POSITION_POLL_SECONDS * 10 {
                    if stop.load(Ordering::Relaxed) {
                        return;
                    }
                    sleep(Duration::from_millis(100));
                }
            }
        })
        .expect("failed to start source position polling thread")
}

#[allow(clippy::too_many_arguments)]
fn process_activity_batch(
    batch: ActivityBatch,
    employees: &[WatchedEmployee],
    rules: &WatchRules,
    profiles: &HashMap<String, EmployeeProfile>,
    trade_histories: &mut HashMap<String, Vec<UserTrade>>,
    auto_copy: &mut Option<AutoCopyEngine>,
    observer_alert_throttle: &mut ObserverAlertThrottle,
    latest_source_positions: Option<&SourcePositionsSnapshot>,
    telegram_sender: Option<&SyncSender<String>>,
    alerts_sent: &mut usize,
    last_alert_at: &mut Option<u64>,
) {
    let Some(employee) = employees.get(batch.employee_index) else {
        return;
    };
    let seed_only = batch.seed_only;
    let observed_at_secs = batch.observed_at_secs;
    let request_elapsed_millis = batch.request_elapsed_millis;

    let is_auto_copy_source = auto_copy.as_ref().is_some_and(|engine| {
        employee
            .wallet
            .eq_ignore_ascii_case(&engine.config().source_wallet)
    });
    let trades = if is_auto_copy_source {
        batch
            .trades
            .into_iter()
            .map(|trade| AggregatedActivityTrade {
                trade,
                fill_count: 1,
            })
            .collect()
    } else {
        aggregate_activity_trades(batch.trades)
    };

    for aggregated in trades {
        let trade = aggregated.trade;
        let history = trade_histories.entry(employee.wallet.clone()).or_default();
        let processing_started = Instant::now();
        let processing_started_at_secs = now_secs();

        if seed_only {
            if let Some(engine) = auto_copy.as_mut() {
                if engine.should_backfill_startup_trade(
                    employee,
                    &trade,
                    processing_started_at_secs,
                ) {
                    let reports = engine.handle_trade(employee, &trade);
                    publish_timed_reports(
                        reports,
                        &trade,
                        observed_at_secs,
                        request_elapsed_millis,
                        processing_started,
                        processing_started_at_secs,
                        telegram_sender,
                        alerts_sent,
                        last_alert_at,
                    );
                }
            }
        } else {
            let mut auto_copy_handled = false;
            if let Some(engine) = auto_copy.as_mut() {
                let source_positions = if trade.side.eq_ignore_ascii_case("SELL")
                    && employee
                        .wallet
                        .eq_ignore_ascii_case(&engine.config().source_wallet)
                {
                    fresh_source_positions_for_trade(latest_source_positions, &trade)
                } else {
                    None
                };
                let reports =
                    engine.handle_trade_with_source_positions(employee, &trade, source_positions);
                auto_copy_handled = !reports.is_empty();
                publish_timed_reports(
                    reports,
                    &trade,
                    observed_at_secs,
                    request_elapsed_millis,
                    processing_started,
                    processing_started_at_secs,
                    telegram_sender,
                    alerts_sent,
                    last_alert_at,
                );
            }

            if auto_copy_handled {
                remember_trade(
                    history,
                    trade,
                    rules.profile_trade_limit.max(rules.trade_limit),
                );
                continue;
            }

            let profile = profiles.get(&employee.wallet);
            let alert = if trade.side.eq_ignore_ascii_case("SELL") {
                build_sell_alert_with_fill_count(
                    employee,
                    &trade,
                    rules,
                    profile,
                    history,
                    aggregated.fill_count,
                )
            } else {
                build_alert_with_fill_count(employee, &trade, rules, profile, aggregated.fill_count)
            };

            if let Some(alert) = alert
                .filter(|_| observer_alert_throttle.should_publish(employee, &trade, now_secs()))
            {
                publish_message(&alert, telegram_sender);
                *alerts_sent += 1;
                *last_alert_at = Some(now_secs());
            }
        }

        remember_trade(
            history,
            trade,
            rules.profile_trade_limit.max(rules.trade_limit),
        );
    }
}

impl ObserverAlertThrottle {
    fn should_publish(&mut self, employee: &WatchedEmployee, trade: &UserTrade, now: u64) -> bool {
        let Some(price) = trade.price else {
            return true;
        };
        let key = format!(
            "{}:{}:{}",
            employee.wallet.to_lowercase(),
            trade.side.to_uppercase(),
            trade_position_key(trade)
        );
        let should_publish = self.last_sent.get(&key).map_or(true, |last| {
            now.saturating_sub(last.sent_at_secs) >= OBSERVER_ALERT_COOLDOWN_SECONDS
                || (price - last.price).abs() >= OBSERVER_ALERT_PRICE_MOVE
        });
        if should_publish {
            self.last_sent.insert(
                key,
                ObserverAlertStamp {
                    sent_at_secs: now,
                    price,
                },
            );
        }
        should_publish
    }
}

fn aggregate_activity_trades(trades: Vec<UserTrade>) -> Vec<AggregatedActivityTrade> {
    let mut aggregated = Vec::<AggregatedActivityTrade>::new();
    let mut indexes = HashMap::<String, usize>::new();

    for trade in trades {
        let key = format!(
            "{}:{}",
            trade.side.to_uppercase(),
            trade_position_key(&trade)
        );
        if let Some(index) = indexes.get(&key).copied() {
            merge_activity_trade(&mut aggregated[index], trade);
        } else {
            indexes.insert(key, aggregated.len());
            aggregated.push(AggregatedActivityTrade {
                trade,
                fill_count: 1,
            });
        }
    }

    aggregated.sort_by_key(|item| item.trade.timestamp.unwrap_or(0));
    aggregated
}

fn merge_activity_trade(target: &mut AggregatedActivityTrade, incoming: UserTrade) {
    let target_size = target.trade.size.unwrap_or(0.0).max(0.0);
    let incoming_size = incoming.size.unwrap_or(0.0).max(0.0);
    let combined_size = target_size + incoming_size;
    if combined_size > 0.0 {
        let weighted_price = (target.trade.price.unwrap_or(0.0) * target_size
            + incoming.price.unwrap_or(0.0) * incoming_size)
            / combined_size;
        target.trade.size = Some(combined_size);
        target.trade.price = Some(weighted_price);
    }
    if incoming.timestamp.unwrap_or(0) >= target.trade.timestamp.unwrap_or(0) {
        target.trade.timestamp = incoming.timestamp;
        target.trade.transaction_hash = incoming.transaction_hash;
    }
    target.fill_count += 1;
}

fn fresh_source_positions_for_trade<'a>(
    snapshot: Option<&'a SourcePositionsSnapshot>,
    trade: &UserTrade,
) -> Option<&'a HashMap<String, ObservedSourcePosition>> {
    let snapshot = snapshot?;
    let trade_timestamp = trade.timestamp?;
    let now = now_secs();
    if snapshot.observed_at_secs < trade_timestamp
        || now.saturating_sub(snapshot.observed_at_secs) > SOURCE_POSITION_MAX_AGE_SECONDS
    {
        return None;
    }
    Some(&snapshot.positions)
}

fn publish_timed_reports(
    reports: Vec<crate::autocopy::AutoCopyReport>,
    trade: &UserTrade,
    observed_at_secs: u64,
    request_elapsed_millis: u128,
    processing_started: Instant,
    processing_started_at_secs: u64,
    telegram_sender: Option<&SyncSender<String>>,
    alerts_sent: &mut usize,
    last_alert_at: &mut Option<u64>,
) {
    let discovery_delay = trade
        .timestamp
        .map(|timestamp| observed_at_secs.saturating_sub(timestamp))
        .unwrap_or(0);
    let queue_delay = processing_started_at_secs.saturating_sub(observed_at_secs);
    let execution_millis = processing_started.elapsed().as_millis();

    for mut report in reports {
        if !report.should_notify() {
            continue;
        }
        report.text.push_str(&format!(
            "\n延迟拆分: API/轮询发现 {}秒, 本地排队 {}秒, 执行 {}ms, /activity 请求 {}ms",
            discovery_delay, queue_delay, execution_millis, request_elapsed_millis
        ));
        publish_message(&report.text, telegram_sender);
        *alerts_sent += 1;
        *last_alert_at = Some(now_secs());
    }
}

fn spawn_telegram_dispatcher(telegram: Option<&TelegramNotifier>) -> Option<SyncSender<String>> {
    let notifier = telegram.cloned()?;
    let (sender, receiver) = mpsc::sync_channel(TELEGRAM_QUEUE_CAPACITY);
    thread::Builder::new()
        .name("telegram-dispatcher".to_owned())
        .spawn(move || send_telegram_messages(notifier, receiver))
        .expect("failed to start Telegram dispatcher thread");
    Some(sender)
}

fn send_telegram_messages(notifier: TelegramNotifier, receiver: Receiver<String>) {
    while let Ok(message) = receiver.recv() {
        if let Err(error) = notifier.send_message(&message) {
            eprintln!("failed to send Telegram update: {error}");
        }
    }
}

fn publish_message(message: &str, telegram_sender: Option<&SyncSender<String>>) {
    println!("{message}");
    flush_stdout();
    let Some(sender) = telegram_sender else {
        return;
    };
    match sender.try_send(message.to_owned()) {
        Ok(()) => {}
        Err(TrySendError::Full(_)) => {
            eprintln!("Telegram queue is full; dropped one notification without blocking trading")
        }
        Err(TrySendError::Disconnected(_)) => {
            eprintln!("Telegram dispatcher stopped; notification was not sent")
        }
    }
}

pub fn load_employee_profiles(
    client: &PolymarketDataClient,
    employees: &[WatchedEmployee],
    rules: &WatchRules,
) -> Vec<EmployeeProfile> {
    load_employee_profile_snapshots(client, employees, rules)
        .into_iter()
        .map(|snapshot| snapshot.profile)
        .collect()
}

pub fn load_employee_profile_snapshots(
    client: &PolymarketDataClient,
    employees: &[WatchedEmployee],
    rules: &WatchRules,
) -> Vec<EmployeeProfileSnapshot> {
    if !rules.profiles_enabled {
        return Vec::new();
    }

    employees
        .iter()
        .filter_map(|employee| {
            let trades = match client.trades(&employee.wallet, rules.profile_trade_limit, 0, None) {
                Ok(trades) => trades,
                Err(error) => {
                    eprintln!(
                        "failed to load profile trades for {}: {error}",
                        employee.label()
                    );
                    Vec::new()
                }
            };
            let closed_positions = load_profile_closed_positions(client, employee, rules);

            if trades.is_empty() && closed_positions.is_empty() {
                eprintln!(
                    "profile unavailable for {}: no trades or closed positions",
                    employee.label()
                );
                return None;
            }

            let profile = build_employee_profile(employee, &trades, &closed_positions, now_secs());
            println!(
                "Profile {} score={} strategies={} closed={} matched_trades={} suspected_mm={}",
                employee.label(),
                profile.copy_trade_score,
                profile.strategy_labels().join(","),
                profile.closed_positions,
                profile.matched_trades,
                profile.suspected_market_making,
            );
            flush_stdout();

            Some(EmployeeProfileSnapshot { profile, trades })
        })
        .collect()
}

fn load_profile_closed_positions(
    client: &PolymarketDataClient,
    employee: &WatchedEmployee,
    rules: &WatchRules,
) -> Vec<crate::polymarket::ClosedPosition> {
    let mut positions = Vec::new();

    for page in 0..rules.profile_closed_pages {
        let offset = page * rules.profile_closed_page_size;
        let page_positions =
            match client.closed_positions(&employee.wallet, rules.profile_closed_page_size, offset)
            {
                Ok(page_positions) => page_positions,
                Err(error) => {
                    eprintln!(
                        "failed to load profile closed positions for {}: {error}",
                        employee.label()
                    );
                    break;
                }
            };
        let page_len = page_positions.len();
        positions.extend(page_positions);

        if page_len < rules.profile_closed_page_size {
            break;
        }
    }

    positions
}

fn flush_stdout() {
    let _ = io::stdout().flush();
}

fn should_send_heartbeat(
    rules: &WatchRules,
    next_heartbeat_at_secs: &mut u64,
    polls_completed: usize,
    max_polls: usize,
) -> bool {
    if rules.heartbeat_seconds == 0 {
        return false;
    }

    if polls_completed == 1 && max_polls == 1 {
        return false;
    }

    let current_time_secs = now_secs();
    if current_time_secs >= *next_heartbeat_at_secs {
        *next_heartbeat_at_secs =
            next_heartbeat_boundary_secs(current_time_secs, rules.heartbeat_seconds);
        true
    } else {
        false
    }
}

fn load_source_positions(
    client: &PolymarketDataClient,
    wallet: &str,
) -> Result<HashMap<String, ObservedSourcePosition>, String> {
    let mut positions_by_asset = HashMap::new();
    let today = shanghai_date_yyyy_mm_dd(now_secs());

    for offset in [0, 50, 100, 150] {
        let positions = client
            .positions_fast(wallet, 50, offset)
            .map_err(|error| error.to_string())?;
        let count = positions.len();

        for position in positions {
            if position.redeemable == Some(true) {
                continue;
            }
            if source_position_ended_before(&position.end_date, &today) {
                continue;
            }
            let size = position.size.unwrap_or(0.0);
            if size <= 0.0 {
                continue;
            }

            if let Some(asset) = position.asset.filter(|asset| !asset.trim().is_empty()) {
                positions_by_asset.insert(
                    asset,
                    ObservedSourcePosition {
                        size_shares: size,
                        avg_price: position.avg_price,
                        current_price: position.cur_price,
                        end_date: position.end_date,
                        condition_id: position.condition_id,
                        market_title: position.title,
                        outcome: position.outcome,
                        slug: position.slug,
                        event_slug: position.event_slug,
                    },
                );
            }
        }

        if count < 50 {
            break;
        }
    }

    Ok(positions_by_asset)
}

fn source_position_ended_before(end_date: &Option<String>, today: &str) -> bool {
    end_date
        .as_deref()
        .and_then(iso_date_prefix)
        .is_some_and(|date| date < today)
}

fn iso_date_prefix(value: &str) -> Option<&str> {
    let prefix = value.get(..10)?;
    let bytes = prefix.as_bytes();
    (bytes.len() == 10
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| index == 4 || index == 7 || byte.is_ascii_digit()))
    .then_some(prefix)
}

fn shanghai_date_yyyy_mm_dd(now_secs: u64) -> String {
    let days = now_secs.saturating_add(8 * 3_600) / 86_400;
    let z = days as i64 + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

fn next_heartbeat_boundary_secs(now_secs: u64, interval_secs: u64) -> u64 {
    if interval_secs == 0 {
        u64::MAX
    } else {
        let interval_secs = interval_secs.max(1);
        ((now_secs / interval_secs) + 1) * interval_secs
    }
}

fn build_heartbeat(
    employees: &[WatchedEmployee],
    rules: &WatchRules,
    polls_completed: usize,
    employee_polls_completed: usize,
    alerts_sent: usize,
    uptime_seconds: u64,
    last_alert_at: Option<u64>,
) -> String {
    let last_alert = last_alert_at
        .map(|timestamp| format!("{}s ago", now_secs().saturating_sub(timestamp)))
        .unwrap_or_else(|| "none".to_owned());
    let domains = employees
        .iter()
        .map(|employee| employee.domain.as_str())
        .collect::<HashSet<_>>()
        .len();

    format!(
        "Polymarket watcher heartbeat\n\
status: running\n\
uptime: {}\n\
employees: {}\n\
domains: {}\n\
polls: {}\n\
employee_polls: {}\n\
alerts: {}\n\
last_alert: {}\n\
rules: poll={}s, heartbeat={}s, min_notional=${:.2}, max_entry={:.3}\n\
note: no action is normal when no new employee BUY/SELL passes the filters.",
        format_duration(uptime_seconds),
        employees.len(),
        domains,
        polls_completed,
        employee_polls_completed,
        alerts_sent,
        last_alert,
        rules.poll_seconds,
        rules.heartbeat_seconds,
        rules.min_notional_usd,
        rules.max_entry_price,
    )
}

fn employee_is_due(
    employee: &WatchedEmployee,
    rules: &WatchRules,
    last_polled: Option<Instant>,
    activity_failures: u32,
) -> bool {
    match last_polled {
        Some(last_polled) => {
            last_polled.elapsed().as_secs()
                >= effective_employee_poll_interval_seconds(employee, rules, activity_failures)
        }
        None => true,
    }
}

fn effective_employee_poll_interval_seconds(
    employee: &WatchedEmployee,
    rules: &WatchRules,
    activity_failures: u32,
) -> u64 {
    let base_seconds = employee.poll_interval_seconds(rules.poll_seconds);
    match activity_failures {
        0 => base_seconds,
        1 => base_seconds.max(ACTIVITY_FAILURE_BACKOFF_FIRST_SECONDS),
        _ => base_seconds.max(ACTIVITY_FAILURE_BACKOFF_MAX_SECONDS),
    }
}

fn activity_recovery_extra_pages(activity_failures: u32) -> usize {
    match activity_failures {
        0 => 0,
        1..=2 => 1,
        3..=5 => 2,
        _ => ACTIVITY_RECOVERY_MAX_EXTRA_PAGES,
    }
}

fn activity_first_page_limit(trade_limit: usize) -> usize {
    trade_limit.clamp(1, 100)
}

fn load_activity_recovery_pages(
    client: &PolymarketDataClient,
    employee: &WatchedEmployee,
    rules: &WatchRules,
    activity_failures: u32,
) -> (Vec<UserTrade>, ActivityRecoveryStats) {
    let mut recovered = Vec::new();
    let mut stats = ActivityRecoveryStats::default();
    let extra_pages = activity_recovery_extra_pages(activity_failures);
    if extra_pages == 0 {
        return (recovered, stats);
    }

    let page_size = ACTIVITY_RECOVERY_PAGE_SIZE;
    let mut offset = activity_first_page_limit(rules.trade_limit);
    for _ in 0..extra_pages {
        match client.activity(&employee.wallet, page_size, offset) {
            Ok(mut trades) => {
                let count = trades.len();
                stats.pages_loaded += 1;
                stats.extra_trades += count;
                recovered.append(&mut trades);
                if count < page_size {
                    break;
                }
                offset = offset.saturating_add(page_size);
            }
            Err(error) => {
                stats.failed_page_offset = Some(offset);
                stats.failure_message = Some(error.to_string());
                break;
            }
        }
    }

    (recovered, stats)
}

fn format_activity_recovery_line(stats: Option<&ActivityRecoveryStats>) -> String {
    let Some(stats) = stats else {
        return String::new();
    };

    let mut line = format!(
        "\n补偿: 已补拉 {} 页，额外获取 {} 笔历史成交。",
        stats.pages_loaded, stats.extra_trades
    );
    if let Some(offset) = stats.failed_page_offset {
        line.push_str(&format!(
            "\n补偿异常: offset={} 后续页补拉失败: {}",
            offset,
            stats
                .failure_message
                .as_deref()
                .unwrap_or("unknown recovery failure")
        ));
    }
    line
}

fn format_duration(seconds: u64) -> String {
    let hours = seconds / 3_600;
    let minutes = (seconds % 3_600) / 60;
    let secs = seconds % 60;

    format!("{hours}h {minutes}m {secs}s")
}

fn fill_aggregation_line(fill_count: usize) -> String {
    if fill_count <= 1 {
        String::new()
    } else {
        format!("成交聚合: 本轮合并 {fill_count} 笔同 market/outcome 碎片成交\n")
    }
}

pub fn build_alert(
    employee: &WatchedEmployee,
    trade: &UserTrade,
    rules: &WatchRules,
    profile: Option<&EmployeeProfile>,
) -> Option<String> {
    build_alert_with_fill_count(employee, trade, rules, profile, 1)
}

fn build_alert_with_fill_count(
    employee: &WatchedEmployee,
    trade: &UserTrade,
    rules: &WatchRules,
    profile: Option<&EmployeeProfile>,
    fill_count: usize,
) -> Option<String> {
    if !trade.side.eq_ignore_ascii_case("BUY") {
        return None;
    }

    if !matches_keywords(employee, trade) {
        return None;
    }

    let price = trade.price?;
    let size = trade.size?;
    let notional = price * size;
    let preferred_min_notional_usd = employee.min_notional_usd(rules.min_notional_usd);
    let below_preferred_notional = notional < preferred_min_notional_usd;

    if price > rules.max_entry_price || notional < buy_dust_floor_usd() {
        return None;
    }

    let title = trade.title.as_deref().unwrap_or("-");
    let slug = trade.slug.as_deref().unwrap_or("-");
    let outcome = trade.outcome.as_deref().unwrap_or("-");
    let tx = trade.transaction_hash.as_deref().unwrap_or("-");
    let timestamp = trade.timestamp.unwrap_or_default();
    let age_seconds = now_secs().saturating_sub(timestamp);
    let max_follow_price = (price + rules.follow_price_buffer).min(0.99);
    let market_url = market_url(trade);
    let mut profile_signal = profile
        .map(|profile| profile.analyze_trade(trade))
        .unwrap_or_else(TradeProfileSignal::without_profile);

    if below_preferred_notional {
        profile_signal.cautions.push(format!(
            "金额 ${:.2} 低于参考阈值 ${:.2}，但已命中员工专业领域；按试探仓保留提醒，不再硬过滤。",
            notional, preferred_min_notional_usd
        ));
    }
    let profile_summary = profile
        .map(profile_summary)
        .unwrap_or_else(|| "策略画像: 暂无历史画像".to_owned());
    let analysis_lines = format_signal_lines(&profile_signal.reasons);
    let caution_lines = format_signal_lines(&profile_signal.cautions);
    let aggregation_line = fill_aggregation_line(fill_count);

    Some(format!(
        "Polymarket 跟单提醒 [{}]\n\
一句话: {} 买入【{}】, 押注这个问题的答案是【{}】。\n\
问题: {}\n\
他买: {} @ {:.2}c / 隐含概率 {:.1}%\n\
可跟上限: {:.2}c\n\
金额: ${:.2} (size {:.4})\n\
{}\
距今: {}s\n\
链接: {}\n\
\n\
画像评分: {}/100\n\
{}\n\
分析:\n\
{}\n\
风险:\n\
{}\n\
跟单提示: {}\n\
\n\
员工: {}\n\
领域: {}\n\
钱包: {}\n\
slug: {}\n\
tx: {}\n\
\n\
手动检查: 你要跟的话就是买同一个 outcome【{}】；下单前确认当前卖一价、盘口深度、市场规则和价格漂移。",
        profile_signal.level.label(),
        employee.name.as_deref().unwrap_or("-"),
        outcome,
        outcome,
        title,
        outcome,
        price * 100.0,
        price * 100.0,
        max_follow_price * 100.0,
        notional,
        size,
        aggregation_line,
        age_seconds,
        market_url,
        profile_signal.score,
        profile_summary,
        analysis_lines,
        caution_lines,
        profile_signal.level.guidance(),
        employee.name.as_deref().unwrap_or("-"),
        employee.domain,
        employee.wallet,
        slug,
        tx,
        outcome,
    ))
}

pub fn build_sell_alert(
    employee: &WatchedEmployee,
    trade: &UserTrade,
    rules: &WatchRules,
    profile: Option<&EmployeeProfile>,
    history: &[UserTrade],
) -> Option<String> {
    build_sell_alert_with_fill_count(employee, trade, rules, profile, history, 1)
}

fn build_sell_alert_with_fill_count(
    employee: &WatchedEmployee,
    trade: &UserTrade,
    rules: &WatchRules,
    profile: Option<&EmployeeProfile>,
    history: &[UserTrade],
    fill_count: usize,
) -> Option<String> {
    if !trade.side.eq_ignore_ascii_case("SELL") {
        return None;
    }

    if !matches_keywords(employee, trade) {
        return None;
    }

    let price = trade.price?;
    let size = trade.size?;
    let notional = price * size;
    let min_notional_usd = (employee.min_notional_usd(rules.min_notional_usd) * 0.5).max(5.0);

    if notional < min_notional_usd {
        return None;
    }

    let title = trade.title.as_deref().unwrap_or("-");
    let slug = trade.slug.as_deref().unwrap_or("-");
    let outcome = trade.outcome.as_deref().unwrap_or("-");
    let tx = trade.transaction_hash.as_deref().unwrap_or("-");
    let timestamp = trade.timestamp.unwrap_or_default();
    let age_seconds = now_secs().saturating_sub(timestamp);
    let market_url = market_url(trade);
    let analysis = analyze_sell_trade(profile, history, trade);
    let profile_summary = profile
        .map(profile_summary)
        .unwrap_or_else(|| "策略画像: 暂无历史画像".to_owned());
    let analysis_lines = format_signal_lines(&analysis.reasons);
    let caution_lines = format_signal_lines(&analysis.cautions);
    let aggregation_line = fill_aggregation_line(fill_count);

    Some(format!(
        "Polymarket 员工卖出提醒 [{} / {}]\n\
一句话: {} 卖出【{}】仓位。\n\
问题: {}\n\
他卖: {} @ {:.2}c / 隐含概率 {:.1}%\n\
卖出金额: ${:.2} (size {:.4})\n\
{}\
估算成本: {}\n\
估算收益: {}\n\
卖出比例: {}\n\
距今: {}s\n\
链接: {}\n\
\n\
卖出评分: {}/100\n\
{}\n\
分析:\n\
{}\n\
风险:\n\
{}\n\
跟单提示: {}\n\
\n\
员工: {}\n\
领域: {}\n\
钱包: {}\n\
slug: {}\n\
tx: {}\n\
\n\
手动检查: 如果此前跟了同一个 outcome【{}】，现在应重点检查是否同步止盈/止损、盘口深度和该员工是否继续卖出。",
        analysis.level.label(),
        analysis.action.label(),
        employee.name.as_deref().unwrap_or("-"),
        outcome,
        title,
        outcome,
        price * 100.0,
        price * 100.0,
        notional,
        size,
        aggregation_line,
        format_optional_price(analysis.avg_entry_price),
        format_optional_pct(analysis.return_pct),
        format_sell_fraction(analysis.sell_fraction, analysis.known_position_size),
        age_seconds,
        market_url,
        analysis.score,
        profile_summary,
        analysis_lines,
        caution_lines,
        analysis.action.guidance(),
        employee.name.as_deref().unwrap_or("-"),
        employee.domain,
        employee.wallet,
        slug,
        tx,
        outcome,
    ))
}

fn analyze_sell_trade(
    profile: Option<&EmployeeProfile>,
    history: &[UserTrade],
    sell: &UserTrade,
) -> SellAnalysis {
    let price = sell.price.unwrap_or(0.0);
    let size = sell.size.unwrap_or(0.0);
    let (known_position_size, known_position_cost) = estimate_known_position(history, sell);
    let avg_entry_price = if known_position_size > 0.0 {
        Some(known_position_cost / known_position_size)
    } else {
        None
    };
    let return_pct = avg_entry_price
        .filter(|entry| *entry > 0.0)
        .map(|entry| (price - entry) / entry);
    let sell_fraction = if known_position_size > 0.0 {
        Some((size / known_position_size).min(9.99))
    } else {
        None
    };
    let action = match return_pct {
        Some(value) if value >= 0.10 => SellAction::TakeProfit,
        Some(value) if value <= -0.10 => SellAction::StopLoss,
        Some(_) => SellAction::Trim,
        None => SellAction::Unknown,
    };
    let mut score: i32 = match action {
        SellAction::TakeProfit | SellAction::StopLoss => 65,
        SellAction::Trim => 55,
        SellAction::Unknown => 35,
    };
    let mut reasons = Vec::new();
    let mut cautions = Vec::new();

    match avg_entry_price {
        Some(entry) => {
            reasons.push(format!(
                "找到同 outcome 的已知持仓，估算成本 {:.2}c，当前卖出 {:.2}c。",
                entry * 100.0,
                price * 100.0
            ));
        }
        None => {
            cautions
                .push("近期历史里没有找到可匹配的买入成本，可能是旧仓、做市或调仓。".to_owned());
        }
    }

    if let Some(return_pct) = return_pct {
        if return_pct >= 0.10 {
            score += 10;
            reasons.push(format!(
                "卖出价高于估算成本 {:.1}%，更像止盈。",
                return_pct * 100.0
            ));
        } else if return_pct <= -0.10 {
            score += 15;
            reasons.push(format!(
                "卖出价低于估算成本 {:.1}%，更像止损/撤退。",
                return_pct * 100.0
            ));
        } else {
            reasons.push(format!(
                "卖出价接近估算成本，收益约 {:.1}%，更像调仓或降低风险。",
                return_pct * 100.0
            ));
        }
    }

    if let Some(fraction) = sell_fraction {
        if fraction >= 0.75 {
            score += 10;
            reasons.push(format!(
                "本次卖出约占已知持仓 {:.1}%，接近清仓。",
                fraction * 100.0
            ));
        } else if fraction >= 0.25 {
            score += 5;
            reasons.push(format!(
                "本次卖出约占已知持仓 {:.1}%，属于明显减仓。",
                fraction * 100.0
            ));
        } else {
            cautions.push(format!(
                "本次卖出约占已知持仓 {:.1}%，更像小幅减仓。",
                fraction * 100.0
            ));
        }
    }

    if let Some(profile) = profile {
        let notional = price * size;
        if profile.large_trade_threshold_usd > 0.0 && notional >= profile.large_trade_threshold_usd
        {
            score += 8;
            reasons.push(format!(
                "卖出金额 ${:.2} 高于该员工历史 P80 ${:.2}。",
                notional, profile.large_trade_threshold_usd
            ));
        }

        if profile.suspected_market_making {
            score -= 30;
            cautions.push("该员工画像疑似做市/价差型，单笔 SELL 需要降级理解。".to_owned());
        }

        if profile
            .strategy_archetypes
            .contains(&StrategyArchetype::EarlyExitTrader)
        {
            reasons.push("该员工画像带有提前卖出型标签，SELL 本身是重要跟踪信号。".to_owned());
        }

        if profile
            .strategy_archetypes
            .contains(&StrategyArchetype::ShortTermOperator)
        {
            cautions.push("该员工画像带有短线操作型标签，买入信号有效期可能较短。".to_owned());
        }
    }

    let score = score.clamp(0, 100) as u8;

    SellAnalysis {
        action,
        avg_entry_price,
        return_pct,
        known_position_size: if known_position_size > 0.0 {
            Some(known_position_size)
        } else {
            None
        },
        sell_fraction,
        level: copy_signal_level(score),
        score,
        reasons,
        cautions,
    }
}

fn estimate_known_position(history: &[UserTrade], target: &UserTrade) -> (f64, f64) {
    let target_key = trade_position_key(target);
    let target_timestamp = target.timestamp.unwrap_or(u64::MAX);
    let target_trade_key = trade_key(target);
    let mut ordered = history
        .iter()
        .filter(|trade| trade_position_key(trade) == target_key)
        .filter(|trade| trade_key(trade) != target_trade_key)
        .filter(|trade| trade.timestamp.unwrap_or(0) <= target_timestamp)
        .collect::<Vec<_>>();

    ordered.sort_by_key(|trade| trade.timestamp.unwrap_or(0));

    let mut position_size = 0.0;
    let mut position_cost = 0.0;

    for trade in ordered {
        let Some(price) = trade.price else {
            continue;
        };
        let Some(size) = trade.size else {
            continue;
        };

        if price <= 0.0 || size <= 0.0 {
            continue;
        }

        if trade.side.eq_ignore_ascii_case("BUY") {
            position_size += size;
            position_cost += price * size;
            continue;
        }

        if !trade.side.eq_ignore_ascii_case("SELL") || position_size <= 0.0 {
            continue;
        }

        let matched_size = size.min(position_size);
        let avg_cost = position_cost / position_size;
        position_size -= matched_size;
        position_cost -= avg_cost * matched_size;

        if position_size <= 0.000_001 {
            position_size = 0.0;
            position_cost = 0.0;
        }
    }

    (position_size, position_cost)
}

fn profile_summary(profile: &EmployeeProfile) -> String {
    let strategies = profile.strategy_labels();
    let strategy_text = if strategies.is_empty() {
        "-".to_owned()
    } else {
        strategies.join(",")
    };
    let best_subcategory = profile
        .best_subcategories
        .first()
        .map(|metric| {
            format!(
                "{} ROI {:.1}% PnL ${:.2}",
                metric.name,
                metric.roi * 100.0,
                metric.profit_usd
            )
        })
        .unwrap_or_else(|| "-".to_owned());
    let best_price_band = profile
        .best_price_bands
        .first()
        .map(|metric| format!("{} ROI {:.1}%", metric.band, metric.roi * 100.0))
        .unwrap_or_else(|| "-".to_owned());

    format!(
        "策略画像: {} | 员工分 {} | ROI {:.1}% | P80 ${:.2} | 净仓变化 {:.1}% | SELL占比 {:.1}% | 快进快出 {:.1}% | 做市嫌疑 {}\n\
优势: 子领域 {} | 价格区间 {}",
        strategy_text,
        profile.copy_trade_score,
        profile.realized_roi * 100.0,
        profile.large_trade_threshold_usd,
        profile.net_position_change_ratio * 100.0,
        profile.sell_notional_ratio * 100.0,
        profile.quick_flip_ratio * 100.0,
        if profile.suspected_market_making {
            "是"
        } else {
            "否"
        },
        best_subcategory,
        best_price_band,
    )
}

fn format_signal_lines(lines: &[String]) -> String {
    if lines.is_empty() {
        return "- 无".to_owned();
    }

    lines
        .iter()
        .map(|line| format!("- {line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn format_optional_price(price: Option<f64>) -> String {
    price
        .map(|price| format!("{:.2}c", price * 100.0))
        .unwrap_or_else(|| "未知".to_owned())
}

fn buy_dust_floor_usd() -> f64 {
    5.0
}

fn format_optional_pct(value: Option<f64>) -> String {
    value
        .map(|value| format!("{:+.1}%", value * 100.0))
        .unwrap_or_else(|| "未知".to_owned())
}

fn format_sell_fraction(fraction: Option<f64>, known_position_size: Option<f64>) -> String {
    match (fraction, known_position_size) {
        (Some(fraction), Some(position_size)) => {
            format!(
                "约 {:.1}% 已知持仓 (known size {:.4})",
                fraction * 100.0,
                position_size
            )
        }
        _ => "未知".to_owned(),
    }
}

fn copy_signal_level(score: u8) -> CopySignalLevel {
    match score {
        80..=100 => CopySignalLevel::Strong,
        60..=79 => CopySignalLevel::Normal,
        40..=59 => CopySignalLevel::Watch,
        _ => CopySignalLevel::LowPriority,
    }
}

fn remember_trade(history: &mut Vec<UserTrade>, trade: UserTrade, max_history: usize) {
    let key = trade_key(&trade);
    if history.iter().any(|existing| trade_key(existing) == key) {
        return;
    }

    history.push(trade);
    history.sort_by_key(|trade| std::cmp::Reverse(trade.timestamp.unwrap_or(0)));
    history.truncate(max_history.max(20));
}

fn trade_position_key(trade: &UserTrade) -> String {
    format!("{}:{}", trade.condition_id, trade.asset)
}

fn parse_employee_min_notional(value: Option<&str>) -> Result<Option<f64>, String> {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };

    let min_notional = value
        .parse::<f64>()
        .map_err(|_| format!("employee min notional must be a positive number: {value}"))?;
    if min_notional <= 0.0 {
        return Err("employee min notional must be greater than zero".to_owned());
    }

    Ok(Some(min_notional))
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

fn matches_keywords(employee: &WatchedEmployee, trade: &UserTrade) -> bool {
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

fn count_since(timestamps: &[u64], now_secs: u64, days: u64) -> usize {
    let window = days.saturating_mul(86_400);

    timestamps
        .iter()
        .filter(|timestamp| now_secs.saturating_sub(**timestamp) <= window)
        .count()
}

fn classify_frequency(matched_buys_7d: usize, matched_buys_30d: usize) -> FrequencyTier {
    if matched_buys_7d >= 5 || matched_buys_30d >= 15 {
        FrequencyTier::High
    } else if matched_buys_30d >= 3 {
        FrequencyTier::Medium
    } else if matched_buys_30d > 0 {
        FrequencyTier::Low
    } else {
        FrequencyTier::Dormant
    }
}

fn median(mut values: Vec<f64>) -> Option<f64> {
    if values.is_empty() {
        return None;
    }

    values.sort_by(|left, right| left.total_cmp(right));
    let mid = values.len() / 2;

    if values.len() % 2 == 0 {
        Some(round2((values[mid - 1] + values[mid]) / 2.0))
    } else {
        Some(round2(values[mid]))
    }
}

fn round2(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

fn trade_key(trade: &UserTrade) -> String {
    trade.transaction_hash.clone().unwrap_or_else(|| {
        format!(
            "{}:{}:{}:{:.8}:{:.8}",
            trade.proxy_wallet,
            trade.asset,
            trade.timestamp.unwrap_or_default(),
            trade.price.unwrap_or_default(),
            trade.size.unwrap_or_default()
        )
    })
}

pub fn default_keywords_for_domain(domain: &str) -> Vec<String> {
    match domain.to_uppercase().as_str() {
        "TECH" => parse_keywords("ai,gemini,llm,model,arena,openai,anthropic,xai,grok,google"),
        "CRYPTO" => parse_keywords("bitcoin,btc,ethereum,eth,solana,sol,crypto"),
        "SPORTS" => parse_keywords("nba,nhl,nfl,mlb,ufc,soccer,tennis,championship"),
        "POLITICS" => parse_keywords("trump,biden,election,senate,house,president,china"),
        "FINANCE" | "ECONOMICS" => parse_keywords("fed,rates,cpi,gdp,oil,wti,gold,stock"),
        "WEATHER" => parse_keywords("weather,temperature,hurricane,storm,rain,snow"),
        "CULTURE" | "MENTIONS" => parse_keywords("twitter,x,post,tweet,album,movie,celebrity"),
        _ => Vec::new(),
    }
}

fn parse_keywords(value: &str) -> Vec<String> {
    value
        .split([',', '|'])
        .map(|keyword| keyword.trim().to_lowercase())
        .filter(|keyword| !keyword.is_empty())
        .collect()
}

fn parse_employee_poll_seconds(value: Option<&str>) -> Result<Option<u64>, String> {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };

    let poll_seconds = value
        .parse::<u64>()
        .map_err(|_| format!("employee poll interval must be a positive integer: {value}"))?;
    if poll_seconds == 0 {
        return Err("employee poll interval must be greater than zero".to_owned());
    }

    Ok(Some(poll_seconds))
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

    fn trade(title: &str, price: f64, size: f64) -> UserTrade {
        UserTrade {
            proxy_wallet: "0xemployee".to_owned(),
            side: "BUY".to_owned(),
            asset: "asset".to_owned(),
            condition_id: "condition".to_owned(),
            size: Some(size),
            price: Some(price),
            timestamp: Some(now_secs()),
            title: Some(title.to_owned()),
            slug: Some(title.to_lowercase().replace(' ', "-")),
            event_slug: None,
            outcome: Some("Yes".to_owned()),
            outcome_index: Some(0),
            name: Some("employee".to_owned()),
            pseudonym: None,
            transaction_hash: Some("0xtx".to_owned()),
        }
    }

    #[test]
    fn ai_employee_alerts_on_cheap_relevant_trade() {
        let employee = WatchedEmployee::parse("0xemployee:worker:TECH:gemini|ai").unwrap();
        let rules = WatchRules::default();
        let alert = build_alert(
            &employee,
            &trade("Will Gemini 4 release by June?", 0.24, 1_000.0),
            &rules,
            None,
        );

        assert!(alert.is_some());
    }

    #[test]
    fn employee_spec_can_set_poll_interval() {
        let employee = WatchedEmployee::parse("0xemployee:worker:TECH:gemini|ai:120:10").unwrap();

        assert_eq!(employee.poll_seconds, Some(120));
        assert_eq!(employee.poll_interval_seconds(10), 120);
        assert_eq!(employee.min_notional_usd, Some(10.0));
        assert_eq!(employee.min_notional_usd(100.0), 10.0);
    }

    #[test]
    fn activity_failures_back_off_high_frequency_polling() {
        let fast_employee =
            WatchedEmployee::parse("0xemployee:worker:WEATHER:weather|temperature:1:1").unwrap();
        let slow_employee =
            WatchedEmployee::parse("0xemployee:worker:WEATHER:weather|temperature:30:1").unwrap();
        let rules = WatchRules {
            poll_seconds: 1,
            ..WatchRules::default()
        };

        assert_eq!(
            effective_employee_poll_interval_seconds(&fast_employee, &rules, 0),
            1
        );
        assert_eq!(
            effective_employee_poll_interval_seconds(&fast_employee, &rules, 1),
            3
        );
        assert_eq!(
            effective_employee_poll_interval_seconds(&fast_employee, &rules, 2),
            5
        );
        assert_eq!(
            effective_employee_poll_interval_seconds(&slow_employee, &rules, 2),
            30
        );
    }

    #[test]
    fn activity_recovery_depth_scales_with_failure_count() {
        assert_eq!(activity_recovery_extra_pages(0), 0);
        assert_eq!(activity_recovery_extra_pages(1), 1);
        assert_eq!(activity_recovery_extra_pages(2), 1);
        assert_eq!(activity_recovery_extra_pages(3), 2);
        assert_eq!(activity_recovery_extra_pages(5), 2);
        assert_eq!(
            activity_recovery_extra_pages(ACTIVITY_DEGRADED_ALERT_FAILURES),
            ACTIVITY_RECOVERY_MAX_EXTRA_PAGES
        );
    }

    #[test]
    fn activity_recovery_notice_summarizes_backfill() {
        let stats = ActivityRecoveryStats {
            pages_loaded: 2,
            extra_trades: 37,
            failed_page_offset: Some(220),
            failure_message: Some("timeout".to_owned()),
        };

        let line = format_activity_recovery_line(Some(&stats));

        assert!(line.contains("已补拉 2 页"));
        assert!(line.contains("额外获取 37 笔"));
        assert!(line.contains("offset=220"));
    }

    #[test]
    fn sell_uses_only_fresh_post_trade_position_cache() {
        let now = now_secs();
        let mut sell = trade("Weather market", 0.50, 20.0);
        sell.side = "SELL".to_owned();
        sell.timestamp = Some(now);
        let positions = HashMap::from([(
            "asset".to_owned(),
            ObservedSourcePosition {
                size_shares: 10.0,
                avg_price: Some(0.50),
                current_price: Some(0.50),
                end_date: Some("2099-12-31".to_owned()),
                condition_id: Some("condition".to_owned()),
                market_title: Some("Weather market".to_owned()),
                outcome: Some("Yes".to_owned()),
                slug: Some("weather-market".to_owned()),
                event_slug: Some("weather-market".to_owned()),
            },
        )]);
        let before_trade = SourcePositionsSnapshot {
            positions: positions.clone(),
            observed_at_secs: now.saturating_sub(1),
        };
        let after_trade = SourcePositionsSnapshot {
            positions,
            observed_at_secs: now,
        };

        assert!(fresh_source_positions_for_trade(Some(&before_trade), &sell).is_none());
        assert!(fresh_source_positions_for_trade(Some(&after_trade), &sell).is_some());
    }

    #[test]
    fn heartbeat_targets_next_wall_clock_boundary() {
        assert_eq!(
            next_heartbeat_boundary_secs(20 * 3_600 + 59 * 60, 3_600),
            21 * 3_600
        );
        assert_eq!(next_heartbeat_boundary_secs(21 * 3_600, 3_600), 22 * 3_600);
        assert_eq!(
            next_heartbeat_boundary_secs(21 * 3_600 + 29 * 60, 1_800),
            21 * 3_600 + 30 * 60
        );
        assert_eq!(next_heartbeat_boundary_secs(21 * 3_600, 0), u64::MAX);
    }

    #[test]
    fn source_position_date_filter_uses_shanghai_calendar_day() {
        assert_eq!(shanghai_date_yyyy_mm_dd(0), "1970-01-01");
        assert!(source_position_ended_before(
            &Some("2026-06-18T00:00:00Z".to_owned()),
            "2026-06-21"
        ));
        assert!(!source_position_ended_before(
            &Some("2026-06-21".to_owned()),
            "2026-06-21"
        ));
        assert!(!source_position_ended_before(
            &Some("2026-06-23T00:00:00Z".to_owned()),
            "2026-06-21"
        ));
        assert!(!source_position_ended_before(&None, "2026-06-21"));
    }

    #[test]
    fn ai_employee_skips_expensive_or_irrelevant_trade() {
        let employee = WatchedEmployee::parse("0xemployee:worker:TECH:gemini|ai").unwrap();
        let rules = WatchRules::default();

        assert!(build_alert(
            &employee,
            &trade("Will Gemini 4 release by June?", 0.92, 1_000.0),
            &rules,
            None,
        )
        .is_none());
        assert!(build_alert(
            &employee,
            &trade("Bitcoin Up or Down", 0.24, 1_000.0),
            &rules,
            None,
        )
        .is_none());
    }

    #[test]
    fn relevant_small_buy_is_observation_not_filtered() {
        let employee = WatchedEmployee::parse("0xemployee:worker:TECH:gemini|ai").unwrap();
        let rules = WatchRules::default();
        let alert = build_alert(
            &employee,
            &trade("Will Gemini 4 release by June?", 0.10, 60.0),
            &rules,
            None,
        )
        .unwrap();

        assert!(alert.contains("试探仓保留提醒"));
    }

    #[test]
    fn split_fills_for_same_outcome_are_aggregated() {
        let mut first = trade("UFC match", 0.20, 25.0);
        first.transaction_hash = Some("0xfirst".to_owned());
        first.timestamp = Some(100);
        let mut second = trade("UFC match", 0.22, 75.0);
        second.transaction_hash = Some("0xsecond".to_owned());
        second.timestamp = Some(101);

        let aggregated = aggregate_activity_trades(vec![first, second]);

        assert_eq!(aggregated.len(), 1);
        assert_eq!(aggregated[0].fill_count, 2);
        assert_eq!(aggregated[0].trade.size, Some(100.0));
        assert!((aggregated[0].trade.price.unwrap() - 0.215).abs() < 0.000_001);
        assert_eq!(
            aggregated[0].trade.transaction_hash.as_deref(),
            Some("0xsecond")
        );
    }

    #[test]
    fn aggregated_alert_mentions_fill_count() {
        let employee = WatchedEmployee::parse("0xemployee:surfandturf:SPORTS:ufc").unwrap();
        let rules = WatchRules::default();
        let alert = build_alert_with_fill_count(
            &employee,
            &trade("UFC match", 0.20, 1_000.0),
            &rules,
            None,
            12,
        )
        .unwrap();

        assert!(alert.contains("本轮合并 12 笔"));
    }

    #[test]
    fn observer_alert_cooldown_allows_direction_or_large_price_change() {
        let employee = WatchedEmployee::parse("0xemployee:surfandturf:SPORTS:ufc").unwrap();
        let mut throttle = ObserverAlertThrottle::default();
        let buy = trade("UFC match", 0.20, 100.0);

        assert!(throttle.should_publish(&employee, &buy, 1_000));
        assert!(!throttle.should_publish(&employee, &buy, 1_100));

        let mut moved_buy = buy.clone();
        moved_buy.price = Some(0.24);
        assert!(throttle.should_publish(&employee, &moved_buy, 1_101));

        let mut sell = buy;
        sell.side = "SELL".to_owned();
        assert!(throttle.should_publish(&employee, &sell, 1_102));
    }

    #[test]
    fn sell_alert_classifies_known_profitable_exit() {
        let employee = WatchedEmployee::parse("0xemployee:worker:TECH:gemini|ai").unwrap();
        let rules = WatchRules::default();
        let now = now_secs();
        let mut buy = trade("Will Gemini 4 release by June?", 0.20, 1_000.0);
        buy.timestamp = Some(now - 3_600);
        buy.transaction_hash = Some("0xbuy".to_owned());

        let mut sell = trade("Will Gemini 4 release by June?", 0.35, 500.0);
        sell.side = "SELL".to_owned();
        sell.timestamp = Some(now);
        sell.transaction_hash = Some("0xsell".to_owned());

        let alert = build_sell_alert(&employee, &sell, &rules, None, &[buy]).unwrap();

        assert!(alert.contains("止盈减仓"));
        assert!(alert.contains("+75.0%"));
    }

    #[test]
    fn activity_frequency_classifies_matched_domain_buys() {
        let employee = WatchedEmployee::parse("0xemployee:worker:TECH:gemini|ai").unwrap();
        let now = now_secs();
        let mut trades = Vec::new();

        for days_ago in [1, 2, 3, 5, 6] {
            let mut trade = trade("Will Gemini 4 release by June?", 0.24, 1_000.0);
            trade.timestamp = Some(now - (days_ago * 86_400));
            trades.push(trade);
        }

        let mut irrelevant = trade("Bitcoin Up or Down", 0.24, 1_000.0);
        irrelevant.timestamp = Some(now - 86_400);
        trades.push(irrelevant);

        let activity = analyze_employee_trades(&employee, &trades, now);

        assert_eq!(activity.matched_buys_7d, 5);
        assert_eq!(activity.frequency, FrequencyTier::High);
    }
}
