import {
  estimatedTokens,
  formatNumber,
  heatLevel,
  normalizeUsageRows,
  percentText,
  reportedTokens,
  summarizeDaily,
  totalTokens,
} from "./usage.js";

const state = {
  days: 7,
  mode: "overview",
  section: "usage",
};

const palette = ["#3b82f6", "#5b7ee5", "#8ecdf5", "#14b86f", "#0ea5b8", "#2d5fd7"];

document.addEventListener("DOMContentLoaded", () => {
  bindControls();
  refresh();
});

function bindControls() {
  document.querySelectorAll("[data-section]").forEach((button) => {
    button.addEventListener("click", () => {
      state.section = button.dataset.section;
      updateSection();
      refresh();
    });
  });
  document.querySelectorAll("[data-mode]").forEach((button) => {
    button.addEventListener("click", () => {
      state.mode = button.dataset.mode;
      updateMode();
    });
  });
  document.querySelectorAll("[data-days]").forEach((button) => {
    button.addEventListener("click", () => {
      state.days = Number(button.dataset.days);
      updateRanges();
      loadUsage();
    });
  });
  document.getElementById("refresh-button").addEventListener("click", refresh);
}

async function refresh() {
  if (state.section === "routes") {
    await loadRoutes();
    return;
  }
  if (state.section === "health") {
    await loadHealth();
    return;
  }
  await loadUsage();
}

async function loadUsage() {
  setStatus("Loading usage...");
  try {
    const usageDays = state.days || 0;
    const dailyDays = state.days || 366;
    const [usage, daily] = await Promise.all([
      apiGet(`/admin/metadata/usage?days=${usageDays}`),
      apiGet(`/admin/metadata/usage/daily?days=${dailyDays}`),
    ]);
    renderUsage(usage, daily.days || []);
    setStatus("Token totals include prompt and completion tokens; reported and estimated sources are shown separately.");
  } catch (error) {
    setStatus(`Usage load failed: ${error.message}`);
  }
}

async function loadRoutes() {
  setStatus("Loading routes...");
  try {
    const [routes, groups, adaptive, families] = await Promise.all([
      apiGet("/admin/routing/routes"),
      apiGet("/admin/routing/groups"),
      apiGet("/admin/routing/adaptive"),
      apiGet("/admin/models/families"),
    ]);
    renderRoutes(routes, groups, adaptive, families);
    setStatus("Routes show the browser-visible OpenAI-compatible namespaces.");
  } catch (error) {
    setStatus(`Route load failed: ${error.message}`);
  }
}

async function loadHealth() {
  setStatus("Loading deployment health...");
  try {
    const [deployments, attempts, analysis] = await Promise.all([
      apiGet("/admin/metadata/deployments"),
      apiGet("/admin/metadata/attempts?limit=40"),
      apiGet("/admin/metadata/attempts/analyze?limit=100"),
    ]);
    renderHealth(deployments.deployments || [], attempts.attempts || [], analysis.analysis || {});
    setStatus("Deployment health is learned from real chat attempts and admin validation probes.");
  } catch (error) {
    setStatus(`Health load failed: ${error.message}`);
  }
}

async function apiGet(path) {
  const response = await fetch(path, { headers: { Accept: "application/json" } });
  if (!response.ok) throw new Error(`${response.status} ${response.statusText}`);
  return response.json();
}

function renderUsage(usage, days) {
  const summary = usage.summary || {};
  const rows = normalizeUsageRows(usage.usage || []);
  const dailySummary = summarizeDaily(days);
  const total = Number(summary.total_tokens || 0);
  const reportedTotal = Number(summary.reported_tokens ?? dailySummary.reported ?? 0);
  const estimatedTotal = Number(summary.estimated_tokens ?? dailySummary.estimated ?? 0);
  const unreported = Math.max(
    0,
    Number(summary.total_requests || 0) - Number(summary.token_reported_requests || 0),
  );

  renderMetrics([
    ["Requests", formatNumber(summary.total_requests), "calls"],
    ["Models", formatNumber(rows.length), "active"],
    ["Total Tokens", formatNumber(total), "prompt + completion"],
    ["Active Days", formatNumber(dailySummary.active), "days"],
    ["Current Streak", `${dailySummary.current}d`, ""],
    ["Longest Streak", `${dailySummary.longest}d`, ""],
    ["Reported Tokens", formatNumber(reportedTotal), "upstream usage"],
    ["Estimated Tokens", formatNumber(estimatedTotal), "local estimate"],
    ["Reported Calls", formatNumber(summary.token_reported_requests), "calls"],
    ["Estimated Calls", formatNumber(unreported), "calls"],
  ]);
  renderHeatmap(days);
  renderModels(rows, total);
  document.getElementById("summary-line").textContent =
    `Recent usage: ${formatNumber(dailySummary.total)} tokens (${formatNumber(dailySummary.reported)} reported / ${formatNumber(dailySummary.estimated)} estimated) across ${dailySummary.active} active days.`;
}

