/* Lane 4 记忆面板：记忆图谱（第二大脑 · 子任务 1）。
 * 纯脚本 IIFE，注册 window.OwoPanels.memory。防御性降级。
 */
window.OwoPanels = window.OwoPanels || {};
window.OwoPanels.memory = (function () {
  "use strict";

  var id = "memory";
  var H = null;

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

  function nav() {
    return (
      '<section data-panel="' + id + '">' +
      "<style>" +
      ".owo-memory-card{display:inline-block;margin:4px;padding:4px 10px;border:1px solid #bbb;border-radius:12px;font-size:12px;background:#f5f5f5}" +
      ".owo-memory-row{display:flex;gap:8px;align-items:center;padding:3px 0;border-bottom:1px solid #eee;font-size:12px}" +
      ".owo-memory-rel{display:inline-block;margin:2px;padding:2px 8px;border:1px solid #9cf;border-radius:8px;font-size:12px}" +
      ".owo-memory-hit{background:#fff8dc;padding:2px 4px;border-radius:4px;font-size:12px}" +
      "</style>" +
      '<div class="stack">' +
      '<div class="sub">记忆图谱（结构化检索 / 时间线 / 实体 / 关系 / recall）</div>' +
      '<div class="owo-memory-row"><input id="owo-memory-recall" placeholder="recall 查询（如：张子豪）" style="flex:1">' +
      '<button class="primary" id="owo-memory-recall-btn">检索</button></div>' +
      '<div id="owo-memory-recall-box"></div>' +
      '<div class="sub">时间线</div><div id="owo-memory-timeline" class="list"></div>' +
      '<div class="sub">实体（词元频次 + 共现）</div><div id="owo-memory-entities"></div>' +
      '<div class="sub">手动关系</div>' +
      '<div class="owo-memory-row"><input id="owo-memory-rel-a" placeholder="实体A" style="flex:1">' +
      '<input id="owo-memory-rel-b" placeholder="实体B" style="flex:1">' +
      '<input id="owo-memory-rel-r" placeholder="关系（如：约定）" style="flex:1">' +
      '<button id="owo-memory-rel-add">添加</button></div>' +
      '<div id="owo-memory-relations"></div>' +
      '<div class="sub">条目（app/时间过滤）</div>' +
      '<div class="owo-memory-row"><input id="owo-memory-app" placeholder="app（如 qq）" style="flex:1">' +
      '<button id="owo-memory-refresh">刷新</button></div>' +
      '<div id="owo-memory-entries" class="list"></div>' +
      "</div>"
    );
  }

  function mount(root, helpers) {
    if (helpers) H = helpers;
    root.innerHTML = nav();
    root.querySelector("#owo-memory-recall-btn").addEventListener("click", doRecall);
    root.querySelector("#owo-memory-recall").addEventListener("keydown", function (e) {
      if (e.key === "Enter") doRecall();
    });
    root.querySelector("#owo-memory-rel-add").addEventListener("click", addRelation);
    root.querySelector("#owo-memory-refresh").addEventListener("click", refresh);
    refresh();
  }

  function refresh() {
    loadTimeline();
    loadEntities();
    loadRelations();
    loadEntries();
  }

  function doRecall() {
    var input = document.getElementById("owo-memory-recall");
    var q = (input && input.value.trim()) || "";
    H.get("/memory/graph/recall?q=" + encodeURIComponent(q) + "&top_k=5")
      .then(function (data) {
        var box = document.getElementById("owo-memory-recall-box");
        if (!box) return;
        box.innerHTML =
          "<div class='sub'>命中 " + (data.count || 0) + " 条</div>" +
          (data.hits || [])
            .map(function (h) {
              return (
                '<div class="owo-memory-hit">[' + H.esc(h.app_id) + "] " + H.esc(h.ts) + " — " +
                H.esc(h.summary) +
                (h.matched_entities && h.matched_entities.length ? " ｜ 实体命中：" + h.matched_entities.map(H.esc).join("、") : "") +
                "</div>"
              );
            })
            .join("");
      })
      .catch(function (e) {
        var box = document.getElementById("owo-memory-recall-box");
        if (box) box.innerHTML = '<div class="owo-memory-hit">' + H.esc(H.friendlyError(e)) + "</div>";
      });
  }

  function loadTimeline() {
    H.get("/memory/graph/timeline")
      .then(function (data) {
        var el = document.getElementById("owo-memory-timeline");
        if (!el) return;
        el.innerHTML = (data.buckets || [])
          .map(function (b) {
            return (
              '<div class="owo-memory-row"><b>' + H.esc(b.day) + "</b> ｜ " +
              b.count + " 条</div>"
            );
          })
          .join("");
      })
      .catch(function () {});
  }

  function loadEntities() {
    H.get("/memory/graph/entities?limit=30")
      .then(function (data) {
        var el = document.getElementById("owo-memory-entities");
        if (!el) return;
        el.innerHTML = (data.entities || [])
          .map(function (e) {
            var related = (e.related || [])
              .map(function (r) {
                return H.esc(r.entity) + "×" + r.count;
              })
              .join(", ");
            return (
              '<span class="owo-memory-card"><b>' + H.esc(e.entity) + "</b>×" + e.count +
              (related ? " <small>(" + related + ")</small>" : "") +
              "</span>"
            );
          })
          .join("");
      })
      .catch(function () {});
  }

  function loadRelations() {
    H.get("/memory/graph/links")
      .then(function (data) {
        var el = document.getElementById("owo-memory-relations");
        if (!el) return;
        el.innerHTML = (data.links || [])
          .map(function (l) {
            return (
              '<span class="owo-memory-rel">' + H.esc(l.a) + " —" + H.esc(l.relation) + "→ " + H.esc(l.b) + "</span>"
            );
          })
          .join("");
      })
      .catch(function () {});
  }

  function addRelation() {
    var a = document.getElementById("owo-memory-rel-a");
    var b = document.getElementById("owo-memory-rel-b");
    var r = document.getElementById("owo-memory-rel-r");
    H.post("/memory/graph/link", { a: a.value.trim(), b: b.value.trim(), relation: r.value.trim() })
      .then(function () {
        a.value = "";
        b.value = "";
        r.value = "";
        loadRelations();
      })
      .catch(function (e) {
        alert(H.friendlyError(e));
      });
  }

  function loadEntries() {
    var app = document.getElementById("owo-memory-app");
    var query = (app && app.value.trim()) ? "?app=" + encodeURIComponent(app.value.trim()) : "";
    H.get("/memory/graph/entries" + query + (query ? "&" : "?") + "limit=50")
      .then(function (data) {
        var el = document.getElementById("owo-memory-entries");
        if (!el) return;
        el.innerHTML = (data.entries || [])
          .map(function (e) {
            return (
              '<div class="owo-memory-row">[' + H.esc(e.app_id) + "] " + H.esc(e.ts) + " — " + H.esc(e.summary) + "</div>"
            );
          })
          .join("");
      })
      .catch(function (e) {
        var el = document.getElementById("owo-memory-entries");
        if (el) el.innerHTML = H.esc(H.friendlyError(e));
      });
  }

  return {
    id: id,
    title: "记忆图谱",
    nav: nav,
    mount: mount,
    refresh: refresh,
  };
})();
