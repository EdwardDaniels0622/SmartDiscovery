use smart_wallet_discovery::{
    autocopy::{AutoCopyConfig, AutoCopyMode},
    discovery::{
        default_categories, discover_smart_money, scan_smart_money_employees, DiscoveryRunConfig,
        EmployeeScan, SmartMoneyConfig, SmartMoneyRoster,
    },
    employee_stats::{
        compact_report_value, rebuild_employee_stats, refresh_employee_stats,
        render_employee_stats_markdown, render_employee_stats_text, EmployeeIdentity,
        EmployeeStatsConfig, EmployeeStatsReport, EmployeeStatsStore, RefreshSelection,
    },
    leaderboard_recruitment::{
        scan_leaderboard_employees, LeaderboardRecruitmentConfig, LeaderboardRecruitmentReport,
        ROTATING_RECRUITMENT_DOMAINS,
    },
    model::{DiscoveryConfig, WalletId, WalletMetrics},
    monitor::{
        analyze_employee_activity, load_employee_profiles, watch_employees, EmployeeActivity,
        WatchRules, WatchedEmployee,
    },
    polymarket::PolymarketDataClient,
    profile::EmployeeProfile,
    recruitment::{
        default_recruitment_domains, recruit_from_tape, TapeEmployeeCandidate,
        TapeRecruitmentConfig, TapeRecruitmentReport,
    },
    scoring::score_wallet,
    telegram::TelegramNotifier,
};
use std::{fs, path::PathBuf};

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
    auto_copy: AutoCopyConfig,
    stdout_only: bool,
    json: bool,
    proxy: ProxySetting,
}

#[derive(Debug, Clone)]
struct RecruitArgs {
    domains: Vec<String>,
    trade_pages: usize,
    trade_page_size: usize,
    lookahead_minutes: u64,
    min_copy_window_seconds: u64,
    min_entry_price: f64,
    max_entry_price: f64,
    min_source_notional: f64,
    max_source_notional: f64,
    min_later_trades: usize,
    min_tape_move: f64,
    min_candidate_trades: usize,
    min_candidate_score: f64,
    top: usize,
    include_fast_markets: bool,
    exclude_keywords: Option<Vec<String>>,
    wallet_vetting: bool,
    closed_pages: usize,
    min_wallet_closed_positions: usize,
    min_wallet_pnl: f64,
    min_wallet_roi: f64,
    min_wallet_recent_pnl: f64,
    min_wallet_recent_roi: f64,
    max_current_loss: f64,
    max_current_loss_ratio: f64,
    max_two_sided_ratio: f64,
    reject_wallets: Option<Vec<String>>,
    json: bool,
    proxy: ProxySetting,
}

#[derive(Debug, Clone)]
struct LeaderboardRecruitArgs {
    domain: String,
    periods: Vec<String>,
    leaderboard_depth: usize,
    wallet_limit: usize,
    history_days: u64,
    closed_pages: usize,
    pause_ms: u64,
    top: usize,
    min_lifetime_pnl: f64,
    max_lifetime_pnl: f64,
    min_monthly_positions: usize,
    max_monthly_positions: usize,
    min_domain_positions: usize,
    min_domain_roi: f64,
    min_domain_profit_share: f64,
    max_top5_profit_share: f64,
    max_high_price_profit_share: f64,
    max_active_loss: f64,
    max_active_loss_ratio: f64,
    json: bool,
    proxy: ProxySetting,
}

#[derive(Debug, Clone, Copy)]
enum EmployeeStatsAction {
    Show,
    Refresh,
    Rebuild,
}

#[derive(Debug, Clone, Copy)]
enum EmployeeStatsOutputFormat {
    Text,
    Json,
    CompactJson,
    Markdown,
}

#[derive(Debug, Clone)]
struct EmployeeStatsArgs {
    action: EmployeeStatsAction,
    selector: String,
    display_name: Option<String>,
    username: Option<String>,
    primary_domain: String,
    keywords: Vec<String>,
    cache_dir: PathBuf,
    window_days: u64,
    retention_days: u64,
    action_gap_seconds: u64,
    max_pages: usize,
    selection: RefreshSelection,
    output_format: EmployeeStatsOutputFormat,
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
            auto_copy: AutoCopyConfig::weatherhk_default(),
            stdout_only: false,
            json: false,
            proxy: ProxySetting::Default,
        }
    }
}