function renderMetrics(items) {
  document.getElementById("metric-grid").innerHTML = items
    .map(([label, value, hint]) => `
      <article class="metric-card">
        <div class="label">${escapeHtml(label)}</div>
        <div class="value">${escapeHtml(value)}</div>
        <div class="label">${escapeHtml(hint)}</div>
      </article>
    `)
    .join("");
}

function renderHeatmap(days) {
  const grid = document.getElementById("usage-heatmap");
  if (!days.length) {
    grid.innerHTML = '<div class="empty">No usage data yet</div>';
    return;
  }
  const maxTokens = Math.max(...days.map(totalTokens));
  grid.innerHTML = days
    .map((day) => {
      const tokens = totalTokens(day);
      const reported = reportedTokens(day);
      const estimated = estimatedTokens(day);
      const level = heatLevel(tokens, maxTokens);
      const title = `${day.date}: ${formatNumber(tokens)} tokens (${formatNumber(reported)} reported / ${formatNumber(estimated)} estimated), ${formatNumber(day.total_requests)} requests, coverage ${percentText(day.token_reporting_coverage)}`;
      const error = Number(day.total_errors || 0) > 0 ? " error" : "";
      return `<span class="heat-cell level-${level}${error}" title="${escapeHtml(title)}"></span>`;
    })
    .join("");
}

function renderModels(rows, total) {
  const chart = document.getElementById("model-chart");
  const list = document.getElementById("model-list");
  if (!rows.length) {
    chart.innerHTML = '<div class="empty">No model usage yet</div>';
    list.innerHTML = "";
    return;
  }
  const top = rows.slice(0, 6);
  const max = Math.max(...top.map(totalTokens), 1);
  chart.innerHTML = top
    .map((row, index) => {
      const tokens = totalTokens(row);
      const height = Math.max(2, Math.round((tokens / max) * 100));
      const name = shortModelName(row.model_id || "unknown");
      return `
        <div class="bar-group" title="${escapeHtml(row.provider || "unknown")} / ${escapeHtml(row.model_id || "unknown")}">
          <div class="bar"><div class="bar-fill" style="height:${height}%;background:${palette[index % palette.length]}"></div></div>
          <div class="bar-label">${escapeHtml(name)}</div>
        </div>
      `;
    })
    .join("");
  list.innerHTML = rows.slice(0, 10).map((row, index) => {
    const tokens = totalTokens(row);
    const reported = reportedTokens(row);
    const estimated = estimatedTokens(row);
    const share = total > 0 ? tokens / total : 0;
    return `
      <div class="model-row">
        <span class="model-swatch" style="background:${palette[index % palette.length]}"></span>
        <span class="model-name">${escapeHtml(row.model_id || "unknown")}</span>
        <span class="model-meta">${formatNumber(row.total_prompt_tokens)} input / ${formatNumber(row.total_completion_tokens)} output / ${formatNumber(reported)} reported / ${formatNumber(estimated)} estimated / ${percentText(share)}</span>
      </div>
    `;
  }).join("");
}

function renderRoutes(routes, groups, adaptive, families) {
  const routeItems = routes.routes || [];
  const groupItems = groups.groups || [];
  const familyItems = families.families || [];
  const multiFamilies = familyItems.filter((family) => Number(family.variant_count || 0) > 1);
  const diagnostics = adaptive.diagnostics || {};
  document.getElementById("route-count").textContent = formatNumber(routeItems.length);
  document.getElementById("group-count").textContent = formatNumber(groupItems.length);
  document.getElementById("family-count").textContent = formatNumber(multiFamilies.length);
  document.getElementById("route-summary").innerHTML = [
    ["Adaptive", diagnostics.adaptive_enabled ? "ON" : "OFF", diagnostics.scope || "auto"],
    ["Routes", formatNumber(routeItems.length), "paths"],
    ["Groups", formatNumber(groupItems.length), "provider groups"],
    ["Candidates", formatNumber((diagnostics.candidates || []).length), "models"],
  ].map(metricHtml).join("");
  document.getElementById("route-list").innerHTML = routeItems.length
    ? routeItems.map(renderRouteRow).join("")
    : '<div class="empty">No routes configured</div>';
  document.getElementById("group-list").innerHTML = groupItems.length
    ? groupItems.map(renderGroupRow).join("")
    : '<div class="empty">No provider groups configured</div>';
  document.getElementById("family-list").innerHTML = multiFamilies.length
    ? multiFamilies.slice(0, 30).map(renderFamilyRow).join("")
    : '<div class="empty">No multi-variant model families</div>';
}

