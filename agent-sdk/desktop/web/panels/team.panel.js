// 团队技能包共享面板（Agent 2 子任务 2）：导出/导入评审/版本历史/审计。
(function () {
  "use strict";

  window.OwoPanels = window.OwoPanels || {};

  window.OwoPanels["team"] = {
    id: "team",
    title: "团队技能包",

    nav: function () {
      return (
        '<section data-panel="team" class="owo-team-panel">' +
        '<div class="owo-team-tools">' +
        "<h3>导出</h3>" +
        '<div class="inline"><input class="owo-team-export-id" placeholder="技能包 id（本地 store）"><button class="owo-team-exportbtn">导出</button></div>' +
        '<pre class="owo-team-export-result sub">—</pre>' +
        "</div>" +
        '<div class="owo-team-tools">' +
        "<h3>导入 / 评审</h3>" +
        '<textarea class="owo-team-import-b64" rows="4" spellcheck="false" placeholder="package_b64（base64 打包字节）"></textarea>' +
        '<div class="inline">' +
        '<button class="owo-team-reviewbtn">只评审</button>' +
        '<button class="owo-team-importbtn primary">导入（评审通过才落盘）</button>' +
        "</div>" +
        '<div class="owo-team-findings sub"></div>' +
        "</div>" +
        '<div class="owo-team-tools">' +
        "<h3>版本历史</h3>" +
        '<div class="inline"><input class="owo-team-versions-id" placeholder="技能包 id"><button class="owo-team-versionsbtn">查询</button></div>' +
        '<pre class="owo-team-versions sub">—</pre>' +
        "</div>" +
        '<div class="owo-team-tools">' +
        "<h3>审计尾部</h3>" +
        '<pre class="owo-team-audit sub">—</pre>' +
        "</div>" +
        "<style>" +
        ".owo-team-panel { display: flex; flex-direction: column; gap: 10px; }" +
        ".owo-team-tools { border: 1px solid var(--border, #333); border-radius: 6px; padding: 8px; }" +
        ".owo-team-tools h3 { margin: 0 0 6px; font-size: 13px; }" +
        ".owo-team-findings { white-space: pre-wrap; max-height: 220px; overflow: auto; }" +
        ".owo-team-versions, .owo-team-audit, .owo-team-export-result { white-space: pre-wrap; max-height: 160px; overflow: auto; }" +
        ".owo-team-panel textarea { width: 100%; box-sizing: border-box; }" +
        ".owo-team-high { color: var(--red, #e5534b); }" +
        ".owo-team-medium { color: var(--yellow, #d29922); }" +
        "</style>" +
        "</section>"
      );
    },

    mount: function (root, helpers) {
      var self = this;
      this.helpers = helpers || {};
      this.baseUrl =
        this.helpers.baseUrl ||
        (window.OwoPanels && window.OwoPanels.baseUrl) ||
        "http://127.0.0.1:4098";
      this.get = this.helpers.get || this._get;
      this.post = this.helpers.post || this._post;
      this.esc = this.helpers.esc || this._esc;
      this.friendlyError = this.helpers.friendlyError || this._friendlyError;

      root.innerHTML = this.nav();
      var $ = function (sel) {
        return root.querySelector(sel);
      };

      $(".owo-team-exportbtn").addEventListener("click", function () {
        self.doExport($(".owo-team-export-id").value);
      });
      $(".owo-team-reviewbtn").addEventListener("click", function () {
        self.doReview($(".owo-team-import-b64").value);
      });
      $(".owo-team-importbtn").addEventListener("click", function () {
        self.doImport($(".owo-team-import-b64").value);
      });
      $(".owo-team-versionsbtn").addEventListener("click", function () {
        self.doVersions($(".owo-team-versions-id").value);
      });

      this.refreshAudit();
    },

    refresh: function () {
      this.refreshAudit();
    },

    doExport: function (id) {
      var self = this;
      if (!id) return this._findings("请填写技能包 id");
      this.post("/team/export", { type: "flow", id: id })
        .then(function (data) {
          var el = self._root().querySelector(".owo-team-export-result");
          if (el) {
            el.textContent =
              "导出成功：" +
              (data.manifest || {}).id +
              " v" +
              (data.manifest || {}).version +
              "（" +
              data.size_bytes +
              " 字节，base64 " +
              data.package_b64.length +
              " 字符）";
          }
          self.refreshAudit();
        })
        .catch(function (error) {
          self._findings("导出失败：" + self.friendlyError(error));
        });
    },

    doReview: function (b64) {
      var self = this;
      if (!b64) return this._findings("请填写 package_b64");
      this.post("/team/review", { package_b64: b64 })
        .then(function (data) {
          self.renderFindings(data);
        })
        .catch(function (error) {
          self._findings("评审失败：" + self.friendlyError(error));
        });
    },

    doImport: function (b64) {
      var self = this;
      if (!b64) return this._findings("请填写 package_b64");
      this.post("/team/import", { package_b64: b64 })
        .then(function (data) {
          if (data.blocked) {
            self.renderFindings(data);
            return;
          }
          var pkg = data.package || {};
          self._findings(
            "导入成功：" +
              pkg.id +
              " v" +
              pkg.version +
              "\n版本历史 " +
              (data.versions || []).length +
              " 条"
          );
          self.refreshAudit();
        })
        .catch(function (error) {
          self._findings("导入失败：" + self.friendlyError(error));
        });
    },

    renderFindings: function (data) {
      var lines = [];
      lines.push((data.package || {}).id ? "包：" + data.package.id + " v" + data.package.version : "");
      lines.push("blocked：" + data.blocked);
      for (var i = 0; i < (data.findings || []).length; i++) {
        var f = data.findings[i];
        lines.push(
          (f.severity === "high" ? "⚠ " : "· ") +
            f.category +
            "：" +
            f.detail +
            "（" +
            f.severity +
            "）"
        );
      }
      if (!(data.findings || []).length) lines.push("无 findings（通过）");
      this._findings(lines.join("\n"));
    },

    doVersions: function (id) {
      var self = this;
      if (!id) return this._findings("请填写技能包 id");
      this.get("/team/versions?id=" + encodeURIComponent(id))
        .then(function (data) {
          var el = self._root().querySelector(".owo-team-versions");
          if (!el) return;
          var lines = ["共 " + data.count + " 个版本"];
          for (var i = 0; i < (data.versions || []).length; i++) {
            var v = data.versions[i];
            lines.push(
              "  " + v.version + "（" + v.imported_at + "）sha " + String(v.sha256).slice(0, 12) + "…"
            );
          }
          el.textContent = lines.join("\n");
        })
        .catch(function (error) {
          self._findings("版本查询失败：" + self.friendlyError(error));
        });
    },

    refreshAudit: function () {
      var self = this;
      this.get("/team/audit")
        .then(function (data) {
          var el = self._root().querySelector(".owo-team-audit");
          if (el) el.textContent = ((data.entries || []).join("\n")) || "（空）";
        })
        .catch(function () {
          /* 审计读取失败不阻断 */
        });
    },

    _findings: function (text) {
      var el = this._root().querySelector(".owo-team-findings");
      if (el) el.textContent = text;
    },

    _root: function () {
      return this.helpers.root || document;
    },

    // ---- helpers 缺省实现 ----

    _get: function (path) {
      return fetch(this.baseUrl + path).then(function (response) {
        if (!response.ok) {
          return response.text().then(function (body) {
            throw new Error(response.status + ": " + body);
          });
        }
        return response.status === 204 ? null : response.json();
      });
    },

    _post: function (path, body) {
      return fetch(this.baseUrl + path, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(body || {}),
      }).then(function (response) {
        if (!response.ok) {
          return response.text().then(function (text) {
            throw new Error(response.status + ": " + text);
          });
        }
        return response.json();
      });
    },

    _esc: function (text) {
      var div = document.createElement("div");
      div.textContent = text == null ? "" : String(text);
      return div.innerHTML;
    },

    _friendlyError: function (error) {
      var msg = String((error && error.message) || error || "");
      var match = msg.match(/^(\d{3}):/);
      var status = match ? Number(match[1]) : 0;
      if (status === 404 || status === 405 || status >= 500) {
        return "服务接口不可用（HTTP " + status + "）";
      }
      return msg || "未知错误";
    },
  };
})();
