/* 工作流面板（Lane C）：.owflow 列表 / 定义预览 / validate / 运行 / runs / 步骤时间线 / abort / audit。
 * 纯脚本 IIFE，依赖 window.OwoPanels.<lane> 契约与 helpers（防御性降级）。 */
(function () {
  "use strict";

  window.OwoPanels = window.OwoPanels || {};

  var BASE = (window.OwoPanels && window.OwoPanels.baseUrl) || "http://127.0.0.1:4098";
  var self = null; // 面板实例（模块级单例）

  function getHelpers() {
    return (self && self.helpers) || {};
  }

  function esc(s) {
    var h = getHelpers().esc;
    if (h) { return h(s); }
    return String(s == null ? "" : s)
      .replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;")
      .replace(/"/g, "&quot;").replace(/'/g, "&#39;");
  }

  function friendlyError(e) {
    var h = getHelpers().friendlyError;
    if (h) { return h(e); }
    if (e && e.error) { return String(e.error); }
    return String((e && e.message) || e || "请求失败");
  }

  function get(path) {
    var h = getHelpers();
    if (h && h.get) { return h.get(path); }
    return fetch(BASE + path).then(function (r) {
      if (!r.ok) { return r.json().then(function (j) { throw j; }); }
      return r.json();
    });
  }

  function post(path, body) {
    var h = getHelpers();
    if (h && h.post) { return h.post(path, body); }
    return fetch(BASE + path, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(body == null ? {} : body),
    }).then(function (r) {
      if (!r.ok) { return r.json().then(function (j) { throw j; }); }
      return r.json();
    });
  }

  function style() {
    return (
      "<style>" +
      ".owo-workflow-section{margin-bottom:14px}" +
      ".owo-workflow-grid{display:grid;grid-template-columns:1fr 1fr;gap:12px}" +
      ".owo-workflow-card{border:1px solid #e2e2e2;border-radius:8px;padding:10px;margin-bottom:8px}" +
      ".owo-workflow-card h4{margin:0 0 6px 0}" +
      ".owo-workflow-name{font-weight:600}" +
      ".owo-workflow-badge{display:inline-block;padding:1px 8px;border-radius:10px;font-size:12px}" +
      ".owo-workflow-badge-ok{background:#e6f4ea;color:#1e7e34}" +
      ".owo-workflow-badge-bad{background:#fdecea;color:#b3261e}" +
      ".owo-workflow-badge-run{background:#e8f0fe;color:#1a56db}" +
      ".owo-workflow-json{background:#f6f8fa;border-radius:6px;padding:8px;font-family:monospace;font-size:12px;white-space:pre-wrap;word-break:break-all;max-height:220px;overflow:auto}" +
      ".owo-workflow-step{display:flex;gap:8px;align-items:baseline;padding:3px 0;border-bottom:1px dashed #eee}" +
      ".owo-workflow-step-ok{color:#1e7e34}.owo-workflow-step-fail{color:#b3261e}" +
      ".owo-workflow-btn{margin-right:6px}" +
      ".owo-workflow-input{width:100%;box-sizing:border-box;margin:4px 0;padding:6px;border:1px solid #ccc;border-radius:6px}" +
      ".owo-workflow-audit{max-height:180px;overflow:auto;font-size:12px;font-family:monospace}" +
      "</style>"
    );
  }

  function nav() {
    return (
      '<section data-panel="workflow" class="owo-workflow-root">' +
      '<h3>工作流（.owflow）</h3>' +
      '<div class="owo-workflow-grid">' +
      '<div class="owo-workflow-section">' +
      '<h4>流程列表</h4>' +
      '<div id="owo-workflow-list" class="owo-workflow-list">加载中…</div>' +
      '<button id="owo-workflow-refresh" class="primary owo-workflow-btn">刷新</button>' +
      '</div>' +
      '<div class="owo-workflow-section">' +
      '<h4>运行器</h4>' +
      '<div id="owo-workflow-runner">选择左侧流程查看定义并运行</div>' +
      '</div>' +
      '</div>' +
      '<div class="owo-workflow-section">' +
      '<h4>校验（内联 DSL）</h4>' +
      '<textarea id="owo-workflow-validate-dsl" class="owo-workflow-input" rows="6" placeholder="粘贴 .owflow JSON 定义…"></textarea>' +
      '<button id="owo-workflow-validate-btn" class="owo-workflow-btn">校验</button>' +
      '<span id="owo-workflow-validate-result"></span>' +
      '</div>' +
      '<div class="owo-workflow-section">' +
      '<h4>Runs</h4>' +
      '<div id="owo-workflow-runs">无</div>' +
      '</div>' +
      '<div class="owo-workflow-section">' +
      '<h4>运行审计</h4>' +
      '<div id="owo-workflow-audit" class="owo-workflow-audit">—</div>' +
      '</div>' +
      '</section>'
    );
  }

  function renderList(flows) {
    var el = document.getElementById("owo-workflow-list");
    if (!el) { return; }
    if (!flows || !flows.length) {
      el.innerHTML = '<div class="sub">未发现 .owflow 文件（工作区 ' + esc(BASE) + '）</div>';
      return;
    }
    el.innerHTML = flows
      .map(function (f) {
        return (
          '<div class="owo-workflow-card">' +
          '<span class="owo-workflow-name">' + esc(f.name) + '</span>' +
          '<span class="sub"> — ' + esc(f.path) + '</span><br/>' +
          '<button class="owo-workflow-btn owo-workflow-load" data-name="' + esc(f.name) + '">加载</button>' +
          '<button class="owo-workflow-btn owo-workflow-run" data-name="' + esc(f.name) + '">运行</button>' +
          '</div>'
        );
      })
      .join("");
    el.querySelectorAll(".owo-workflow-load").forEach(function (btn) {
      btn.addEventListener("click", function () { loadFlow(btn.getAttribute("data-name")); });
    });
    el.querySelectorAll(".owo-workflow-run").forEach(function (btn) {
      btn.addEventListener("click", function () { runFlow(btn.getAttribute("data-name")); });
    });
  }

  function loadFlow(name) {
    var runner = document.getElementById("owo-workflow-runner");
    runner.innerHTML = "加载 " + esc(name) + "…";
    get("/workflow/" + encodeURIComponent(name))
      .then(function (data) {
        var badge = data.valid
          ? '<span class="owo-workflow-badge owo-workflow-badge-ok">valid</span>'
          : '<span class="owo-workflow-badge owo-workflow-badge-bad">invalid</span>';
        var issues = (data.issues || []).map(function (i) { return "<div class='owo-workflow-step owo-workflow-step-fail'>" + esc(i) + "</div>"; }).join("");
        runner.innerHTML =
          "<h4>" + esc(name) + " " + badge + "</h4>" +
          '<div class="owo-workflow-json">' + esc(JSON.stringify(data.definition, null, 2)) + "</div>" +
          issues +
          '<label>ctx（JSON 对象，可选）</label>' +
          '<input id="owo-workflow-ctx" class="owo-workflow-input" placeholder=\'{"key": "value"}\' />' +
          '<button id="owo-workflow-run-this" class="primary">运行</button>';
        document.getElementById("owo-workflow-run-this").addEventListener("click", function () {
          runFlow(name, document.getElementById("owo-workflow-ctx").value);
        });
      })
      .catch(function (e) {
        runner.innerHTML = '<div class="owo-workflow-step owo-workflow-step-fail">' + esc(friendlyError(e)) + "</div>";
      });
  }

  function runFlow(name, ctxText) {
    var ctx = {};
    if (ctxText && ctxText.trim()) {
      try { ctx = JSON.parse(ctxText); } catch (e) { alert("ctx 不是合法 JSON：" + e.message); return; }
    }
    post("/workflow/" + encodeURIComponent(name) + "/run", { ctx: ctx })
      .then(function (data) {
        var runId = data.run_id;
        var result = document.getElementById("owo-workflow-runner");
        result.innerHTML += '<div class="sub">已启动 run：' + esc(runId) + "</div>";
        pollRun(runId, 0);
        refreshRuns(name);
      })
      .catch(function (e) {
        alert(friendlyError(e));
      });
  }

  function pollRun(runId, attempt) {
    if (attempt > 200) { return; }
    get("/workflow/run/" + encodeURIComponent(runId))
      .then(function (snap) {
        renderSnapshot(snap, attempt === 0);
        if (snap.state === "running") {
          setTimeout(function () { pollRun(runId, attempt + 1); }, 300);
        }
      })
      .catch(function () {});
  }

  function renderSnapshot(snap) {
    var el = document.getElementById("owo-workflow-runner");
    if (!el) { return; }
    var badge = "owo-workflow-badge-run";
    if (snap.state === "succeeded") { badge = "owo-workflow-badge-ok"; }
    if (snap.state === "failed" || snap.state === "aborted") { badge = "owo-workflow-badge-bad"; }
    var steps = (snap.steps || [])
      .map(function (s) {
        var cls = s.ok ? "owo-workflow-step-ok" : "owo-workflow-step-fail";
        return (
          '<div class="owo-workflow-step"><span class="' + cls + '">' +
          (s.ok ? "✓" : "✗") + " " + esc(s.kind) + "</span>" +
          '<span class="sub">' + esc(s.id) + " — " + esc(s.detail) + "</span></div>"
        );
      })
      .join("") || '<div class="sub">（无步骤）</div>';
    el.innerHTML =
      "<h4>run " + esc(snap.run_id) + ' <span class="owo-workflow-badge ' + badge + '">' + esc(snap.state) + "</span></h4>" +
      (snap.rollback_to ? '<div class="owo-workflow-step owo-workflow-step-fail">已回滚到检查点：' + esc(snap.rollback_to) + "</div>" : "") +
      steps +
      '<div><button id="owo-workflow-abort" class="owo-workflow-btn">abort</button>' +
      '<button id="owo-workflow-audit-btn" class="owo-workflow-btn">审计尾部</button></div>';
    document.getElementById("owo-workflow-abort").addEventListener("click", function () {
      post("/workflow/run/" + encodeURIComponent(snap.run_id) + "/abort", {})
        .then(function () { pollRun(snap.run_id, 0); })
        .catch(function (e) { alert(friendlyError(e)); });
    });
    document.getElementById("owo-workflow-audit-btn").addEventListener("click", function () {
      loadAudit(snap.run_id);
    });
  }

  function loadAudit(runId) {
    get("/workflow/run/" + encodeURIComponent(runId) + "/audit")
      .then(function (data) {
        var el = document.getElementById("owo-workflow-audit");
        el.innerHTML = (data.audit || [])
          .map(function (a) {
            return '<div>' + esc(a.ts || "") + " [" + esc(a.event || "") + "] " + esc(a.detail || "") + "</div>";
          })
          .join("") || "（空）";
      })
      .catch(function (e) { alert(friendlyError(e)); });
  }

  function refreshRuns(name) {
    var el = document.getElementById("owo-workflow-runs");
    if (!el) { return; }
    get("/workflow/" + encodeURIComponent(name) + "/runs")
      .then(function (data) {
        var runs = data.runs || [];
        if (!runs.length) { el.innerHTML = "无"; return; }
        el.innerHTML = runs
          .map(function (r) {
            return (
              '<div class="owo-workflow-step">' +
              '<span class="owo-workflow-name">' + esc(r.run_id) + "</span>" +
              '<span class="sub">' + esc(r.state) + " · " + esc(r.created_at) + "</span>" +
              '<button class="owo-workflow-btn owo-workflow-snap" data-run="' + esc(r.run_id) + '">快照</button>' +
              "</div>"
            );
          })
          .join("");
        el.querySelectorAll(".owo-workflow-snap").forEach(function (btn) {
          btn.addEventListener("click", function () { pollRun(btn.getAttribute("data-run"), 0); });
        });
      })
      .catch(function (e) {
        el.innerHTML = '<span class="owo-workflow-step owo-workflow-step-fail">' + esc(friendlyError(e)) + "</span>";
      });
  }

  function bindValidate() {
    var btn = document.getElementById("owo-workflow-validate-btn");
    if (!btn) { return; }
    btn.addEventListener("click", function () {
      var dsl = document.getElementById("owo-workflow-validate-dsl").value;
      var result = document.getElementById("owo-workflow-validate-result");
      var parsed;
      try { parsed = JSON.parse(dsl); } catch (e) { result.innerHTML = '<span class="owo-workflow-step-fail">JSON 解析失败：' + esc(e.message) + "</span>"; return; }
      post("/workflow/validate", parsed)
        .then(function (data) {
          if (data.valid) {
            result.innerHTML = '<span class="owo-workflow-badge owo-workflow-badge-ok">valid</span>';
          } else {
            result.innerHTML = '<span class="owo-workflow-badge owo-workflow-badge-bad">invalid</span>' +
              (data.issues || []).map(function (i) { return "<div class='owo-workflow-step owo-workflow-step-fail'>" + esc(i) + "</div>"; }).join("");
          }
        })
        .catch(function (e) { result.innerHTML = '<span class="owo-workflow-step-fail">' + esc(friendlyError(e)) + "</span>"; });
    });
  }

  function bindRefresh() {
    var btn = document.getElementById("owo-workflow-refresh");
    if (!btn) { return; }
    btn.addEventListener("click", function () { self.refresh(); });
  }

  window.OwoPanels.workflow = {
    id: "workflow",
    title: "工作流",
    nav: nav,
    mount: function (root, helpers) {
      self = this;
      this.helpers = helpers || {};
      root.innerHTML = style() + this.nav();
      this.refresh();
      bindValidate();
      bindRefresh();
    },
    refresh: function () {
      get("/workflow")
        .then(function (data) { renderList(data.flows); })
        .catch(function (e) {
          var el = document.getElementById("owo-workflow-list");
          if (el) { el.innerHTML = '<span class="owo-workflow-step owo-workflow-step-fail">' + esc(friendlyError(e)) + "</span>"; }
        });
    },
  };
})();