function renderHealth(deployments, attempts, analysis) {
  const activeCooldowns = deployments.filter((item) => Number(item.cooldown_until || 0) > nowSeconds());
  const degraded = deployments.filter((item) => Number(item.consecutive_failures || 0) > 0);
  const failedAttempts = attempts.filter((item) => !item.success);
  document.getElementById("health-summary").innerHTML = [
    ["Deployments", formatNumber(deployments.length), "provider/model/key"],
    ["Cooldown", formatNumber(activeCooldowns.length), "skipped"],
    ["Degraded", formatNumber(degraded.length), "penalized"],
    ["Recent Failures", formatNumber(failedAttempts.length), "attempts"],
  ].map(metricHtml).join("");
  document.getElementById("deployment-count").textContent = formatNumber(deployments.length);
  document.getElementById("attempt-count").textContent = formatNumber(attempts.length);
  renderAnalysis(analysis);
  document.getElementById("deployment-list").innerHTML = deployments.length
    ? deployments.slice(0, 40).map(renderDeploymentRow).join("")
    : '<div class="empty">No deployment state learned yet</div>';
  document.getElementById("attempt-list").innerHTML = attempts.length
    ? attempts.slice(0, 40).map(renderAttemptRow).join("")
    : '<div class="empty">No routing attempts recorded yet</div>';
}

function renderAnalysis(analysis) {
  const recommendations = Array.isArray(analysis.recommendations) ? analysis.recommendations : [];
  const categories = Array.isArray(analysis.top_error_categories) ? analysis.top_error_categories : [];
  const deployments = Array.isArray(analysis.hot_deployments) ? analysis.hot_deployments : [];
  document.getElementById("analysis-count").textContent = formatNumber(recommendations.length);
  const categoryText = categories.length
    ? categories.slice(0, 4).map((item) => `${item.category}: ${formatNumber(item.count)}`).join(" / ")
    : "no recent error categories";
  const deploymentText = deployments.length
    ? deployments.slice(0, 3).map((item) => `${item.deployment} (${formatNumber(item.failures)})`).join(" / ")
    : "no hot deployments";
  document.getElementById("analysis-list").innerHTML = `
    <article class="route-card analysis-card">
      <div class="route-title">
        <span>Local analysis</span>
        <span class="pill">${formatNumber(analysis.failed_attempts || 0)} failures</span>
      </div>
      <p>${escapeHtml(categoryText)}</p>
      <p>${escapeHtml(deploymentText)}</p>
    </article>
    ${recommendations.length
      ? recommendations.map((item) => `<article class="route-card analysis-card"><p>${escapeHtml(item)}</p></article>`).join("")
      : '<div class="empty">No routing recommendations yet</div>'}
  `;
}

function metricHtml([label, value, hint]) {
  return `
    <article class="metric-card">
      <div class="label">${escapeHtml(label)}</div>
      <div class="value">${escapeHtml(value)}</div>
      <div class="label">${escapeHtml(hint)}</div>
    </article>
  `;
}

function renderDeploymentRow(item) {
  const cooldownUntil = Number(item.cooldown_until || 0);
  const cooldownActive = cooldownUntil > nowSeconds();
  const status = cooldownActive
    ? "cooldown"
    : Number(item.consecutive_failures || 0) > 0
      ? "degraded"
      : "healthy";
  return `
    <article class="route-card health-card ${status}">
      <div class="route-title">
        <span>${escapeHtml(item.provider || "unknown")}</span>
        <span class="pill">${escapeHtml(status)}</span>
      </div>
      <code>${escapeHtml(item.model_id || "unknown")}</code>
      <p>key ${escapeHtml(item.key_id || "unknown")} / success ${formatNumber(item.success_count || 0)} / errors ${formatNumber(item.error_count || 0)}</p>
      <p>consecutive ${formatNumber(item.consecutive_failures || 0)} / last ${escapeHtml(item.last_error_category || "ok")}</p>
    </article>
  `;
}

function renderAttemptRow(item) {
  const status = item.success ? "success" : (item.error_category || "failed");
  return `
    <article class="route-card attempt-card ${item.success ? "healthy" : "degraded"}">
      <div class="route-title">
        <span>${escapeHtml(item.provider || "unknown")} #${formatNumber(item.attempt_index || 0)}</span>
        <span class="pill">${escapeHtml(status)}</span>
      </div>
      <code>${escapeHtml(item.model_id || "unknown")}</code>
      <p>${escapeHtml(item.request_id || "unknown")} / key ${escapeHtml(item.key_id || "unknown")}</p>
      <p>HTTP ${escapeHtml(item.http_status || "-")} / fallback ${item.fallback ? "yes" : "no"}</p>
    </article>
  `;
}

