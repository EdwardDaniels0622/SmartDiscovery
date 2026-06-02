use smart_wallet_discovery::{
    discovery::{
        default_categories, discover_smart_money, scan_smart_money_employees, DiscoveryRunConfig,
        EmployeeScan, SmartMoneyConfig, SmartMoneyRoster,
    },
    model::{DiscoveryConfig, WalletId, WalletMetrics},
    monitor::{
        analyze_employee_activity, load_employee_profiles, watch_employees, EmployeeActivity,
        WatchRules, WatchedEmployee,
    },
    polymarket::PolymarketDataClient,
    profile::EmployeeProfile,
    scoring::score_wallet,
    telegram::TelegramNotifier,
};

#[derive(Debug, Clone)]
struct DiscoverArgs {
    categories: Vec<String>,
    time_period: String,
    candidate_limit: usize,
    closed_pages: usize,
    top_per_category: usize,
    min_closed_positions: usize,
    min_pnl: f64,
    min_roi: f64,
    max_drawdown: f64,
    max_inactive_days: u64,
    max_recent_loss_streak: usize,
    recent_window_days: u64,
    min_recent_closed_positions: usize,
    min_recent_pnl: f64,
    min_recent_roi: f64,
    min_recent_win_rate: f64,
    max_current_loss: f64,
    max_current_loss_ratio: f64,
    top: usize,
    proxy: ProxySetting,
    json: bool,
}

#[derive(Debug, Clone)]
struct WatchArgs {
    employees: Vec<WatchedEmployee>,
    scan_top: usize,
    categories: Vec<String>,
    time_period: String,
    candidate_limit: usize,
    closed_pages: usize,
    poll_seconds: u64,
    heartbeat_seconds: u64,
    iterations: Option<usize>,
    trade_limit: usize,
    profile_trade_limit: usize,
    profile_closed_pages: usize,
    profile_closed_page_size: usize,
    profiles_enabled: bool,
    min_notional: f64,
    max_entry_price: f64,
    follow_price_buffer: f64,
    stdout_only: bool,
    json: bool,
    proxy: ProxySetting,
}

#[derive(Debug, Clone)]
enum ProxySetting {
    Default,
    Direct,
    Url(String),
}

impl Default for DiscoverArgs {
    fn default() -> Self {
        Self {
            categories: default_categories(),
            time_period: "MONTH".to_owned(),
            candidate_limit: 5,
            closed_pages: 2,
            top_per_category: 1,
            min_closed_positions: 8,
            min_pnl: 100.0,
            min_roi: 0.05,
            max_drawdown: 0.35,
            max_inactive_days: 60,
            max_recent_loss_streak: 3,
            recent_window_days: 30,
            min_recent_closed_positions: 3,
            min_recent_pnl: 0.0,
            min_recent_roi: 0.0,
            min_recent_win_rate: 0.45,
            max_current_loss: 50_000.0,
            max_current_loss_ratio: 0.20,
            top: 10,
            proxy: ProxySetting::Default,
            json: false,
        }
    }
}

