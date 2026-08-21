/* R5 Agent 3 面板：eval 护栏（历史列表/报告详情/运行按钮）。
 * 纯脚本 IIFE，注册 window.OwoPanels.eval；helpers 防御性降级。
 */
window.OwoPanels = window.OwoPanels || {};
window.OwoPanels.eval = (function () {
  "use strict";

  var id = "eval";

  function defaultHelpers() {
    var baseUrl = (window.OwoPanels && window.OwoPanels.baseUrl) || "http://127.0.0.1:4098";
    function get(path) {
      return fetch(baseUrl + path).then(function (r) {
        if (!r.ok) throw new Error("HTTP " + r.status);
        return r.json();
      });
    }
    function post(path, body) {
      return fetch(baseUrl + path, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(body || {}),
      }).then(function (r) {
        if (!r.ok) {
          return r.json().then(function (j) {
            throw new Error((j && j.error) || "HTTP " + r.status);
          });
        }
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
    return { baseUrl: baseUrl, get: get, post: post, esc: esc, friendlyError: friendlyError };
  }

  var H = defaultHelpers();
  var state = { reports: [], current: null, running: false };

  function nav() {
    return (
      '<section data-panel="' + id + '">' +
      '<style>' +
      '.owo-eval-row{display:flex;gap:8px;align-items:center;padding:4px 0;border-bottom:1px solid #eee}' +
      '.owo-eval-item{cursor:pointer}' +
      '.owo-eval-item:hover{background:#f4f6f8}' +
      '.owo-eval-pre{max-height:260px;overflow:auto;background:#111;color:#b5e8b5;font-family:monospace;font-size:11px;padding:6px}' +
      '.owo-eval-badge{display:inline-block;padding:1px 6px;border-radius:8px;font-size:11px;color:#fff}' +
      '.owo-eval-badge.ok{background:#2e7d32}.owo-eval-badge.bad{background:#c62828}' +
      '.owo-eval-badge.warn{background:#ef6c00}' +
      '</style>' +
      '<div class="stack">' +
      '<div class="sub">eval 护栏（真实模型，缺 OPENAI_API_KEY 自动跳过）</div>' +
      '<div class="owo-eval-row">' +
      '<input id="owo-eval-suite" placeholder="套件（留空=内置 builtin；可填 .json 路径）" style="flex:1">' +
      '<button class="primary" id="owo-eval-run">运行</button></div>' +
      '<div id="owo-eval-status" class="sub">—</div>' +
      '<div class="sub">历史报告</div>' +
      '<div id="owo-eval-list" class="list"></div>' +
      '<details><summary>报告详情</summary><pre class="owo-eval-pre" id="owo-eval-detail">—</pre></details>' +
      '</div>' +
      '</section>'
    );
  }

  function mount(root, helpers) {
    if (helpers) H = helpers;
    root.innerHTML = nav();
    root.querySelector("#owo-eval-run").addEventListener("click", run);
    refresh();
  }

  function run() {
    if (state.running) return;
    var suite = document.getElementById("owo-eval-suite").value.trim();
    var btn = document.getElementById("owo-eval-run");
    btn.disabled = true;
    state.running = true;
    H.post("/eval/gate/run", suite ? { suite: suite } : {})
      .then(function (data) {
        var status = document.getElementById("owo-eval-status");
        if (data.skipped) {
          status.textContent = "已跳过：" + (data.reason || "无凭据");
          status.className = "sub";
        } else {
          var r = data.report || {};
          status.textContent =
            "套件 " + (r.suite || "?") + " 通过率 " + (r.pass_rate * 100).toFixed(1) + "%（" +
            r.passed + "/" + r.total + "）耗时 " + (r.total_duration_ms / 1000).toFixed(1) + "s 模型 " + (r.model || "?");
          state.current = r;
          renderDetail();
        }
        return refresh();
      })
      .catch(function (e) {
        var status = document.getElementById("owo-eval-status");
        status.textContent = H.friendlyError(e);
        status.className = "owo-eval-badge bad";
      })
      .finally(function () {
        btn.disabled = false;
        state.running = false;
      });
  }

  function refresh() {
    H.get("/eval/gate/reports")
      .then(function (data) {
        state.reports = (data && data.reports) || [];
        renderList();
      })
      .catch(function (e) {
        var el = document.getElementById("owo-eval-list");
        if (el) el.innerHTML = '<div class="owo-eval-badge bad">' + H.esc(H.friendlyError(e)) + "</div>";
      });
  }

  function renderList() {
    var el = document.getElementById("owo-eval-list");
    if (!el) return;
    if (!state.reports.length) {
      el.innerHTML = '<div class="sub">暂无报告（点“运行”生成；无凭据会提示跳过原因）</div>';
      return;
    }
    el.innerHTML = state.reports
      .map(function (r) {
        var badge = (r.pass_rate || 0) >= 0.8 ? "ok" : (r.pass_rate || 0) > 0 ? "warn" : "bad";
        return (
          '<div class="owo-eval-row owo-eval-item" data-file="' + H.esc(r.file) + '">' +
          '<span class="owo-eval-badge ' + badge + '">' + Math.round((r.pass_rate || 0) * 100) + "%</span> " +
          "<strong>" + H.esc(r.suite || "?") + "</strong>" +
          '<span class="sub">' + (r.passed || 0) + "/" + (r.total || 0) + " ｜ " +
          H.esc((r.timestamp || "").slice(0, 19).replace("T", " ")) + " ｜ " + H.esc(r.model || "") + "</span>" +
          "</div>"
        );
      })
      .join("");
    for (var button of el.querySelectorAll(".owo-eval-item")) {
      button.addEventListener("click", function () {
        H.get("/eval/gate/report")
          .then(function (data) {
            state.current = data.report || null;
            renderDetail();
          })
          .catch(function (e) {
            var el = document.getElementById("owo-eval-detail");
            if (el) el.textContent = H.friendlyError(e);
          });
      });
    }
  }

  function renderDetail() {
    var el = document.getElementById("owo-eval-detail");
    if (!el) return;
    el.textContent = state.current ? JSON.stringify(state.current, null, 2) : "—";
  }

  return {
    id: id,
    title: "eval 护栏",
    nav: nav,
    mount: mount,
    refresh: refresh,
  };
})();