impl Default for RecruitArgs {
    fn default() -> Self {
        Self {
            domains: Vec::new(),
            trade_pages: 5,
            trade_page_size: 100,
            lookahead_minutes: 30,
            min_copy_window_seconds: 20,
            min_entry_price: 0.01,
            max_entry_price: 0.75,
            min_source_notional: 5.0,
            max_source_notional: 1_000.0,
            min_later_trades: 1,
            min_tape_move: 0.015,
            min_candidate_trades: 2,
            min_candidate_score: 60.0,
            top: 10,
            include_fast_markets: false,
            exclude_keywords: None,
            wallet_vetting: true,
            closed_pages: 2,
            min_wallet_closed_positions: 3,
            min_wallet_pnl: 0.0,
            min_wallet_roi: 0.0,
            min_wallet_recent_pnl: 0.0,
            min_wallet_recent_roi: 0.0,
            max_current_loss: 5_000.0,
            max_current_loss_ratio: 0.20,
            max_two_sided_ratio: 0.25,
            reject_wallets: None,
            json: false,
            proxy: ProxySetting::Default,
        }
    }
}

impl Default for LeaderboardRecruitArgs {
    fn default() -> Self {
        let config = LeaderboardRecruitmentConfig::default();
        Self {
            domain: config.domain,
            periods: config.periods,
            leaderboard_depth: config.leaderboard_depth,
            wallet_limit: config.wallet_limit,
            history_days: config.history_window_days,
            closed_pages: config.closed_position_pages,
            pause_ms: config.pause_between_wallets_ms,
            top: config.top,
            min_lifetime_pnl: config.min_lifetime_pnl_usd,
            max_lifetime_pnl: config.max_lifetime_pnl_usd,
            min_monthly_positions: config.min_monthly_positions,
            max_monthly_positions: config.max_monthly_positions,
            min_domain_positions: config.min_domain_14d_positions,
            min_domain_roi: config.min_domain_14d_roi,
            min_domain_profit_share: config.min_domain_gross_profit_share,
            max_top5_profit_share: config.max_top5_profit_share,
            max_high_price_profit_share: config.max_high_price_profit_share,
            max_active_loss: config.max_active_loss_usd,
            max_active_loss_ratio: config.max_active_loss_ratio,
            json: false,
            proxy: ProxySetting::Default,
        }
    }
}

impl EmployeeStatsArgs {
    fn parse(raw_args: &[String]) -> Result<Self, String> {
        let action = match raw_args.first().map(String::as_str) {
            Some("show") => EmployeeStatsAction::Show,
            Some("refresh") => EmployeeStatsAction::Refresh,
            Some("rebuild") => EmployeeStatsAction::Rebuild,
            Some(value) => {
                return Err(format!(
                    "unknown employee-stats action {value:?}; expected show, refresh, or rebuild"
                ))
            }
            None => {
                return Err(
                    "employee-stats requires an action: show, refresh, or rebuild".to_owned(),
                )
            }
        };
        let defaults = EmployeeStatsConfig::default();
        let mut args = Self {
            action,
            selector: String::new(),
            display_name: None,
            username: None,
            primary_domain: String::new(),
            keywords: Vec::new(),
            cache_dir: defaults.cache_dir,
            window_days: defaults.window_days,
            retention_days: defaults.retention_days,
            action_gap_seconds: defaults.action_gap_seconds,
            max_pages: defaults.max_pages,
            selection: RefreshSelection::all(),
            output_format: EmployeeStatsOutputFormat::Text,
            proxy: ProxySetting::Default,
        };
        let mut index = 1;

        while index < raw_args.len() {
            let raw = &raw_args[index];
            if raw == "--json" {
                args.output_format = EmployeeStatsOutputFormat::Json;
                index += 1;
                continue;
            }
            if raw == "--compact-json" {
                args.output_format = EmployeeStatsOutputFormat::CompactJson;
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
                "--wallet" => args.selector = value.trim().to_lowercase(),
                "--employee" => {
                    if value.contains(':') {
                        let employee = WatchedEmployee::parse(&value)?;
                        args.selector = employee.wallet.to_lowercase();
                        args.display_name = employee.name;
                        args.primary_domain = employee.domain;
                        args.keywords = employee.keywords;
                    } else {
                        args.selector = value.trim().to_owned();
                    }
                }
                "--name" => args.display_name = Some(value),
                "--username" => args.username = Some(value),
                "--domain" => args.primary_domain = value.trim().to_uppercase(),
                "--keywords" => {
                    args.keywords = value
                        .split(['|', ','])
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                        .map(str::to_owned)
                        .collect();
                }
                "--cache-dir" => args.cache_dir = PathBuf::from(value),
                "--window-days" => args.window_days = parse_value(key, &value)?,
                "--retention-days" => args.retention_days = parse_value(key, &value)?,
                "--action-gap-seconds" => {
                    args.action_gap_seconds = parse_value(key, &value)?
                }
                "--max-pages" => args.max_pages = parse_value(key, &value)?,
                "--only" => args.selection = parse_refresh_selection(&value)?,
                "--format" => {
                    args.output_format = match value.to_lowercase().as_str() {
                        "text" => EmployeeStatsOutputFormat::Text,
                        "json" => EmployeeStatsOutputFormat::Json,
                        "compact-json" | "compact" => EmployeeStatsOutputFormat::CompactJson,
                        "markdown" | "md" => EmployeeStatsOutputFormat::Markdown,
                        _ => {
                            return Err(format!(
                                "unsupported employee-stats format {value:?}; expected text, json, compact-json, or markdown"
                            ))
                        }
                    }
                }
                "--proxy" => args.proxy = parse_proxy_setting(&value),
                unknown => return Err(format!("unknown employee-stats argument: {unknown}")),
            }
            index += 1;
        }

        if args.selector.trim().is_empty() {
            return Err("employee-stats requires --wallet or --employee".to_owned());
        }
        Ok(args)
    }

