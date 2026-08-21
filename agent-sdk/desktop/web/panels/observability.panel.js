// R11:observability 面板质量收尾完成。
// R12:observability 面板复核完成（SLO/用量/告警/周报/遥测展示，无需改动）。
/* R5 Agent 3 面板：可观测性（概览卡/回合耗时折线/工具排行/健康清单）。
 * R8 增：SLO 基线 + 用量与预算；R9 增：告警规则/最近告警 + 周期报告入口；
 * R10 增：可选遥测状态展示（默认关，仅聚合指标）。
 * 纯脚本 IIFE，注册 window.OwoPanels.observability；helpers 防御性降级。
 * 折线用内联 SVG 绘制（无外部依赖）。
 */
window.OwoPanels = window.OwoPanels || {};
window.OwoPanels.observability = (function () {
  "use strict";

  var id = "observability";

  function defaultHelpers() {
    var baseUrl = (window.OwoPanels && window.OwoPanels.baseUrl) || "http://127.0.0.1:4098";
    function get(path) {
      return fetch(baseUrl + path).then(function (r) {
        if (!r.ok) throw new Error("HTTP " + r.status);
        return r.json();
      });
    }
    function esc(s) {
      return String(s == null ? "" : s)
        .replace(/&/g, "&amp;")
        .replace(/</g, "&lt;")
        .replace(/>/g, "&gt;")
        .replace(/"/g, "&quot;");
    }
    function friendlyError(e) {
      return "操作失败：" + (e && e.message ? e.message : String(e));
    }
    return { baseUrl: baseUrl, get: get, esc: esc, friendlyError: friendlyError };
  }

  var H = defaultHelpers();
  var state = { overview: null, turns: [], tools: [], health: null, runtime: null, slo: null, usage: null, alerts: null, report: null, telemetry: null };

  function nav() {
    return (
      '<section data-panel="' + id + '">' +
      '<style>' +
      '.owo-mtr-row{display:flex;gap:8px;align-items:center;padding:4px 0;border-bottom:1px solid #eee}' +
      '.owo-mtr-card{display:inline-block;min-width:88px;padding:6px 10px;margin:2px;background:#f4f6f8;border-radius:6px;text-align:center}' +
      '.owo-mtr-card b{display:block;font-size:16px}' +
      '.owo-mtr-card span{font-size:11px;color:#666}' +
      '.owo-mtr-table{width:100%;border-collapse:collapse;font-size:12px}' +
      '.owo-mtr-table td,.owo-mtr-table th{border:1px solid #ddd;padding:3px 6px;text-align:left}' +
      '.owo-mtr-health b.ok{color:#2e7d32}.owo-mtr-health b.bad{color:#c62828}' +
      '</style>' +
      '<div class="stack">' +
      '<div class="sub">可观测性 / 性能护栏</div>' +
      '<div class="owo-mtr-row"><button class="primary" id="owo-mtr-refresh">刷新</button>' +
      '<span id="owo-mtr-updated" class="sub">—</span></div>' +
      '<div id="owo-mtr-cards"></div>' +
      '<div class="sub">运行时韧性指标（Wave 1/2）</div>' +
      '<div id="owo-mtr-runtime" class="owo-mtr-runtime">—</div>' +
      '<div class="sub">SLO 基线（Wave 2）</div>' +
      '<div id="owo-mtr-slo" class="owo-mtr-slo">—</div>' +
      '<div class="sub">用量与预算（R8）</div>' +
      '<div id="owo-mtr-usage" class="owo-mtr-usage">—</div>' +
      '<div class="sub">SLO 告警（R9）</div>' +
      '<div id="owo-mtr-alerts" class="owo-mtr-alerts">—</div>' +
      '<div class="sub">周期报告（R9，7 天窗口）</div>' +
      '<div id="owo-mtr-report" class="owo-mtr-report"><button class="primary" id="owo-mtr-report-refresh">加载周报</button></div>' +
      '<div class="sub">可选遥测（R10，默认关，仅聚合指标）</div>' +
      '<div id="owo-mtr-telemetry" class="owo-mtr-telemetry">—</div>' +
      '<div class="sub">回合耗时（最近 50 次，ms）</div>' +
      '<div id="owo-mtr-chart">—</div>' +
      '<div class="sub">工具调用排行</div>' +
      '<table class="owo-mtr-table" id="owo-mtr-tools"><thead><tr><th>工具</th><th>调用</th><th>失败</th><th>失败率</th></tr></thead><tbody></tbody></table>' +
      '<div class="sub">组件健康</div>' +
      '<div id="owo-mtr-health" class="owo-mtr-health">—</div>' +
      '</div>' +
      '</section>'
    );
  }

  function mount(root, helpers) {
    if (helpers) H = helpers;
    root.innerHTML = nav();
    root.querySelector("#owo-mtr-refresh").addEventListener("click", refresh);
    var reportBtn = root.querySelector("#owo-mtr-report-refresh");
    if (reportBtn) reportBtn.addEventListener("click", loadReport);
    refresh();
  }

  function refresh() {
    H.get("/metrics/overview")
      .then(function (data) {
        state.overview = data;
        renderCards();
      })
      .catch(function (e) {
        var el = document.getElementById("owo-mtr-cards");
        if (el) el.innerHTML = '<span style="color:#c62828">' + H.esc(H.friendlyError(e)) + "</span>";
      });
    H.get("/metrics/turns?limit=50")
      .then(function (data) {
        state.turns = (data && data.turns) || [];
        renderChart();
      })
      .catch(function () {});
    H.get("/metrics/tools")
      .then(function (data) {
        state.tools = (data && data.tools) || [];
        renderTools();
      })
      .catch(function () {});
    H.get("/metrics/health")
      .then(function (data) {
        state.health = data;
        renderHealth();
      })
      .catch(function () {});
    H.get("/metrics/runtime")
      .then(function (data) {
        state.runtime = data;
        renderRuntime();
      })
      .catch(function () {});
    H.get("/metrics/slo")
      .then(function (data) {
        state.slo = data;
        renderSlo();
      })
      .catch(function () {});
    H.get("/usage/summary")
      .then(function (data) {
        state.usage = data;
        renderUsage();
      })
      .catch(function () {});
    H.get("/metrics/slo/alerts")
      .then(function (data) {
        state.alerts = data;
        renderAlerts();
      })
      .catch(function () {});
    H.get("/metrics/telemetry/status")
      .then(function (data) {
        state.telemetry = data;
        renderTelemetry();
      })
      .catch(function () {});
  }

  function loadReport() {
    H.get("/metrics/slo/report?days=7")
      .then(function (data) {
        state.report = data;
        renderReport();
      })
      .catch(function (e) {
        var el = document.getElementById("owo-mtr-report");
        if (el) el.innerHTML = '<span style="color:#c62828">' + H.esc(H.friendlyError(e)) + "</span>";
      });
  }

  function renderCards() {
    var el = document.getElementById("owo-mtr-cards");
    if (!el || !state.overview) return;
    var o = state.overview;
    el.innerHTML =
      card(o.traces_count, "traces") +
      card(o.avg_turn_ms, "均耗时 ms") +
      card(o.p50_ms, "p50 ms") +
      card(o.p95_ms, "p95 ms") +
      card(o.tool_calls_total, "工具调用") +
      card(o.approvals_total, "审批") +
      card(o.denied, "拒绝") +
      card(o.failures, "失败");
    var updated = document.getElementById("owo-mtr-updated");
    if (updated) updated.textContent = "更新于 " + H.esc((o.updated_at || "").slice(0, 19).replace("T", " "));
  }

  function card(value, label) {
    return '<div class="owo-mtr-card"><b>' + (value == null ? "—" : H.esc(String(value))) + "</b><span>" + H.esc(label) + "</span></div>";
  }

  function renderChart() {
    var el = document.getElementById("owo-mtr-chart");
    if (!el) return;
    var data = state.turns;
    if (!data.length) {
      el.textContent = "暂无回合数据";
      return;
    }
    var values = data.map(function (t) { return t.duration_ms; }).slice(0, 50).reverse();
    var width = 560;
    var height = 120;
    var max = Math.max.apply(null, values.concat([1]));
    var points = values
      .map(function (v, i) {
        var x = (i / Math.max(1, values.length - 1)) * (width - 8) + 4;
        var y = height - 8 - (v / max) * (height - 20);
        return x.toFixed(1) + "," + y.toFixed(1);
      })
      .join(" ");
    el.innerHTML =
      '<svg width="100%" viewBox="0 0 ' + width + " " + height + '" style="max-width:560px">' +
      '<polyline points="' + points + '" fill="none" stroke="#2e7d32" stroke-width="1.5"></polyline>' +
      "<text x=\"4\" y=\"14\" font-size=\"10\" fill=\"#666\">峰值 " + max + " ms（最近 " + values.length + " 次）</text>" +
      "</svg>";
  }

  function renderTools() {
    var el = document.querySelector("#owo-mtr-tools tbody");
    if (!el) return;
    if (!state.tools.length) {
      el.innerHTML = '<tr><td colspan="4" class="sub">暂无工具调用</td></tr>';
      return;
    }
    el.innerHTML = state.tools
      .map(function (t) {
        return (
          "<tr><td>" + H.esc(t.tool) + "</td><td>" + t.calls + "</td><td>" + t.failures +
          '</td><td>' + (t.failure_rate * 100).toFixed(1) + "%</td></tr>"
        );
      })
      .join("");
  }

  function renderHealth() {
    var el = document.getElementById("owo-mtr-health");
    if (!el || !state.health) return;
    var c = state.health.components || {};
    var stt = c.stt && c.stt.ready;
    el.innerHTML =
      "STT：<b class=\"" + (stt ? "ok" : "bad") + "\">" + (stt ? "就绪" : "未就绪") + "</b> ｜ " +
      "云端传输：<b class=\"ok\">" + H.esc((c.cloud_transport && c.cloud_transport.kind) || "?") + "</b> ｜ " +
      "插件：" + ((c.plugins && c.plugins.count) || 0) + " ｜ " +
      "笔记：" + ((c.notes && c.notes.count) || 0) + " ｜ " +
      "traces：" + ((c.traces && c.traces.count) || 0);
  }

  function renderRuntime() {
    var el = document.getElementById("owo-mtr-runtime");
    if (!el || !state.runtime) return;
    var r = state.runtime;
    var tool = r.tool || {};
    var approval = r.approval || {};
    var sse = r.sse || {};
    var events = r.events || {};
    var pct = function (v) {
      return v == null ? "—" : (v * 100).toFixed(1) + "%";
    };
    var ms = function (v) {
      return v == null ? "—" : H.esc(String(v)) + " ms";
    };
    el.innerHTML =
      '<table class="owo-mtr-table">' +
      "<tr><td>工具调度 p95 / p50</td><td>" + ms(tool.p95_ms) + " / " + ms(tool.p50_ms) + "（样本 " + (tool.samples || 0) + "）</td></tr>" +
      "<tr><td>审批通过率</td><td>" + pct(approval.pass_rate) + "</td></tr>" +
      "<tr><td>审批拦截率</td><td>" + pct(approval.intercept_rate) + "（通过 " + (approval.approved || 0) + " / 拦截 " + (approval.denied || 0) + " / 共 " + (approval.total || 0) + "）</td></tr>" +
      "<tr><td>事件队列深度</td><td>" + (r.queue_depth == null ? 0 : r.queue_depth) + "</td></tr>" +
      "<tr><td>SSE 活跃连接</td><td>" + (sse.active_connections || 0) + "（累计 " + (sse.total_connections || 0) + "，慢消费者断开 " + (sse.lagged_total || 0) + "）</td></tr>" +
      "<tr><td>事件流发布/丢弃</td><td>" + (events.published || 0) + " / " + (events.dropped || 0) + "</td></tr>" +
      "</table>";
  }

  function renderSlo() {
    var el = document.getElementById("owo-mtr-slo");
    if (!el || !state.slo) return;
    var items = (state.slo.slo || []).slice().sort(function (a, b) {
      return (a.name || "").localeCompare(b.name || "");
    });
    if (!items.length) {
      el.innerHTML = '<span class="sub">暂无 SLO 数据（服务端未注册报告探针）</span>';
      return;
    }
    var rows = items
      .map(function (item) {
        var budget = item.error_budget || {};
        var target = item.target_ms == null ? (item.success_floor == null ? "—" : (item.success_floor * 100).toFixed(1) + "%") : item.target_ms + " ms";
        var p95 = item.p95_ms == null ? "—" : item.p95_ms + " ms";
        var rate = item.success_rate == null ? "—" : (item.success_rate * 100).toFixed(2) + "%";
        var status = item.achieving
          ? '<b class="ok" style="color:#2e7d32">达标</b>'
          : '<b style="color:#c62828">未达标</b>';
        return (
          "<tr><td>" + H.esc(item.name) + "</td><td>" + H.esc(target) +
          "</td><td>" + p95 + "</td><td>" + rate +
          "</td><td>" + (item.samples || 0) +
          "</td><td>" + (budget.bad || 0) + " / " + (budget.allowed_bad || 0) +
          "</td><td>" + status + "</td></tr>"
        );
      })
      .join("");
    el.innerHTML =
      '<table class="owo-mtr-table">' +
      "<thead><tr><th>SLO</th><th>目标</th><th>p95</th><th>成功率</th><th>样本</th><th>违规/预算</th><th>状态</th></tr></thead>" +
      "<tbody>" + rows + "</tbody></table>";
  }

  function renderUsage() {
    var el = document.getElementById("owo-mtr-usage");
    if (!el || !state.usage) return;
    var u = state.usage;
    if (u.error) {
      el.innerHTML = '<span class="sub">用量端点未就绪（主控接线后可用）</span>';
      return;
    }
    var dims = u.dimensions || [];
    var rows = dims
      .map(function (d) {
        var budget = d.budget ? "，预算 " + H.esc(String(d.budget.limit_usd)) + " USD" : "";
        var exceeded = d.budget && d.budget.exceeded ? ' <b style="color:#c62828">超限</b>' : "";
        return (
          "<tr><td>" + H.esc(d.dimension) + "</td><td>" + (d.calls || 0) +
          "</td><td>" + (d.total_tokens || 0) +
          "</td><td>" + H.esc(String(d.cost_usd)) + " USD" +
          "</td><td>" + H.esc(String(d.budget ? d.budget.spent_usd : 0)) + " / " +
          H.esc(String(d.budget ? d.budget.limit_usd : "—")) + budget + exceeded + "</td></tr>"
        );
      })
      .join("");
    var stop = u.hard_stop
      ? ' <b style="color:#c62828">硬熔断中</b>' + (u.hard_stop_reason ? "（" + H.esc(u.hard_stop_reason) + "）" : "")
      : "";
    el.innerHTML =
      "<div class=\"owo-mtr-row\">记录 " + (u.count || 0) + " 条，单价 " + H.esc(String(u.price_per_mtok)) + " $/Mtok" + stop + "</div>" +
      '<table class="owo-mtr-table">' +
      "<thead><tr><th>维度</th><th>调用</th><th>Token</th><th>成本</th><th>花费/预算</th></tr></thead>" +
      "<tbody>" + (rows || '<tr><td colspan="5" class="sub">暂无用量记录</td></tr>') + "</tbody></table>";
  }

  function renderAlerts() {
    var el = document.getElementById("owo-mtr-alerts");
    if (!el || !state.alerts) return;
    var data = state.alerts;
    if (data.note) {
      el.innerHTML = '<span class="sub">告警探针未注册（主控接线后可用）</span>';
      return;
    }
    var rules = data.rules || [];
    var ruleHtml = rules
      .map(function (r) {
        return "<tr><td>" + H.esc(r.name) + "</td><td>" + H.esc(r.slo_name) +
          "</td><td>" + H.esc(String(r.kind)) + " &gt; " + H.esc(String(r.threshold)) +
          "</td><td>连续 " + (r.consecutive || 0) + " 次</td><td>" +
          H.esc(r.severity || "") + "</td></tr>";
      })
      .join("");
    var alerts = (data.alerts || []).slice(0, 8);
    var alertHtml = alerts
      .map(function (a) {
        var color = a.kind === "recovered" ? "#2e7d32" : a.severity === "critical" ? "#c62828" : "#ef6c00";
        return "<div class=\"owo-mtr-row\"><span style=\"color:" + color + "\">[" + H.esc(a.kind) +
          "] " + H.esc(a.rule) + "</span><span class=\"sub\">" +
          H.esc(String(a.at || "").slice(11, 19)) + "</span></div><div class=\"sub\">" +
          H.esc(a.detail || "") + "</div>";
      })
      .join("");
    el.innerHTML =
      '<table class="owo-mtr-table">' +
      "<thead><tr><th>规则</th><th>SLO</th><th>判定</th><th>连续</th><th>级别</th></tr></thead>" +
      "<tbody>" + (ruleHtml || '<tr><td colspan="5" class="sub">暂无规则</td></tr>') + "</tbody></table>" +
      '<div class="sub">最近告警（' + (data.count || 0) + '）</div>' +
      (alertHtml || '<div class="sub">暂无告警</div>');
  }

  function renderReport() {
    var el = document.getElementById("owo-mtr-report");
    if (!el || !state.report) return;
    var data = state.report;
    if (data.note) {
      el.innerHTML = '<span class="sub">周期报告探针未注册（主控接线后可用）</span>';
      return;
    }
    var items = (data.slo || []).slice().sort(function (a, b) {
      return (a.name || "").localeCompare(b.name || "");
    });
    var rows = items
      .map(function (item) {
        var p95 = item.p95_ms == null ? "—" : item.p95_ms + " ms";
        var rate = item.success_rate == null ? "—" : (item.success_rate * 100).toFixed(2) + "%";
        var status = item.achieving
          ? '<b style="color:#2e7d32">达标</b>'
          : '<b style="color:#c62828">未达标</b>';
        return (
          "<tr><td>" + H.esc(item.name) + "</td><td>" + p95 +
          "</td><td>" + rate + "</td><td>" + (item.samples || 0) +
          "</td><td>" + (item.violations_in_window || 0) + "</td><td>" + status + "</td></tr>"
        );
      })
      .join("");
    el.innerHTML =
      '<div class="owo-mtr-row">周期 ' + (data.period_days || 7) + " 天，共 " + items.length + " 项</div>" +
      '<table class="owo-mtr-table">' +
      "<thead><tr><th>SLO</th><th>p95</th><th>成功率</th><th>样本</th><th>违规</th><th>状态</th></tr></thead>" +
      "<tbody>" + (rows || '<tr><td colspan="6" class="sub">暂无周期数据</td></tr>') + "</tbody></table>" +
      '<div class="sub"><button class="primary" id="owo-mtr-report-refresh">重新加载</button></div>';
    var btn = el.querySelector("#owo-mtr-report-refresh");
    if (btn) btn.addEventListener("click", loadReport);
  }

  function renderTelemetry() {
    var el = document.getElementById("owo-mtr-telemetry");
    if (!el || !state.telemetry) return;
    var t = state.telemetry;
    if (t.error) {
      el.innerHTML = '<span class="sub">遥测端点未就绪（主控接线后可用）</span>';
      return;
    }
    var enabled = !!t.enabled;
    var status = enabled
      ? '<b style="color:#ef6c00">开（仅聚合指标，不含内容）</b>'
      : '<b class="ok" style="color:#2e7d32">关（默认）</b>';
    var counters = t.counters || {};
    var codes = t.error_codes || {};
    var perf = t.performance || {};
    var counterSummary = Object.keys(counters)
      .map(function (k) { return H.esc(k) + "=" + counters[k]; })
      .join("，") || "无";
    var codeSummary = Object.keys(codes)
      .map(function (k) { return H.esc(k) + "×" + codes[k]; })
      .join("，") || "无";
    var dict = t.data_dictionary || {};
    el.innerHTML =
      '<table class="owo-mtr-table">' +
      "<tr><td>开关</td><td>" + status + "</td></tr>" +
      "<tr><td>功能计数</td><td>" + counterSummary + "</td></tr>" +
      "<tr><td>错误码分布</td><td>" + codeSummary + "</td></tr>" +
      "<tr><td>性能分位</td><td>工具 p50=" + (perf.tool_p50_ms == null ? "—" : H.esc(String(perf.tool_p50_ms)) + " ms") +
      "，p95=" + (perf.tool_p95_ms == null ? "—" : H.esc(String(perf.tool_p95_ms)) + " ms") + "</td></tr>" +
      "<tr><td>数据字典</td><td class=\"sub\">" + H.esc(t.note || "") +
      (dict.note ? "；" + H.esc(dict.note) : "") + "</td></tr>" +
      "</table>";
  }

  return {
    id: id,
    title: "可观测性",
    nav: nav,
    mount: mount,
    refresh: refresh,
  };
})();
