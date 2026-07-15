#!/usr/bin/env node

const fs = require("fs");
const path = require("path");

const args = parseArgs(process.argv.slice(2));
const dir = args.dir || "logs/recruitment";
const outputPath = args.output || null;
const hours = numberArg(args.hours, 24);
const top = numberArg(args.top, 20);
const minRuns = numberArg(args["min-runs"], 1);
const minQualified = numberArg(args["min-qualified"], 2);
const minScore = numberArg(args["min-score"], 0);
const allowUnvetted = ["1", "true", "yes"].includes(
  String(args["allow-unvetted"] || "").toLowerCase()
);
const rejectedWallets = new Set(
  [
    "0x06dc51826bc524d9a83770e7de9dd7e005b04524",
    ...csvArg(args["reject-wallets"]),
  ].map((wallet) => wallet.toLowerCase())
);
const domainKeywords = {
  WEATHER: ["weather", "temperature", "hurricane", "storm", "rain", "snow"],
  TECH: ["ai", "gemini", "llm", "model", "arena", "openai", "anthropic", "xai", "grok", "google"],
  FINANCE: ["fed", "rates", "cpi", "gdp", "oil", "wti", "gold", "stock", "inflation"],
  SPORTS: ["nba", "nfl", "mlb", "nhl", "ufc", "soccer", "tennis", "championship"],
  CRYPTO: ["bitcoin", "btc", "ethereum", "eth", "solana", "sol", "crypto", "etf"],
  POLITICS: ["trump", "biden", "election", "senate", "house", "president", "china"],
  CULTURE: ["twitter", "tweet", "album", "movie", "celebrity", "streaming"],
  ECONOMICS: ["cpi", "fed", "gdp", "inflation", "unemployment", "jobs", "tariff"],
};
const nowSecs = Math.floor(Date.now() / 1000);
const sinceSecs = nowSecs - hours * 3600;
const lines = [];

const reports = loadReports(dir).filter((report) => {
  const generated = Number(report.generated_at_secs || 0);
  return generated >= sinceSecs;
});
const rejections = aggregateRejections(reports).slice(0, 8);
const candidates = aggregateCandidates(reports)
  .filter((candidate) => candidate.runs >= minRuns)
  .filter((candidate) => candidate.totalQualified >= minQualified)
  .filter((candidate) => candidate.maxScore >= minScore)
  .sort((left, right) => {
    return (
      right.finalScore - left.finalScore ||
      right.totalQualified - left.totalQualified ||
      right.runs - left.runs ||
      right.avgTapeMove - left.avgTapeMove
    );
  })
  .slice(0, top);

emit(
  [
    `Recruitment summary: dir=${dir}`,
    `hours=${hours}`,
    `reports=${reports.length}`,
    `selected=${candidates.length}`,
    `rejected=${rejections.reduce((sum, row) => sum + row.runs, 0)}`,
  ].join(" ")
);

if (candidates.length === 0) {
  emit(
    "No repeated candidates met the summary thresholds. Lower --min-qualified or wait for more hourly samples."
  );
  emitRecentRejections(rejections);
  writeOutput();
  process.exit(0);
}

emit(
  pad("domain", 10),
  pad("wallet", 42),
  pad("name", 18),
  leftPad("score", 7),
  leftPad("runs", 5),
  leftPad("good/eval", 10),
  leftPad("max", 7),
  leftPad("move", 8),
  leftPad("median$", 9),
  "last_seen"
);

for (const candidate of candidates) {
  emit(
    pad(candidate.domain, 10),
    pad(candidate.wallet, 42),
    pad(candidate.name || candidate.pseudonym || "-", 18),
    leftPad(candidate.finalScore.toFixed(1), 7),
    leftPad(String(candidate.runs), 5),
    leftPad(`${candidate.totalQualified}/${candidate.totalEvaluated}`, 10),
    leftPad(candidate.maxScore.toFixed(1), 7),
    leftPad(`${(candidate.avgTapeMove * 100).toFixed(2)}c`, 8),
    leftPad(candidate.medianNotional.toFixed(2), 9),
    candidate.lastSeen ? String(candidate.lastSeen) : "-"
  );
  emit(`  watch_spec: ${candidate.watchSpec}`);
  if (candidate.health) {
    emit(
      `  health: pnl=$${money(candidate.health.realized_pnl_usd)}, roi=${pctRate(
        candidate.health.realized_roi
      )}, recent_${candidate.health.recent_window_days || 0}d=$${money(
        candidate.health.recent_pnl_usd
      )}, cur_loss=$${money(candidate.health.current_loss_usd)}`
    );
  }
  for (const example of candidate.examples.slice(0, 2)) {
    emit(
      `  example: ${example.outcome || "-"} @ ${pct(example.price)} -> ${pct(
        example.later_price
      )} (+${pct(example.tape_move)}, ${example.seconds_to_later_price || 0}s, $${
        example.notional_usd || 0
      }) ${truncate(example.title || "-", 72)}`
    );
  }
}