    fn config(&self) -> EmployeeStatsConfig {
        EmployeeStatsConfig {
            cache_dir: self.cache_dir.clone(),
            window_days: self.window_days,
            retention_days: self.retention_days,
            action_gap_seconds: self.action_gap_seconds,
            max_pages: self.max_pages,
        }
    }
}

fn parse_refresh_selection(value: &str) -> Result<RefreshSelection, String> {
    let mut selection = RefreshSelection::none();
    for component in value
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        match component.to_lowercase().as_str() {
            "activity" => selection.activity = true,
            "trades" => selection.trades = true,
            "closed-positions" | "closed_positions" | "closed" => {
                selection.closed_positions = true
            }
            "positions" | "current-positions" | "current_positions" => {
                selection.positions = true
            }
            unknown => {
                return Err(format!(
                    "unknown refresh component {unknown:?}; expected activity,trades,closed-positions,positions"
                ))
            }
        }
    }
    if !selection.any() {
        return Err("--only requires at least one component".to_owned());
    }
    Ok(selection)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    load_env_file();
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
        Some("recruit-employees") | Some("tape-recruit") => {
            let recruit_args = RecruitArgs::parse(&args[1..])?;
            run_recruit_employees(recruit_args)?;
        }
        Some("scan-domain-employees") | Some("leaderboard-recruit") => {
            let recruit_args = LeaderboardRecruitArgs::parse(&args[1..])?;
            run_leaderboard_recruitment(recruit_args)?;
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
        Some("employee-stats") => {
            let stats_args = EmployeeStatsArgs::parse(&args[1..])?;
            run_employee_stats(stats_args)?;
        }
        _ => run_demo(),
    }

    Ok(())
}

fn load_env_file() {
    let path = std::env::var("SMART_WALLET_ENV_FILE")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(".env"));
    let content = match fs::read_to_string(&path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
        Err(error) => {
            eprintln!("failed to read env file {}: {error}", path.display());
            return;
        }
    };

    for (line_number, line) in content.lines().enumerate() {
        let Some((key, value)) = parse_env_line(line) else {
            continue;
        };

        if !is_valid_env_key(&key) {
            eprintln!(
                "ignored invalid env key in {}:{}",
                path.display(),
                line_number + 1
            );
            continue;
        }

        if std::env::var_os(&key).is_none() {
            std::env::set_var(key, value);
        }
    }
}

fn parse_env_line(line: &str) -> Option<(String, String)> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }

    let line = line.strip_prefix("export ").unwrap_or(line);
    let (key, value) = line.split_once('=')?;
    let key = key.trim().to_owned();
    let value = parse_env_value(value.trim());

    Some((key, value))
}

fn parse_env_value(value: &str) -> String {
    if let Some(stripped) = value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
    {
        return unescape_double_quoted_env(stripped);
    }

    if let Some(stripped) = value
        .strip_prefix('\'')
        .and_then(|value| value.strip_suffix('\''))
    {
        return stripped.to_owned();
    }

    strip_inline_comment(value).trim().to_owned()
}

fn strip_inline_comment(value: &str) -> &str {
    let bytes = value.as_bytes();
    for index in 0..bytes.len() {
        if bytes[index] == b'#' && (index == 0 || bytes[index - 1].is_ascii_whitespace()) {
            return &value[..index];
        }
    }

    value
}

