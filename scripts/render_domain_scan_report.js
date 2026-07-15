#!/usr/bin/env node

const fs = require("fs");
const path = require("path");

function parseArgs(argv) {
  const args = {};
  for (let index = 0; index < argv.length; index += 1) {
    const key = argv[index];
    if (!key.startsWith("--")) continue;
    const value = argv[index + 1];
    if (!value || value.startsWith("--")) {
      args[key.slice(2)] = true;
    } else {
      args[key.slice(2)] = value;
      index += 1;
    }
  }
  return args;
}

function money(value) {
  return Number(value || 0).toFixed(2);
}

function optionalMoney(value) {
  return value === null || value === undefined ? "n/a" : `$${money(value)}`;
}

function percent(value) {
  return `${(Number(value || 0) * 100).toFixed(1)}%`;
}

function rankText(ranks) {
  return Object.entries(ranks || {})
    .map(([period, rank]) => `${period}:${rank}`)
    .join(" / ");
}

function shortWallet(wallet) {
  if (!wallet || wallet.length < 12) return wallet || "unknown";
  return `${wallet.slice(0, 6)}...${wallet.slice(-4)}`;
}

function cell(value) {
  return String(value ?? "-")
    .replace(/\r?\n/g, " ")
    .replace(/\|/g, "\\|")
    .trim();
}

function looksLikeGeneratedName(name, wallet) {
  const trimmed = String(name || "").trim();
  if (!trimmed) return true;

  const lowerName = trimmed.toLowerCase();
  const lowerWallet = String(wallet || "").toLowerCase();
  if (lowerName === "anonymous") return false;
  if (lowerWallet && (lowerName === lowerWallet || lowerName.startsWith(`${lowerWallet}-`))) {
    return true;
  }

  return /^0x[a-z0-9]{16,}(?:[-_].*)?$/i.test(trimmed);
}

function displayName(candidate) {
  const rawName = candidate.user_name || "";
  if (looksLikeGeneratedName(rawName, candidate.wallet)) {
    return `anonymous (${shortWallet(candidate.wallet)})`;
  }
  return cell(rawName);
}

function tagsText(candidate) {
  return cell((candidate.candidate_tags || []).join(", ") || "-");
}

function topSpecialtyText(candidate) {
  const segment = (candidate.specialty_segments || [])[0];
  if (!segment) return "-";
  return cell(`${segment.name}: $${money(segment.pnl_usd_30d)} / ${percent(
    segment.roi_30d
  )} / n=${segment.positions_30d} / avg $${money(segment.avg_position_usd_30d)} / max $${money(
    segment.max_position_usd_30d
  )}`);
}

function localTimestamp(timestampSeconds) {
  const date = new Date(timestampSeconds * 1000);
  const parts = new Intl.DateTimeFormat("en-CA", {
    timeZone: "Asia/Shanghai",
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
    hourCycle: "h23",
  }).formatToParts(date);
  const values = Object.fromEntries(parts.map((part) => [part.type, part.value]));
  return `${values.year}-${values.month}-${values.day} ${values.hour}:${values.minute}:${values.second} CST`;
}

function profileLink(wallet) {
  return `[\`${wallet}\`](https://polymarket.com/profile/${wallet})`;
}

