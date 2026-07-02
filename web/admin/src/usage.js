export function formatNumber(value) {
  const number = Number(value || 0);
  if (number >= 1_000_000_000) return `${trim(number / 1_000_000_000)}B`;
  if (number >= 1_000_000) return `${trim(number / 1_000_000)}M`;
  if (number >= 1_000) return `${trim(number / 1_000)}K`;
  return new Intl.NumberFormat("en-US").format(number);
}

export function percentText(value) {
  if (value === null || value === undefined || Number.isNaN(Number(value))) return "--";
  return `${Math.round(Number(value) * 100)}%`;
}

export function totalTokens(row) {
  return Number(row?.total_prompt_tokens || 0) + Number(row?.total_completion_tokens || 0);
}

export function reportedTokens(row) {
  return Number(row?.reported_prompt_tokens || 0) + Number(row?.reported_completion_tokens || 0);
}

export function estimatedTokens(row) {
  return Number(row?.estimated_prompt_tokens || 0) + Number(row?.estimated_completion_tokens || 0);
}

export function normalizeUsageRows(rows) {
  return [...(rows || [])].sort((a, b) => {
    const tokenDelta = totalTokens(b) - totalTokens(a);
    if (tokenDelta !== 0) return tokenDelta;
    return Number(b.total_requests || 0) - Number(a.total_requests || 0);
  });
}

export function summarizeDaily(days) {
  const rows = days || [];
  const active = rows.filter((day) => Number(day.total_requests || 0) > 0).length;
  const total = rows.reduce((sum, day) => sum + totalTokens(day), 0);
  const reported = rows.reduce((sum, day) => sum + reportedTokens(day), 0);
  const estimated = rows.reduce((sum, day) => sum + estimatedTokens(day), 0);
  let current = 0;
  for (let index = rows.length - 1; index >= 0; index -= 1) {
    if (Number(rows[index].total_requests || 0) <= 0) break;
    current += 1;
  }
  let longest = 0;
  let streak = 0;
  for (const day of rows) {
    if (Number(day.total_requests || 0) > 0) {
      streak += 1;
      longest = Math.max(longest, streak);
    } else {
      streak = 0;
    }
  }
  return { active, current, estimated, longest, reported, total };
}

export function heatLevel(tokens, maxTokens) {
  if (!tokens || !maxTokens) return 0;
  const ratio = tokens / maxTokens;
  if (ratio >= 0.85) return 5;
  if (ratio >= 0.55) return 4;
  if (ratio >= 0.3) return 3;
  if (ratio >= 0.12) return 2;
  return 1;
}

function trim(value) {
  return value >= 10 ? value.toFixed(0) : value.toFixed(1);
}