fn unescape_double_quoted_env(value: &str) -> String {
    let mut output = String::new();
    let mut chars = value.chars();

    while let Some(ch) = chars.next() {
        if ch != '\\' {
            output.push(ch);
            continue;
        }

        match chars.next() {
            Some('n') => output.push('\n'),
            Some('r') => output.push('\r'),
            Some('t') => output.push('\t'),
            Some('"') => output.push('"'),
            Some('\\') => output.push('\\'),
            Some(other) => {
                output.push('\\');
                output.push(other);
            }
            None => output.push('\\'),
        }
    }

    output
}

fn is_valid_env_key(key: &str) -> bool {
    let mut chars = key.chars();
    let Some(first) = chars.next() else {
        return false;
    };

    if !first.is_ascii_alphabetic() && first != '_' {
        return false;
    }

    chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
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

fn run_employee_stats(args: EmployeeStatsArgs) -> Result<(), Box<dyn std::error::Error>> {
    let config = args.config();
    let mut store = EmployeeStatsStore::open(&config)?;
    let report = match args.action {
        EmployeeStatsAction::Show => store.load_report(&args.selector)?,
        EmployeeStatsAction::Rebuild => {
            rebuild_employee_stats(&mut store, &args.selector, &config)?
        }
        EmployeeStatsAction::Refresh => {
            let client = build_client(args.proxy.clone());
            let mut employee = EmployeeIdentity::new(args.selector.clone());
            employee.display_name = args.display_name.clone();
            employee.username = args.username.clone();
            employee.primary_domain = args.primary_domain.clone();
            employee.keywords = args.keywords.clone();
            employee.primary_domain_source = if employee.primary_domain.is_empty() {
                "unknown".to_owned()
            } else {
                "provided".to_owned()
            };
            refresh_employee_stats(&client, &mut store, employee, &config, args.selection)?
        }
    };

    print_employee_stats_report(&report, &args)?;
    Ok(())
}

fn print_employee_stats_report(
    report: &EmployeeStatsReport,
    args: &EmployeeStatsArgs,
) -> Result<(), Box<dyn std::error::Error>> {
    let window_days = report.window_days;
    match args.output_format {
        EmployeeStatsOutputFormat::Text => {
            println!("{}", render_employee_stats_text(report, window_days));
        }
        EmployeeStatsOutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(report)?);
        }
        EmployeeStatsOutputFormat::CompactJson => {
            println!(
                "{}",
                serde_json::to_string_pretty(&compact_report_value(report, window_days))?
            );
        }
        EmployeeStatsOutputFormat::Markdown => {
            println!("{}", render_employee_stats_markdown(report, window_days));
        }
    }
    Ok(())
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

fn run_recruit_employees(args: RecruitArgs) -> Result<(), Box<dyn std::error::Error>> {
    let client = build_client(args.proxy.clone());
    let mut config = TapeRecruitmentConfig {
        trade_pages: args.trade_pages,
        trade_page_size: args.trade_page_size,
        lookahead_seconds: args.lookahead_minutes.saturating_mul(60),
        min_copy_window_seconds: args.min_copy_window_seconds,
        min_entry_price: args.min_entry_price,
        max_entry_price: args.max_entry_price,
        min_source_notional_usd: args.min_source_notional,
        max_source_notional_usd: args.max_source_notional,
        min_later_trades: args.min_later_trades,
        min_tape_move: args.min_tape_move,
        min_candidate_trades: args.min_candidate_trades,
        min_candidate_score: args.min_candidate_score,
        top: args.top,
        include_fast_markets: args.include_fast_markets,
        wallet_vetting: args.wallet_vetting,
        closed_position_pages: args.closed_pages,
        min_wallet_closed_positions: args.min_wallet_closed_positions,
        min_wallet_realized_pnl_usd: args.min_wallet_pnl,
        min_wallet_realized_roi: args.min_wallet_roi,
        min_wallet_recent_pnl_usd: args.min_wallet_recent_pnl,
        min_wallet_recent_roi: args.min_wallet_recent_roi,
        max_wallet_current_loss_usd: args.max_current_loss,
        max_wallet_current_loss_ratio: args.max_current_loss_ratio,
        max_wallet_two_sided_condition_ratio: args.max_two_sided_ratio,
        ..TapeRecruitmentConfig::default()
    };

    if let Some(exclude_keywords) = args.exclude_keywords {
        config.exclude_keywords = exclude_keywords;
    }
    if let Some(reject_wallets) = args.reject_wallets {
        config.rejected_wallets = reject_wallets;
    }

    if !args.domains.is_empty() {
        let requested = args
            .domains
            .iter()
            .map(|domain| domain.trim().to_uppercase())
            .filter(|domain| !domain.is_empty())
            .collect::<Vec<_>>();
        config.domains = default_recruitment_domains()
            .into_iter()
            .filter(|domain| requested.iter().any(|requested| requested == &domain.name))
            .collect();

        if config.domains.is_empty() {
            return Err(format!(
                "no known recruitment domains matched: {}",
                requested.join(",")
            )
            .into());
        }
    }

    let report = recruit_from_tape(&client, &config);

    if args.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print_tape_recruitment_report(&report);
    }

    Ok(())
}

