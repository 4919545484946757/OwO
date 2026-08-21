/* Lane 4 命令面板：统一自然语言/多模态命令入口（子任务 2）。
 * 纯脚本 IIFE，注册 window.OwoPanels.command。防御性降级。
 */
window.OwoPanels = window.OwoPanels || {};
window.OwoPanels.command = (function () {
  "use strict";

  var id = "command";
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
      ".owo-command-log{height:180px;overflow:auto;background:#111;color:#7cff9b;font-family:monospace;font-size:12px;padding:6px}" +
      ".owo-command-result{border:1px solid #ddd;border-radius:6px;padding:6px;margin:4px 0;font-size:12px;background:#fafafa}" +
      ".owo-command-tag{display:inline-block;padding:1px 8px;border-radius:8px;font-size:11px;background:#e3f2fd;margin-right:6px}" +
      "</style>" +
      '<div class="stack">' +
      '<div class="sub">统一命令入口（文本 / 语音 / 区域占位）</div>' +
      '<div class="owo-command-row" style="display:flex;gap:8px;align-items:center">' +
      '<select id="owo-command-mode" style="padding:4px"><option value="text">文本</option>' +
      '<option value="voice">语音</option><option value="region" disabled>区域（占位）</option></select>' +
      '<input id="owo-command-text" placeholder="例如：创建目标：整理桌面 / 搜索记忆：张子豪 / 运行工作流：报告" style="flex:1;padding:6px">' +
      '<button class="primary" id="owo-command-run">执行</button></div>' +
      '<input type="file" id="owo-command-wav" accept="audio/wav" style="display:none">' +
      '<div class="sub">意图</div><div id="owo-command-intent"></div>' +
      '<div class="sub">结果</div><div id="owo-command-results"></div>' +
      '<div class="sub">命令审计</div><div class="owo-command-log" id="owo-command-audit">（暂无）</div>' +
      "</div>"
    );
  }

  function mount(root, helpers) {
    if (helpers) H = helpers;
    root.innerHTML = nav();
    root.querySelector("#owo-command-run").addEventListener("click", runCommand);
    root.querySelector("#owo-command-text").addEventListener("keydown", function (e) {
      if (e.key === "Enter") runCommand();
    });
    root.querySelector("#owo-command-mode").addEventListener("change", function (e) {
      var wav = document.getElementById("owo-command-wav");
      if (wav) wav.style.display = e.target.value === "voice" ? "inline-block" : "none";
    });
    refresh();
  }

  function refresh() {
    H.get("/command/audit")
      .then(function (data) {
        var el = document.getElementById("owo-command-audit");
        if (!el) return;
        el.textContent = (data.audit || [])
          .slice(0, 20)
          .map(function (a) {
            return a.event + " — " + a.detail;
          })
          .join("\n");
      })
      .catch(function () {});
  }

  function readWavBase64(file) {
    return new Promise(function (resolve, reject) {
      var reader = new FileReader();
      reader.onload = function () {
        var base64 = String(reader.result).split(",")[1] || "";
        resolve(base64);
      };
      reader.onerror = reject;
      reader.readAsDataURL(file);
    });
  }

  function runCommand() {
    var mode = document.getElementById("owo-command-mode").value;
    var text = document.getElementById("owo-command-text").value;
    var body = { mode: mode };
    if (mode === "voice") {
      var wavInput = document.getElementById("owo-command-wav");
      if (!wavInput.files || !wavInput.files.length) {
        alert("语音模式请先选择 wav 文件");
        return;
      }
      readWavBase64(wavInput.files[0]).then(function (wavB64) {
        body.wav_b64 = wavB64;
        postCommand(body);
      });
      return;
    }
    body.text = text;
    postCommand(body);
  }

  function postCommand(body) {
    H.post("/command/run", body)
      .then(function (data) {
        var intentEl = document.getElementById("owo-command-intent");
        if (intentEl) {
          intentEl.innerHTML =
            '<span class="owo-command-tag">' + H.esc(data.intent) + "</span>" +
            "置信度 " + (data.confidence || 0).toFixed(2) + " ｜ " + H.esc(data.text || "");
        }
        var resultsEl = document.getElementById("owo-command-results");
        if (resultsEl) {
          resultsEl.innerHTML = renderResults(data.results || {});
        }
        refresh();
      })
      .catch(function (e) {
        var intentEl = document.getElementById("owo-command-intent");
        if (intentEl) intentEl.innerHTML = '<span class="owo-command-tag">' + H.esc(H.friendlyError(e)) + "</span>";
      });
  }

  function renderResults(results) {
    if (results && results.blocked) {
      return '<div class="owo-command-result"><b>已拦截</b>：' + H.esc(results.reason || "") + "</div>";
    }
    var lines = Object.keys(results).map(function (key) {
      var value = results[key];
      var rendered;
      if (Array.isArray(value)) {
        rendered = value
          .map(function (v) {
            return typeof v === "object" ? JSON.stringify(v) : String(v);
          })
          .join("<br>");
      } else if (typeof value === "object" && value !== null) {
        rendered = H.renderMarkdown(JSON.stringify(value, null, 2));
      } else {
        rendered = H.esc(String(value));
      }
      return "<b>" + H.esc(key) + "：</b>" + rendered;
    });
    return lines.length ? lines.map(function (l) { return '<div class="owo-command-result">' + l + "</div>"; }).join("") : "";
  }

  return {
    id: id,
    title: "统一命令入口",
    nav: nav,
    mount: mount,
    refresh: refresh,
  };
})();