function candidateRow(candidate) {
  const name = displayName(candidate);
  return `| ${name} | ${profileLink(candidate.wallet)} | ${candidate.score.toFixed(1)} | ${rankText(
    candidate.ranks
  )} | ${tagsText(candidate)} | ${topSpecialtyText(candidate)} | ${optionalMoney(
    candidate.lifetime_pnl_usd
  )} | ${
    candidate.total_30d?.positions || 0
  } | $${money(candidate.total_7d.pnl_usd)} / $${money(
    candidate.total_14d.pnl_usd
  )} / $${money(candidate.total_30d?.pnl_usd)} | $${money(
    candidate.domain_7d.pnl_usd
  )} / $${money(
    candidate.domain_14d.pnl_usd
  )} / $${money(candidate.domain_30d?.pnl_usd)} | ${percent(
    candidate.domain_14d.roi
  )} / ${percent(candidate.domain_30d?.roi)} | ${percent(
    candidate.domain_gross_profit_share_14d
  )} | ${percent(candidate.top5_profit_share_14d)} | ${percent(
    candidate.high_price_profit_share_14d
  )} | ${percent(candidate.ultra_fast_position_share_14d)} / ${percent(
    candidate.ultra_fast_gross_profit_share_14d
  )} | $${money(candidate.active_loss_usd)} / ${percent(candidate.active_loss_ratio)} |`;
}

function nearMissRow(candidate) {
  const name = displayName(candidate);
  return `| ${name} | ${profileLink(candidate.wallet)} | ${candidate.score.toFixed(
    1
  )} | ${tagsText(candidate)} | ${topSpecialtyText(candidate)} | ${optionalMoney(
    candidate.lifetime_pnl_usd
  )} | ${
    candidate.total_30d?.positions || 0
  } | $${money(
    candidate.domain_14d.pnl_usd
  )} | ${percent(candidate.domain_14d.roi)} | ${cell(candidate.flags.join(", "))} |`;
}

function ultraFastRow(candidate) {
  const name = displayName(candidate);
  return `| ${name} | ${profileLink(candidate.wallet)} | ${candidate.ultra_fast_14d.positions} | $${money(
    candidate.ultra_fast_14d.pnl_usd
  )} | ${percent(candidate.ultra_fast_14d.roi)} | ${percent(
    candidate.ultra_fast_position_share_14d
  )} | ${percent(candidate.ultra_fast_gross_profit_share_14d)} | ${percent(
    candidate.ultra_fast_invested_share_14d
  )} |`;
}

function specialRow(candidate) {
  const name = displayName(candidate);
  return `| ${name} | ${profileLink(candidate.wallet)} | ${candidate.score.toFixed(
    1
  )} | ${tagsText(candidate)} | ${topSpecialtyText(candidate)} | ${optionalMoney(
    candidate.lifetime_pnl_usd
  )} | ${
    candidate.total_30d?.positions || 0
  } | $${money(candidate.domain_30d?.pnl_usd)} | ${percent(
    candidate.domain_30d?.roi
  )} | ${percent(candidate.top5_profit_share_14d)} | ${percent(
    candidate.high_price_profit_share_14d
  )} | ${cell(candidate.flags.join(", "))} |`;
}

const args = parseArgs(process.argv.slice(2));
if (!args.input || !args.output) {
  throw new Error("usage: render_domain_scan_report.js --input report.json --output report.md");
}

const report = JSON.parse(fs.readFileSync(args.input, "utf8"));
const generatedAt = localTimestamp(report.generated_at_secs);
const lines = [
  `## ${generatedAt.slice(0, 10)} ${report.domain}`,
  "",
  `Generated: ${generatedAt}`,
  "",
  `Leaderboard periods: ${report.periods.join(", ")}. Depth per period: ${report.leaderboard_depth}. Pool: ${report.pool_wallets}. Deep-scan target: ${report.wallet_limit}. Scanned: ${report.scanned_wallets}. Eligible: ${report.eligible_wallets}. History window: ${report.history_window_days || 30}d.`,
  "",
  "V3 focuses on WEATHER, CRYPTO, and SPORTS only. WEATHER allows active daily-weather specialists and rising stars. CRYPTO ordinary candidates exclude 5/10/15-minute markets and use tighter concentration/high-price filters. SPORTS can qualify through league-specialist or followable-whale profiles when entries are not dominated by late high-probability buys.",
  "",
  ...(report.domain === "CRYPTO"
    ? [
        "CRYPTO ordinary-employee metrics exclude 5/10/15-minute markets. Ultra-fast results are diagnostics only and require delayed-copy validation.",
        "",
      ]
    : []),
  "### Candidates",
  "",
];