fn run_leaderboard_recruitment(
    args: LeaderboardRecruitArgs,
) -> Result<(), Box<dyn std::error::Error>> {
    let domain = args.domain.trim().to_uppercase();
    if !ROTATING_RECRUITMENT_DOMAINS.contains(&domain.as_str()) {
        return Err(format!(
            "unsupported recruitment domain {domain}; expected one of {}",
            ROTATING_RECRUITMENT_DOMAINS.join(",")
        )
        .into());
    }

    let client = build_client(args.proxy);
    let report = scan_leaderboard_employees(
        &client,
        &LeaderboardRecruitmentConfig {
            domain,
            periods: args
                .periods
                .iter()
                .map(|period| period.trim().to_uppercase())
                .filter(|period| !period.is_empty())
                .collect(),
            leaderboard_depth: args.leaderboard_depth,
            wallet_limit: args.wallet_limit,
            history_window_days: args.history_days,
            closed_position_pages: args.closed_pages,
            pause_between_wallets_ms: args.pause_ms,
            top: args.top,
            min_lifetime_pnl_usd: args.min_lifetime_pnl,
            max_lifetime_pnl_usd: args.max_lifetime_pnl,
            min_monthly_positions: args.min_monthly_positions,
            max_monthly_positions: args.max_monthly_positions,
            min_domain_14d_positions: args.min_domain_positions,
            min_domain_14d_roi: args.min_domain_roi,
            min_domain_gross_profit_share: args.min_domain_profit_share,
            max_top5_profit_share: args.max_top5_profit_share,
            max_high_price_profit_share: args.max_high_price_profit_share,
            max_active_loss_usd: args.max_active_loss,
            max_active_loss_ratio: args.max_active_loss_ratio,
            ..LeaderboardRecruitmentConfig::default()
        },
    );

    if args.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print_leaderboard_recruitment_report(&report);
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
        auto_copy: if args.auto_copy.enabled {
            Some(args.auto_copy.clone())
        } else {
            None
        },
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

impl RecruitArgs {
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

            if raw == "--include-fast-markets" {
                args.include_fast_markets = true;
                index += 1;
                continue;
            }

            if raw == "--no-wallet-vetting" {
                args.wallet_vetting = false;
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
                "--domains" => {
                    args.domains = parse_csv(&value);
                }
                "--trade-pages" => args.trade_pages = parse_value(key, &value)?,
                "--trade-page-size" => args.trade_page_size = parse_value(key, &value)?,
                "--lookahead-minutes" => args.lookahead_minutes = parse_value(key, &value)?,
                "--min-copy-window-seconds" => {
                    args.min_copy_window_seconds = parse_value(key, &value)?
                }
                "--min-entry-price" => args.min_entry_price = parse_value(key, &value)?,
                "--max-entry-price" => args.max_entry_price = parse_value(key, &value)?,
                "--min-source-notional" => args.min_source_notional = parse_value(key, &value)?,
                "--max-source-notional" => args.max_source_notional = parse_value(key, &value)?,
                "--min-later-trades" => args.min_later_trades = parse_value(key, &value)?,
                "--min-tape-move" => args.min_tape_move = parse_value(key, &value)?,
                "--min-candidate-trades" => args.min_candidate_trades = parse_value(key, &value)?,
                "--min-candidate-score" => args.min_candidate_score = parse_value(key, &value)?,
                "--top" => args.top = parse_value(key, &value)?,
                "--exclude-keywords" => args.exclude_keywords = Some(parse_csv(&value)),
                "--closed-pages" => args.closed_pages = parse_value(key, &value)?,
                "--min-wallet-closed-positions" => {
                    args.min_wallet_closed_positions = parse_value(key, &value)?
                }
                "--min-wallet-pnl" => args.min_wallet_pnl = parse_value(key, &value)?,
                "--min-wallet-roi" => args.min_wallet_roi = parse_value(key, &value)?,
                "--min-wallet-recent-pnl" => args.min_wallet_recent_pnl = parse_value(key, &value)?,
                "--min-wallet-recent-roi" => args.min_wallet_recent_roi = parse_value(key, &value)?,
                "--max-current-loss" => args.max_current_loss = parse_value(key, &value)?,
                "--max-current-loss-ratio" => {
                    args.max_current_loss_ratio = parse_value(key, &value)?
                }
                "--max-two-sided-ratio" => args.max_two_sided_ratio = parse_value(key, &value)?,
                "--reject-wallets" => args.reject_wallets = Some(parse_csv(&value)),
                "--proxy" => args.proxy = parse_proxy_setting(&value),
                unknown => return Err(format!("unknown argument: {unknown}")),
            }

            index += 1;
        }

        Ok(args)
    }
}

