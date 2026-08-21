// 控制面（P2 双节点网格）面板：节点注册/列表、任务提交/查询/取消/事件、审批响应。
// 纯脚本 IIFE 注册 window.OwoPanels.fleet；helpers 缺省时自建（fetch + esc + friendlyError）。
(function () {
  "use strict";

  window.OwoPanels = window.OwoPanels || {};

  window.OwoPanels.fleet = (function () {
    var H = {};

    function defaultGet(path) {
      return fetch((H.baseUrl || "") + path).then(function (r) {
        return r.json();
      });
    }
    function defaultPost(path, body) {
      return fetch((H.baseUrl || "") + path, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(body || {}),
      }).then(function (r) {
        return r.json();
      });
    }
    function defaultEsc(s) {
      return String(s == null ? "" : s)
        .replace(/&/g, "&amp;")
        .replace(/</g, "&lt;")
        .replace(/>/g, "&gt;")
        .replace(/"/g, "&quot;");
    }
    function defaultFriendlyError(e) {
      return String((e && e.message) || e || "未知错误");
    }

    function nav() {
      return (
        '<section data-panel="fleet" class="owo-fleet-panel">' +
        '<div class="owo-fleet-tools">' +
        "<h3>节点注册</h3>" +
        '<div class="inline">' +
        '<input class="owo-fleet-node-id" placeholder="node_id（如 node-a）">' +
        '<input class="owo-fleet-node-worker" placeholder="worker（如 shell）">' +
        '<button class="owo-fleet-node-register">注册</button>' +
        "</div>" +
        '<pre class="owo-fleet-node-result sub">—</pre>' +
        "</div>" +
        '<div class="owo-fleet-tools">' +
        "<h3>节点列表</h3>" +
        '<div class="inline"><button class="owo-fleet-nodes-refresh">刷新</button></div>' +
        '<pre class="owo-fleet-nodes sub">—</pre>' +
        "</div>" +
        '<div class="owo-fleet-tools">' +
        "<h3>任务提交</h3>" +
        '<div class="inline">' +
        '<input class="owo-fleet-task-id" placeholder="task_id（如 t-1）">' +
        '<input class="owo-fleet-task-worker" placeholder="worker（如 node-a）">' +
        "</div>" +
        '<textarea class="owo-fleet-task-input" rows="3" spellcheck="false" placeholder=\'input JSON，如 {"q":1}\'></textarea>' +
        '<label class="inline"><input type="checkbox" class="owo-fleet-task-approval"> 需审批（approval_required）</label>' +
        '<div class="inline"><button class="owo-fleet-task-submit primary">提交</button></div>' +
        '<pre class="owo-fleet-task-submit-result sub">—</pre>' +
        "</div>" +
        '<div class="owo-fleet-tools">' +
        "<h3>任务查询 / 取消 / 事件</h3>" +
        '<div class="inline">' +
        '<input class="owo-fleet-task-get-id" placeholder="task_id">' +
        '<button class="owo-fleet-task-get">查询</button>' +
        '<button class="owo-fleet-task-cancel">取消</button>' +
        '<button class="owo-fleet-task-events">事件(JSON)</button>' +
        "</div>" +
        '<pre class="owo-fleet-task-view sub">—</pre>' +
        "</div>" +
        '<div class="owo-fleet-tools">' +
        "<h3>审批响应</h3>" +
        '<div class="inline">' +
        '<input class="owo-fleet-approval-id" placeholder="task_id（审批任务）">' +
        '<select class="owo-fleet-approval-decision"><option value="approve">approve</option><option value="reject">reject</option></select>' +
        '<input class="owo-fleet-approval-by" placeholder="approved_by（如 owner）">' +
        '<button class="owo-fleet-approval-respond primary">裁决</button>' +
        "</div>" +
        '<pre class="owo-fleet-approval-result sub">—</pre>' +
        "</div>" +
        "<style>" +
        ".owo-fleet-panel { display: flex; flex-direction: column; gap: 10px; }" +
        ".owo-fleet-tools { border: 1px solid var(--border, #333); border-radius: 6px; padding: 8px; }" +
        ".owo-fleet-tools h3 { margin: 0 0 6px; font-size: 13px; }" +
        ".owo-fleet-panel textarea { width: 100%; box-sizing: border-box; }" +
        ".owo-fleet-panel input { width: auto; }" +
        ".owo-fleet-nodes, .owo-fleet-node-result, .owo-fleet-task-view, .owo-fleet-task-submit-result, .owo-fleet-approval-result { white-space: pre-wrap; max-height: 180px; overflow: auto; }" +
        "</style>" +
        "</section>"
      );
    }

    function mount(root, helpers) {
      H = helpers || {};
      H.baseUrl = H.baseUrl || (window.OwoPanels && window.OwoPanels.baseUrl) || "";
      H.get = H.get || defaultGet;
      H.post = H.post || defaultPost;
      H.esc = H.esc || defaultEsc;
      H.friendlyError = H.friendlyError || defaultFriendlyError;

      root.innerHTML = nav();
      var $ = function (sel) {
        return root.querySelector(sel);
      };

      $(".owo-fleet-node-register").addEventListener("click", function () {
        doRegister($(".owo-fleet-node-id").value, $(".owo-fleet-node-worker").value);
      });
      $(".owo-fleet-nodes-refresh").addEventListener("click", function () {
        listNodes();
      });
      $(".owo-fleet-task-submit").addEventListener("click", function () {
        doSubmit(
          $(".owo-fleet-task-id").value,
          $(".owo-fleet-task-worker").value,
          $(".owo-fleet-task-input").value,
          $(".owo-fleet-task-approval").checked
        );
      });
      $(".owo-fleet-task-get").addEventListener("click", function () {
        getTask($(".owo-fleet-task-get-id").value);
      });
      $(".owo-fleet-task-cancel").addEventListener("click", function () {
        cancelTask($(".owo-fleet-task-get-id").value);
      });
      $(".owo-fleet-task-events").addEventListener("click", function () {
        taskEvents($(".owo-fleet-task-get-id").value);
      });
      $(".owo-fleet-approval-respond").addEventListener("click", function () {
        respondApproval(
          $(".owo-fleet-approval-id").value,
          $(".owo-fleet-approval-decision").value,
          $(".owo-fleet-approval-by").value
        );
      });

      listNodes();
    }

    function refresh() {
      listNodes();
    }

    function output(sel, text) {
      var el = document.querySelector(".owo-fleet-panel " + sel);
      if (el) el.textContent = text;
    }

    function doRegister(nodeId, worker) {
      if (!nodeId) return output(".owo-fleet-node-result", "请填写 node_id");
      H.post("/fleet/nodes/register", {
        node_id: nodeId,
        card: { worker: worker || nodeId, actions: ["shell"] },
      })
        .then(function (d) {
          output(".owo-fleet-node-result", JSON.stringify(d, null, 2));
          listNodes();
        })
        .catch(function (e) {
          output(".owo-fleet-node-result", "注册失败：" + H.friendlyError(e));
        });
    }

    function listNodes() {
      H.get("/fleet/nodes")
        .then(function (d) {
          var nodes = (d && d.nodes) || [];
          var text = "count=" + nodes.length;
          for (var i = 0; i < nodes.length; i++) {
            var n = nodes[i] || {};
            text +=
              "\n" +
              H.esc(n.id || "?") +
              "  healthy=" +
              H.esc(n.healthy) +
              "  registered=" +
              H.esc(n.registered) +
              "  lease_epoch=" +
              H.esc(n.lease_epoch);
          }
          output(".owo-fleet-nodes", text);
        })
        .catch(function (e) {
          output(".owo-fleet-nodes", "列表失败：" + H.friendlyError(e));
        });
    }

    function doSubmit(taskId, worker, inputText, approvalRequired) {
      if (!taskId || !worker) return output(".owo-fleet-task-submit-result", "请填写 task_id 与 worker");
      var input = {};
      try {
        input = inputText.trim() ? JSON.parse(inputText) : { q: 1 };
      } catch (e) {
        return output(".owo-fleet-task-submit-result", "input 不是合法 JSON：" + H.friendlyError(e));
      }
      H.post("/fleet/tasks/submit", {
        task_id: taskId,
        worker: worker,
        input: input,
        approval_required: !!approvalRequired,
      })
        .then(function (d) {
          output(".owo-fleet-task-submit-result", JSON.stringify(d, null, 2));
        })
        .catch(function (e) {
          output(".owo-fleet-task-submit-result", "提交失败：" + H.friendlyError(e));
        });
    }

    function getTask(taskId) {
      if (!taskId) return output(".owo-fleet-task-view", "请填写 task_id");
      H.get("/fleet/tasks/" + encodeURIComponent(taskId))
        .then(function (d) {
          output(".owo-fleet-task-view", JSON.stringify(d, null, 2));
        })
        .catch(function (e) {
          output(".owo-fleet-task-view", "查询失败：" + H.friendlyError(e));
        });
    }

    function cancelTask(taskId) {
      if (!taskId) return output(".owo-fleet-task-view", "请填写 task_id");
      H.post("/fleet/tasks/" + encodeURIComponent(taskId) + "/cancel", {})
        .then(function (d) {
          output(".owo-fleet-task-view", JSON.stringify(d, null, 2));
        })
        .catch(function (e) {
          output(".owo-fleet-task-view", "取消失败：" + H.friendlyError(e));
        });
    }

    function taskEvents(taskId) {
      if (!taskId) return output(".owo-fleet-task-view", "请填写 task_id");
      H.get("/fleet/tasks/" + encodeURIComponent(taskId) + "/events?format=json")
        .then(function (d) {
          output(".owo-fleet-task-view", "事件：\n" + JSON.stringify(d, null, 2));
        })
        .catch(function (e) {
          output(".owo-fleet-task-view", "事件拉取失败：" + H.friendlyError(e));
        });
    }

    function respondApproval(taskId, decision, approvedBy) {
      if (!taskId) return output(".owo-fleet-approval-result", "请填写审批任务 task_id");
      H.post("/fleet/approvals/" + encodeURIComponent(taskId) + "/respond", {
        decision: decision,
        approved_by: approvedBy || "workbench",
      })
        .then(function (d) {
          output(".owo-fleet-approval-result", JSON.stringify(d, null, 2));
        })
        .catch(function (e) {
          output(".owo-fleet-approval-result", "裁决失败：" + H.friendlyError(e));
        });
    }

    return {
      id: "fleet",
      title: "控制面（Fleet）",
      nav: nav,
      mount: mount,
      refresh: refresh,
    };
  })();
})();