emitRecentRejections(rejections);

writeOutput();

function parseArgs(raw) {
  const result = {};
  for (let index = 0; index < raw.length; index += 1) {
    const item = raw[index];
    if (!item.startsWith("--")) {
      continue;
    }
    const [key, inline] = item.slice(2).split("=", 2);
    if (inline !== undefined) {
      result[key] = inline;
    } else {
      result[key] = raw[index + 1];
      index += 1;
    }
  }
  return result;
}

function numberArg(value, fallback) {
  const parsed = Number(value);
  return Number.isFinite(parsed) ? parsed : fallback;
}

function csvArg(value) {
  if (!value) {
    return [];
  }

  return String(value)
    .split(",")
    .map((item) => item.trim())
    .filter(Boolean);
}

function loadReports(dirPath) {
  if (!fs.existsSync(dirPath)) {
    return [];
  }

  return fs
    .readdirSync(dirPath)
    .filter((file) => file.endsWith(".json"))
    .map((file) => path.join(dirPath, file))
    .flatMap((filePath) => {
      try {
        return [JSON.parse(fs.readFileSync(filePath, "utf8"))];
      } catch (error) {
        console.error(`failed to read ${filePath}: ${error.message}`);
        return [];
      }
    });
}

function aggregateCandidates(reports) {
  const byKey = new Map();

  for (const report of reports) {
    for (const candidate of report.candidates || []) {
      if (rejectedWallets.has(String(candidate.wallet || "").toLowerCase())) {
        continue;
      }
      if (!allowUnvetted && !candidate.wallet_health) {
        continue;
      }
      if (!candidateHasDomainEvidence(candidate)) {
        continue;
      }

      const key = `${String(candidate.wallet || "").toLowerCase()}::${candidate.domain || ""}`;
      if (!byKey.has(key)) {
        byKey.set(key, {
          wallet: candidate.wallet || "-",
          name: candidate.name || null,
          pseudonym: candidate.pseudonym || null,
          domain: candidate.domain || "-",
          watchSpec: candidate.watch_spec || "-",
          runs: 0,
          scoreSum: 0,
          maxScore: 0,
          totalQualified: 0,
          totalEvaluated: 0,
          tapeMoveSum: 0,
          tapeMoveWeight: 0,
          notionals: [],
          lastSeen: null,
          health: null,
          examples: [],
        });
      }

      const row = byKey.get(key);
      const score = Number(candidate.score || 0);
      const qualified = Number(candidate.qualified_trades || 0);
      const evaluated = Number(candidate.evaluated_trades || 0);

      row.runs += 1;
      row.scoreSum += score;
      row.maxScore = Math.max(row.maxScore, score);
      row.totalQualified += qualified;
      row.totalEvaluated += evaluated;
      row.tapeMoveSum += Number(candidate.avg_tape_move || 0) * Math.max(qualified, 1);
      row.tapeMoveWeight += Math.max(qualified, 1);
      row.notionals.push(Number(candidate.median_source_notional_usd || 0));
      row.lastSeen = Math.max(row.lastSeen || 0, Number(candidate.last_seen_secs || 0));
      if (score >= row.maxScore) {
        row.watchSpec = candidate.watch_spec || row.watchSpec;
        row.health = candidate.wallet_health || row.health;
      }
      row.examples.push(...(candidate.examples || []));
    }
  }

  return [...byKey.values()].map((row) => {
    row.avgScore = row.runs > 0 ? row.scoreSum / row.runs : 0;
    row.avgTapeMove = row.tapeMoveWeight > 0 ? row.tapeMoveSum / row.tapeMoveWeight : 0;
    row.medianNotional = median(row.notionals);
    row.finalScore = finalScore(row);
    row.examples = row.examples
      .sort((left, right) => Number(right.quality_score || 0) - Number(left.quality_score || 0))
      .slice(0, 3);
    return row;
  });
}