impl LeaderboardRecruitArgs {
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
                "--domain" => args.domain = value,
                "--periods" => args.periods = parse_csv(&value),
                "--leaderboard-depth" => args.leaderboard_depth = parse_value(key, &value)?,
                "--wallet-limit" => args.wallet_limit = parse_value(key, &value)?,
                "--history-days" => args.history_days = parse_value(key, &value)?,
                "--closed-pages" => args.closed_pages = parse_value(key, &value)?,
                "--pause-ms" => args.pause_ms = parse_value(key, &value)?,
                "--top" => args.top = parse_value(key, &value)?,
                "--min-lifetime-pnl" => args.min_lifetime_pnl = parse_value(key, &value)?,
                "--max-lifetime-pnl" => args.max_lifetime_pnl = parse_value(key, &value)?,
                "--min-monthly-positions" => args.min_monthly_positions = parse_value(key, &value)?,
                "--max-monthly-positions" => args.max_monthly_positions = parse_value(key, &value)?,
                "--min-domain-positions" => args.min_domain_positions = parse_value(key, &value)?,
                "--min-domain-roi" => args.min_domain_roi = parse_value(key, &value)?,
                "--min-domain-profit-share" => {
                    args.min_domain_profit_share = parse_value(key, &value)?
                }
                "--max-top5-profit-share" => args.max_top5_profit_share = parse_value(key, &value)?,
                "--max-high-price-profit-share" => {
                    args.max_high_price_profit_share = parse_value(key, &value)?
                }
                "--max-active-loss" => args.max_active_loss = parse_value(key, &value)?,
                "--max-active-loss-ratio" => args.max_active_loss_ratio = parse_value(key, &value)?,
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

            if raw == "--weatherhk-auto-copy" {
                args.auto_copy.enabled = true;
                index += 1;
                continue;
            }

            if raw == "--no-weatherhk-auto-copy" {
                args.auto_copy.enabled = false;
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
                "--weatherhk-auto-copy-mode" => args.auto_copy.mode = AutoCopyMode::parse(&value)?,
                "--weatherhk-auto-copy-exec" => {
                    args.auto_copy.executor_command = Some(value);
                }
                "--weatherhk-state-path" | "--weatherhk-auto-copy-state-path" => {
                    args.auto_copy.state_path = value.into();
                }
                "--weatherhk-strategy-config" | "--weatherhk-auto-copy-strategy-config" => {
                    let path = std::path::PathBuf::from(value);
                    args.auto_copy.strategy_config_path = Some(path.clone());
                    args.auto_copy.strategy =
                        smart_wallet_discovery::autocopy::load_strategy_config_for_path(&path);
                }
                "--weatherhk-max-single-copy" => {
                    args.auto_copy.max_single_copy_usd = parse_value(key, &value)?
                }
                "--weatherhk-max-market-exposure" => {
                    args.auto_copy.max_market_exposure_usd = parse_value(key, &value)?
                }
                "--weatherhk-max-daily-spend" => {
                    args.auto_copy.max_daily_spend_usd = parse_value(key, &value)?
                }
                "--weatherhk-max-daily-loss" => {
                    args.auto_copy.max_daily_loss_usd = parse_value(key, &value)?
                }
                "--weatherhk-max-chase-pct" => {
                    args.auto_copy.max_chase_pct = parse_value(key, &value)?
                }
                "--weatherhk-passive-offset-pct" => {
                    args.auto_copy.passive_offset_pct = parse_value(key, &value)?
                }
                "--weatherhk-max-chase-delta" => {
                    args.auto_copy.max_chase_delta = parse_value(key, &value)?
                }
                "--weatherhk-passive-offset" => {
                    args.auto_copy.passive_offset = parse_value(key, &value)?
                }
                "--weatherhk-buy-take-enabled" => {
                    args.auto_copy.buy_take_enabled = parse_value(key, &value)?
                }
                "--weatherhk-min-buy-source-notional" => {
                    args.auto_copy.min_buy_source_notional_usd = parse_value(key, &value)?
                }
                "--weatherhk-skip-buy-price-at-or-above" => {
                    args.auto_copy.skip_buy_price_at_or_above = parse_value(key, &value)?
                }
                "--weatherhk-skip-buy-price-at-or-below" => {
                    args.auto_copy.skip_buy_price_at_or_below = parse_value(key, &value)?
                }
                "--weatherhk-min-sell-sync-notional" => {
                    args.auto_copy.min_sell_sync_notional_usd = parse_value(key, &value)?
                }
                "--weatherhk-passive-ttl" => {
                    args.auto_copy.passive_order_ttl_seconds = parse_value(key, &value)?
                }
                "--weatherhk-pending-sync-seconds" => {
                    args.auto_copy.pending_sync_seconds = parse_value(key, &value)?
                }
                "--weatherhk-sell-fraction" | "--weatherhk-clear-sell-notional" => {
                    let _: f64 = parse_value(key, &value)?;
                    eprintln!(
                        "{key} is deprecated and ignored; WeatherHK SELL now clears the tracked outcome position."
                    );
                }
                "--proxy" => args.proxy = parse_proxy_setting(&value),
                unknown => return Err(format!("unknown argument: {unknown}")),
            }

