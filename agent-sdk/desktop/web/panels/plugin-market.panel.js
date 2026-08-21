// 插件市场面板（Lane B）：目录/安装/更新/卸载/校验/扫描/seed/审计。
// 纯脚本 IIFE，无 ES module；经 OwoPanels 注册，主应用注入 helpers。
(function () {
  "use strict";

  window.OwoPanels = window.OwoPanels || {};

  window.OwoPanels["plugin-market"] = {
    id: "plugin-market",
    title: "插件市场",

    nav: function () {
      return (
        '<section data-panel="plugin-market" class="owo-market-panel">' +
        '<div class="owo-market-tools">' +
        '<h3>目录</h3>' +
        '<div class="inline" style="margin-bottom:6px">' +
        '<button class="owo-market-refresh">刷新</button>' +
        '<span class="owo-market-env sub"></span>' +
        "</div>" +
        '<ul class="owo-market-list list"></ul>' +
        "</div>" +
        '<div class="owo-market-tools">' +
        "<h3>操作</h3>" +
        '<div class="stack">' +
        '<div class="inline"><input class="owo-market-dir" placeholder="插件目录路径（相对 workspace 或绝对）"><button class="owo-market-scan">扫描</button><button class="owo-market-verify">校验</button></div>' +
        '<div class="inline"><input class="owo-market-dir2" placeholder="插件目录（安装/更新源）"><input class="owo-market-id" placeholder="更新目标 id（update 时）"><button class="owo-market-install">安装</button><button class="owo-market-update">更新</button></div>' +
        '<div class="inline"><input class="owo-market-uid" placeholder="卸载 id"><button class="owo-market-uninstall">卸载</button></div>' +
        '<div class="owo-market-result sub"></div>' +
        "</div>" +
        "</div>" +
        '<div class="owo-market-tools">' +
        "<h3>远端市场</h3>" +
        '<div class="stack">' +
        '<div class="inline"><input class="owo-market-url" placeholder="市场 URL（OWO_MARKET_URL 缺省）"><button class="owo-market-refreshremote">拉取 registry</button></div>' +
        '<div class="inline"><input class="owo-market-rid" placeholder="远端插件 id"><input class="owo-market-rver" placeholder="版本（可选）"><button class="owo-market-installremote primary">下载并安装</button></div>' +
        "</div>" +
        "</div>" +
        '<div class="owo-market-tools">' +
        "<h3>Seed 示例市场条目</h3>" +
        '<div class="stack">' +
        '<textarea class="owo-market-seed" rows="4" spellcheck="false" placeholder=\'{"entries":[{"id":"owo.plugin.demo","name":"Demo","version":"1.0.0","min_app_version":"0.5.0"}]}\'></textarea>' +
        '<button class="owo-market-seedbtn">写入 seed</button>' +
        "</div>" +
        "</div>" +
        '<div class="owo-market-tools">' +
        "<h3>审计尾部</h3>" +
        '<pre class="owo-market-audit sub">—</pre>' +
        "</div>" +
        "<style>" +
        ".owo-market-panel { display: flex; flex-direction: column; gap: 10px; }" +
        ".owo-market-tools { border: 1px solid var(--border, #333); border-radius: 6px; padding: 8px; }" +
        ".owo-market-tools h3 { margin: 0 0 6px; font-size: 13px; }" +
        ".owo-market-list li { font-size: 12px; }" +
        ".owo-market-result { white-space: pre-wrap; max-height: 200px; overflow: auto; }" +
        ".owo-market-audit { white-space: pre-wrap; max-height: 180px; overflow: auto; }" +
        ".owo-market-panel input { flex: 1; min-width: 0; }" +
        ".owo-market-risk { color: var(--red, #e5534b); }" +
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

      $(".owo-market-refresh").addEventListener("click", function () {
        self.refresh();
      });
      $(".owo-market-scan").addEventListener("click", function () {
        self.doScan($(".owo-market-dir").value);
      });
      $(".owo-market-verify").addEventListener("click", function () {
        self.doVerify($(".owo-market-dir").value);
      });
      $(".owo-market-install").addEventListener("click", function () {
        self.doInstall($(".owo-market-dir2").value);
      });
      $(".owo-market-update").addEventListener("click", function () {
        self.doUpdate($(".owo-market-id").value, $(".owo-market-dir2").value);
      });
      $(".owo-market-uninstall").addEventListener("click", function () {
        self.doUninstall($(".owo-market-uid").value);
      });
      $(".owo-market-seedbtn").addEventListener("click", function () {
        self.doSeed($(".owo-market-seed").value);
      });
      $(".owo-market-refreshremote").addEventListener("click", function () {
        self.doRefreshRemote($(".owo-market-url").value);
      });
      $(".owo-market-installremote").addEventListener("click", function () {
        self.doInstallRemote(
          $(".owo-market-rid").value,
          $(".owo-market-rver").value,
          $(".owo-market-url").value
        );
      });

      this.refresh();
      this.refreshAudit();
    },

    refresh: function () {
      var self = this;
      this.get("/plugins/market")
        .then(function (data) {
          self.renderCatalog(data);
        })
        .catch(function (error) {
          var list = self._root().querySelector(".owo-market-list");
          if (list) {
            list.innerHTML =
              '<li class="sub">' +
              self.esc(self.friendlyError(error)) +
              "</li>";
          }
        });
    },

    renderCatalog: function (data) {
      var list = this._root().querySelector(".owo-market-list");
      if (!list) return;
      var envEl = this._root().querySelector(".owo-market-env");
      if (envEl) {
        envEl.textContent =
          "App " + data.app_version + " ｜ 签名" + (data.require_signature ? "开启" : "关闭");
      }
      list.innerHTML = "";
      var plugins = data.plugins || [];
      for (var i = 0; i < plugins.length; i++) {
        var plugin = plugins[i];
        var li = document.createElement("li");
        var risks = plugin.risks && plugin.risks.length ? plugin.risks.join("；") : "";
        var riskBadge = risks
          ? '<span class="owo-market-risk">⚠ ' + this.esc(risks) + "</span>"
          : "";
        var updateBadge = plugin.has_update
          ? '<span class="sub">⬆ 可更新</span>'
          : "";
        li.innerHTML =
          "<strong>" +
          this.esc(plugin.name || plugin.id) +
          "</strong>" +
          '<span class="sub">' +
          this.esc(plugin.id) +
          " v" +
          this.esc(plugin.version) +
          " ｜ " +
          this.esc(plugin.source) +
          updateBadge +
          "</span>" +
          '<span class="sub">' +
          this.esc(plugin.description || "") +
          riskBadge +
          "</span>";
        list.appendChild(li);
      }
      if (!plugins.length) {
        list.innerHTML = '<li class="sub">暂无插件（本地无已发现插件，market.json 为空）</li>';
      }
    },

    doScan: function (dir) {
      var self = this;
      if (!dir) return this._result("请填写插件目录");
      this.get("/plugins/market/scan?dir=" + encodeURIComponent(dir))
        .then(function (data) {
          self._result(
            "扫描：" + data.dir + "\n通过：" + data.pass + "\n风险：" + JSON.stringify(data.risks)
          );
        })
        .catch(function (error) {
          self._result("扫描失败：" + self.friendlyError(error));
        });
    },

    doVerify: function (dir) {
      var self = this;
      if (!dir) return this._result("请填写插件目录");
      this.post("/plugins/market/verify", { dir: dir })
        .then(function (data) {
          var report = data.report || {};
          self._result(
            "校验通过：" + report.id + " v" + report.version + "\n" + (report.audit || []).join("\n")
          );
        })
        .catch(function (error) {
          self._result("校验失败：" + self.friendlyError(error));
        });
    },

    doInstall: function (dir) {
      var self = this;
      if (!dir) return this._result("请填写插件目录");
      this.post("/plugins/market/install", { dir: dir })
        .then(function (data) {
          var report = data.report || {};
          self._result(
            "安装完成：" + report.id + " v" + report.version + " 状态 " + report.state
          );
          self.refresh();
          self.refreshAudit();
        })
        .catch(function (error) {
          self._result("安装失败：" + self.friendlyError(error));
        });
    },

    doUpdate: function (id, dir) {
      var self = this;
      if (!dir) return this._result("请填写插件目录");
      this.post("/plugins/market/update", { id: id, dir: dir })
        .then(function (data) {
          var report = data.report || {};
          self._result("更新完成：" + report.id + " → v" + report.version);
          self.refresh();
          self.refreshAudit();
        })
        .catch(function (error) {
          self._result("更新失败：" + self.friendlyError(error));
        });
    },

    doUninstall: function (id) {
      var self = this;
      if (!id) return this._result("请填写卸载 id");
      this.post("/plugins/market/uninstall", { id: id })
        .then(function (data) {
          self._result(
            "已卸载：" + id + "，移除 " + ((data.removed || []).length) + " 个文件"
          );
          self.refresh();
          self.refreshAudit();
        })
        .catch(function (error) {
          self._result("卸载失败：" + self.friendlyError(error));
        });
    },

    doSeed: function (raw) {
      var self = this;
      var body;
      try {
        body = JSON.parse(raw || "{}");
      } catch (error) {
        return this._result("seed JSON 解析失败：" + error.message);
      }
      this.post("/plugins/market/seed", body)
        .then(function (data) {
          self._result("已写入 " + data.entries + " 条市场条目");
          self.refresh();
          self.refreshAudit();
        })
        .catch(function (error) {
          self._result("seed 失败：" + self.friendlyError(error));
        });
    },

    doRefreshRemote: function (url) {
      var self = this;
      var body = url ? { url: url } : {};
      this.post("/plugins/market/refresh", body)
        .then(function (data) {
          self._result(
            "registry 已刷新：" + (data.entries || 0) + " 条（来源 " + (data.source || "?") + "）"
          );
          self.refresh();
        })
        .catch(function (error) {
          self._result("刷新失败：" + self.friendlyError(error));
        });
    },

    doInstallRemote: function (id, version, url) {
      var self = this;
      if (!id) return this._result("请填写远端插件 id");
      var body = { id: id };
      if (version) body.version = version;
      if (url) body.url = url;
      this.post("/plugins/market/install-remote", body)
        .then(function (data) {
          var report = data.report || {};
          self._result(
            "远端安装完成：" + (report.id || id) + " v" + (report.version || "?") + " 状态 " + (report.state || "?")
          );
          self.refresh();
          self.refreshAudit();
        })
        .catch(function (error) {
          self._result("远端安装失败：" + self.friendlyError(error));
        });
    },

    refreshAudit: function () {
      var self = this;
      this.get("/plugins/market/audit?n=20")
        .then(function (data) {
          var el = self._root().querySelector(".owo-market-audit");
          if (el) {
            el.textContent = ((data.entries || []).join("\n")) || "（空）";
          }
        })
        .catch(function () {
          /* 审计读取失败不阻断面板 */
        });
    },

    _result: function (text) {
      var el = this._root().querySelector(".owo-market-result");
      if (el) el.textContent = text;
    },

    _root: function () {
      return this.helpers.root || document;
    },

    // ---- helpers 缺省实现（防御性降级） ----

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