function aggregateRejections(reports) {
  const byKey = new Map();

  for (const report of reports) {
    for (const rejected of report.rejected_candidates || []) {
      const key = `${String(rejected.wallet || "").toLowerCase()}::${rejected.domain || ""}`;
      if (!byKey.has(key)) {
        byKey.set(key, {
          wallet: rejected.wallet || "-",
          name: rejected.name || rejected.pseudonym || "-",
          domain: rejected.domain || "-",
          runs: 0,
          maxScore: 0,
          reasons: new Map(),
          health: null,
        });
      }

      const row = byKey.get(key);
      row.runs += 1;
      row.maxScore = Math.max(row.maxScore, Number(rejected.score || 0));
      row.health = rejected.wallet_health || row.health;
      for (const reason of rejected.reasons || []) {
        row.reasons.set(reason, (row.reasons.get(reason) || 0) + 1);
      }
    }
  }

  return [...byKey.values()].sort((left, right) => {
    return right.runs - left.runs || right.maxScore - left.maxScore;
  });
}

function candidateHasDomainEvidence(candidate) {
  const keywords = candidate.keywords || domainKeywords[candidate.domain] || [];
  if (keywords.length === 0) {
    return true;
  }

  const text = (candidate.examples || [])
    .map((example) =>
      [example.title, example.slug, example.event_slug, example.outcome].filter(Boolean).join(" ")
    )
    .join(" ")
    .toLowerCase();

  if (!text.trim()) {
    return true;
  }

  return matchesAnyKeyword(text, keywords);
}

function matchesAnyKeyword(text, keywords) {
  return keywords.some((keyword) => keywordMatchesText(text, String(keyword).toLowerCase().trim()));
}

function keywordMatchesText(text, keyword) {
  if (!keyword) {
    return false;
  }
  if (/\s/.test(keyword)) {
    return text.includes(keyword);
  }

  let searchStart = 0;
  while (searchStart < text.length) {
    const index = text.indexOf(keyword, searchStart);
    if (index === -1) {
      return false;
    }
    const end = index + keyword.length;
    if (isKeywordBoundary(text, index, end)) {
      return true;
    }
    searchStart = end;
  }

  return false;
}

function isKeywordBoundary(text, start, end) {
  return !isKeywordChar(text[start - 1]) && !isKeywordChar(text[end]);
}

function isKeywordChar(ch) {
  return Boolean(ch && /[a-z0-9]/i.test(ch));
}

function emitRecentRejections(rejections) {
  if (rejections.length === 0) {
    return;
  }

  emit("");
  emit("Rejected by wallet health:");
  for (const row of rejections) {
    const reason = [...row.reasons.entries()]
      .sort((left, right) => right[1] - left[1])
      .map(([text, count]) => (count > 1 ? `${text} x${count}` : text))
      .slice(0, 2)
      .join("; ");
    const health = row.health
      ? ` pnl=$${money(row.health.realized_pnl_usd)} roi=${pctRate(
          row.health.realized_roi
        )} cur_loss=$${money(row.health.current_loss_usd)}`
      : "";
    emit(
      `- ${row.domain} ${row.wallet} runs=${row.runs} max_score=${row.maxScore.toFixed(
        1
      )}${health}: ${reason}`
    );
  }
}

function finalScore(row) {
  const repeatScore = Math.min(row.runs / 4, 1) * 15;
  const qualifiedScore = Math.min(row.totalQualified / 8, 1) * 20;
  const scoreComponent = row.avgScore * 0.35 + row.maxScore * 0.30;
  return scoreComponent + repeatScore + qualifiedScore;
}

function median(values) {
  const filtered = values.filter((value) => Number.isFinite(value)).sort((a, b) => a - b);
  if (filtered.length === 0) {
    return 0;
  }
  const mid = Math.floor(filtered.length / 2);
  if (filtered.length % 2 === 0) {
    return (filtered[mid - 1] + filtered[mid]) / 2;
  }
  return filtered[mid];
}

function pct(value) {
  return `${(Number(value || 0) * 100).toFixed(2)}c`;
}

function pctRate(value) {
  return `${(Number(value || 0) * 100).toFixed(1)}%`;
}

function money(value) {
  return Number(value || 0).toFixed(2);
}

function pad(value, width) {
  return truncate(String(value), width).padEnd(width, " ");
}

function leftPad(value, width) {
  return truncate(String(value), width).padStart(width, " ");
}

function truncate(value, width) {
  const text = String(value);
  return text.length > width ? text.slice(0, width) : text;
}

function emit(...parts) {
  const line = parts.join(" ");
  lines.push(line);
  console.log(line);
}

function writeOutput() {
  if (!outputPath) {
    return;
  }

  fs.mkdirSync(path.dirname(outputPath), { recursive: true });
  fs.writeFileSync(
    outputPath,
    [
      "# Employee Recruitment Summary",
      "",
      `Updated: ${new Date().toISOString()}`,
      "",
      "```text",
      ...lines,
      "```",
      "",
    ].join("\n")
  );
}