impl Default for WatchArgs {
    fn default() -> Self {
        Self {
            employees: Vec::new(),
            scan_top: 0,
            categories: default_categories(),
            time_period: "MONTH".to_owned(),
            candidate_limit: 5,
            closed_pages: 2,
            poll_seconds: 10,
            heartbeat_seconds: 3_600,
            iterations: None,
            trade_limit: 20,
            profile_trade_limit: 100,
            profile_closed_pages: 2,
            profile_closed_page_size: 50,
            profiles_enabled: true,
            min_notional: 100.0,
            max_entry_price: 0.75,
            follow_price_buffer: 0.05,
            stdout_only: false,
            json: false,
            proxy: ProxySetting::Default,
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();

    match args.first().map(String::as_str) {
        Some("discover") => {
            let discover_args = DiscoverArgs::parse(&args[1..])?;
            run_discover(discover_args)?;
        }
        Some("scan-employees") => {
            let scan_args = DiscoverArgs::parse(&args[1..])?;
            run_scan_employees(scan_args)?;
        }
        Some("watch") => {
            let watch_args = WatchArgs::parse(&args[1..])?;
            run_watch(watch_args)?;
        }
        Some("activity") => {
            let watch_args = WatchArgs::parse(&args[1..])?;
            run_activity(watch_args)?;
        }
        Some("profiles") => {
            let watch_args = WatchArgs::parse(&args[1..])?;
            run_profiles(watch_args)?;
        }
        _ => run_demo(),
    }

    Ok(())
}

fn run_demo() {
    let config = DiscoveryConfig::default();
    let demo_metrics = WalletMetrics {
        wallet: WalletId("0xdemo-smart-wallet".to_owned()),
        total_trades: 128,
        resolved_markets: 72,
        realized_pnl_usd: 18_400.0,
        realized_roi: 0.58,
        max_drawdown: 0.16,
        positive_month_ratio: 0.78,
        median_entry_price: 0.56,
        late_entry_ratio: 0.10,
        hedge_market_ratio: 0.07,
        maker_like_ratio: 0.11,
        avg_clv_1h: 0.014,
        avg_clv_24h: 0.041,
        copyable_trade_ratio: 0.67,
        liquidity_replicability: 0.74,
        recency_score: 0.91,
        category_focus_score: 0.73,
    };

    let score = score_wallet(&demo_metrics, &config);
    println!("{score:#?}");
}

fn build_client(proxy: ProxySetting) -> PolymarketDataClient {
    match proxy {
        ProxySetting::Default => PolymarketDataClient::new(),
        ProxySetting::Direct => PolymarketDataClient::new().with_proxy_url(None),
        ProxySetting::Url(proxy_url) => PolymarketDataClient::new().with_proxy_url(Some(proxy_url)),
    }
}

fn run_discover(args: DiscoverArgs) -> Result<(), Box<dyn std::error::Error>> {
    let config = DiscoveryRunConfig {
        categories: args
            .categories
            .iter()
            .map(|category| category.trim().to_uppercase())
            .filter(|category| !category.is_empty())
            .collect(),
        time_period: args.time_period.to_uppercase(),
        candidate_limit: args.candidate_limit,
        closed_position_pages: args.closed_pages,
        top_per_category: args.top_per_category,
        scoring: SmartMoneyConfig {
            min_closed_positions: args.min_closed_positions,
            min_realized_pnl_usd: args.min_pnl,
            min_realized_roi: args.min_roi,
            max_drawdown_ratio: args.max_drawdown,
            max_inactive_days: args.max_inactive_days,
            max_recent_loss_streak: args.max_recent_loss_streak,
            recent_window_days: args.recent_window_days,
            min_recent_closed_positions: args.min_recent_closed_positions,
            min_recent_pnl_usd: args.min_recent_pnl,
            min_recent_roi: args.min_recent_roi,
            min_recent_win_position_ratio: args.min_recent_win_rate,
            max_current_loss_usd: args.max_current_loss,
            max_current_loss_ratio: args.max_current_loss_ratio,
        },
        ..DiscoveryRunConfig::default()
    };

    let client = build_client(args.proxy);
    let roster = discover_smart_money(&client, &config);

    if args.json {
        println!("{}", serde_json::to_string_pretty(&roster)?);
    } else {
        print_roster(&roster);
    }

    Ok(())
}

fn run_scan_employees(args: DiscoverArgs) -> Result<(), Box<dyn std::error::Error>> {
    let config = discovery_config_from_args(&args);
    let client = build_client(args.proxy);
    let scan = scan_smart_money_employees(&client, &config, args.top);

    if args.json {
        println!("{}", serde_json::to_string_pretty(&scan)?);
    } else {
        print_employee_scan(&scan);
    }

    Ok(())
}

fn run_watch(args: WatchArgs) -> Result<(), Box<dyn std::error::Error>> {
    let client = build_client(args.proxy.clone());
    let mut employees = args.employees.clone();

    if args.scan_top > 0 {
        let discover_args = DiscoverArgs {
            categories: args.categories.clone(),
            time_period: args.time_period.clone(),
            candidate_limit: args.candidate_limit,
            closed_pages: args.closed_pages,
            proxy: args.proxy,
            top: args.scan_top,
            ..DiscoverArgs::default()
        };
        let scan = scan_smart_money_employees(
            &client,
            &discovery_config_from_args(&discover_args),
            args.scan_top,
        );
        employees.extend(scan.employees.iter().map(WatchedEmployee::from_evaluation));
    }

    if employees.is_empty() {
        return Err("no employees to watch; use --scan-top or --employee".into());
    }

    print_watched_employees(&employees);

    let rules = WatchRules {
        poll_seconds: args.poll_seconds,
        heartbeat_seconds: args.heartbeat_seconds,
        trade_limit: args.trade_limit,
        profile_trade_limit: args.profile_trade_limit,
        profile_closed_pages: args.profile_closed_pages,
        profile_closed_page_size: args.profile_closed_page_size,
        profiles_enabled: args.profiles_enabled,
        min_notional_usd: args.min_notional,
        max_entry_price: args.max_entry_price,
        follow_price_buffer: args.follow_price_buffer,
        iterations: args.iterations,
    };
    let telegram = if args.stdout_only {
        None
    } else {
        match TelegramNotifier::from_env() {
            Ok(notifier) => Some(notifier),
            Err(error) => {
                eprintln!("Telegram disabled: {error}");
                None
            }
        }
    };
    let outcome = watch_employees(&client, &employees, &rules, telegram.as_ref());

    println!(
        "watch finished: polls={} employee_polls={} employees={} alerts={} heartbeats={}",
        outcome.polls_completed,
        outcome.employee_polls_completed,
        outcome.employees,
        outcome.alerts_sent,
        outcome.heartbeats_sent
    );

    Ok(())
}

fn run_activity(args: WatchArgs) -> Result<(), Box<dyn std::error::Error>> {
    let client = build_client(args.proxy.clone());
    let mut employees = args.employees.clone();

    if args.scan_top > 0 {
        let discover_args = DiscoverArgs {
            categories: args.categories.clone(),
            time_period: args.time_period.clone(),
            candidate_limit: args.candidate_limit,
            closed_pages: args.closed_pages,
            proxy: args.proxy,
            top: args.scan_top,
            ..DiscoverArgs::default()
        };
        let scan = scan_smart_money_employees(
            &client,
            &discovery_config_from_args(&discover_args),
            args.scan_top,
        );
        employees.extend(scan.employees.iter().map(WatchedEmployee::from_evaluation));
    }

    if employees.is_empty() {
        return Err("no employees to analyze; use --scan-top or --employee".into());
    }

    let activity = analyze_employee_activity(&client, &employees, args.trade_limit);
    print_employee_activity(&activity);

    Ok(())
}

fn run_profiles(args: WatchArgs) -> Result<(), Box<dyn std::error::Error>> {
    let client = build_client(args.proxy.clone());
    let mut employees = args.employees.clone();

    if args.scan_top > 0 {
        let discover_args = DiscoverArgs {
            categories: args.categories.clone(),
            time_period: args.time_period.clone(),
            candidate_limit: args.candidate_limit,
            closed_pages: args.closed_pages,
            proxy: args.proxy,
            top: args.scan_top,
            ..DiscoverArgs::default()
        };
        let scan = scan_smart_money_employees(
            &client,
            &discovery_config_from_args(&discover_args),
            args.scan_top,
        );
        employees.extend(scan.employees.iter().map(WatchedEmployee::from_evaluation));
    }

    if employees.is_empty() {
        return Err("no employees to profile; use --scan-top or --employee".into());
    }

    let rules = WatchRules {
        profile_trade_limit: args.profile_trade_limit,
        profile_closed_pages: args.profile_closed_pages,
        profile_closed_page_size: args.profile_closed_page_size,
        profiles_enabled: args.profiles_enabled,
        ..WatchRules::default()
    };
    let profiles = load_employee_profiles(&client, &employees, &rules);

    if args.json {
        println!("{}", serde_json::to_string_pretty(&profiles)?);
    } else {
        print_employee_profiles(&profiles);
    }

    Ok(())
}

fn discovery_config_from_args(args: &DiscoverArgs) -> DiscoveryRunConfig {
    DiscoveryRunConfig {
        categories: args
            .categories
            .iter()
            .map(|category| category.trim().to_uppercase())
            .filter(|category| !category.is_empty())
            .collect(),
        time_period: args.time_period.to_uppercase(),
        candidate_limit: args.candidate_limit,
        closed_position_pages: args.closed_pages,
        top_per_category: args.top_per_category,
        scoring: SmartMoneyConfig {
            min_closed_positions: args.min_closed_positions,
            min_realized_pnl_usd: args.min_pnl,
            min_realized_roi: args.min_roi,
            max_drawdown_ratio: args.max_drawdown,
            max_inactive_days: args.max_inactive_days,
            max_recent_loss_streak: args.max_recent_loss_streak,
            recent_window_days: args.recent_window_days,
            min_recent_closed_positions: args.min_recent_closed_positions,
            min_recent_pnl_usd: args.min_recent_pnl,
            min_recent_roi: args.min_recent_roi,
            min_recent_win_position_ratio: args.min_recent_win_rate,
            max_current_loss_usd: args.max_current_loss,
            max_current_loss_ratio: args.max_current_loss_ratio,
        },
        ..DiscoveryRunConfig::default()
    }
}

impl DiscoverArgs {
    fn parse(raw_args: &[String]) -> Result<Self, String> {
        let mut args = Self::default();
        let mut index = 0;

        while index < raw_args.len() {
            let raw = &raw_args[index];

            if raw == "--json" {
                args.json = true;
                index += 1;
                continue;
            }

            let (key, inline_value) = match raw.split_once('=') {
                Some((key, value)) => (key, Some(value.to_owned())),
                None => (raw.as_str(), None),
            };

            let value = match inline_value {
                Some(value) => value,
                None => {
                    index += 1;
                    raw_args
                        .get(index)
                        .cloned()
                        .ok_or_else(|| format!("missing value for {key}"))?
                }
            };

            match key {
                "--categories" => {
                    args.categories = value
                        .split(',')
                        .map(|category| category.trim().to_owned())
                        .filter(|category| !category.is_empty())
                        .collect();
                }
                "--time-period" => args.time_period = value,
                "--candidate-limit" => args.candidate_limit = parse_value(key, &value)?,
                "--closed-pages" => args.closed_pages = parse_value(key, &value)?,
                "--top-per-category" => args.top_per_category = parse_value(key, &value)?,
                "--min-closed-positions" => args.min_closed_positions = parse_value(key, &value)?,
                "--min-pnl" => args.min_pnl = parse_value(key, &value)?,
                "--min-roi" => args.min_roi = parse_value(key, &value)?,
                "--max-drawdown" => args.max_drawdown = parse_value(key, &value)?,
                "--max-inactive-days" => args.max_inactive_days = parse_value(key, &value)?,
                "--max-recent-loss-streak" => {
                    args.max_recent_loss_streak = parse_value(key, &value)?
                }
                "--recent-window-days" => args.recent_window_days = parse_value(key, &value)?,
                "--min-recent-closed-positions" => {
                    args.min_recent_closed_positions = parse_value(key, &value)?
                }
                "--min-recent-pnl" => args.min_recent_pnl = parse_value(key, &value)?,
                "--min-recent-roi" => args.min_recent_roi = parse_value(key, &value)?,
                "--min-recent-win-rate" => args.min_recent_win_rate = parse_value(key, &value)?,
                "--max-current-loss" => args.max_current_loss = parse_value(key, &value)?,
                "--max-current-loss-ratio" => {
                    args.max_current_loss_ratio = parse_value(key, &value)?
                }
                "--top" => args.top = parse_value(key, &value)?,
                "--proxy" => args.proxy = parse_proxy_setting(&value),
                unknown => return Err(format!("unknown argument: {unknown}")),
            }

            index += 1;
        }

        Ok(args)
    }
}

impl WatchArgs {
    fn parse(raw_args: &[String]) -> Result<Self, String> {
        let mut args = Self::default();
        let mut index = 0;

        while index < raw_args.len() {
            let raw = &raw_args[index];

            if raw == "--stdout-only" {
                args.stdout_only = true;
                index += 1;
                continue;
            }

            if raw == "--no-profiles" {
                args.profiles_enabled = false;
                index += 1;
                continue;
            }

            if raw == "--json" {
                args.json = true;
                index += 1;
                continue;
            }

            let (key, inline_value) = match raw.split_once('=') {
                Some((key, value)) => (key, Some(value.to_owned())),
                None => (raw.as_str(), None),
            };
            let value = match inline_value {
                Some(value) => value,
                None => {
                    index += 1;
                    raw_args
                        .get(index)
                        .cloned()
                        .ok_or_else(|| format!("missing value for {key}"))?
                }
            };

            match key {
                "--employee" => args.employees.push(WatchedEmployee::parse(&value)?),
                "--scan-top" => args.scan_top = parse_value(key, &value)?,
                "--categories" => {
                    args.categories = value
                        .split(',')
                        .map(|category| category.trim().to_owned())
                        .filter(|category| !category.is_empty())
                        .collect();
                }
                "--time-period" => args.time_period = value,
                "--candidate-limit" => args.candidate_limit = parse_value(key, &value)?,
                "--closed-pages" => args.closed_pages = parse_value(key, &value)?,
                "--poll-seconds" => args.poll_seconds = parse_value(key, &value)?,
                "--heartbeat-seconds" => args.heartbeat_seconds = parse_value(key, &value)?,
                "--iterations" => {
                    let iterations = parse_value::<usize>(key, &value)?;
                    args.iterations = if iterations == 0 {
                        None
                    } else {
                        Some(iterations)
                    };
                }
                "--trade-limit" => args.trade_limit = parse_value(key, &value)?,
                "--profile-trade-limit" => args.profile_trade_limit = parse_value(key, &value)?,
                "--profile-closed-pages" => args.profile_closed_pages = parse_value(key, &value)?,
                "--profile-closed-page-size" => {
                    args.profile_closed_page_size = parse_value(key, &value)?
                }
                "--min-notional" => args.min_notional = parse_value(key, &value)?,
                "--max-entry-price" => args.max_entry_price = parse_value(key, &value)?,
                "--follow-price-buffer" => args.follow_price_buffer = parse_value(key, &value)?,
                "--proxy" => args.proxy = parse_proxy_setting(&value),
                unknown => return Err(format!("unknown argument: {unknown}")),
            }

            index += 1;
        }

        Ok(args)
    }
}

fn parse_proxy_setting(value: &str) -> ProxySetting {
    let value = value.trim();

    if value.is_empty()
        || value.eq_ignore_ascii_case("none")
        || value.eq_ignore_ascii_case("off")
        || value.eq_ignore_ascii_case("direct")
    {
        ProxySetting::Direct
    } else {
        ProxySetting::Url(value.to_owned())
    }
}

fn parse_value<T>(key: &str, value: &str) -> Result<T, String>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    value
        .parse::<T>()
        .map_err(|error| format!("invalid value for {key}: {error}"))
}

fn print_roster(roster: &SmartMoneyRoster) {
    println!(
        "Smart-money roster, generated_at={}",
        roster.generated_at_secs
    );
    println!(
        "{:<10} {:<8} {:<42} {:<18} {:>7} {:>10} {:>10} {:>10} {:>8} {:>8} {:>7} {:>8} {:<10}",
        "category",
        "status",
        "wallet",
        "name",
        "score",
        "pnl",
        "r_pnl",
        "cur_loss",
        "r_roi",
        "max_dd",
        "closed",
        "inactive",
        "flags"
    );

    for category in &roster.categories {
        if category.picks.is_empty() {
            println!(
                "{:<10} {:<8} {:<42} scanned={} eligible={}",
                category.category,
                "EMPTY",
                "-",
                category.scanned_wallets,
                category.eligible_wallets
            );
            continue;
        }

        for pick in &category.picks {
            let status = if pick.eligible { "HIRE" } else { "REVIEW" };
            let name = pick.user_name.as_deref().unwrap_or("-");
            let flags = if pick.flags.is_empty() {
                "-".to_owned()
            } else {
                pick.flags
                    .iter()
                    .map(|flag| format!("{flag:?}"))
                    .collect::<Vec<_>>()
                    .join(",")
            };
            let inactive = pick
                .metrics
                .last_activity_days
                .map(|days| format!("{days}d"))
                .unwrap_or_else(|| "-".to_owned());

            println!(
                "{:<10} {:<8} {:<42} {:<18} {:>7.3} {:>10.2} {:>10.2} {:>10.2} {:>7.1}% {:>7.1}% {:>7} {:>8} {:<10}",
                pick.category,
                status,
                pick.wallet,
                truncate(name, 18),
                pick.score,
                pick.metrics.realized_pnl_usd,
                pick.metrics.recent_pnl_usd,
                pick.metrics.current_loss_usd,
                pick.metrics.recent_roi * 100.0,
                pick.metrics.max_drawdown_ratio * 100.0,
                pick.metrics.closed_positions,
                inactive,
                flags
            );
        }
    }

    if !roster.warnings.is_empty() {
        println!();
        println!("Warnings:");
        for warning in &roster.warnings {
            println!("- {warning}");
        }
    }
}

fn print_employee_scan(scan: &EmployeeScan) {
    println!(
        "Employee scan, generated_at={}, scanned={}, eligible={}, selected={}",
        scan.generated_at_secs,
        scan.scanned_wallets,
        scan.eligible_wallets,
        scan.employees.len()
    );
    println!(
        "{:<10} {:<42} {:<18} {:>7} {:>10} {:>10} {:>10} {:<20}",
        "domain", "wallet", "name", "score", "pnl", "r_pnl", "cur_loss", "watch_spec"
    );

    for employee in &scan.employees {
        let watched = WatchedEmployee::from_evaluation(employee);
        let spec = format!(
            "{}:{}:{}:{}",
            watched.wallet,
            watched.name.as_deref().unwrap_or("-"),
            watched.domain,
            watched.keywords.join("|")
        );
        println!(
            "{:<10} {:<42} {:<18} {:>7.3} {:>10.2} {:>10.2} {:>10.2} {:<20}",
            employee.category,
            employee.wallet,
            truncate(employee.user_name.as_deref().unwrap_or("-"), 18),
            employee.score,
            employee.metrics.realized_pnl_usd,
            employee.metrics.recent_pnl_usd,
            employee.metrics.current_loss_usd,
            spec
        );
    }

    if !scan.warnings.is_empty() {
        println!();
        println!("Warnings:");
        for warning in &scan.warnings {
            println!("- {warning}");
        }
    }
}

fn print_watched_employees(employees: &[WatchedEmployee]) {
    println!("Watching employees:");
    for employee in employees {
        let poll = employee
            .poll_seconds
            .map(|seconds| format!("{seconds}s"))
            .unwrap_or_else(|| "default".to_owned());
        let min_notional = employee
            .min_notional_usd
            .map(|value| format!("${value:.2}"))
            .unwrap_or_else(|| "default".to_owned());
        println!(
            "- {} domain={} poll={} min_notional={} keywords={}",
            employee.label(),
            employee.domain,
            poll,
            min_notional,
            employee.keywords.join(",")
        );
    }
}

fn print_employee_activity(activity: &[EmployeeActivity]) {
    println!(
        "{:<10} {:<18} {:<42} {:>8} {:>8} {:>8} {:>9} {:>9} {:>10}",
        "freq", "name", "wallet", "matched", "7d", "30d", "last_d", "avg_h", "median_h"
    );

    for row in activity {
        println!(
            "{:<10} {:<18} {:<42} {:>8} {:>8} {:>8} {:>9} {:>9} {:>10}",
            format!("{:?}", row.frequency),
            truncate(row.name.as_deref().unwrap_or("-"), 18),
            row.wallet,
            row.matched_buys,
            row.matched_buys_7d,
            row.matched_buys_30d,
            row.last_matched_buy_age_days
                .map(|value| value.to_string())
                .unwrap_or_else(|| "-".to_owned()),
            row.avg_gap_hours
                .map(|value| format!("{value:.1}"))
                .unwrap_or_else(|| "-".to_owned()),
            row.median_gap_hours
                .map(|value| format!("{value:.1}"))
                .unwrap_or_else(|| "-".to_owned()),
        );
    }
}

fn print_employee_profiles(profiles: &[EmployeeProfile]) {
    println!(
        "{:<10} {:<18} {:<42} {:>6} {:>8} {:>8} {:>8} {:>8} {:>7} {:>7} {:>7} {:<24} {:<18}",
        "domain",
        "name",
        "wallet",
        "score",
        "pnl",
        "roi",
        "median",
        "p80",
        "sell%",
        "quick%",
        "mm",
        "strategy",
        "best"
    );

    for profile in profiles {
        let strategy = profile.strategy_labels().join(",");
        let best = profile
            .best_subcategories
            .first()
            .map(|metric| format!("{} {:.1}%", metric.name, metric.roi * 100.0))
            .unwrap_or_else(|| "-".to_owned());

        println!(
            "{:<10} {:<18} {:<42} {:>6} {:>8.2} {:>7.1}% {:>8.2} {:>8.2} {:>6.1}% {:>6.1}% {:>7} {:<24} {:<18}",
            profile.domain,
            truncate(profile.name.as_deref().unwrap_or("-"), 18),
            profile.wallet,
            profile.copy_trade_score,
            profile.realized_pnl_usd,
            profile.realized_roi * 100.0,
            profile.median_trade_size_usd,
            profile.large_trade_threshold_usd,
            profile.sell_notional_ratio * 100.0,
            profile.quick_flip_ratio * 100.0,
            if profile.suspected_market_making {
                "yes"
            } else {
                "no"
            },
            truncate(&strategy, 24),
            truncate(&best, 18),
        );

        for note in profile.copy_trade_notes.iter().take(3) {
            println!("  - {note}");
        }
    }
}

fn truncate(value: &str, max_chars: usize) -> String {
    let mut result = String::new();

    for ch in value.chars().take(max_chars) {
        result.push(ch);
    }

    result
}
