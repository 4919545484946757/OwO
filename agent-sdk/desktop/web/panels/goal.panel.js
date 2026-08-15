/* Lane D 编排面板：Goal/Plan 编排 API + 云端 SSE 进度。
 * 纯脚本 IIFE，注册 window.OwoPanels.goal。
 * 防御性降级：helpers 缺省时自建（fetch + esc + friendlyError）。
 */
window.OwoPanels = window.OwoPanels || {};
window.OwoPanels.goal = (function () {
  "use strict";

  var id = "goal";

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
    function renderMarkdown(text) {
      return esc(text);
    }
    return { baseUrl: baseUrl, get: get, post: post, esc: esc, friendlyError: friendlyError, renderMarkdown: renderMarkdown };
  }

  var H = defaultHelpers();
  var state = {
    goals: [],
    current: null,
    status: null,
    pollTimer: null,
    cloudLog: [],
  };

  function nav() {
    return (
      '<section data-panel="' + id + '">' +
      '<style>' +
      '.owo-goal-row{display:flex;gap:8px;align-items:center;padding:4px 0;border-bottom:1px solid #eee}' +
      '.owo-goal-steps{width:100%;min-height:120px;font-family:monospace;font-size:12px}' +
      '.owo-goal-table{width:100%;border-collapse:collapse;font-size:12px}' +
      '.owo-goal-table td,.owo-goal-table th{border:1px solid #ddd;padding:3px 6px;text-align:left}' +
      '.owo-goal-badge{display:inline-block;padding:1px 6px;border-radius:8px;font-size:11px;color:#fff}' +
      '.owo-goal-badge.ok{background:#2e7d32}.owo-goal-badge.bad{background:#c62828}' +
      '.owo-goal-badge.warn{background:#ef6c00}' +
      '.owo-goal-cloudlog{height:140px;overflow:auto;background:#111;color:#7cff9b;font-family:monospace;font-size:12px;padding:6px}' +
      '</style>' +
      '<div class="stack">' +
      '<div class="sub">编排目标（Goal/Plan）</div>' +
      '<div class="owo-goal-row"><input id="owo-goal-objective" placeholder="目标描述（objective）" style="flex:1">' +
      '<button class="primary" id="owo-goal-create">创建目标</button></div>' +
      '<div id="owo-goal-list" class="list"></div>' +
      '<hr>' +
      '<div id="owo-goal-detail"></div>' +
      '<hr>' +
      '<div class="sub">云端进度（SSE 订阅）</div>' +
      '<div class="owo-goal-row"><input id="owo-goal-cloud-task" placeholder="cloud task id（如 cloud-0001）" style="flex:1">' +
      '<button id="owo-goal-cloud-sub">订阅</button><button id="owo-goal-cloud-close">断开</button></div>' +
      '<div class="owo-goal-cloudlog" id="owo-goal-cloudlog">（输入 task id 订阅 /cloud/tasks/{id}/events）</div>' +
      '</div>'
    );
  }

  function mount(root, helpers) {
    if (helpers) H = helpers;
    root.innerHTML = nav();
    root.querySelector("#owo-goal-create").addEventListener("click", createGoal);
    root.querySelector("#owo-goal-cloud-sub").addEventListener("click", subscribeCloud);
    root.querySelector("#owo-goal-cloud-close").addEventListener("click", closeCloud);
    refresh();
  }

  function refresh() {
    H.get("/goal")
      .then(function (data) {
        state.goals = (data && data.goals) || [];
        renderList();
        if (state.current) loadGoal(state.current);
      })
      .catch(function (e) {
        var el = document.getElementById("owo-goal-list");
        if (el) el.innerHTML = '<div class="owo-goal-badge bad">' + H.esc(H.friendlyError(e)) + "</div>";
      });
  }

  function renderList() {
    var el = document.getElementById("owo-goal-list");
    if (!el) return;
    if (!state.goals.length) {
      el.innerHTML = '<div class="sub">暂无目标，先创建一个。</div>';
      return;
    }
    el.innerHTML = state.goals
      .map(function (g) {
        var badge = g.status === "Succeeded" ? "ok" : g.status === "Failed" || g.status === "Aborted" ? "bad" : "warn";
        return (
          '<div class="owo-goal-row">' +
          '<span class="owo-goal-badge ' + badge + '">' + H.esc(g.status) + "</span>" +
          "<span>" + H.esc(g.objective) + "</span>" +
          '<button data-id="' + H.esc(g.id) + '" class="owo-goal-open">打开</button>' +
          '<button data-id="' + H.esc(g.id) + '" class="owo-goal-run">运行</button>' +
          '<button data-id="' + H.esc(g.id) + '" class="owo-goal-abort">中止</button>' +
          "</div>"
        );
      })
      .join("");
    Array.prototype.forEach.call(el.querySelectorAll(".owo-goal-open"), function (b) {
      b.addEventListener("click", function () {
        state.current = b.getAttribute("data-id");
        loadGoal(state.current);
      });
    });
    Array.prototype.forEach.call(el.querySelectorAll(".owo-goal-run"), function (b) {
      b.addEventListener("click", function () {
        runGoal(b.getAttribute("data-id"));
      });
    });
    Array.prototype.forEach.call(el.querySelectorAll(".owo-goal-abort"), function (b) {
      b.addEventListener("click", function () {
        abortGoal(b.getAttribute("data-id"));
      });
    });
  }

  function createGoal() {
    var input = document.getElementById("owo-goal-objective");
    var objective = (input && input.value.trim()) || "新目标";
    H.post("/goal", { objective: objective })
      .then(function (data) {
        input.value = "";
        state.current = data.goal.id;
        refresh();
      })
      .catch(function (e) {
        alert(H.friendlyError(e));
      });
  }

  function loadGoal(goalId) {
    H.get("/goal/" + encodeURIComponent(goalId))
      .then(function (goal) {
        H.get("/goal/" + encodeURIComponent(goalId) + "/plan")
          .then(function (planData) {
            renderDetail(goal, planData);
          })
          .catch(function () {
            renderDetail(goal, null);
          });
      })
      .catch(function (e) {
        var el = document.getElementById("owo-goal-detail");
        if (el) el.innerHTML = '<div class="owo-goal-badge bad">' + H.esc(H.friendlyError(e)) + "</div>";
      });
  }

  function renderDetail(goal, planData) {
    var el = document.getElementById("owo-goal-detail");
    if (!el) return;
    var plan = (planData && planData.plan) || null;
    var waves = (planData && planData.waves) || null;
    var stepsJson = plan
      ? JSON.stringify(
          (plan.steps || []).map(function (s) {
            return { id: s.id, worker: s.worker, deps: s.depends_on || [], verify: s.verify ? (s.verify.OutputContains || s.verify.OutputEquals || s.verify.OutputNonEmpty || "") : null, max_retries: s.retries || 0, input: s.input || {} };
          }),
          null,
          2
        )
      : '[{"id":"a","worker":"echo","input":{"text":"A"}}]';
    var wavesHtml = waves
      ? waves
          .map(function (w, i) {
            return "wave" + (i + 1) + ": " + (w || []).join(", ");
          })
          .join("<br>")
      : "（暂无计划）";
    el.innerHTML =
      '<div class="sub">目标：' + H.esc(goal.objective) + "（" + H.esc(goal.status) + "）</div>" +
      '<div class="owo-goal-row"><span>步骤定义（JSON）</span>' +
      '<button id="owo-goal-save-plan">保存计划</button></div>' +
      '<textarea class="owo-goal-steps" id="owo-goal-steps">' + H.esc(stepsJson) + "</textarea>" +
      '<div class="owo-goal-row"><span>waves 预览</span>' +
      '<button id="owo-goal-run-now">运行（parallelism=2）</button>' +
      '<button id="owo-goal-poll">刷新状态</button></div>' +
      '<div class="sub">' + wavesHtml + "</div>" +
      '<table class="owo-goal-table" id="owo-goal-status"><tr><th>步骤</th><th>状态</th><th>尝试</th><th>输出</th></tr></table>' +
      '<div class="owo-goal-row"><button id="owo-goal-audit">审计尾部</button></div>' +
      '<div id="owo-goal-audit-box"></div>';
    el.querySelector("#owo-goal-save-plan").addEventListener("click", function () {
      savePlan(goal.id);
    });
    el.querySelector("#owo-goal-run-now").addEventListener("click", function () {
      runGoal(goal.id);
    });
    el.querySelector("#owo-goal-poll").addEventListener("click", function () {
      pollStatus(goal.id);
    });
    el.querySelector("#owo-goal-audit").addEventListener("click", function () {
      showAudit(goal.id);
    });
    pollStatus(goal.id);
  }

  function savePlan(goalId) {
    var textarea = document.getElementById("owo-goal-steps");
    var steps;
    try {
      steps = JSON.parse(textarea.value);
    } catch (e) {
      alert("steps JSON 非法：" + e.message);
      return;
    }
    var normalized = (steps || []).map(function (s) {
      return {
        id: s.id,
        worker: s.worker,
        deps: s.deps || [],
        verify: s.verify || null,
        max_retries: s.max_retries || 0,
        input: s.input || {},
        parallel: !!s.parallel,
      };
    });
    H.post("/goal/" + encodeURIComponent(goalId) + "/plan", { steps: normalized })
      .then(function (data) {
        var waves = data.waves || [];
        var preview = waves.map(function (w, i) { return "wave" + (i + 1) + ": " + w.join(", "); }).join("<br>");
        var box = document.querySelector("#owo-goal-detail .sub:nth-of-type(3)");
        if (box) box.innerHTML = preview;
        loadGoal(goalId);
      })
      .catch(function (e) {
        alert(H.friendlyError(e));
      });
  }

  function runGoal(goalId) {
    H.post("/goal/" + encodeURIComponent(goalId) + "/run", { config: { parallelism: 2, allow_replan: true } })
      .then(function () {
        pollStatus(goalId);
      })
      .catch(function (e) {
        alert(H.friendlyError(e));
      });
  }

  function abortGoal(goalId) {
    H.post("/goal/" + encodeURIComponent(goalId) + "/abort", {})
      .then(function () {
        pollStatus(goalId);
      })
      .catch(function (e) {
        alert(H.friendlyError(e));
      });
  }

  function pollStatus(goalId) {
    H.get("/goal/" + encodeURIComponent(goalId) + "/status")
      .then(function (status) {
        state.status = status;
        var table = document.getElementById("owo-goal-status");
        if (!table) return;
        var records = (status && status.records) || {};
        var rows = Object.keys(records)
          .map(function (stepId) {
            var r = records[stepId];
            var badge = r.status === "Succeeded" ? "ok" : r.status === "Failed" || r.status === "Aborted" ? "bad" : "warn";
            return (
              "<tr><td>" + H.esc(stepId) + '</td><td><span class="owo-goal-badge ' + badge + '">' + H.esc(r.status) + "</span></td>" +
              "<td>" + H.esc(r.attempts) + "</td><td>" + H.esc((r.output || "").slice(0, 60)) + "</td></tr>"
            );
          })
          .join("");
        var goalStatus = (status && status.goal_status) || "";
        table.innerHTML = "<tr><th>步骤</th><th>状态</th><th>尝试</th><th>输出</th></tr>" + rows +
          '<tr><td colspan="4">goal: ' + H.esc(goalStatus) + " ｜ steps_taken: " + H.esc(status.steps_taken) + " ｜ replan: " + H.esc(status.replan_count) + "</td></tr>";
      })
      .catch(function () {});
  }

  function showAudit(goalId) {
    H.get("/goal/" + encodeURIComponent(goalId) + "/audit")
      .then(function (data) {
        var box = document.getElementById("owo-goal-audit-box");
        if (!box) return;
        box.innerHTML = (data.audit || [])
          .slice(0, 20)
          .map(function (e) {
            return "<div>" + H.esc(e.event) + " — " + H.esc(e.detail) + "</div>";
          })
          .join("");
      })
      .catch(function (e) {
        var box = document.getElementById("owo-goal-audit-box");
        if (box) box.innerHTML = H.esc(H.friendlyError(e));
      });
  }

  var cloudSource = null;

  function subscribeCloud() {
    var input = document.getElementById("owo-goal-cloud-task");
    var taskId = (input && input.value.trim()) || "";
    if (!taskId) return;
    closeCloud();
    var base = H.baseUrl || "http://127.0.0.1:4098";
    var log = document.getElementById("owo-goal-cloudlog");
    if (log) log.textContent = "订阅 " + taskId + " ...";
    cloudSource = new EventSource(base + "/cloud/tasks/" + encodeURIComponent(taskId) + "/events");
    cloudSource.onmessage = function (ev) {
      appendCloudLog(ev.data);
    };
    cloudSource.onerror = function () {
      appendCloudLog("（连接错误/关闭）");
      if (cloudSource) {
        cloudSource.close();
        cloudSource = null;
      }
    };
  }

  function closeCloud() {
    if (cloudSource) {
      cloudSource.close();
      cloudSource = null;
    }
    appendCloudLog("（已断开）");
  }

  function appendCloudLog(line) {
    var log = document.getElementById("owo-goal-cloudlog");
    if (!log) return;
    state.cloudLog.push(String(line));
    if (state.cloudLog.length > 200) state.cloudLog.shift();
    log.textContent = state.cloudLog.join("\n");
    log.scrollTop = log.scrollHeight;
  }

  return {
    id: id,
    title: "编排（Goal/Plan + 云端进度）",
    nav: nav,
    mount: mount,
    refresh: refresh,
  };
})();