function renderRouteRow(route) {
  const providers = Array.isArray(route.providers) ? route.providers : [];
  const agents = Array.isArray(route.agents) ? route.agents : [];
  const examples = [route.models_route, route.chat_route].filter(Boolean);
  return `
    <article class="route-card">
      <div class="route-title">
        <span>${escapeHtml(route.name || route.kind || "route")}</span>
        <span class="pill">${route.enabled ? "enabled" : "disabled"}</span>
      </div>
      <code>${escapeHtml(route.route_prefix || "")}</code>
      <p>${escapeHtml(route.kind || "route")} / ${formatNumber(providers.length)} providers / ${formatNumber(agents.length)} agents</p>
      ${examples.map((item) => `<code>${escapeHtml(item)}</code>`).join("")}
    </article>
  `;
}

function renderGroupRow(group) {
  const providers = Array.isArray(group.providers) ? group.providers : [];
  const agents = Array.isArray(group.agents) ? group.agents : [];
  const providerNames = providers.map((provider) =>
    typeof provider === "string" ? provider : provider.name,
  ).filter(Boolean);
  return `
    <article class="route-card">
      <div class="route-title">
        <span>${escapeHtml(group.name || "group")}</span>
        <span class="pill">${formatNumber(providerNames.length)} providers</span>
      </div>
      <code>${escapeHtml(group.route_prefix || `/provider-groups/${group.name || "group"}/v1`)}</code>
      <p>${providerNames.map(escapeHtml).join(" / ") || "No providers configured"}</p>
      <p>${agents.length ? `Agents: ${agents.map(escapeHtml).join(" / ")}` : "No agent bindings"}</p>
    </article>
  `;
}

function renderFamilyRow(family) {
  const variants = Array.isArray(family.variants) ? family.variants : [];
  const providers = Array.isArray(family.providers) ? family.providers : [];
  const tiers = Array.isArray(family.tiers) ? family.tiers : [];
  const caps = [
    family.supports_tools ? "tools" : null,
    family.supports_vision ? "vision" : null,
    family.supports_reasoning ? "reasoning" : null,
    family.max_context_window ? `${formatNumber(family.max_context_window)} ctx` : null,
  ].filter(Boolean);
  return `
    <article class="route-card family-card">
      <div class="route-title">
        <span>${escapeHtml(family.id || "family")}</span>
        <span class="pill">${formatNumber(family.variant_count || variants.length)} variants</span>
      </div>
      <p>${providers.map(escapeHtml).join(" / ")}${tiers.length ? ` / ${tiers.map(escapeHtml).join(", ")}` : ""}</p>
      ${caps.length ? `<p>${caps.map(escapeHtml).join(" / ")}</p>` : ""}
      <div class="variant-list">
        ${variants.map(renderVariantPill).join("")}
      </div>
    </article>
  `;
}

function renderVariantPill(variant) {
  const provider = variant.provider || variant.owned_by || "unknown";
  const tier = variant.tier || "default";
  return `
    <span class="variant-pill" title="${escapeHtml(provider)} / ${escapeHtml(variant.id || "")}">
      <span>${escapeHtml(provider)}</span>
      <code>${escapeHtml(variant.id || "")}</code>
      <span>${escapeHtml(tier)}</span>
    </span>
  `;
}

function updateSection() {
  document.querySelectorAll("[data-section]").forEach((button) => {
    button.classList.toggle("active", button.dataset.section === state.section);
  });
  document.getElementById("usage-section").classList.toggle("active", state.section === "usage");
  document.getElementById("routes-section").classList.toggle("active", state.section === "routes");
  document.getElementById("health-section").classList.toggle("active", state.section === "health");
}

function updateMode() {
  document.querySelectorAll("[data-mode]").forEach((button) => {
    button.classList.toggle("active", button.dataset.mode === state.mode);
  });
  document.getElementById("overview-view").classList.toggle("active", state.mode === "overview");
  document.getElementById("models-view").classList.toggle("active", state.mode === "models");
}

function updateRanges() {
  document.querySelectorAll("[data-days]").forEach((button) => {
    button.classList.toggle("active", Number(button.dataset.days) === state.days);
  });
}

function setStatus(message) {
  document.getElementById("status-line").textContent = message;
}

function shortModelName(name) {
  const clean = String(name).replace(/^.*\//, "");
  return clean.length > 14 ? `${clean.slice(0, 12)}...` : clean;
}

function escapeHtml(value) {
  return String(value)
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;")
    .replaceAll("'", "&#039;");
}

function nowSeconds() {
  return Math.floor(Date.now() / 1000);
}