            index += 1;
        }

        args.auto_copy.validate()?;
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

fn parse_csv(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(|item| item.trim().to_owned())
        .filter(|item| !item.is_empty())
        .collect()
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

fn print_tape_recruitment_report(report: &TapeRecruitmentReport) {
    println!(
        "Tape recruitment, generated_at={}, scanned_trades={}, evaluated={}, qualified={}, selected={}",
        report.generated_at_secs,
        report.scanned_trades,
        report.evaluated_trades,
        report.qualified_trades,
        report.candidates.len()
    );

    println!(
        "{:<10} {:<42} {:<18} {:>7} {:>9} {:>8} {:>8} {:>8} {:>9} {:<20}",
        "domain",
        "wallet",
        "name",
        "score",
        "good/eval",
        "win%",
        "entry",
        "move",
        "median$",
        "last_seen"
    );

    for candidate in &report.candidates {
        print_tape_candidate(candidate);
    }

    if !report.rejected_candidates.is_empty() {
        println!();
        println!(
            "Rejected by wallet health: {} candidate(s)",
            report.rejected_candidates.len()
        );
        for rejected in report.rejected_candidates.iter().take(5) {
            println!(
                "- {} {} score={:.1} good/eval={}/{}: {}",
                rejected.domain,
                rejected.wallet,
                rejected.score,
                rejected.qualified_trades,
                rejected.evaluated_trades,
                rejected.reasons.join("; ")
            );
        }
    }

    if !report.warnings.is_empty() {
        println!();
        println!("Warnings:");
        for warning in &report.warnings {
            println!("- {warning}");
        }
    }
}

fn print_leaderboard_recruitment_report(report: &LeaderboardRecruitmentReport) {
    println!(
        "Leaderboard recruitment: domain={} pool={} scanned={} eligible={} selected={}",
        report.domain,
        report.pool_wallets,
        report.scanned_wallets,
        report.eligible_wallets,
        report.candidates.len()
    );
    println!(
        "{:<18} {:<42} {:>6} {:>10} {:>7} {:>9} {:>8} {:>9} {:>8} {:>8} {:>8} {:>9}",
        "name",
        "wallet",
        "score",
        "all_pnl",
        "30d_n",
        "14d_pnl",
        "14d_roi",
        "domain14",
        "focus",
        "top5",
        ">=80c",
        "act_loss"
    );

    for candidate in &report.candidates {
        println!(
            "{:<18} {:<42} {:>6.1} {:>10.2} {:>7} {:>9.2} {:>7.1}% {:>9.2} {:>7.1}% {:>7.1}% {:>7.1}% {:>9.2}",
            truncate(candidate.user_name.as_deref().unwrap_or("-"), 18),
            candidate.wallet,
            candidate.score,
            candidate.lifetime_pnl_usd.unwrap_or(0.0),
            candidate.total_30d.positions,
            candidate.total_14d.pnl_usd,
            candidate.domain_14d.roi * 100.0,
            candidate.domain_14d.pnl_usd,
            candidate.domain_gross_profit_share_14d * 100.0,
            candidate.top5_profit_share_14d * 100.0,
            candidate.high_price_profit_share_14d * 100.0,
            candidate.active_loss_usd,
        );
        println!(
            "  ranks={} 1/7/14/30 total={:.2}/{:.2}/{:.2}/{:.2} domain={:.2}/{:.2}/{:.2}/{:.2}",
            candidate
                .ranks
                .iter()
                .map(|(period, rank)| format!("{period}:{rank}"))
                .collect::<Vec<_>>()
                .join(","),
            candidate.total_1d.pnl_usd,
            candidate.total_7d.pnl_usd,
            candidate.total_14d.pnl_usd,
            candidate.total_30d.pnl_usd,
            candidate.domain_1d.pnl_usd,
            candidate.domain_7d.pnl_usd,
            candidate.domain_14d.pnl_usd,
            candidate.domain_30d.pnl_usd,
        );
    }

    if !report.near_misses.is_empty() {
        println!();
        println!("Near misses:");
        for candidate in report.near_misses.iter().take(10) {
            println!(
                "- {} {} score={:.1} domain14=${:.2} roi={:.1}% flags={}",
                candidate.user_name.as_deref().unwrap_or("-"),
                candidate.wallet,
                candidate.score,
                candidate.domain_14d.pnl_usd,
                candidate.domain_14d.roi * 100.0,
                candidate.flags.join(",")
            );
        }
    }

    if !report.warnings.is_empty() {
        println!();
        println!("Warnings: {}", report.warnings.len());
        for warning in report.warnings.iter().take(10) {
            println!("- {warning}");
        }
    }
}

fn print_tape_candidate(candidate: &TapeEmployeeCandidate) {
    let name = candidate
        .name
        .as_deref()
        .or(candidate.pseudonym.as_deref())
        .unwrap_or("-");
    let last_seen = candidate
        .last_seen_secs
        .map(|timestamp| timestamp.to_string())
        .unwrap_or_else(|| "-".to_owned());

    println!(
        "{:<10} {:<42} {:<18} {:>7.1} {:>4}/{:<4} {:>7.1}% {:>7.2}c {:>7.2}c {:>9.2} {:<20}",
        candidate.domain,
        candidate.wallet,
        truncate(name, 18),
        candidate.score,
        candidate.qualified_trades,
        candidate.evaluated_trades,
        candidate.positive_move_rate * 100.0,
        candidate.avg_entry_price * 100.0,
        candidate.avg_tape_move * 100.0,
        candidate.median_source_notional_usd,
        last_seen
    );
    println!("  watch_spec: {}", candidate.watch_spec);

    for reason in candidate.reasons.iter().take(3) {
        println!("  + {reason}");
    }
    if let Some(health) = &candidate.wallet_health {
        println!(
            "  health: pnl=${:.2}, roi={:.1}%, recent_{}d=${:.2}, cur_loss=${:.2}, cur_loss_ratio={:.1}%",
            health.realized_pnl_usd,
            health.realized_roi * 100.0,
            health.recent_window_days,
            health.recent_pnl_usd,
            health.current_loss_usd,
            health.current_loss_ratio * 100.0
        );
    }
    for caution in candidate.cautions.iter().take(2) {
        println!("  ! {caution}");
    }

    for example in candidate.examples.iter().take(2) {
        println!(
            "  example: {} @ {:.2}c -> {:.2}c (+{:.2}c, {}s, ${:.2}) {}",
            example.outcome.as_deref().unwrap_or("-"),
            example.price * 100.0,
            example.later_price * 100.0,
            example.tape_move * 100.0,
            example.seconds_to_later_price,
            example.notional_usd,
            truncate(example.title.as_deref().unwrap_or("-"), 64)
        );
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

#[cfg(test)]
mod employee_stats_cli_tests {
    use super::*;

    #[test]
    fn parses_address_only_deep_refresh() {
        let raw = vec![
            "refresh".to_owned(),
            "--wallet".to_owned(),
            "0x1111111111111111111111111111111111111111".to_owned(),
            "--compact-json".to_owned(),
        ];
        let args = EmployeeStatsArgs::parse(&raw).unwrap();
        assert!(matches!(args.action, EmployeeStatsAction::Refresh));
        assert!(matches!(
            args.output_format,
            EmployeeStatsOutputFormat::CompactJson
        ));
        assert!(args.selection.activity);
        assert!(args.selection.trades);
        assert!(args.selection.closed_positions);
        assert!(args.selection.positions);
    }

    #[test]
    fn parses_employee_spec_and_position_only_refresh() {
        let raw = vec![
            "refresh".to_owned(),
            "--employee".to_owned(),
            "0x1111111111111111111111111111111111111111:Weather:WEATHER:weather|temperature"
                .to_owned(),
            "--only".to_owned(),
            "positions".to_owned(),
        ];
        let args = EmployeeStatsArgs::parse(&raw).unwrap();
        assert_eq!(args.primary_domain, "WEATHER");
        assert_eq!(args.keywords, vec!["weather", "temperature"]);
        assert!(!args.selection.activity);
        assert!(!args.selection.trades);
        assert!(!args.selection.closed_positions);
        assert!(args.selection.positions);
    }
}
