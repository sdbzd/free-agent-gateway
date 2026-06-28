/// Admin HTML dashboard — self-contained monitoring page.
use axum::{extract::State, response::Html};

use crate::AppState;

/// GET /admin — Serve the admin dashboard HTML.
pub async fn admin_index(State(_state): State<AppState>) -> Html<&'static str> {
    Html(ADMIN_DASHBOARD_HTML)
}

const ADMIN_DASHBOARD_HTML: &str = r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>free-agent-gateway — Admin</title>
<style>
  * { margin: 0; padding: 0; box-sizing: border-box; }
  body {
    font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, "Helvetica Neue", Arial, sans-serif;
    background: var(--bg);
    color: var(--text);
    min-height: 100vh;
  }
  .header {
    background: var(--card);
    border-bottom: 1px solid var(--border);
    padding: 16px 24px;
    display: flex;
    align-items: center;
    justify-content: space-between;
    position: sticky;
    top: 0;
    z-index: 100;
  }
  .header h1 { font-size: 20px; font-weight: 600; }
  .header .subtitle { color: var(--text-dim); font-size: 13px; margin-top: 2px; }
  .header .uptime { font-size: 13px; color: var(--success); }
  .tabs {
    display: flex;
    gap: 0;
    background: var(--card);
    border-bottom: 1px solid var(--border);
    padding: 0 24px;
  }
  .tab {
    padding: 12px 24px;
    cursor: pointer;
    border-bottom: 2px solid transparent;
    color: var(--text-dim);
    font-size: 14px;
    font-weight: 500;
    transition: all 0.2s;
  }
  .tab:hover { color: var(--text); background: var(--card-hover); }
  .tab.active { color: var(--success); border-bottom-color: var(--success); }
  .content { padding: 24px; max-width: 1400px; margin: 0 auto; }
  .tab-content { display: none; }
  .tab-content.active { display: block; }
  .stats-bar {
    display: flex;
    gap: 16px;
    margin-bottom: 24px;
    flex-wrap: wrap;
  }
  .stat-card {
    background: var(--card);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    padding: 16px 20px;
    flex: 1;
    min-width: 150px;
  }
  .stat-card .label { font-size: 12px; color: var(--text-dim); text-transform: uppercase; letter-spacing: 0.5px; }
  .stat-card .value { font-size: 24px; font-weight: 700; margin-top: 4px; }
  .stat-card .value.green { color: var(--success); }
  .stat-card .value.red { color: var(--danger); }
  .stat-card .value.yellow { color: var(--warning); }
  .providers-grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(340px, 1fr));
    gap: 16px;
    margin-bottom: 24px;
  }
  .provider-card {
    background: var(--card);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    padding: 20px;
    transition: all 0.2s;
  }
  .provider-card:hover { border-color: var(--accent-light); }
  .provider-card .header-row {
    display: flex;
    justify-content: space-between;
    align-items: center;
    margin-bottom: 12px;
  }
  .provider-card .name {
    font-size: 16px;
    font-weight: 600;
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .status-dot {
    width: 10px;
    height: 10px;
    border-radius: 50%;
    display: inline-block;
    flex-shrink: 0;
  }
  .status-dot.healthy { background: var(--success); box-shadow: 0 0 8px var(--success); }
  .status-dot.unhealthy { background: var(--danger); box-shadow: 0 0 8px var(--danger); }
  .status-dot.exhausted { background: var(--danger); box-shadow: 0 0 8px var(--danger); }
  .status-dot.degraded { background: var(--warning); box-shadow: 0 0 8px var(--warning); }
  .status-dot.unknown { background: var(--text-dim); }
  .status-dot.disabled { background: #555; }
  .provider-card .type-badge {
    font-size: 11px;
    padding: 2px 8px;
    border-radius: 4px;
    background: var(--accent);
    color: var(--text);
  }
  .provider-card.exhausted { border-color: var(--danger); box-shadow: 0 0 16px rgba(255,68,68,0.15); }
  .provider-card .exhausted-banner {
    background: rgba(233,69,96,0.12);
    border: 1px solid rgba(233,69,96,0.3);
    border-radius: 6px;
    padding: 10px 14px;
    margin-bottom: 12px;
    font-size: 12px;
    color: var(--danger);
    display: flex;
    align-items: center;
    gap: 8px;
    line-height: 1.4;
  }
  .provider-card .exhausted-banner .exhausted-icon { font-size: 18px; flex-shrink: 0; }
  .provider-card .exhausted-banner .exhausted-text { flex: 1; }
  .provider-card .exhausted-banner .exhausted-detail { color: var(--text-dim); font-size: 11px; }
  .provider-card .exhausted-banner .exhausted-countdown { color: var(--warning); font-weight: 600; }
  .provider-card .stats {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 8px;
    margin-bottom: 12px;
  }
  .provider-card .stat-item { font-size: 13px; }
  .provider-card .stat-item .stat-label { color: var(--text-dim); }
  .provider-card .stat-item .stat-value { font-weight: 600; }
  .provider-card .error-text {
    color: var(--danger);
    font-size: 12px;
    margin-bottom: 12px;
    padding: 8px;
    background: rgba(233,69,96,0.1);
    border-radius: 4px;
    word-break: break-all;
    max-height: 60px;
    overflow-y: auto;
  }
  .provider-card .actions {
    display: flex;
    gap: 8px;
  }
  .btn {
    padding: 6px 14px;
    border: 1px solid var(--border);
    border-radius: 4px;
    background: transparent;
    color: var(--text);
    cursor: pointer;
    font-size: 12px;
    font-weight: 500;
    transition: all 0.2s;
  }
  .btn:hover { background: var(--accent); border-color: var(--accent-light); }
  .btn:disabled { opacity: 0.5; cursor: not-allowed; }
  .btn.primary { background: var(--accent); border-color: var(--accent-light); }
  .btn.primary:hover { background: var(--accent-light); }
  .btn.danger { border-color: var(--danger); color: var(--danger); }
  .btn.danger:hover { background: rgba(233,69,96,0.2); }
  .btn.success { border-color: var(--success); color: var(--success); }
  .btn.success:hover { background: rgba(78,204,163,0.2); }
  .model-section {
    background: var(--card);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    margin-bottom: 16px;
    overflow: hidden;
  }
  .model-section-header {
    padding: 12px 20px;
    cursor: pointer;
    display: flex;
    justify-content: space-between;
    align-items: center;
    font-weight: 600;
    font-size: 14px;
    transition: background 0.2s;
  }
  .model-section-header:hover { background: var(--card-hover); }
  .model-section-header .count { color: var(--text-dim); font-size: 12px; }
.model-list { padding: 0 20px 16px; }
.model-list.collapsed { display: none; }
  .model-list .model-filter {
    width:100%;
    padding:6px 10px;
    margin-bottom:8px;
    border:1px solid var(--border);
    border-radius:4px;
    background:var(--bg);
    color:var(--text);
    font-size:12px;
    font-family:"SF Mono","Fira Code",monospace;
    outline:none;
    transition:border-color 0.2s;
  }
  .model-list .model-filter:focus { border-color:var(--accent-light); }
  .model-list .model-filter::placeholder { color:var(--text-dim); }
  .model-list .model-item {
    font-size: 12px;
    padding: 4px 0;
    border-bottom: 1px solid rgba(255,255,255,0.05);
    font-family: "SF Mono", "Fira Code", monospace;
    color: var(--text-dim);
  }
  .model-list .model-item:last-child { border-bottom: none; }
.meta-model-row:hover { background: var(--card-hover); }
  .model-item.hidden { display: none; }

  /* ─── Rich Model Cards ─── */
  .rich-model-item {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 8px 10px;
    margin: 4px 0;
    border: 1px solid var(--border);
    border-radius: 6px;
    background: var(--bg);
    transition: background 0.15s, border-color 0.15s;
    font-size: 12px;
    cursor: default;
    position: relative;
  }
  .rich-model-item:hover { border-color: var(--border-hover); background: var(--card-hover); }
  .rich-model-item.disabled { opacity: 0.55; }
  .rich-model-item .rm-check { min-width: 32px; text-align: center; }
  .rich-model-item .rm-check input { cursor: pointer; }
  .rich-model-item .rm-id {
    flex: 1;
    font-family: var(--font-mono);
    font-size: 12px;
    display: flex;
    align-items: center;
    gap: 6px;
    min-width: 0;
    overflow: hidden;
  }
  .rich-model-item .rm-id .mid-text {
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .rich-model-item .rm-copy {
    cursor: pointer;
    font-size: 11px;
    opacity: 0.4;
    padding: 2px 4px;
    border-radius: 3px;
    transition: opacity 0.15s, background 0.15s;
    flex-shrink: 0;
  }
  .rich-model-item .rm-copy:hover { opacity: 1; background: var(--card-hover); }
  .rich-model-item .rm-copy.copied { color: var(--success); opacity: 1; }
  .rich-model-item .rm-provider {
    font-size: 10px;
    padding: 2px 6px;
    border-radius: 3px;
    background: var(--accent-glow);
    color: var(--accent);
    font-weight: 600;
    flex-shrink: 0;
  }
  .rich-model-item .rm-stats {
    display: flex;
    gap: 10px;
    align-items: center;
    flex-shrink: 0;
    font-size: 11px;
    color: var(--text-dim);
  }
  .rich-model-item .rm-stats .rm-stat {
    display: flex;
    align-items: center;
    gap: 3px;
  }
  .rich-model-item .rm-stats .rm-stat .rm-stat-label { font-size: 10px; }
  .rich-model-item .rm-stats .rm-stat .rm-stat-value { font-weight: 600; color: var(--text); }
  .rich-model-item .rm-rate-bars {
    display: flex;
    gap: 6px;
    align-items: center;
    flex-shrink: 0;
  }
  .rich-model-item .rm-rate-bar {
    display: flex;
    align-items: center;
    gap: 3px;
  }
  .rich-model-item .rm-rate-bar .rrb-bg {
    width: 36px;
    height: 4px;
    background: var(--border);
    border-radius: 2px;
    overflow: hidden;
  }
  .rich-model-item .rm-rate-bar .rrb-fill {
    height: 100%;
    border-radius: 2px;
    transition: width 0.4s ease;
  }
  .rrb-fill.rrbg { background: var(--success); }
  .rrb-fill.rrby { background: var(--warning); }
  .rrb-fill.rrbr { background: var(--danger); }
  .rich-model-item .rm-rate-bar .rrb-label { font-size: 9px; color: var(--text-dim); min-width: 24px; }
  .rich-model-item .rm-detail-toggle {
    cursor: pointer;
    font-size: 10px;
    opacity: 0.5;
    padding: 2px 6px;
    border-radius: 3px;
    transition: opacity 0.15s;
    flex-shrink: 0;
    background: none;
    border: none;
    color: var(--text-dim);
  }
  .rich-model-item .rm-detail-toggle:hover { opacity: 1; color: var(--accent); }
  .rich-model-detail {
    display: none;
    padding: 10px 12px 12px 42px;
    margin-top: -2px;
    margin-bottom: 4px;
    background: var(--bg);
    border: 1px solid var(--border);
    border-top: none;
    border-radius: 0 0 6px 6px;
    font-size: 12px;
  }
  .rich-model-detail.open { display: block; }
  .rich-model-detail .rmd-grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(160px, 1fr));
    gap: 6px 16px;
  }
  .rich-model-detail .rmd-field { }
  .rich-model-detail .rmd-field .rmd-label { font-size: 10px; color: var(--text-dim); text-transform: uppercase; letter-spacing: 0.5px; }
  .rich-model-detail .rmd-field .rmd-value { font-weight: 600; font-family: var(--font-mono); font-size: 12px; color: var(--text); }
  .rich-model-detail .rmd-key-list { margin-top: 8px; }
  .rich-model-detail .rmd-key-list .rmd-key-item { font-size: 11px; font-family: var(--font-mono); color: var(--text-dim); padding: 2px 0; }
  .rmd-score-bar {
    display: inline-flex;
    align-items: center;
    gap: 4px;
  }
  .rmd-score-bar .score-bg {
    width: 50px;
    height: 4px;
    background: var(--border);
    border-radius: 2px;
    overflow: hidden;
    display: inline-block;
    vertical-align: middle;
  }
  .rmd-score-bar .score-fill {
    height: 100%;
    border-radius: 2px;
    background: var(--accent);
  }

  .config-editor {
    background: var(--card);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    padding: 20px;
  }
  .config-editor pre {
    font-family: "SF Mono", "Fira Code", monospace;
    font-size: 12px;
    line-height: 1.5;
    overflow-x: auto;
    white-space: pre-wrap;
    word-break: break-all;
    max-height: 70vh;
    overflow-y: auto;
    color: var(--text);
  }
  .config-editor .toolbar {
    display: flex;
    gap: 8px;
    margin-bottom: 12px;
  }
  .logs-view {
    background: var(--card);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    padding: 20px;
    max-height: 70vh;
    overflow-y: auto;
  }
  .logs-view .log-entry {
    font-size: 12px;
    font-family: "SF Mono", "Fira Code", monospace;
    padding: 6px 0;
    border-bottom: 1px solid rgba(255,255,255,0.05);
    display: flex;
    gap: 8px;
  }
  .logs-view .log-time { color: var(--text-dim); flex-shrink: 0; }
  .logs-view .log-type { font-weight: 600; flex-shrink: 0; }
  .logs-view .log-type.health { color: var(--success); }
  .logs-view .log-type.config { color: var(--warning); }
  .logs-view .log-type.test { color: var(--accent-light); }
  .logs-view .log-type.test-success { color: var(--success); }
  .logs-view .log-type.test-fail { color: var(--danger); }
  .test-result {
    margin-top: 8px;
    padding: 8px;
    border-radius: 4px;
    font-size: 12px;
    display: none;
  }
  .test-result.success { display: block; background: rgba(78,204,163,0.1); color: var(--success); border: 1px solid rgba(78,204,163,0.3); }
  .test-result.fail { display: block; background: rgba(233,69,96,0.1); color: var(--danger); border: 1px solid rgba(233,69,96,0.3); }
  .spinner {
    display: inline-block;
    width: 14px;
    height: 14px;
    border: 2px solid var(--border);
    border-top-color: var(--success);
    border-radius: 50%;
    animation: spin 0.8s linear infinite;
    vertical-align: middle;
    margin-right: 4px;
  }
  @keyframes spin { to { transform: rotate(360deg); } }
  .empty-state {
    text-align: center;
    padding: 40px;
    color: var(--text-dim);
  }
  .toast {
    position: fixed;
    bottom: 24px;
    right: 24px;
    padding: 12px 20px;
    border-radius: var(--radius);
    background: var(--card);
    border: 1px solid var(--border);
    color: var(--text);
    font-size: 13px;
    z-index: 1000;
    opacity: 0;
    transform: translateY(20px);
    transition: all 0.3s;
  }
  .toast.show { opacity: 1; transform: translateY(0); }
  .toast.success { border-color: var(--success); }
  .toast.error { border-color: var(--danger); }
  /* Modal */
  .modal-overlay { display:none; position:fixed; inset:0; background:rgba(0,0,0,.55); z-index:2000; align-items:center; justify-content:center; }
  .modal-overlay.open { display:flex; }
  .modal-box { background:var(--card); border:1px solid var(--border); border-radius:12px; padding:24px; min-width:320px; max-width:420px; box-shadow:0 8px 32px rgba(0,0,0,.5); }
  .modal-title { font-size:16px; font-weight:600; margin-bottom:12px; }
  .modal-body { font-size:13px; color:var(--text-muted); margin-bottom:20px; line-height:1.5; }
  .modal-actions { display:flex; gap:8px; justify-content:flex-end; }
  .modal-actions .btn { min-width:72px; }
  /* ─── FCM-style Dark (default) ─── */
  :root {
    --bg: #050505;
    --card: #080908;
    --card-hover: #121512;
    --text: #ffffff;
    --text-dim: #455245;
    --text-muted: #8fa08f;
    --accent: #c0f20c;
    --accent-light: #d2ff2b;
    --accent-glow: rgba(192,242,12,0.15);
    --danger: #ff4444;
    --success: #00ff88;
    --warning: #ffaa00;
    --border: #161a16;
    --border-hover: #2e3b2e;
    --radius: 8px;
    --font-mono: "SF Mono","Fira Code","Cascadia Code",monospace;
  }
  /* ─── Light Theme ─── */
  [data-theme="light"] {
    --bg: #ffffff;
    --card: #ffffff;
    --card-hover: #f5f5f5;
    --text: #000000;
    --text-dim: #999999;
    --text-muted: #666666;
    --accent: #000000;
    --accent-light: #333333;
    --accent-glow: rgba(0,0,0,0.05);
    --danger: #c8143a;
    --success: #008f4d;
    --warning: #b86b00;
    --border: #eaeaea;
    --border-hover: #d5d5d5;
  }
  /* ─── Smooth theme transition ─── */
  *, *::before, *::after {
    transition: background 0.3s ease, border-color 0.3s ease, color 0.3s ease;
  }
  body { transition: background 0.3s ease; }

  /* ─── Model Table (FCM-style) ─── */
  .model-table-wrap { margin-top: 8px; }
  #model-table th {
    padding: 10px 12px;
    font-size: 11px;
    font-weight: 700;
    text-transform: uppercase;
    letter-spacing: 0.5px;
    color: var(--bg);
    background: var(--accent);
    border-bottom: 2px solid var(--border);
    text-align: left;
    white-space: nowrap;
    cursor: pointer;
    user-select: none;
    position: sticky;
    top: 0;
    z-index: 5;
  }
  #model-table th:hover { background: var(--accent-light); }
  #model-table th .sort-arrow { margin-left: 4px; opacity: 0.5; }
  #model-table th.sort-asc .sort-arrow::after { content: ' ▲'; opacity: 0.8; }
  #model-table th.sort-desc .sort-arrow::after { content: ' ▼'; opacity: 0.8; }
  #model-table td {
    padding: 8px 12px;
    border-bottom: 1px solid var(--border);
    white-space: nowrap;
  }
  #model-table tbody tr { transition: background 0.1s; }
  #model-table tbody tr:hover { background: var(--card-hover); }
  #model-table tbody tr.disabled td { color: var(--text-dim); text-decoration: line-through; opacity: 0.6; }
  #model-table .provider-badge {
    font-size: 11px;
    padding: 2px 8px;
    border-radius: 4px;
    background: var(--accent-glow);
    color: var(--accent);
    font-weight: 600;
  }
  #model-table .toggle-btn {
    font-size: 11px;
    padding: 3px 10px;
    border: 1px solid var(--border);
    border-radius: 4px;
    background: transparent;
    color: var(--text);
    cursor: pointer;
    transition: all 0.15s;
  }
  #model-table .toggle-btn:hover { border-color: var(--accent-light); background: var(--accent-dim); }
  #model-table .toggle-btn.enabled { border-color: var(--success); color: var(--success); }
  #model-table .toggle-btn.disabled { border-color: var(--danger); color: var(--danger); }
  #model-table .toggle-btn.spinner { opacity: 0.5; pointer-events: none; }
  #model-table tbody tr { cursor: pointer; }
  #model-table .detail-row { display: none; }
  #model-table .detail-row.open { display: table-row; }
  #model-table .detail-row td {
    padding: 0;
    border-bottom: 2px solid var(--accent);
    background: var(--card-hover);
  }
  #model-table .detail-inner {
    padding: 16px 20px;
    font-size: 13px;
    display: flex;
    gap: 24px;
    flex-wrap: wrap;
  }
  #model-table .detail-inner .detail-field { min-width: 140px; }
  #model-table .detail-inner .detail-label { font-size: 11px; color: var(--text-dim); text-transform: uppercase; letter-spacing: 0.5px; margin-bottom: 2px; }
  #model-table .detail-inner .detail-value { font-weight: 600; font-family: var(--font-mono); }
  #model-table .detail-inner .detail-actions { margin-left: auto; display: flex; gap: 8px; align-items: center; }

  /* ─── Multi-filter bar ─── */
  .filter-bar {
    display: flex;
    gap: 8px;
    align-items: center;
    margin-bottom: 12px;
    flex-wrap: wrap;
  }
  .filter-bar .filter-search {
    flex: 1;
    min-width: 200px;
    padding: 8px 12px;
    border: 1px solid var(--border);
    border-radius: 4px;
    background: var(--bg);
    color: var(--text);
    font-size: 13px;
    outline: none;
  }
  .filter-bar .filter-search:focus { border-color: var(--accent); }
  .filter-bar .filter-chip {
    font-size: 11px;
    padding: 5px 12px;
    border: 1px solid var(--border);
    border-radius: 16px;
    background: transparent;
    color: var(--text-dim);
    cursor: pointer;
    transition: all 0.15s;
    white-space: nowrap;
    user-select: none;
  }
  .filter-bar .filter-chip:hover { border-color: var(--accent); color: var(--text); }
  .filter-bar .filter-chip.active {
    background: var(--accent);
    color: var(--bg);
    border-color: var(--accent);
    font-weight: 600;
  }
  .filter-bar .filter-chip.active:hover { background: var(--accent-light); border-color: var(--accent-light); }
  .filter-bar .filter-chip.prov-chip { margin: 0; }
  .filter-bar .filter-dropdown {
    position: relative;
    display: inline-block;
  }
  .filter-bar .filter-dropdown-menu {
    display: none;
    position: absolute;
    top: 100%;
    left: 0;
    z-index: 20;
    min-width: 180px;
    background: var(--card);
    border: 1px solid var(--border);
    border-radius: 6px;
    padding: 6px;
    margin-top: 4px;
    box-shadow: 0 8px 24px rgba(0,0,0,0.4);
  }
  .filter-bar .filter-dropdown-menu.open { display: block; }
  .filter-bar .filter-dropdown-item {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 6px 10px;
    font-size: 12px;
    cursor: pointer;
    border-radius: 4px;
    transition: background 0.1s;
  }
  .filter-bar .filter-dropdown-item:hover { background: var(--card-hover); }
  .filter-bar .filter-dropdown-item label { cursor: pointer; flex: 1; }
  .filter-bar .filter-count {
    font-size: 12px;
    color: var(--text-dim);
    white-space: nowrap;
  }

  /* ─── Key Rate Progress Bars ─── */
  .key-detail-toggle {
    font-size: 12px;
    color: var(--text-dim);
    cursor: pointer;
    padding: 4px 0;
    display: flex;
    align-items: center;
    gap: 6px;
    margin-top: 8px;
    border: none;
    background: none;
  }
  .key-detail-toggle:hover { color: var(--accent); }
  .key-detail-panel { display: none; margin-top: 8px; }
  .key-detail-panel.open { display: block; }
  .key-entry {
    background: var(--bg);
    border: 1px solid var(--border);
    border-radius: 6px;
    padding: 10px 12px;
    margin-bottom: 6px;
    font-size: 12px;
  }
  .key-entry .key-header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: 8px;
    margin-bottom: 6px;
    font-family: var(--font-mono);
    font-size: 11px;
  }
  .key-entry .key-title {
    min-width: 0;
    overflow-wrap: anywhere;
  }
  .key-entry .key-meta {
    display: inline-flex;
    align-items: center;
    gap: 8px;
    flex-shrink: 0;
    color: var(--text-dim);
    font-size: 10px;
  }
  .key-entry .key-restore-btn {
    border: 1px solid var(--success);
    background: transparent;
    color: var(--success);
    border-radius: 4px;
    padding: 2px 8px;
    font-size: 10px;
    cursor: pointer;
  }
  .key-entry .key-restore-btn:hover {
    background: rgba(0,255,136,0.12);
  }
  .key-entry .key-restore-btn:disabled {
    opacity: 0.6;
    cursor: wait;
  }
  .key-entry .key-status-badge {
    font-size: 10px;
    padding: 1px 6px;
    border-radius: 3px;
    font-weight: 600;
  }
  .key-status-badge.available { background: rgba(0,255,136,0.15); color: var(--success); }
  .key-status-badge.cooldown { background: rgba(255,170,0,0.15); color: var(--warning); }
  .key-status-badge.rate_limited { background: rgba(255,68,68,0.15); color: var(--danger); }
  .key-status-badge.disabled { background: rgba(85,85,85,0.2); color: var(--text-dim); }
  /* ─── Key Overview Strip (visible at card level) ─── */
  .key-overview-strip {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
    margin-bottom: 10px;
  }
  .key-overview-pill {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    padding: 4px 10px;
    border: 1px solid var(--border);
    border-radius: 20px;
    font-size: 11px;
    font-family: var(--font-mono);
    cursor: pointer;
    transition: all 0.15s;
    background: var(--bg);
  }
  .key-overview-pill:hover {
    border-color: var(--accent-light);
    background: var(--card-hover);
  }
  .key-overview-pill .pill-status-dot {
    width: 8px; height: 8px; border-radius: 50%; flex-shrink: 0;
  }
  .pill-status-dot.available { background: var(--success); box-shadow: 0 0 4px var(--success); }
  .pill-status-dot.cooldown { background: var(--warning); box-shadow: 0 0 4px var(--warning); }
  .pill-status-dot.rate_limited { background: var(--danger); box-shadow: 0 0 4px var(--danger); }
  .pill-status-dot.disabled { background: #555; }
  .key-overview-pill .pill-label {
    color: var(--text);
    font-size: 11px;
    white-space: nowrap;
  }
  .key-overview-pill .pill-usage {
    color: var(--text-dim);
    font-size: 10px;
    white-space: nowrap;
  }
  .key-overview-pill .pill-usage .pill-pct {
    font-weight: 600;
  }
  .pill-pct.good { color: var(--success); }
  .pill-pct.mid  { color: var(--warning); }
  .pill-pct.bad  { color: var(--danger); }
  .pill-pct.neutral { color: var(--text-dim); }
  .key-entry .rate-rows { display: flex; flex-wrap: wrap; gap: 4px 16px; }
  .key-entry .rate-row { display: flex; align-items: center; gap: 6px; min-width: 140px; flex: 1; }
  .key-entry .rate-label { font-size: 10px; color: var(--text-dim); min-width: 28px; font-weight: 600; }
  .key-entry .rate-bar-bg {
    flex: 1;
    height: 6px;
    background: var(--border);
    border-radius: 3px;
    overflow: hidden;
    min-width: 40px;
  }
  .key-entry .rate-bar-fill {
    height: 100%;
    border-radius: 3px;
    transition: width 0.5s ease;
  }
  .rate-bar-fill.green { background: var(--success); }
  .rate-bar-fill.yellow { background: var(--warning); }
  .rate-bar-fill.red { background: var(--danger); }
  .key-entry .rate-text { font-size: 10px; color: var(--text-dim); min-width: 50px; text-align: right; font-family: var(--font-mono); }

  @media (max-width: 768px) {
    .providers-grid { grid-template-columns: 1fr; }
    .stats-bar { flex-direction: column; }
    .header { flex-direction: column; gap: 8px; text-align: center; }
    .tabs { padding: 0; overflow-x: auto; }
    .tab { padding: 10px 16px; font-size: 13px; }
    .content { padding: 16px; }
  }
</style>
</head>
<body>
<div class="header">
  <div>
    <h1>🦀 free-agent-gateway</h1>
    <div class="subtitle">v<span id="version">—</span> — <span id="uptime-display">0s</span></div>
  </div>
  <div style="display:flex;align-items:center;gap:12px;">
    <button class="btn" onclick="refreshAll()" title="Refresh all data now" id="refresh-btn" style="font-size:13px;padding:4px 12px;">🔄 Refresh</button>
    <button class="btn theme-btn" onclick="toggleTheme()" title="Toggle theme" id="theme-btn" style="font-size:16px;padding:4px 10px;">🌙</button>
    <span class="uptime">⚡ Last refreshed: <span id="last-refresh">never</span></span>
  </div>
</div>

<div class="tabs">
  <div class="tab active" data-tab="dashboard" onclick="switchTab('dashboard')">📊 Dashboard</div>
  <div class="tab" data-tab="models" onclick="switchTab('models')">🗄️ Models</div>
  <div class="tab" data-tab="usage" onclick="switchTab('usage')">🔤 Usage</div>
  <div class="tab" data-tab="config" onclick="switchTab('config')">⚙️ Config</div>
  <div class="tab" data-tab="logs" onclick="switchTab('logs')">📋 Live Logs</div>
  <div class="tab" data-tab="knowledge" onclick="switchTab('knowledge')">🧠 Knowledge</div>
  <div class="tab" data-tab="chat" onclick="switchTab('chat')">💬 Chat Test</div>
</div>

<div class="content">
  <!-- Dashboard Tab -->
  <div class="tab-content active" id="tab-dashboard">
    <div class="stats-bar" id="stats-bar">
      <div class="stat-card"><div class="label">Total Requests</div><div class="value" id="stat-requests">—</div></div>
      <div class="stat-card"><div class="label">Total Errors</div><div class="value red" id="stat-errors">—</div></div>
      <div class="stat-card"><div class="label">Healthy Providers</div><div class="value green" id="stat-healthy">—</div></div>
      <div class="stat-card"><div class="label">Unhealthy</div><div class="value" id="stat-unhealthy">—</div></div>
    </div>
    <div class="providers-grid" id="providers-grid">
      <div class="empty-state">Loading providers...</div>
    </div>
    <div id="model-sections"></div>
    <div class="recent-events" style="margin-top:24px;">
      <h3 style="font-size:15px;font-weight:600;margin-bottom:12px;">📡 Recent Events</h3>
      <div id="recent-events-list" class="logs-view" style="max-height:300px;">Waiting for events...</div>
    </div>
  </div>

  <!-- Models Tab (FCM-style full table) -->
  <div class="tab-content" id="tab-models">
    <div class="stats-bar" id="models-stats-bar">
      <div class="stat-card"><div class="label">Total Models</div><div class="value" id="models-total">—</div></div>
      <div class="stat-card"><div class="label">Enabled</div><div class="value green" id="models-enabled">—</div></div>
      <div class="stat-card"><div class="label">Disabled</div><div class="value red" id="models-disabled">—</div></div>
      <div class="stat-card"><div class="label">Providers</div><div class="value" id="models-providers">—</div></div>
      <div class="stat-card"><div class="label">Requests</div><div class="value" id="models-requests">—</div></div>
      <div class="stat-card"><div class="label">Tokens</div><div class="value" id="models-tokens">—</div></div>
    </div>
    <div class="model-table-wrap">
      <div class="filter-bar">
        <input type="text" class="filter-search" id="model-table-filter" placeholder="Search model ID or provider..." oninput="applyModelFilters()">
        <div class="filter-dropdown">
          <span class="filter-chip" id="provider-filter-chip" onclick="toggleProviderDropdown()">Provider ▾</span>
          <div class="filter-dropdown-menu" id="provider-dropdown-menu"></div>
        </div>
        <span class="filter-chip" id="status-filter-all" onclick="setStatusFilter('all')">All</span>
        <span class="filter-chip" id="status-filter-enabled" onclick="setStatusFilter('enabled')">Enabled</span>
        <span class="filter-chip" id="status-filter-disabled" onclick="setStatusFilter('disabled')">Disabled</span>
        <span class="filter-count" id="model-table-count"></span>
        <button class="btn" style="font-size:11px;padding:3px 10px;" onclick="batchToggleModels(true)">Enable All</button>
        <button class="btn danger" style="font-size:11px;padding:3px 10px;" onclick="batchToggleModels(false)">Disable All</button>
        <button class="btn" id="save-btn" style="font-size:11px;padding:3px 10px;display:none;" onclick="saveChanges()">💾 Save Changes</button>
        <span class="filter-chip" onclick="resetModelFilters()" title="Reset all filters">✕</span>
      </div>
      <div style="overflow-x:auto;border:1px solid var(--border);border-radius:var(--radius);background:var(--card);">
        <table id="model-table" style="width:100%;border-collapse:collapse;font-size:13px;">
          <thead id="model-table-head"></thead>
          <tbody id="model-table-body"></tbody>
        </table>
        <div id="model-table-empty" class="empty-state" style="display:none;">Loading models...</div>
      </div>
    </div>
  </div>

  <!-- Usage Tab -->
  <div class="tab-content" id="tab-usage">
    <div class="stats-bar" id="usage-stats-bar">
      <div class="stat-card"><div class="label">Total Tokens</div><div class="value" id="usage-total-tokens">—</div></div>
      <div class="stat-card"><div class="label">Prompt Tokens</div><div class="value" id="usage-prompt-tokens">—</div></div>
      <div class="stat-card"><div class="label">Completion Tokens</div><div class="value" id="usage-completion-tokens">—</div></div>
      <div class="stat-card"><div class="label">Requests</div><div class="value" id="usage-requests">—</div></div>
      <div class="stat-card"><div class="label">Success</div><div class="value green" id="usage-success">—</div></div>
      <div class="stat-card"><div class="label">Errors</div><div class="value red" id="usage-errors">—</div></div>
    </div>
    <div style="overflow-x:auto;border:1px solid var(--border);border-radius:var(--radius);background:var(--card);">
      <table id="usage-table" style="width:100%;border-collapse:collapse;font-size:13px;">
        <thead>
          <tr>
            <th>Provider</th>
            <th>Model</th>
            <th>Requests</th>
            <th>Prompt</th>
            <th>Completion</th>
            <th>Total Tokens</th>
            <th>Success</th>
            <th>Errors</th>
            <th>Last Used</th>
          </tr>
        </thead>
        <tbody id="usage-table-body"></tbody>
      </table>
      <div id="usage-empty" class="empty-state" style="display:none;">No token usage recorded yet</div>
    </div>
  </div>

  <!-- Config Tab -->
  <div class="tab-content" id="tab-config">
    <div class="config-editor">
      <div class="toolbar">
        <button class="btn primary" onclick="loadConfig()">🔄 Refresh</button>
        <span style="color:var(--text-dim);font-size:12px;align-self:center;">Config is read-only from the dashboard. Edit config.yaml to make permanent changes.</span>
      </div>
      <pre id="config-display">Loading...</pre>
    </div>
  </div>

  <!-- Logs Tab -->
  <div class="tab-content" id="tab-logs">
    <div class="logs-view" id="logs-view">
      <div class="empty-state">Waiting for events... (auto-connects to SSE)</div>
    </div>
  </div>

  <!-- Chat Test Tab -->
  <div class="tab-content" id="tab-chat">
    <div class="stats-bar">
      <div class="stat-card" style="flex:0 0 auto;min-width:auto;">
        <div class="label">Provider</div>
        <select id="chat-provider" onchange="onChatProviderChange()" style="margin-top:4px;padding:6px 10px;border:1px solid var(--border);border-radius:4px;background:var(--bg);color:var(--text);font-size:13px;min-width:140px;">
          <option value="">-- Select --</option>
        </select>
      </div>
      <div class="stat-card" style="flex:0 0 auto;min-width:auto;">
        <div class="label">Model</div>
        <select id="chat-model" style="margin-top:4px;padding:6px 10px;border:1px solid var(--border);border-radius:4px;background:var(--bg);color:var(--text);font-size:13px;min-width:200px;">
          <option value="">-- Select --</option>
        </select>
      </div>
      <div class="stat-card" style="flex:0 0 auto;min-width:auto;">
        <div class="label">Stream</div>
        <label style="display:flex;align-items:center;gap:6px;margin-top:8px;font-size:13px;cursor:pointer;">
          <input type="checkbox" id="chat-stream" checked> Enable streaming
        </label>
      </div>
      <div class="stat-card" style="flex:0 0 auto;min-width:auto;display:flex;align-items:flex-end;gap:8px;">
        <button class="btn primary" onclick="sendChatMessage()" id="chat-send-btn" style="font-size:13px;padding:8px 20px;">🚀 Send</button>
        <button class="btn danger" onclick="stopChatMessage()" id="chat-stop-btn" style="font-size:13px;padding:8px 20px;display:none;">⏹ Stop</button>
      </div>
    </div>

    <div style="display:grid;grid-template-columns:1fr 1fr;gap:16px;margin-bottom:16px;">
      <div class="config-editor" style="padding:12px;">
        <div class="label" style="font-size:12px;color:var(--text-dim);text-transform:uppercase;letter-spacing:0.5px;margin-bottom:6px;">System Prompt</div>
        <textarea id="chat-system" placeholder="Optional system prompt..." style="width:100%;min-height:80px;padding:8px;border:1px solid var(--border);border-radius:4px;background:var(--bg);color:var(--text);font-size:13px;font-family:var(--font-mono);resize:vertical;outline:none;"></textarea>
      </div>
      <div class="config-editor" style="padding:12px;">
        <div class="label" style="font-size:12px;color:var(--text-dim);text-transform:uppercase;letter-spacing:0.5px;margin-bottom:6px;">User Message</div>
        <textarea id="chat-message" placeholder="Type your message..." style="width:100%;min-height:80px;padding:8px;border:1px solid var(--border);border-radius:4px;background:var(--bg);color:var(--text);font-size:13px;font-family:var(--font-mono);resize:vertical;outline:none;"></textarea>
      </div>
    </div>

    <div class="config-editor" style="padding:16px;">
      <div style="display:flex;justify-content:space-between;align-items:center;margin-bottom:8px;">
        <span class="label" style="font-size:12px;color:var(--text-dim);text-transform:uppercase;letter-spacing:0.5px;">Response</span>
        <div style="display:flex;gap:8px;align-items:center;">
          <span id="chat-status" style="font-size:12px;color:var(--text-dim);"></span>
          <span id="chat-tokens" style="font-size:12px;color:var(--text-dim);display:none;"></span>
          <button class="btn" onclick="clearChatResult()" style="font-size:11px;padding:3px 10px;">🗑 Clear</button>
          <button class="btn" onclick="copyChatResult()" style="font-size:11px;padding:3px 10px;">📋 Copy</button>
        </div>
      </div>
      <pre id="chat-response" style="font-family:var(--font-mono);font-size:12px;line-height:1.5;overflow-x:auto;overflow-y:auto;max-height:400px;white-space:pre-wrap;word-break:break-word;color:var(--text);background:var(--bg);border:1px solid var(--border);border-radius:4px;padding:12px;min-height:60px;">Response will appear here...</pre>
    </div>
  </div>

  <!-- Knowledge Tab (Model Metadata) -->
  <div class="tab-content" id="tab-knowledge">
    <div class="stats-bar" id="meta-stats-bar">
      <div class="stat-card"><div class="label">Known Models</div><div class="value" id="meta-total">—</div></div>
      <div class="stat-card"><div class="label">With Context</div><div class="value green" id="meta-context">—</div></div>
      <div class="stat-card"><div class="label">With Vision</div><div class="value" id="meta-vision">—</div></div>
      <div class="stat-card"><div class="label">With Pricing</div><div class="value" id="meta-pricing">—</div></div>
      <div class="stat-card"><div class="label">Sync Sources</div><div class="value" id="meta-synced">—</div></div>
      <div class="stat-card"><div class="label">Usage Records</div><div class="value" id="meta-usage">—</div></div>
    </div>

    <div class="stats-bar" style="margin-top:12px;" id="meta-error-bar">
      <div class="stat-card"><div class="label">Total Errors (30d)</div><div class="value red" id="meta-error-total">—</div></div>
      <div class="stat-card"><div class="label">Rate Limits</div><div class="value yellow" id="meta-err-rate_limit">—</div></div>
      <div class="stat-card"><div class="label">Auth Failures</div><div class="value red" id="meta-err-auth">—</div></div>
      <div class="stat-card"><div class="label">Timeouts</div><div class="value" id="meta-err-timeout">—</div></div>
      <div class="stat-card"><div class="label">Upstream 5xx</div><div class="value" id="meta-err-upstream">—</div></div>
      <div class="stat-card"><div class="label">Other</div><div class="value" id="meta-err-other">—</div></div>
    </div>

    <div class="model-section">
      <div class="model-section-header" onclick="toggleMetaSync(this)">
        <span>🔄 Sync Status</span>
        <span class="count" id="meta-sync-info">▼</span>
      </div>
      <div class="model-list" id="meta-sync-list">
        <div class="empty-state" id="meta-sync-loading">Loading sync status...</div>
      </div>
    </div>

    <div class="model-section">
      <div class="model-section-header" onclick="toggleMetaModels(this)">
        <span>📦 Learned Models</span>
        <span class="count" id="meta-model-count">▼</span>
      </div>
      <div class="model-list" id="meta-model-list">
        <input class="model-filter" type="text" placeholder="Filter models..." oninput="filterMetaModels(this)">
        <div class="empty-state" id="meta-models-loading">Loading model metadata...</div>
      </div>
    </div>

    <div class="model-section">
      <div class="model-section-header" onclick="toggleMetaErrors(this)">
        <span>❌ Error Breakdown (30d)</span>
        <span class="count" id="meta-error-count">▼</span>
      </div>
      <div class="model-list collapsed" id="meta-error-list">
        <div class="empty-state" id="meta-errors-loading">Loading error data...</div>
      </div>
    </div>
  </div>
</div>

<div class="toast" id="toast"></div>
<div class="modal-overlay" id="modal-overlay">
  <div class="modal-box">
    <div class="modal-title" id="modal-title"></div>
    <div class="modal-body" id="modal-body"></div>
    <div class="modal-actions" id="modal-actions"></div>
  </div>
</div>

<script>
// ─── State ─────────────────────────────────────────
const state = {
  status: null,
  config: null,
  models: null,
  sseConnected: false,
};

// ─── Tab Switching ─────────────────────────────────
function switchTab(name) {
  document.querySelectorAll('.tab').forEach(t => t.classList.remove('active'));
  document.querySelectorAll('.tab-content').forEach(t => t.classList.remove('active'));
  document.querySelector('.tab[data-tab="' + name + '"]').classList.add('active');
  document.getElementById('tab-' + name).classList.add('active');
  if (name === 'models') {
    if (modelTableData.length === 0) loadModelTable();
    else renderModelTable();
  }
  if (name === 'knowledge') {
    loadMetadataStats();
    loadMetadataSyncStatus();
    loadMetadataModels();
    loadMetadataErrors();
  }
  if (name === 'usage') {
    loadUsagePage();
  }
  if (name === 'chat') {
    populateChatProviders();
    // If no provider selected and we have data, auto-select first
    var sel = document.getElementById('chat-provider');
    if (sel && !sel.value && sel.options.length > 1) {
      sel.value = sel.options[1].value;
      onChatProviderChange();
    }
  }
}

// ─── Toast notifications ────────────────────────────
function showToast(msg, type = 'success') {
  const t = document.getElementById('toast');
  t.textContent = msg;
  t.className = `toast ${type} show`;
  setTimeout(() => t.classList.remove('show'), 3000);
}

// ─── API helpers ────────────────────────────────────
async function apiGet(path) {
  const r = await fetch(path);
  if (!r.ok) throw new Error(`HTTP ${r.status}: ${r.statusText}`);
  return r.json();
}

async function apiPost(path) {
  const r = await fetch(path, { method: 'POST' });
  if (!r.ok) throw new Error(`HTTP ${r.status}: ${r.statusText}`);
  return r.json();
}

// ─── Theme switching ───────────────────────────────
function setTheme(theme) {
  var btn = document.getElementById('theme-btn');
  if (theme === 'auto') {
    var prefersDark = window.matchMedia('(prefers-color-scheme: dark)').matches;
    if (prefersDark) document.documentElement.removeAttribute('data-theme');
    else document.documentElement.setAttribute('data-theme', 'light');
    if (btn) btn.textContent = prefersDark ? '🌙' : '☀️';
  } else if (theme === 'light') {
    document.documentElement.setAttribute('data-theme', 'light');
    if (btn) btn.textContent = '☀️';
  } else {
    document.documentElement.removeAttribute('data-theme');
    if (btn) btn.textContent = '🌙';
  }
  localStorage.setItem('fag-theme', theme);
}

function toggleTheme() {
  const current = localStorage.getItem('fag-theme') || 'dark';
  const next = current === 'dark' ? 'light' : current === 'light' ? 'auto' : 'dark';
  setTheme(next);
}

// Listen for OS theme changes in auto mode
window.matchMedia('(prefers-color-scheme: dark)').addEventListener('change', () => {
  if (localStorage.getItem('fag-theme') === 'auto') setTheme('auto');
});

// Close provider dropdown on outside click
document.addEventListener('click', function(e) {
  var dd = document.getElementById('provider-dropdown-menu');
  if (!dd || !dd.classList.contains('open')) return;
  // Check if the click target is the chip button itself
  var chip = document.getElementById('provider-filter-chip');
  if (chip && chip.contains(e.target)) return;
  // Check if the click target is still inside the dropdown DOM
  if (dd.contains(e.target)) return;
  dd.classList.remove('open');
});

// ─── Uptime counter ─────────────────────────────────
let startTime = 0;
function formatUptime(s) {
  const d = Math.floor(s / 86400);
  const h = Math.floor((s % 86400) / 3600);
  const m = Math.floor((s % 3600) / 60);
  const sec = s % 60;
  if (d > 0) return `${d}d ${h}h ${m}m ${sec}s`;
  if (h > 0) return `${h}h ${m}m ${sec}s`;
  if (m > 0) return `${m}m ${sec}s`;
  return `${sec}s`;
}

function updateUptime() {
  if (startTime > 0) {
    const elapsed = Math.floor(Date.now() / 1000) - startTime;
    document.getElementById('uptime-display').textContent = formatUptime(elapsed);
  }
}

// ─── Rate limit helper ──────────────────────────────
function rateBar(limit, used, label, remaining) {
  used = used || 0;
  if (!limit) {
    return `<div class="rate-row">
      <span class="rate-label">${label}</span>
      <div class="rate-bar-bg"><div class="rate-bar-fill green" style="width:0%"></div></div>
      <span class="rate-text">${used}/no limit</span>
    </div>`;
  }
  const pct = Math.min(100, Math.round((used / limit) * 100));
  const color = pct >= 90 ? 'red' : pct >= 70 ? 'yellow' : 'green';
  const remainText = remaining === undefined || remaining === null ? '' : ` · left ${remaining}`;
  return `<div class="rate-row">
    <span class="rate-label">${label}</span>
    <div class="rate-bar-bg"><div class="rate-bar-fill ${color}" style="width:${pct}%"></div></div>
    <span class="rate-text">${used}/${limit}${remainText}</span>
  </div>`;
}

// ─── Render key details for a provider ──────────────
function renderKeyDetails(provider, keys) {
  if (!keys || keys.length === 0) return '';
  return keys.map(k => {
    const st = k.status || 'unknown';
    const statusBadge = `<span class="key-status-badge ${st}">${st}</span>`;
    const restoreButton = st === 'disabled'
      ? `<button class="key-restore-btn" onclick="event.stopPropagation();restoreKey('${provider}','${k.key_id}',this)">Restore</button>`
      : '';
    let cooldownHtml = '';
    if (k.cooldown_until) {
      const recoversAt = new Date(k.cooldown_until * 1000).toLocaleTimeString();
      cooldownHtml = ` <span class="key-cooldown" data-until="${k.cooldown_until}" style="color:var(--warning);font-size:10px;">⏳ cooldown until ${recoversAt} (<span class="cd-timer">…</span>)</span>`;
    }
    const usage = k.rate_usage && k.rate_usage.axes ? k.rate_usage.axes : {};
    return `<div class="key-entry" data-key="">
      <div class="key-header">
        <span class="key-title">🔑 ${k.key} ${statusBadge}${cooldownHtml}</span>
        <span class="key-meta">${restoreButton}<span>${k.tier || 'unknown'} | ok:${k.success_count} fail:${k.fail_count}</span></span>
      </div>
      <div class="rate-rows">
        ${rateBar(k.rpm_limit, usage.rpm ? usage.rpm.used : (k.rpm_count || 0), 'RPM', usage.rpm ? usage.rpm.remaining : null)}
        ${rateBar(k.rpd_limit, usage.rpd ? usage.rpd.used : (k.rpd_count || 0), 'RPD', usage.rpd ? usage.rpd.remaining : null)}
        ${rateBar(k.tpm_limit, usage.tpm ? usage.tpm.used : (k.tpm_total || 0), 'TPM', usage.tpm ? usage.tpm.remaining : null)}
        ${rateBar(k.tpd_limit, usage.tpd ? usage.tpd.used : (k.tpd_total || 0), 'TPD', usage.tpd ? usage.tpd.remaining : null)}
      </div>
    </div>`;
  }).join('');
}

// ─── Countdown timer updater (runs each second) ─────
function updateCooldownTimers() {
  const now = Math.floor(Date.now() / 1000);
  document.querySelectorAll('.key-cooldown .cd-timer').forEach(el => {
    const until = parseInt(el.parentElement.getAttribute('data-until'));
    if (!until || until <= now) { el.textContent = 'recovering…'; return; }
    const remaining = until - now;
    if (remaining > 86400) el.textContent = Math.ceil(remaining / 86400) + 'd left';
    else if (remaining > 3600) el.textContent = Math.ceil(remaining / 3600) + 'h left';
    else if (remaining > 60) el.textContent = Math.ceil(remaining / 60) + 'm left';
    else el.textContent = remaining + 's left';
  });
}

// ─── Render Providers ───────────────────────────────
function renderProviders(providers) {
  const grid = document.getElementById('providers-grid');
  if (!providers || providers.length === 0) {
    grid.innerHTML = '<div class="empty-state">No providers registered</div>';
    return;
  }

  const now = Math.floor(Date.now() / 1000);
  // helper: summarize key usage. If limits are configured, show percentage;
  // otherwise show observed request volume instead of a misleading 0%.
  function keyUsageSummary(k) {
    if (!k) return { text: '0 req', className: 'neutral' };
    var pcts = [];
    if (k.rpm_limit && k.rpm_limit > 0) pcts.push((k.rpm_count || 0) / k.rpm_limit * 100);
    if (k.rpd_limit && k.rpd_limit > 0) pcts.push((k.rpd_count || 0) / k.rpd_limit * 100);
    if (k.tpm_limit && k.tpm_limit > 0) pcts.push((k.tpm_total || 0) / k.tpm_limit * 100);
    if (k.tpd_limit && k.tpd_limit > 0) pcts.push((k.tpd_total || 0) / k.tpd_limit * 100);
    if (pcts.length) {
      const pct = Math.round(Math.max.apply(null, pcts));
      return { text: pct + '%', className: pillPctClass(pct) };
    }
    const requests = k.rpd_count || k.rpm_count || 0;
    const tokens = k.tpd_total || k.tpm_total || 0;
    if (tokens > 0) return { text: tokens + ' tok', className: 'neutral' };
    return { text: requests + ' req', className: 'neutral' };
  }
  function pillPctClass(pct) { return pct >= 90 ? 'bad' : pct >= 60 ? 'mid' : 'good'; }
  grid.innerHTML = providers.map(p => {
    const statusClass = p.computed_status || p.status || 'unknown';
    const errorHtml = p.last_error && p.last_error !== 'null'
      ? `<div class="error-text">⚠ ${p.last_error}</div>` : '';
    const keyDetailsHtml = renderKeyDetails(p.name, p.keys);
    const toggleName = 'key-toggle-' + p.name.replace(/[^a-zA-Z0-9]/g, '_');

    // ─── Per-key overview strip (visible at card level) ─
    const keys = p.keys || [];
    const keyStripHtml = keys.length > 0 ? `<div class="key-overview-strip">
      ${keys.map(k => {
        const st = k.status || 'available';
        const usage = keyUsageSummary(k);
        const label = k.key || '****';
        return `<span class="key-overview-pill" onclick="toggleKeyDetails(document.getElementById('${toggleName}'))" title="Click to expand details">
          <span class="pill-status-dot ${st}"></span>
          <span class="pill-label">${label}</span>
          <span class="pill-usage"><span class="pill-pct ${usage.className}">${usage.text}</span></span>
        </span>`;
      }).join('')}
    </div>` : '';

    // ─── Exhaustion detection ──────────────────────
    const totalKeys = p.total_keys || 0;
    const availKeys = p.available_keys || 0;
    const exhausted = totalKeys > 0 && availKeys === 0;
    let exhaustedBanner = '';
    let exhaustedClass = '';
    if (exhausted) {
      const keys = p.keys || [];
      const rateLimited = keys.filter(k => k.status === 'rate_limited').length;
      const inCooldown = keys.filter(k => k.status === 'cooldown').length;
      const disabled = keys.filter(k => k.status === 'disabled').length;
      const parts = [];
      if (disabled > 0) parts.push(disabled + ' disabled');
      if (rateLimited > 0) parts.push(rateLimited + ' rate-limited');
      if (inCooldown > 0) parts.push(inCooldown + ' in cooldown');
      const earliestRecovery = keys
        .filter(k => k.cooldown_until && k.cooldown_until > now)
        .map(k => k.cooldown_until)
        .sort()[0];
      let recoveryHtml = '';
      if (earliestRecovery) {
        const remaining = earliestRecovery - now;
        const recoveryStr = remaining > 3600 ? Math.ceil(remaining / 3600) + 'h' :
                           remaining > 60 ? Math.ceil(remaining / 60) + 'm' : remaining + 's';
        recoveryHtml = `<span class="exhausted-countdown">⏳ Next key recovers in ~${recoveryStr}</span>`;
      } else if (rateLimited === 0 && inCooldown === 0 && disabled > 0) {
        recoveryHtml = '<span class="exhausted-detail">All keys disabled (auth failure)</span>';
      }
      exhaustedClass = ' exhausted';
      exhaustedBanner = `<div class="exhausted-banner">
        <span class="exhausted-icon">🚫</span>
        <span class="exhausted-text"><strong>All keys exhausted</strong> — ${parts.join(', ')}. ${recoveryHtml}</span>
      </div>`;
    }

    return `
      <div class="provider-card${exhaustedClass}" data-provider="${p.name}">
        <div class="header-row">
          <div class="name">
            <span class="status-dot ${statusClass}"></span>
            ${p.name}
            <span class="type-badge">${p.type || '?'}</span>
          </div>
        </div>
        ${exhaustedBanner}
        ${keyStripHtml}
        <div class="stats">
          <div class="stat-item"><span class="stat-label">Status:</span> <span class="stat-value">${statusClass}</span></div>
          <div class="stat-item"><span class="stat-label">Latency:</span> <span class="stat-value">${p.latency_ms || 0}ms</span></div>
          <div class="stat-item"><span class="stat-label">Models:</span> <span class="stat-value">${p.models_count || 0}</span></div>
          <div class="stat-item"><span class="stat-label">Keys:</span> <span class="stat-value">${p.available_keys || 0}/${p.total_keys || 0}</span></div>
        </div>
        ${errorHtml}
        ${keyDetailsHtml ? `<button class="key-detail-toggle" id="${toggleName}" onclick="toggleKeyDetails(this)">▶ Show Keys (${(p.keys || []).length})</button>
        <div class="key-detail-panel">${keyDetailsHtml}</div>` : ''}
        <div class="test-result" id="test-${p.name}"></div>
        <div class="actions">
          <button class="btn success" onclick="refreshProvider('${p.name}', this)">🔄 Refresh</button>
          <button class="btn primary" onclick="testProvider('${p.name}', this)">🧪 Test</button>
        </div>
      </div>
    `;
  }).join('');
}

// ─── Toggle key details panel ───────────────────────
function toggleKeyDetails(btn) {
  const panel = btn.nextElementSibling;
  if (!panel) return;
  const open = panel.classList.toggle('open');
  btn.textContent = open ? '▼ Hide Keys' : '▶ Show Keys';
}

// ─── Render Models ──────────────────────────────────
// ─── Rich Model Card helpers ─────────────────────────
function formatNum(n) {
  if (n == null) return '--';
  if (n >= 1000000) return (n / 1000000).toFixed(1) + 'M';
  if (n >= 1000) return (n / 1000).toFixed(1) + 'K';
  return String(n);
}

function miniRateBar(limit, used, label) {
  if (!limit) return '';
  const pct = Math.min(100, Math.round(((used || 0) / limit) * 100));
  const color = pct >= 90 ? 'rrbr' : pct >= 70 ? 'rrby' : 'rrbg';
  return `<div class="rm-rate-bar" title="${label}: ${used || 0}/${limit}">
    <span class="rrb-label">${label}</span>
    <div class="rrb-bg"><div class="rrb-fill ${color}" style="width:${pct}%"></div></div>
  </div>`;
}

function copyModelId(id, btn) {
  navigator.clipboard.writeText(id).then(() => {
    btn.textContent = '✓';
    btn.classList.add('copied');
    setTimeout(() => { btn.textContent = '📋'; btn.classList.remove('copied'); }, 1200);
  }).catch(() => {});
}

function toggleModelDetail(btn) {
  const panel = btn.closest('.rich-model-item').nextElementSibling;
  if (!panel || !panel.classList.contains('rich-model-detail')) return;
  const open = panel.classList.toggle('open');
  btn.textContent = open ? '▲' : '⋯';
}

// ─── Render Models (rich card view) ─────────────────
async function renderModels(models) {
  const container = document.getElementById('model-sections');
  if (!models || models.length === 0) {
    container.innerHTML = '<div class="empty-state">No models available</div>';
    return;
  }

  // Fetch per-provider model status from admin endpoint (includes rate limits)
  let perProviderModels = {};
  const providers = [...new Set(models.map(m => m.provider || m.owned_by || 'unknown'))];
  for (const prov of providers) {
    try {
      const data = await apiGet(`/admin/providers/${prov}/models`);
      perProviderModels[prov] = data.models || [];
    } catch (e) {
      perProviderModels[prov] = null;
    }
  }

  // Group by provider
  const groups = {};
  models.forEach(m => {
    const prov = m.provider || m.owned_by || 'unknown';
    if (!groups[prov]) groups[prov] = [];
    groups[prov].push(m.id);
  });

  container.innerHTML = Object.entries(groups).map(([provider, modelList]) => {
    const pm = perProviderModels[provider];
    const enabledCount = pm ? pm.filter(m => m.enabled).length : modelList.length;
    const disabledCount = pm ? pm.filter(m => !m.enabled).length : 0;
    const modelItems = (pm || modelList.map(m => ({id: m, enabled: true})))
      .sort((a,b) => a.id.localeCompare(b.id));

    return `
    <div class="model-section">
      <div class="model-section-header" onclick="toggleModels(this)">
        <span>📦 ${provider} <span class="count">(${enabledCount} enabled, ${disabledCount} disabled)</span></span>
        <span class="count">▶</span>
      </div>
      <div class="model-list collapsed">
        <input class="model-filter" type="text" placeholder="Filter models..." oninput="filterModels(this)">
        ${modelItems.map(m => {
          var mid = m.id, eid = encodeURIComponent(mid);
          var rpm = m.rpm_limit, rpd = m.rpd_limit;
          var tpm = m.tpm_limit, tpd = m.tpd_limit;
          var rpmLeft = m.rpm_unconstrained ? 'unknown' : m.rpm_remaining;
          var rpdLeft = m.rpd_unconstrained ? 'unknown' : m.rpd_remaining;
          var kc = m.key_count || 0, kh = m.keys_healthy || 0;
          var hasRates = rpm || rpd || tpm || tpd || rpmLeft !== null || rpdLeft !== null;
          return '<div class="rich-model-item'+(m.enabled?'':' disabled')+'" data-mid="'+mid.toLowerCase()+'">'+
            '<div class="rm-check"><input type="checkbox" '+(m.enabled?'checked':'')+' onchange="toggleModel(\''+provider+'\',\''+mid.replace(/'/g,"\\'")+'\',this.checked)"></div>'+
            '<div class="rm-id"><span class="mid-text">'+mid+'</span>'+
            '<span class="rm-copy" onclick="copyModelId(\''+mid.replace(/'/g,"\\'")+'\',this)">📋</span></div>'+
            (hasRates ? '<div class="rm-rate-bars">'+
              (rpmLeft !== null ? '<span class="rrb-label" title="Current remaining requests per minute">RPM left:'+rpmLeft+'</span>' : (rpm ? '<span class="rrb-label" title="RPM limit">RPM:'+rpm+'</span>' : ''))+
              (rpdLeft !== null ? '<span class="rrb-label" title="Current remaining requests per day">RPD left:'+rpdLeft+'</span>' : (rpd ? '<span class="rrb-label" title="RPD limit">RPD:'+rpd+'</span>' : ''))+
              (tpm ? '<span class="rrb-label" title="TPM limit">TPM:'+tpm+'</span>' : '')+
              (tpd ? '<span class="rrb-label" title="TPD limit">TPD:'+tpd+'</span>' : '')+
            '</div>' : '')+
            '<div class="rm-stats">'+
              '<div class="rm-stat" title="Keys healthy/total"><span class="rm-stat-value" style="color:var(--success)">'+kh+'</span><span class="rm-stat-label"> / '+kc+' keys</span></div>'+
            '</div>'+
            '</div>';
        }).join('')}
      </div>
    </div>`;
  }).join('');
}

// ─── Model Table (FCM-style) ─────────────────────────
var modelTableData = [];
var modelTableSort = { col: 0, asc: true };
var modelFilterState = { providers: {}, status: 'all', query: '' };

// ─── Save / dirty tracking ──────────────────────────
var hasUnsavedChanges = false;
function markDirty() {
  if (hasUnsavedChanges) return;
  hasUnsavedChanges = true;
  var btn = document.getElementById('save-btn');
  if (btn) { btn.style.display = 'inline-block'; btn.style.background = 'var(--accent)'; btn.style.color = '#000'; }
}
function updateSaveBtn() {
  var btn = document.getElementById('save-btn');
  if (!btn) return;
  if (!hasUnsavedChanges) { btn.style.display = 'none'; return; }
  btn.style.display = 'inline-block';
}
async function saveChanges() {
  var btn = document.getElementById('save-btn');
  if (btn) btn.textContent = '⏳ Saving...';
  try {
    var r = await fetch('/admin/save', { method: 'POST' });
    var d = await r.json();
    if (d.success) {
      hasUnsavedChanges = false;
      if (btn) { btn.textContent = '✅ Saved'; setTimeout(function(){ btn.textContent = '💾 Save Changes'; updateSaveBtn(); }, 2000); }
    } else {
      if (btn) btn.textContent = '❌ Failed - ' + (d.message || 'unknown');
    }
  } catch(e) {
    if (btn) btn.textContent = '❌ Error - ' + e.message;
  }
}

// ─── Modal dialog ──────────────────────────────────
function showModal(title, body, buttons) {
  document.getElementById('modal-title').textContent = title;
  document.getElementById('modal-body').textContent = body;
  var actions = document.getElementById('modal-actions');
  actions.innerHTML = '';
  buttons.forEach(function(b) {
    var btn = document.createElement('button');
    btn.className = 'btn' + (b.danger ? ' danger' : '');
    btn.textContent = b.label;
    btn.style.cssText = b.style || '';
    btn.onclick = function() {
      closeModal();
      if (b.action) b.action();
    };
    actions.appendChild(btn);
  });
  document.getElementById('modal-overlay').classList.add('open');
}
function closeModal() {
  document.getElementById('modal-overlay').classList.remove('open');
}
// Close modal on overlay click or ESC
document.getElementById('modal-overlay').addEventListener('click', function(e) {
  if (e.target === this) closeModal();
});
document.addEventListener('keydown', function(e) {
  if (e.key === 'Escape') closeModal();
});

function saveFilterState() {
  try { localStorage.setItem('fag-model-filter', JSON.stringify(modelFilterState)); } catch(e) {}
}
function loadFilterState() {
  try {
    var saved = localStorage.getItem('fag-model-filter');
    if (saved) { var s = JSON.parse(saved); if (s && typeof s.status === 'string') {
      modelFilterState.status = s.status;
      modelFilterState.query = s.query || '';
      if (s.providers && typeof s.providers === 'object') modelFilterState.providers = s.providers;
    }}
  } catch(e) {}
}

function getModelFilters() {
  var q = (document.getElementById('model-table-filter') || {}).value || '';
  modelFilterState.query = q.toLowerCase().trim();
  return modelFilterState;
}

function applyModelFilters() {
  // Close dropdown if open
  var dd = document.getElementById('provider-dropdown-menu');
  if (dd) dd.classList.remove('open');
  saveFilterState();
  renderModelTable();
}

function toggleProviderDropdown() {
  var dd = document.getElementById('provider-dropdown-menu');
  if (!dd) return;
  // Build provider list with model counts
  var provCounts = {};
  modelTableData.forEach(function(d) {
    if (!provCounts[d.provider]) provCounts[d.provider] = { total: 0, enabled: 0 };
    provCounts[d.provider].total++;
    if (d.enabled) provCounts[d.provider].enabled++;
  });
  var names = Object.keys(provCounts).sort();
  // Set default: all checked if none set
  var anyChecked = false;
  for (var p in modelFilterState.providers) { if (modelFilterState.providers[p]) anyChecked = true; break; }
  if (!anyChecked) {
    names.forEach(function(n) { modelFilterState.providers[n] = true; });
  }
  var html = '<div style="padding:4px 8px;font-size:11px;color:var(--text-dim);border-bottom:1px solid var(--border);margin-bottom:4px;display:flex;justify-content:space-between;">' +
    '<span><span style="cursor:pointer;font-weight:600;" onclick="event.stopPropagation();selectAllProviders(true)">All</span> · <span style="cursor:pointer;" onclick="event.stopPropagation();selectAllProviders(false)">None</span></span>' +
    '<span>' + modelTableData.length + ' total</span></div>';
  for (var i = 0; i < names.length; i++) {
    var checked = modelFilterState.providers[names[i]] ? 'checked' : '';
    var c = provCounts[names[i]];
    var pct = Math.round(c.enabled / c.total * 100);
    html += '<div class="filter-dropdown-item" onclick="event.stopPropagation();toggleProvider(\'' + names[i] + '\')">' +
      '<input type="checkbox" ' + checked + ' style="pointer-events:none;">' +
      '<label style="display:flex;flex:1;justify-content:space-between;">' +
      '<span>' + names[i] + '</span>' +
      '<span style="color:var(--text-muted);font-size:11px;">' + c.enabled + '/' + c.total + '</span>' +
      '</label></div>';
  }
  dd.innerHTML = html;
  dd.classList.toggle('open');
}

function refreshProviderDropdownMenu() {
  var dd = document.getElementById('provider-dropdown-menu');
  if (!dd || !dd.classList.contains('open')) return;
  // Rebuild dropdown content without toggling open/closed
  var wasOpen = dd.classList.contains('open');
  toggleProviderDropdownContent(dd);
}

function toggleProviderDropdownContent(dd) {
  var provCounts = {};
  modelTableData.forEach(function(d) {
    if (!provCounts[d.provider]) provCounts[d.provider] = { total: 0, enabled: 0 };
    provCounts[d.provider].total++;
    if (d.enabled) provCounts[d.provider].enabled++;
  });
  var names = Object.keys(provCounts).sort();
  // Ensure every known provider has an entry in modelFilterState
  names.forEach(function(n) {
    if (modelFilterState.providers[n] === undefined) modelFilterState.providers[n] = true;
  });
  var html = '<div style="padding:4px 8px;font-size:11px;color:var(--text-dim);border-bottom:1px solid var(--border);margin-bottom:4px;display:flex;justify-content:space-between;">' +
    '<span><span style="cursor:pointer;font-weight:600;" onclick="event.stopPropagation();selectAllProviders(true)">All</span> · <span style="cursor:pointer;" onclick="event.stopPropagation();selectAllProviders(false)">None</span></span>' +
    '<span>' + modelTableData.length + ' total</span></div>';
  for (var i = 0; i < names.length; i++) {
    var checked = modelFilterState.providers[names[i]] ? 'checked' : '';
    var c = provCounts[names[i]];
    html += '<div class="filter-dropdown-item" onclick="event.stopPropagation();toggleProvider(\'' + names[i] + '\')">' +
      '<input type="checkbox" ' + checked + ' style="pointer-events:none;">' +
      '<label style="display:flex;flex:1;justify-content:space-between;">' +
      '<span>' + names[i] + '</span>' +
      '<span style="color:var(--text-muted);font-size:11px;">' + c.enabled + '/' + c.total + '</span>' +
      '</label></div>';
  }
  dd.innerHTML = html;
}

function toggleProviderDropdown() {
  var dd = document.getElementById('provider-dropdown-menu');
  if (!dd) return;
  toggleProviderDropdownContent(dd);
  dd.classList.toggle('open');
}

function toggleProvider(name) {
  modelFilterState.providers[name] = !modelFilterState.providers[name];
  saveFilterState();
  refreshProviderDropdownMenu();
  renderModelTable();
}

function selectAllProviders(val) {
  for (var p in modelFilterState.providers) modelFilterState.providers[p] = val;
  saveFilterState();
  refreshProviderDropdownMenu();
  renderModelTable();
}

function setStatusFilter(val) {
  modelFilterState.status = val;
  saveFilterState();
  // Update chip active states
  ['all','enabled','disabled'].forEach(function(s) {
    var el = document.getElementById('status-filter-' + s);
    if (el) el.classList.toggle('active', s === val);
  });
  renderModelTable();
}

function resetModelFilters() {
  modelFilterState.query = '';
  modelFilterState.status = 'all';
  for (var p in modelFilterState.providers) modelFilterState.providers[p] = true;
  saveFilterState();
  document.getElementById('model-table-filter').value = '';
  setStatusFilter('all');
  renderModelTable();
}

function renderModelTable() {
  var filters = getModelFilters();
  var data = modelTableData;
  var allCount = data.length;

  // Filter by text query
  if (filters.query) {
    data = data.filter(function(d) {
      return d.model.toLowerCase().includes(filters.query) || d.provider.toLowerCase().includes(filters.query);
    });
  }

  // Filter by provider — when NO providers selected, show ZERO models
  var activeProvs = {};
  var anyChecked = false;
  for (var p in filters.providers) { if (filters.providers[p]) { activeProvs[p] = true; anyChecked = true; } }
  if (anyChecked) {
    data = data.filter(function(d) { return activeProvs[d.provider]; });
  } else {
    data = []; // no providers selected → nothing shown
  }

  // Filter by status
  if (filters.status === 'enabled') data = data.filter(function(d) { return d.enabled; });
  else if (filters.status === 'disabled') data = data.filter(function(d) { return !d.enabled; });

  // Sort
  var col = modelTableSort.col;
  var asc = modelTableSort.asc;
  data = data.slice().sort(function(a, b) {
    var va, vb;
    if (col === 0) { va = a.provider; vb = b.provider; }
    else if (col === 1) { va = a.model; vb = b.model; }
    else if (col === 2) { va = a.available ? 1 : 0; vb = b.available ? 1 : 0; }
    else if (col === 3) { va = a.enabled ? 1 : 0; vb = b.enabled ? 1 : 0; }
    else { va = a.total_tokens || 0; vb = b.total_tokens || 0; }
    if (va < vb) return asc ? -1 : 1;
    if (va > vb) return asc ? 1 : -1;
    return 0;
  });

  // Stats
  document.getElementById('models-total').textContent = allCount;
  document.getElementById('models-enabled').textContent = modelTableData.filter(function(d) { return d.enabled; }).length;
  document.getElementById('models-disabled').textContent = modelTableData.filter(function(d) { return !d.enabled; }).length;
  document.getElementById('models-requests').textContent = formatNum(modelTableData.reduce(function(sum, d) { return sum + (d.total_requests || 0); }, 0));
  document.getElementById('models-tokens').textContent = formatNum(modelTableData.reduce(function(sum, d) { return sum + (d.total_tokens || 0); }, 0));
  var provSet = {};
  modelTableData.forEach(function(d) { provSet[d.provider] = true; });
  document.getElementById('models-providers').textContent = Object.keys(provSet).length;
  document.getElementById('model-table-count').textContent = data.length < allCount ? '(' + data.length + '/' + allCount + ')' : '(' + allCount + ')';

  // Provider chip label
  var pChip = document.getElementById('provider-filter-chip');
  if (pChip) {
    var provNames = Object.keys(provSet).sort();
    var selected = provNames.filter(function(n) { return filters.providers[n]; });
    if (selected.length === provNames.length) pChip.textContent = 'All Providers ▾';
    else if (selected.length === 0) pChip.textContent = 'None ▾';
    else if (selected.length <= 2) pChip.textContent = selected.join(', ') + ' ▾';
    else pChip.textContent = selected.length + ' providers ▾';
  }

  // Headers
  var headers = ['Provider', 'Model ID', 'Availability', 'Enabled', 'Usage'];
  var headHtml = '<tr>';
  for (var i = 0; i < headers.length; i++) {
    var cls = i === col ? (asc ? 'sort-asc' : 'sort-desc') : '';
    headHtml += '<th class="' + cls + '" onclick="sortModelTable(' + i + ')">' + headers[i] + '<span class="sort-arrow"></span></th>';
  }
  headHtml += '<th style="cursor:default;">Action</th>';
  headHtml += '</tr>';
  document.getElementById('model-table-head').innerHTML = headHtml;

  // Body
  if (data.length === 0) {
    document.getElementById('model-table-body').innerHTML = '';
    document.getElementById('model-table-empty').style.display = 'block';
    document.getElementById('model-table-empty').textContent = 'No models match filters';
    return;
  }
  document.getElementById('model-table-empty').style.display = 'none';

  var rows = '';
  for (var i = 0; i < data.length; i++) {
    var d = data[i];
    var provStatus = 'unknown';
    if (state.status && state.status.providers) {
      for (var si = 0; si < state.status.providers.length; si++) {
        if (state.status.providers[si].name === d.provider) { provStatus = state.status.providers[si].status || 'unknown'; break; }
      }
    }
    var safeModel = d.model.replace(/'/g, "\\'");
    var availability = d.available ? 'available' : (d.unavailable_reason || 'unavailable');
    var availabilityClass = d.available ? 'green' : 'red';
    var modelBadge = d.available ? '' : ' <span class="type-badge" style="color:var(--danger);border-color:var(--danger);">不可用</span>';
    var usageText = formatNum(d.total_tokens || 0) + ' tok · ' + formatNum(d.total_requests || 0) + ' req';
    var toggleBtn = '<button class="toggle-btn ' + (d.enabled ? 'enabled' : 'disabled') + '" onclick="event.stopPropagation();modelTableToggle(\'' + d.provider + '\',\'' + safeModel + '\',this)">' + (d.enabled ? 'Disable' : 'Enable') + '</button>';
    rows += '<tr class="' + (d.enabled ? '' : 'disabled') + '" onclick="toggleModelDetail(this,\'' + d.provider + '\',\'' + safeModel + '\',\'' + provStatus + '\')">' +
      '<td><span class="provider-badge">' + d.provider + '</span></td>' +
      '<td>' + d.model + modelBadge + '</td>' +
      '<td><span class="' + availabilityClass + '">' + availability + '</span><span style="color:var(--text-dim);font-size:11px;"> · ' + provStatus + '</span></td>' +
      '<td>' + (d.enabled ? '✅' : '❌') + '</td>' +
      '<td>' + usageText + '</td>' +
      '<td>' + toggleBtn + '</td>' +
      '</tr>' +
      '<tr class="detail-row" id="detail-' + i + '"><td colspan="6"><div class="detail-inner" id="detail-inner-' + i + '"></div></td></tr>';
  }
  document.getElementById('model-table-body').innerHTML = rows;
}

function toggleModelDetail(tr, provider, modelId, provStatus) {
  var detailRow = tr.nextElementSibling;
  if (!detailRow || !detailRow.classList.contains('detail-row')) return;
  var isOpen = detailRow.classList.contains('open');

  // Close all other detail rows
  document.querySelectorAll('#model-table .detail-row.open').forEach(function(r) {
    if (r !== detailRow) r.classList.remove('open');
  });

  if (isOpen) {
    detailRow.classList.remove('open');
    return;
  }

  // Find model data
  var md = null;
  for (var i = 0; i < modelTableData.length; i++) {
    if (modelTableData[i].provider === provider && modelTableData[i].model === modelId) { md = modelTableData[i]; break; }
  }
  if (!md) return;

  var inner = detailRow.querySelector('.detail-inner');
  if (!inner) return;

  var safeModel = modelId.replace(/'/g, "\\'");
  var rpmLeftDetail = md.rpm_unconstrained ? 'unknown' : (md.rpm_remaining ?? '—');
  var rpdLeftDetail = md.rpd_unconstrained ? 'unknown' : (md.rpd_remaining ?? '—');
  var quotaDetail = 'RPM left ' + rpmLeftDetail + ' / RPD left ' + rpdLeftDetail;
  if (md.effective_rpm_limit || md.effective_rpd_limit) {
    quotaDetail += ' · effective limit RPM ' + (md.effective_rpm_limit || '—') + ' / RPD ' + (md.effective_rpd_limit || '—');
  }
  inner.innerHTML =
    '<div class="detail-field"><div class="detail-label">Provider</div><div class="detail-value">' + provider + '</div></div>' +
    '<div class="detail-field"><div class="detail-label">Model ID</div><div class="detail-value" style="word-break:break-all;">' + modelId + '</div></div>' +
    '<div class="detail-field"><div class="detail-label">Status</div><div class="detail-value">' + provStatus + '</div></div>' +
    '<div class="detail-field"><div class="detail-label">Availability</div><div class="detail-value">' + (md.available ? '✅ Available' : '⚠ ' + (md.unavailable_reason || 'Unavailable')) + '</div></div>' +
    '<div class="detail-field"><div class="detail-label">Enabled</div><div class="detail-value">' + (md.enabled ? '✅ Yes' : '❌ No') + '</div></div>' +
    '<div class="detail-field"><div class="detail-label">Usage</div><div class="detail-value">' + formatNum(md.total_tokens || 0) + ' tokens · ' + formatNum(md.total_requests || 0) + ' requests</div></div>' +
    '<div class="detail-field"><div class="detail-label">Keys</div><div class="detail-value">' + (md.keys_healthy || 0) + '/' + (md.key_count || 0) + ' available</div></div>' +
    '<div class="detail-field"><div class="detail-label">Quota</div><div class="detail-value">' + quotaDetail + '</div></div>' +
    '<div class="detail-actions">' +
    '<button class="toggle-btn ' + (md.enabled ? 'enabled' : 'disabled') + '" onclick="event.stopPropagation();modelTableToggle(\'' + provider + '\',\'' + safeModel + '\',this)">' + (md.enabled ? 'Disable' : 'Enable') + '</button>' +
    '</div>';
  detailRow.classList.add('open');
}

function sortModelTable(col) {
  if (modelTableSort.col === col) modelTableSort.asc = !modelTableSort.asc;
  else { modelTableSort.col = col; modelTableSort.asc = true; }
  renderModelTable();
}

// ─── Batch toggle on filtered results ──────────────
async function batchToggleModels(enable) {
  var filters = getModelFilters();
  var targets = [];
  var allCount = modelTableData.length;

  // Apply same filter logic as renderModelTable to determine which models are affected
  for (var i = 0; i < modelTableData.length; i++) {
    var d = modelTableData[i];
    // text filter
    if (filters.query && !d.model.toLowerCase().includes(filters.query) && !d.provider.toLowerCase().includes(filters.query)) continue;
    // provider filter
    var provOk = false;
    for (var p in filters.providers) { if (filters.providers[p] && p === d.provider) { provOk = true; break; } }
    if (!provOk) continue;
    // status filter
    if (filters.status === 'enabled' && !d.enabled) continue;
    if (filters.status === 'disabled' && d.enabled) continue;
    // Only toggle if needed
    if ((enable && !d.enabled) || (!enable && d.enabled)) {
      targets.push({ provider: d.provider, model: d.model, idx: i });
    }
  }

  if (targets.length === 0) {
    showToast('No models to ' + (enable ? 'enable' : 'disable'), 'info');
    return;
  }

  showModal(
    (enable ? 'Enable' : 'Disable') + ' ' + targets.length + ' models?',
    'This will ' + (enable ? 'enable' : 'disable') + ' ' + targets.length + ' model' + (targets.length > 1 ? 's' : '') + ' matching current filters.',
    [
      { label: 'Cancel', style: 'background:transparent;border:1px solid var(--border);', action: null },
      { label: enable ? 'Enable' : 'Disable', danger: !enable, action: function() {
        doBatchToggle(enable, targets);
      }}
    ]
  );
}

async function doBatchToggle(enable, targets) {

  var ok = 0, fail = 0;
  for (var t = 0; t < targets.length; t++) {
    try {
      var r = await fetch('/admin/providers/' + targets[t].provider + '/models/' + encodeURIComponent(targets[t].model) + '/toggle', { method: 'POST' });
      if (r.ok) {
        modelTableData[targets[t].idx].enabled = enable;
        ok++;
      } else { fail++; }
    } catch(e) { fail++; }
  }
  if (ok > 0) markDirty();
  showToast((enable ? 'Enabled' : 'Disabled') + ' ' + ok + '/' + targets.length + ' models' + (fail ? ' (' + fail + ' failed)' : ''), fail > 0 ? 'error' : 'success');
  renderModelTable();
  if (state.models) setTimeout(function() { renderModels(state.models); }, 100);
}

async function modelTableToggle(provider, modelId, btn) {
  btn.disabled = true;
  btn.textContent = '...';
  try {
    var r = await fetch('/admin/providers/' + provider + '/models/' + encodeURIComponent(modelId) + '/toggle', { method: 'POST' });
    if (!r.ok) throw new Error('HTTP ' + r.status);
    for (var i = 0; i < modelTableData.length; i++) {
      if (modelTableData[i].provider === provider && modelTableData[i].model === modelId) {
        modelTableData[i].enabled = !modelTableData[i].enabled;
        break;
      }
    }
    markDirty();
    renderModelTable();
    if (state.models) setTimeout(function() { renderModels(state.models); }, 100);
  } catch (e) {
    showToast('Toggle failed: ' + e.message, 'error');
  }
  btn.disabled = false;
}

async function loadModelTable() {
  try {
    if (!state.status) {
      try { state.status = await apiGet('/admin/status'); } catch (e) {}
    }
    var providers = [];
    if (state.status && state.status.providers) {
      providers = state.status.providers.map(function(p) { return p.name; });
    }
    if (providers.length === 0) {
      var publicModels = await apiGet('/v1/models');
      var provSet = {};
      (publicModels.data || []).forEach(function(m) { provSet[m.provider || m.owned_by || 'unknown'] = true; });
      providers = Object.keys(provSet);
    }

    var usageRows = [];
    try {
      var usageData = await apiGet('/admin/metadata/usage');
      usageRows = usageData.usage || [];
    } catch (e) {}
    var usageMap = {};
    usageRows.forEach(function(u) {
      usageMap[(u.provider || '') + '|' + (u.model_id || '')] = {
        total_requests: u.total_requests || 0,
        total_prompt_tokens: u.total_prompt_tokens || 0,
        total_completion_tokens: u.total_completion_tokens || 0,
        total_tokens: (u.total_prompt_tokens || 0) + (u.total_completion_tokens || 0),
        total_success: u.total_success || 0,
        total_errors: u.total_errors || 0,
        last_used_at: u.last_used_at || null
      };
    });

    var perProv = {};
    for (var i = 0; i < providers.length; i++) {
      try {
        var pd = await apiGet('/admin/providers/' + providers[i] + '/models');
        perProv[providers[i]] = pd.models || [];
      } catch(e) { perProv[providers[i]] = null; }
    }
    modelTableData = [];
    providers.forEach(function(prov) {
      var models = perProv[prov] || [];
      models.forEach(function(m) {
        var usage = usageMap[prov + '|' + m.id] || {};
        modelTableData.push({
          provider: prov,
          model: m.id,
          enabled: m.enabled !== false,
          available: m.available === true,
          unavailable_reason: m.unavailable_reason || null,
          key_count: m.key_count || 0,
          keys_healthy: m.keys_healthy || 0,
          rpm_limit: m.rpm_limit || null,
          rpd_limit: m.rpd_limit || null,
          effective_rpm_limit: m.effective_rpm_limit || null,
          effective_rpd_limit: m.effective_rpd_limit || null,
          rpm_remaining: m.rpm_remaining,
          rpd_remaining: m.rpd_remaining,
          rpm_unconstrained: m.rpm_unconstrained === true,
          rpd_unconstrained: m.rpd_unconstrained === true,
          total_requests: usage.total_requests || 0,
          total_prompt_tokens: usage.total_prompt_tokens || 0,
          total_completion_tokens: usage.total_completion_tokens || 0,
          total_tokens: usage.total_tokens || 0,
          total_success: usage.total_success || 0,
          total_errors: usage.total_errors || 0,
          last_used_at: usage.last_used_at || null
        });
      });
    });
    // Init filter state: all providers checked
    modelFilterState.providers = {};
    providers.forEach(function(p) { modelFilterState.providers[p] = true; });
    loadFilterState();
    setStatusFilter('all');
    renderModelTable();
  } catch (e) {
    document.getElementById('model-table-empty').style.display = 'block';
    document.getElementById('model-table-empty').textContent = 'Failed: ' + e.message;
  }
}

function filterModels(input) {
  if (!input) return;
  const q = input.value.toLowerCase().trim();
  const list = input.closest('.model-list');
  if (!list) return;
  let visible = 0;
  let total = 0;
  list.querySelectorAll('.model-item').forEach(el => {
    total++;
    const modelName = el.getAttribute('data-mid') || el.textContent.toLowerCase();
    const match = !q || modelName.includes(q);
    el.style.display = match ? '' : 'none';
    if (match) visible++;
  });
  const count = list.querySelector('.model-count');
  if (count) {
    if (q) count.textContent = '(' + visible + '/' + total + ')';
    else count.textContent = '(' + total + ')';
  }
}

function toggleModels(header) {
  const list = header.nextElementSibling;
  list.classList.toggle('collapsed');
  const arrow = header.lastElementChild;
  arrow.textContent = list.classList.contains('collapsed') ? '▶' : '▼';
}

async function toggleModel(provider, modelId, enabled) {
  try {
    const r = await fetch(`/admin/providers/${provider}/models/${encodeURIComponent(modelId)}/toggle`, { method: 'POST' });
    if (!r.ok) throw new Error(`HTTP ${r.status}`);
    markDirty();
    showToast(`${modelId}: ${enabled ? 'enabled' : 'disabled'}`, 'success');
    // Refresh the model sections to update counts
    if (state.models) {
      renderModels(state.models);
    }
  } catch (e) {
    showToast(`Toggle failed: ${e.message}`, 'error');
    // Revert checkbox
    if (state.models) renderModels(state.models);
  }
}

async function disableAllModels(provider) {
  try {
    const data = await apiGet(`/admin/providers/${provider}/models`);
    if (!data.models) return;
    for (const m of data.models) {
      if (m.enabled) {
        await fetch(`/admin/providers/${provider}/models/${encodeURIComponent(m.id)}/toggle`, { method: 'POST' });
      }
    }
    markDirty();
    showToast(`${provider}: all models disabled`, 'success');
    if (state.models) renderModels(state.models);
  } catch (e) {
    showToast(`Failed: ${e.message}`, 'error');
  }
}

async function enableAllModels(provider) {
  try {
    const data = await apiGet(`/admin/providers/${provider}/models`);
    if (!data.models) return;
    for (const m of data.models) {
      if (!m.enabled) {
        await fetch(`/admin/providers/${provider}/models/${encodeURIComponent(m.id)}/toggle`, { method: 'POST' });
      }
    }
    markDirty();
    showToast(`${provider}: all models enabled`, 'success');
    if (state.models) renderModels(state.models);
  } catch (e) {
    showToast(`Failed: ${e.message}`, 'error');
  }
}

async function restoreKey(provider, keyId, btn) {
  if (!keyId) {
    showToast('Missing key id', 'error');
    return;
  }
  if (btn) {
    btn.disabled = true;
    btn.textContent = 'Restoring...';
  }
  try {
    const result = await apiPost(`/admin/providers/${encodeURIComponent(provider)}/keys/${encodeURIComponent(keyId)}/restore`);
    if (!result.success) throw new Error(result.error || 'restore failed');
    showToast(`${provider}: key restored`, 'success');
    await loadDashboard();
  } catch (e) {
    showToast(`Restore failed: ${e.message}`, 'error');
    if (btn) {
      btn.disabled = false;
      btn.textContent = 'Restore';
    }
  }
}

// ─── Provider Actions ──────────────────────────────
async function refreshProvider(name, btn) {
  btn.disabled = true;
  btn.innerHTML = '<span class="spinner"></span> Refreshing...';
  try {
    const result = await apiPost(`/admin/providers/${name}/refresh`);
    showToast(`${name}: ${result.models_found} models found`, 'success');
    await loadDashboard();
  } catch (e) {
    showToast(`${name}: ${e.message}`, 'error');
  }
  btn.disabled = false;
  btn.innerHTML = '🔄 Refresh';
}

async function testProvider(name, btn) {
  btn.disabled = true;
  btn.innerHTML = '<span class="spinner"></span> Testing...';
  const testDiv = document.getElementById(`test-${name}`);
  testDiv.className = 'test-result';
  testDiv.textContent = 'Testing...';
  try {
    const result = await apiPost(`/admin/providers/${name}/test`);
    if (result.success) {
      testDiv.className = 'test-result success';
      testDiv.textContent = `✅ ${result.latency_ms}ms — "${result.response_preview || 'ok'}"`;
      showToast(`${name}: OK (${result.latency_ms}ms)`, 'success');
    } else {
      testDiv.className = 'test-result fail';
      testDiv.textContent = `❌ ${result.error || 'Unknown error'}`;
      showToast(`${name}: ${result.error}`, 'error');
    }
  } catch (e) {
    testDiv.className = 'test-result fail';
    testDiv.textContent = `❌ ${e.message}`;
    showToast(`${name}: ${e.message}`, 'error');
  }
  btn.disabled = false;
  btn.innerHTML = '🧪 Test';
}

// ─── Load Dashboard Data ────────────────────────────
async function loadDashboard() {
  try {
    const statusData = await apiGet('/admin/status');
    state.status = statusData;
    startTime = Math.floor(Date.now() / 1000) - statusData.uptime_seconds;

    document.getElementById('version').textContent = statusData.version || '—';
    document.getElementById('stat-requests').textContent = statusData.total_requests || 0;
    document.getElementById('stat-errors').textContent = statusData.total_errors || 0;
    document.getElementById('stat-healthy').textContent = statusData.healthy_count || 0;
    document.getElementById('stat-unhealthy').textContent = statusData.unhealthy_count || 0;
    document.getElementById('last-refresh').textContent = new Date().toLocaleTimeString();

    renderProviders(statusData.providers);
  } catch (e) {
    document.getElementById('providers-grid').innerHTML =
      `<div class="empty-state">Failed to load: ${e.message}</div>`;
  }
}

async function loadModels() {
  try {
    const data = await apiGet('/v1/models');
    state.models = data.data || [];
    renderModels(state.models);
  } catch (e) {
    document.getElementById('model-sections').innerHTML =
      `<div class="empty-state">Failed to load models: ${e.message}</div>`;
  }
}

// ─── Load Config ────────────────────────────────────
async function loadConfig() {
  const display = document.getElementById('config-display');
  display.textContent = 'Loading...';
  try {
    const data = await apiGet('/admin/config');
    display.textContent = JSON.stringify(data, null, 2);
  } catch (e) {
    display.textContent = `Error: ${e.message}`;
  }
}

// ─── SSE Connection ─────────────────────────────────
function connectSSE() {
  const logsView = document.getElementById('logs-view');
  const evtSource = new EventSource('/admin/events');

  evtSource.onopen = () => {
    state.sseConnected = true;
    if (logsView.querySelector('.empty-state')) {
      logsView.innerHTML = '';
    }
  };

  var newEventsBuf = [];

  evtSource.onmessage = (event) => {
    try {
      const data = JSON.parse(event.data);
      const time = new Date(data.timestamp * 1000).toLocaleTimeString();
      let typeClass = 'health';
      if (data.type === 'config_update') typeClass = 'config';
      else if (data.type === 'provider_test') {
        typeClass = data.data?.success ? 'test-success' : 'test-fail';
      }

      // Append to Live Logs tab
      const entry = document.createElement('div');
      entry.className = 'log-entry';
      entry.innerHTML =
        '<span class="log-time">' + time + '</span>' +
        '<span class="log-type ' + typeClass + '">' + data.type + '</span>' +
        '<span>' + JSON.stringify(data.data || {}) + '</span>';
      logsView.appendChild(entry);
      logsView.scrollTop = logsView.scrollHeight;
      while (logsView.children.length > 200) {
        logsView.removeChild(logsView.firstChild);
      }

      // Buffer for Dashboard tab recent events
      newEventsBuf.unshift({ time: time, type: data.type, typeClass: typeClass, summary: (data.data && data.data.message) || data.type });
      if (newEventsBuf.length > 50) newEventsBuf.length = 50;
      renderRecentEvents();
    } catch (e) {
      // Ignore parse errors for ping messages
    }
  };

  function renderRecentEvents() {
    var el = document.getElementById('recent-events-list');
    if (!el) return;
    if (newEventsBuf.length === 0) {
      el.innerHTML = '<div class="empty-state" style="padding:20px;">Listening for events...</div>';
      return;
    }
    el.innerHTML = newEventsBuf.slice(0, 20).map(function(e) {
      return '<div class="log-entry"><span class="log-time">' + e.time + '</span><span class="log-type ' + e.typeClass + '">' + e.type + '</span><span>' + e.summary + '</span></div>';
    }).join('');
  }

  evtSource.onerror = () => {
    state.sseConnected = false;
    // Will auto-reconnect
  };
}

// ─── Knowledge Tab ──────────────────────────────────
function toggleMetaSync(btn) {
  var list = document.getElementById('meta-sync-list');
  if (list) list.classList.toggle('collapsed');
}

function toggleMetaModels(btn) {
  var list = document.getElementById('meta-model-list');
  if (list) list.classList.toggle('collapsed');
}

function toggleMetaErrors(btn) {
  var list = document.getElementById('meta-error-list');
  if (list) list.classList.toggle('collapsed');
}

function filterMetaModels(input) {
  var q = (input.value || '').toLowerCase();
  document.querySelectorAll('#meta-model-list .meta-model-row').forEach(function(row) {
    var text = (row.getAttribute('data-search') || '').toLowerCase();
    row.style.display = text.includes(q) ? '' : 'none';
  });
}

async function loadMetadataStats() {
  try {
    var data = await apiGet('/admin/metadata');
    if (!data.enabled) {
      document.getElementById('meta-total').textContent = 'DB off';
      document.getElementById('meta-sync-loading').textContent = 'Metadata DB not available. Check startup logs.';
      return;
    }
    document.getElementById('meta-total').textContent = data.total_models || 0;
    document.getElementById('meta-context').textContent = data.with_context_window || 0;
    document.getElementById('meta-vision').textContent = data.with_vision || 0;
    document.getElementById('meta-pricing').textContent = data.with_pricing || 0;
    document.getElementById('meta-synced').textContent = data.synced_sources || 0;
    document.getElementById('meta-usage').textContent = data.usage_records || 0;

    // Error stats
    document.getElementById('meta-error-total').textContent = data.error_total || 0;
    var cats = data.error_categories || {};
    document.getElementById('meta-err-rate_limit').textContent = cats.rate_limit || 0;
    document.getElementById('meta-err-auth').textContent = cats.auth || 0;
    document.getElementById('meta-err-timeout').textContent = cats.timeout || 0;
    document.getElementById('meta-err-upstream').textContent = cats.upstream || 0;
    document.getElementById('meta-err-other').textContent = cats.other || 0;
  } catch (e) {
    document.getElementById('meta-total').textContent = 'Error';
  }
}

async function loadUsagePage() {
  try {
    var data = await apiGet('/admin/metadata/usage');
    var summary = data.summary || {};
    document.getElementById('usage-total-tokens').textContent = formatNum(summary.total_tokens || 0);
    document.getElementById('usage-prompt-tokens').textContent = formatNum(summary.total_prompt_tokens || 0);
    document.getElementById('usage-completion-tokens').textContent = formatNum(summary.total_completion_tokens || 0);
    document.getElementById('usage-requests').textContent = formatNum(summary.total_requests || 0);
    document.getElementById('usage-success').textContent = formatNum(summary.total_success || 0);
    document.getElementById('usage-errors').textContent = formatNum(summary.total_errors || 0);

    var rows = data.usage || [];
    var body = document.getElementById('usage-table-body');
    var empty = document.getElementById('usage-empty');
    if (!rows.length) {
      body.innerHTML = '';
      empty.style.display = 'block';
      return;
    }
    empty.style.display = 'none';
    rows = rows.slice().sort(function(a, b) {
      var at = (a.total_prompt_tokens || 0) + (a.total_completion_tokens || 0);
      var bt = (b.total_prompt_tokens || 0) + (b.total_completion_tokens || 0);
      if (bt !== at) return bt - at;
      return (b.total_requests || 0) - (a.total_requests || 0);
    });
    body.innerHTML = rows.map(function(u) {
      var prompt = u.total_prompt_tokens || 0;
      var completion = u.total_completion_tokens || 0;
      var total = prompt + completion;
      var last = u.last_used_at ? new Date(u.last_used_at * 1000).toLocaleString() : '—';
      return '<tr>' +
        '<td><span class="provider-badge">' + (u.provider || 'unknown') + '</span></td>' +
        '<td style="word-break:break-all;">' + (u.model_id || '') + '</td>' +
        '<td>' + formatNum(u.total_requests || 0) + '</td>' +
        '<td>' + formatNum(prompt) + '</td>' +
        '<td>' + formatNum(completion) + '</td>' +
        '<td><strong>' + formatNum(total) + '</strong></td>' +
        '<td class="green">' + formatNum(u.total_success || 0) + '</td>' +
        '<td class="' + ((u.total_errors || 0) > 0 ? 'red' : '') + '">' + formatNum(u.total_errors || 0) + '</td>' +
        '<td style="color:var(--text-dim);">' + last + '</td>' +
      '</tr>';
    }).join('');
  } catch (e) {
    document.getElementById('usage-empty').style.display = 'block';
    document.getElementById('usage-empty').textContent = 'Failed to load usage: ' + e.message;
  }
}

async function loadMetadataSyncStatus() {
  try {
    var data = await apiGet('/admin/metadata/sync');
    var list = document.getElementById('meta-sync-list');
    if (!data.sources || data.sources.length === 0) {
      list.innerHTML = '<div class="empty-state">No sync sources configured.</div>';
      return;
    }
    list.innerHTML = data.sources.map(function(s) {
      var time = s.last_sync_at ? new Date(s.last_sync_at * 1000).toLocaleString() : 'never';
      var hasError = s.error_message ? '<span style="color:var(--danger);margin-left:8px;">⚠ ' + s.error_message + '</span>' : '';
      return '<div style="display:flex;justify-content:space-between;padding:6px 0;border-bottom:1px solid var(--border);font-size:13px;">' +
        '<span><strong>' + s.source_name + '</strong>' + hasError + '</span>' +
        '<span style="color:var(--text-dim);">' + s.items_found + ' found, ' + s.items_updated + ' updated @ ' + time + '</span>' +
        '</div>';
    }).join('');
  } catch (e) {
    document.getElementById('meta-sync-loading').textContent = 'Failed: ' + e.message;
  }
}

async function loadMetadataModels() {
  try {
    var data = await apiGet('/admin/metadata/models');
    var list = document.getElementById('meta-model-list');
    var countEl = document.getElementById('meta-model-count');

    if (!data.models || data.models.length === 0) {
      list.innerHTML = '<div class="empty-state">No model metadata learned yet. Models will appear here after requests and public sync.</div>';
      countEl.textContent = '(0)';
      return;
    }

    countEl.textContent = '(' + data.models.length + ')';

    // Keep the filter input
    var filterHtml = '<input class="model-filter" type="text" placeholder="Filter models..." oninput="filterMetaModels(this)">';

    list.innerHTML = filterHtml + data.models.map(function(m) {
      var ctx = m.context_window ? formatNum(m.context_window) + ' tokens' : '--';
      var ctxClass = m.context_window >= 1000000 ? 'green' : m.context_window >= 128000 ? '' : 'yellow';
      var vision = m.supports_vision ? '✅' : '—';
      var tools = m.supports_tools ? '✅' : '—';
      var pricing = m.pricing_prompt ? '$' + m.pricing_prompt + '/token' : '—';
      var name = m.display_name || '';
      var source = m.source || 'discovery';
      return '<div class="meta-model-row" data-search="' + (m.provider + ' ' + m.model_id + ' ' + name).toLowerCase() + '" style="display:flex;justify-content:space-between;align-items:center;padding:8px 0;border-bottom:1px solid var(--border);font-size:13px;gap:8px;">' +
        '<div style="flex:2;min-width:0;">' +
          '<div style="font-weight:600;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;">' + m.model_id + '</div>' +
          (name ? '<div style="color:var(--text-dim);font-size:11px;">' + name + '</div>' : '') +
          '<div style="font-size:11px;color:var(--text-dim);"><span class="type-badge">' + m.provider + '</span> <span style="color:var(--text-muted);">source: ' + source + '</span></div>' +
        '</div>' +
        '<div style="flex:1;font-size:12px;">ctx: <strong class="' + ctxClass + '">' + ctx + '</strong></div>' +
        '<div style="flex:0 0 auto;font-size:12px;">vision: <strong>' + vision + '</strong> tools: <strong>' + tools + '</strong></div>' +
        '<div style="flex:0 0 auto;font-size:11px;color:var(--text-dim);text-align:right;">' +
          'updated: ' + (m.last_updated_at ? new Date(m.last_updated_at * 1000).toLocaleDateString() : '?') +
        '</div>' +
      '</div>';
    }).join('');
  } catch (e) {
    document.getElementById('meta-models-loading').textContent = 'Failed: ' + e.message;
  }
}

async function loadMetadataErrors() {
  try {
    var data = await apiGet('/admin/metadata/errors');
    var list = document.getElementById('meta-error-list');
    var countEl = document.getElementById('meta-error-count');

    if (!data.errors || data.errors.length === 0) {
      list.innerHTML = '<div class="empty-state">No errors recorded yet.</div>';
      countEl.textContent = '(0)';
      return;
    }

    countEl.textContent = '(' + data.errors.length + ')';

    var catColors = { rate_limit: 'var(--warning)', auth: 'var(--danger)', timeout: 'var(--text)', upstream: '#e67e22', other: 'var(--text-dim)' };
    list.innerHTML = data.errors.map(function(e) {
      var color = catColors[e.category] || 'var(--text-dim)';
      return '<div style="display:flex;justify-content:space-between;padding:6px 0;border-bottom:1px solid var(--border);font-size:13px;">' +
        '<span><span class="type-badge">' + e.provider + '</span> ' + e.model_id + '</span>' +
        '<span><span style="color:' + color + ';font-weight:600;">' + e.category + '</span> × <strong>' + e.total + '</strong></span>' +
        '</div>';
    }).join('');
  } catch (e) {
    document.getElementById('meta-errors-loading').textContent = 'Failed: ' + e.message;
  }
}

// ─── Initial Load ───────────────────────────────────
// Restore theme
(function() {
  var saved = localStorage.getItem('fag-theme');
  if (!saved || saved === 'auto') {
    var prefersDark = window.matchMedia('(prefers-color-scheme: dark)').matches;
    if (prefersDark) document.documentElement.removeAttribute('data-theme');
    else document.documentElement.setAttribute('data-theme', 'light');
    if (!saved) localStorage.setItem('fag-theme', 'auto');
  } else if (saved === 'light') {
    document.documentElement.setAttribute('data-theme', 'light');
  }
  var btn = document.getElementById('theme-btn');
  if (btn) btn.textContent = document.documentElement.getAttribute('data-theme') === 'light' ? '☀️' : '🌙';
})();
loadDashboard();
loadModels();
loadMetadataStats();
loadMetadataSyncStatus();
loadMetadataModels();
loadMetadataErrors();
connectSSE();

// ─── Chat Test Tab ────────────────────────────────
var chatAbortController = null;

function onChatProviderChange() {
  var provider = document.getElementById('chat-provider').value;
  var modelSelect = document.getElementById('chat-model');
  modelSelect.innerHTML = '<option value="">-- Select --</option>';
  if (!provider || !state.models) return;
  var models = state.models.filter(function(m) {
    return (m.provider || m.owned_by || 'unknown') === provider;
  });
  models.sort(function(a,b) { return a.id.localeCompare(b.id); });
  models.forEach(function(m) {
    var opt = document.createElement('option');
    opt.value = m.id;
    opt.textContent = m.id;
    modelSelect.appendChild(opt);
  });
}

function populateChatProviders() {
  var select = document.getElementById('chat-provider');
  if (!select) return;
  var currentVal = select.value;
  select.innerHTML = '<option value="">-- Select --</option>';
  if (state.status && state.status.providers) {
    state.status.providers.forEach(function(p) {
      var opt = document.createElement('option');
      opt.value = p.name;
      opt.textContent = p.name + ' (' + (p.type || '?') + ')';
      select.appendChild(opt);
    });
  }
  if (currentVal) select.value = currentVal;
}

async function sendChatMessage() {
  var provider = document.getElementById('chat-provider').value;
  var model = document.getElementById('chat-model').value;
  var systemMsg = document.getElementById('chat-system').value.trim();
  var userMsg = document.getElementById('chat-message').value.trim();
  var stream = document.getElementById('chat-stream').checked;
  var responseEl = document.getElementById('chat-response');
  var statusEl = document.getElementById('chat-status');
  var tokensEl = document.getElementById('chat-tokens');
  var sendBtn = document.getElementById('chat-send-btn');
  var stopBtn = document.getElementById('chat-stop-btn');

  if (!provider) { showToast('Please select a provider', 'error'); return; }
  if (!model) { showToast('Please select a model', 'error'); return; }
  if (!userMsg) { showToast('Please enter a message', 'error'); return; }

  sendBtn.disabled = true;
  sendBtn.style.display = 'none';
  stopBtn.style.display = 'inline-block';
  responseEl.textContent = '⏳ Sending...';
  statusEl.textContent = 'sending...';
  tokensEl.style.display = 'none';

  var messages = [];
  if (systemMsg) messages.push({ role: 'system', content: systemMsg });
  messages.push({ role: 'user', content: userMsg });

  chatAbortController = new AbortController();

  try {
    if (stream) {
      var response = await fetch('/v1/chat/completions', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ model: model, messages: messages, stream: true, provider: provider }),
        signal: chatAbortController.signal,
      });
      if (!response.ok) {
        var errText = await response.text();
        throw new Error('HTTP ' + response.status + ': ' + errText);
      }
      var reader = response.body.getReader();
      var decoder = new TextDecoder();
      var fullContent = '';
      var inputTokens = 0, outputTokens = 0;
      statusEl.textContent = 'streaming...';
      responseEl.textContent = '';

      while (true) {
        var result = await reader.read();
        if (result.done) break;
        var chunk = decoder.decode(result.value, { stream: true });
        var lines = chunk.split('\n');
        for (var i = 0; i < lines.length; i++) {
          var line = lines[i].trim();
          if (!line || line === 'data: [DONE]') continue;
          if (line.startsWith('data: ')) {
            try {
              var json = JSON.parse(line.slice(6));
              if (json.choices && json.choices[0] && json.choices[0].delta && json.choices[0].delta.content) {
                fullContent += json.choices[0].delta.content;
              }
              if (json.usage) {
                inputTokens = json.usage.prompt_tokens || 0;
                outputTokens = json.usage.completion_tokens || 0;
              }
            } catch(e) {}
          }
        }
        responseEl.textContent = fullContent + '▌';
        responseEl.scrollTop = responseEl.scrollHeight;
      }
      responseEl.textContent = fullContent || '(empty response)';
      statusEl.textContent = '✅ done';
      if (inputTokens || outputTokens) {
        tokensEl.style.display = 'inline';
        tokensEl.textContent = '🔤 ' + inputTokens + ' in / ' + outputTokens + ' out';
      }
    } else {
      statusEl.textContent = 'waiting...';
      var response = await fetch('/v1/chat/completions', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ model: model, messages: messages, stream: false, provider: provider }),
        signal: chatAbortController.signal,
      });
      if (!response.ok) {
        var errText = await response.text();
        throw new Error('HTTP ' + response.status + ': ' + errText);
      }
      var data = await response.json();
      var content = (data.choices && data.choices[0] && data.choices[0].message && data.choices[0].message.content) || JSON.stringify(data, null, 2);
      responseEl.textContent = content;
      statusEl.textContent = '✅ done';
      if (data.usage) {
        tokensEl.style.display = 'inline';
        tokensEl.textContent = '🔤 ' + (data.usage.prompt_tokens || 0) + ' in / ' + (data.usage.completion_tokens || 0) + ' out';
      }
    }
  } catch (e) {
    if (e.name === 'AbortError') {
      responseEl.textContent = responseEl.textContent.replace(/▌$/, '') + '\n\n⏹ Stopped by user';
      statusEl.textContent = '⏹ stopped';
    } else {
      responseEl.textContent = '❌ Error: ' + e.message;
      statusEl.textContent = '❌ failed';
    }
  }
  sendBtn.disabled = false;
  sendBtn.style.display = 'inline-block';
  stopBtn.style.display = 'none';
  chatAbortController = null;
}