if (report.candidates.length === 0) {
  lines.push("No wallet passed every filter in this run.", "");
} else {
  lines.push(
    "| Name | Wallet | Score | Ranks | Tags | Top specialty | ALL PnL | 30d positions | Total PnL 7d / 14d / 30d | Domain PnL 7d / 14d / 30d | Domain ROI 14d / 30d | Domain profit share | Top-5 share | >=80c profit share | Ultra-fast positions / profit | Active loss / ratio |",
    "|---|---|---:|---|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|"
  );
  for (const candidate of report.candidates) lines.push(candidateRow(candidate));
  lines.push("");
}

if ((report.special_observations || []).length > 0) {
  lines.push(
    "### Special Manual Review",
    "",
    "These wallets are not ordinary employee candidates, but their ROI, frequency, or payoff shape is unusual enough to keep for manual review.",
    "",
    "| Name | Wallet | Score | Tags | Top specialty | ALL PnL | 30d positions | Domain PnL 30d | Domain ROI 30d | Top-5 share | >=80c profit share | Failed filters |",
    "|---|---|---:|---|---|---:|---:|---:|---:|---:|---:|---|"
  );
  for (const candidate of report.special_observations) {
    lines.push(specialRow(candidate));
  }
  lines.push("");
}

if ((report.ultra_fast_observations || []).length > 0) {
  lines.push(
    "### Ultra-Fast Observation Only",
    "",
    "These wallets are not ordinary employee candidates. They need entry-timing and 30/60-second delayed-copy backtests before any promotion.",
    "",
    "| Name | Wallet | <=15m positions | <=15m PnL | <=15m ROI | Position share | Gross-profit share | Invested share |",
    "|---|---|---:|---:|---:|---:|---:|---:|"
  );
  for (const candidate of report.ultra_fast_observations) {
    lines.push(ultraFastRow(candidate));
  }
  lines.push("");
}

lines.push("### Near Misses", "");
if (report.near_misses.length === 0) {
  lines.push("None.", "");
} else {
  lines.push(
    "| Name | Wallet | Score | Tags | Top specialty | ALL PnL | 30d positions | Domain PnL 14d | Domain ROI 14d | Failed filters |",
    "|---|---|---:|---|---|---:|---:|---:|---:|---|"
  );
  for (const candidate of report.near_misses.slice(0, 10)) {
    lines.push(nearMissRow(candidate));
  }
  lines.push("");
}

if (report.warnings.length > 0) {
  lines.push("### Warnings", "");
  for (const warning of report.warnings.slice(0, 20)) lines.push(`- ${warning}`);
  lines.push("");
}

fs.mkdirSync(path.dirname(args.output), { recursive: true });
if (!fs.existsSync(args.output)) {
  fs.writeFileSync(
    args.output,
    "# Employee Discovery V3 Daily Report\n\nThis report rotates only WEATHER, CRYPTO, and SPORTS. Each domain uses a separate profile instead of one uniform employee filter.\n\n"
  );
}
const section = `${lines.join("\n")}\n`;
const existing = fs.readFileSync(args.output, "utf8");
const heading = `## ${generatedAt.slice(0, 10)} ${report.domain}`;
const sectionStart = existing.indexOf(heading);

if (sectionStart < 0) {
  fs.appendFileSync(args.output, section);
} else {
  const nextSection = existing.indexOf("\n## ", sectionStart + heading.length);
  const before = existing.slice(0, sectionStart).trimEnd();
  const after = nextSection < 0 ? "" : existing.slice(nextSection + 1).trimStart();
  const updated = [before, section.trimEnd(), after]
    .filter((part) => part.length > 0)
    .join("\n\n");
  fs.writeFileSync(args.output, `${updated}\n`);
}