function stopChatMessage() {
  if (chatAbortController) {
    chatAbortController.abort();
  }
}

function clearChatResult() {
  document.getElementById('chat-response').textContent = 'Response will appear here...';
  document.getElementById('chat-status').textContent = '';
  document.getElementById('chat-tokens').style.display = 'none';
}

function copyChatResult() {
  var text = document.getElementById('chat-response').textContent;
  if (!text || text === 'Response will appear here...') return;
  navigator.clipboard.writeText(text).then(function() {
    showToast('Response copied', 'success');
  }).catch(function() {});
}

// ─── Manual Refresh All ──────────────────────────
async function refreshAll() {
  var btn = document.getElementById('refresh-btn');
  if (btn) { btn.disabled = true; btn.innerHTML = '<span class="spinner"></span> Refreshing...'; }
  try {
    await Promise.all([
      loadDashboard(),
      loadModels(),
      loadMetadataStats(),
      loadMetadataSyncStatus(),
      loadMetadataModels(),
      loadMetadataErrors(),
      loadUsagePage(),
    ]);
    showToast('All data refreshed', 'success');
  } catch (e) {
    showToast('Refresh error: ' + e.message, 'error');
  }
  if (btn) { btn.disabled = false; btn.innerHTML = '🔄 Refresh'; }
}

// ─── Adaptive polling ────────────────────────────
var pollingInterval = 15000;  // start fast
var pollTimer = null;

function schedulePoll() {
  if (pollTimer) clearInterval(pollTimer);
  pollTimer = setInterval(function() {
    loadDashboard();
    // If any provider has 0 available keys (exhausted or degraded), poll faster
    if (state.status && state.status.providers) {
      var degraded = state.status.providers.some(function(p) {
        return (p.total_keys || 0) > 0 && (p.available_keys || 0) === 0;
      });
      var newInterval = degraded ? 10000 : 30000;
      if (newInterval !== pollingInterval) {
        pollingInterval = newInterval;
        schedulePoll();
      }
    }
  }, pollingInterval);
}
schedulePoll();

setInterval(loadModels, 120000);
setInterval(loadMetadataStats, 120000);
setInterval(loadMetadataErrors, 120000);
setInterval(loadUsagePage, 120000);
setInterval(updateUptime, 1000);
setInterval(updateCooldownTimers, 1000);
</script>
</body>
</html>"#;
