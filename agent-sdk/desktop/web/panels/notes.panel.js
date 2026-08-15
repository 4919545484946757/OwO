/* notes 面板（Lane A）：文档列表 + 新建（标题+markdown）+ 块树渲染 + 搜索 + 导出 + 内联编辑。
 * 纯脚本 IIFE 注册；helpers 缺省时自行 fetch；样式 owo-notes- 前缀，mount 时注入 <style>。 */
(function () {
  window.OwoPanels = window.OwoPanels || {};
  window.OwoPanels.notes = {
    id: "notes",
    title: "笔记",

    nav: function () {
      return (
        '<section data-panel="notes" class="owo-notes-root">' +
        '<div class="owo-notes-bar">' +
        '<input class="owo-notes-search" type="text" placeholder="全文搜索…">' +
        '<button class="owo-notes-btn owo-notes-btn-new">＋ 新建</button>' +
        "</div>" +
        '<ul class="owo-notes-list"></ul>' +
        '<div class="owo-notes-editor" hidden>' +
        '<input class="owo-notes-title" type="text" placeholder="标题">' +
        '<textarea class="owo-notes-md" rows="8" placeholder="Markdown 正文（新建时生效）"></textarea>' +
        '<div class="owo-notes-editor-actions">' +
        '<button class="owo-notes-btn owo-notes-btn-save">保存</button>' +
        '<button class="owo-notes-btn owo-notes-btn-cancel">取消</button>' +
        "</div>" +
        "</div>" +
        '<div class="owo-notes-detail" hidden>' +
        '<h3 class="owo-notes-detail-title"></h3>' +
        '<div class="owo-notes-detail-actions">' +
        '<button class="owo-notes-btn owo-notes-btn-export-md">导出 MD</button>' +
        '<button class="owo-notes-btn owo-notes-btn-export-html">导出 HTML</button>' +
        '<button class="owo-notes-btn owo-notes-btn-del">删除</button>' +
        "</div>" +
        '<pre class="owo-notes-tree"></pre>' +
        "</div>" +
        "</section>"
      );
    },

    mount: function (root, helpers) {
      var self = this;
      root.innerHTML = this.nav();
      this.helpers = helpers || {};
      this.baseUrl = this.helpers.baseUrl || window.OwoPanels.baseUrl || "http://127.0.0.1:4098";
      this.get = this.helpers.get || function (path) {
        return fetch(self.baseUrl + path).then(function (r) { return r.json(); });
      };
      this.post = this.helpers.post || function (path, body) {
        return fetch(self.baseUrl + path, {
          method: "POST",
          headers: { "content-type": "application/json" },
          body: JSON.stringify(body || {}),
        }).then(function (r) { return r.json(); });
      };
      this.esc = this.helpers.esc || function (s) {
        return String(s == null ? "" : s).replace(/[&<>"']/g, function (c) {
          return { "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c];
        });
      };
      this.friendlyError = this.helpers.friendlyError || function (e) { return String(e); };
      var style = document.createElement("style");
      style.textContent =
        ".owo-notes-root{display:flex;flex-direction:column;gap:8px}" +
        ".owo-notes-bar{display:flex;gap:8px}" +
        ".owo-notes-search{flex:1;padding:6px}" +
        ".owo-notes-btn{padding:6px 10px;cursor:pointer}" +
        ".owo-notes-list{margin:0;padding:0;list-style:none}" +
        ".owo-notes-list li{padding:6px 4px;border-bottom:1px solid #eee;cursor:pointer;display:flex;justify-content:space-between}" +
        ".owo-notes-list li:hover{background:#f5f5f5}" +
        ".owo-notes-editor{display:flex;flex-direction:column;gap:8px;border:1px solid #ddd;padding:10px}" +
        ".owo-notes-title,.owo-notes-md{padding:6px}" +
        ".owo-notes-detail{border:1px solid #ddd;padding:10px}" +
        ".owo-notes-tree{background:#fafafa;padding:8px;max-height:50vh;overflow:auto;font-size:12px;white-space:pre-wrap}" +
        ".owo-notes-hint{color:#888;font-size:12px}";
      root.appendChild(style);
      this.refresh();
      this.bind();
    },

    refresh: function () {
      var self = this;
      return this.get("/notes")
        .then(function (data) {
          var list = data.notes || [];
          var ul = self.rootEl().querySelector(".owo-notes-list");
          if (!list.length) {
            ul.innerHTML = '<li class="owo-notes-hint">（暂无笔记，点"新建"创建）</li>';
            return;
          }
          ul.innerHTML = list
            .map(function (n) {
              return (
                '<li data-id="' + self.esc(n.id) + '">' +
                '<span class="owo-notes-name">' + self.esc(n.title || n.id) + "</span>" +
                '<span class="owo-notes-hint">' + self.esc((n.updated_at || "").slice(0, 19)) + "</span>" +
                "</li>"
              );
            })
            .join("");
        })
        .catch(function (e) { self.alert("列表加载失败：" + self.friendlyError(e)); });
    },

    bind: function () {
      var self = this;
      var root = this.rootEl();
      var on = function (sel, ev, fn) {
        var el = root.querySelector(sel);
        if (el) el.addEventListener(ev, fn);
      };
      on(".owo-notes-btn-new", "click", function () {
        var editor = root.querySelector(".owo-notes-editor");
        editor.hidden = false;
        editor.querySelector(".owo-notes-title").value = "";
        editor.querySelector(".owo-notes-md").value = "";
        root.querySelector(".owo-notes-detail").hidden = true;
      });
      on(".owo-notes-btn-save", "click", function () {
        var title = root.querySelector(".owo-notes-title").value.trim();
        var md = root.querySelector(".owo-notes-md").value;
        if (!title) { self.alert("标题不能为空"); return; }
        var detail = root.querySelector(".owo-notes-detail");
        var editingId = detail.dataset.id;
        if (editingId) {
          self.post("/notes/" + editingId + "/reindex", {}).catch(function () {});
          self
            .get("/notes/" + editingId + "/export/md")
            .then(function (r) {
              return self.post("/notes/import", { title: title, markdown: r.content || "" });
            })
            .then(function () {
              root.querySelector(".owo-notes-editor").hidden = true;
              self.refresh();
            })
            .catch(function (e) { self.alert("保存失败：" + self.friendlyError(e)); });
        } else {
          self
            .post("/notes", { title: title, markdown: md })
            .then(function () {
              root.querySelector(".owo-notes-editor").hidden = true;
              self.refresh();
            })
            .catch(function (e) { self.alert("创建失败：" + self.friendlyError(e)); });
        }
      });
      on(".owo-notes-btn-cancel", "click", function () {
        root.querySelector(".owo-notes-editor").hidden = true;
      });
      on(".owo-notes-search", "keydown", function (e) {
        if (e.key !== "Enter") return;
        var q = root.querySelector(".owo-notes-search").value.trim();
        if (!q) { self.refresh(); return; }
        self
          .get("/notes/search?q=" + encodeURIComponent(q))
          .then(function (data) {
            var ul = root.querySelector(".owo-notes-list");
            var hits = data.hits || [];
            if (!hits.length) {
              ul.innerHTML = '<li class="owo-notes-hint">无命中</li>';
              return;
            }
            ul.innerHTML = hits
              .map(function (h) {
                return '<li class="owo-notes-hint" data-id="' + self.esc(h.doc_id) + '">' +
                  self.esc(h.snippet) + "（" + self.esc(h.doc_id.slice(0, 8)) + "）</li>";
              })
              .join("");
          })
          .catch(function (err) { self.alert("搜索失败：" + self.friendlyError(err)); });
      });
      on(".owo-notes-list", "click", function (e) {
        var li = e.target.closest("li[data-id]");
        if (!li) return;
        var id = li.dataset.id;
        self
          .get("/notes/" + id)
          .then(function (doc) { self.renderDetail(doc); })
          .catch(function (err) { self.alert("读取失败：" + self.friendlyError(err)); });
      });
      on(".owo-notes-btn-export-md", "click", function () {
        var id = root.querySelector(".owo-notes-detail").dataset.id;
        self.get("/notes/" + id + "/export/md").then(function (r) { self.download(id + ".md", r.content); });
      });
      on(".owo-notes-btn-export-html", "click", function () {
        var id = root.querySelector(".owo-notes-detail").dataset.id;
        self.get("/notes/" + id + "/export/html").then(function (r) { self.download(id + ".html", r.content); });
      });
      on(".owo-notes-btn-del", "click", function () {
        var detail = root.querySelector(".owo-notes-detail");
        var id = detail.dataset.id;
        if (!window.confirm("确认删除这篇笔记？")) return;
        self.post("/notes/" + id + "/reindex", {}).catch(function () {});
        fetch(self.baseUrl + "/notes/" + id, { method: "DELETE" })
          .then(function () {
            detail.hidden = true;
            self.refresh();
          })
          .catch(function (err) { self.alert("删除失败：" + self.friendlyError(err)); });
      });
      on(".owo-notes-detail-title", "dblclick", function () {
        var detail = root.querySelector(".owo-notes-detail");
        var title = prompt("新标题：", detail.querySelector(".owo-notes-detail-title").textContent);
        if (!title) return;
        fetch(self.baseUrl + "/notes/" + detail.dataset.id, {
          method: "PUT",
          headers: { "content-type": "application/json" },
          body: JSON.stringify({ title: title }),
        })
          .then(function () { self.refresh(); self.get("/notes/" + detail.dataset.id).then(function (d) { self.renderDetail(d); }); })
          .catch(function (err) { self.alert("改标题失败：" + self.friendlyError(err)); });
      });
    },

    renderDetail: function (doc) {
      var self = this;
      var detail = this.rootEl().querySelector(".owo-notes-detail");
      detail.hidden = false;
      detail.dataset.id = doc.id;
      detail.querySelector(".owo-notes-detail-title").textContent = doc.title || doc.id;
      var lines = ["id: " + doc.id, "title: " + doc.title, "root: " + doc.root, "updated_at: " + doc.updated_at, "blocks:"];
      Object.keys(doc.blocks || {}).forEach(function (bid) {
        var b = doc.blocks[bid];
        lines.push("  " + bid + " " + JSON.stringify(b.kind) + " children=" + JSON.stringify(b.children || []));
      });
      detail.querySelector(".owo-notes-tree").textContent = lines.join("\n");
    },

    download: function (name, content) {
      var blob = new Blob([content], { type: "text/plain;charset=utf-8" });
      var a = document.createElement("a");
      a.href = URL.createObjectURL(blob);
      a.download = name;
      a.click();
      URL.revokeObjectURL(a.href);
    },

    alert: function (msg) {
      if (window.OwoToast) { window.OwoToast(msg); return; }
      window.alert(msg);
    },

    rootEl: function () {
      return document.querySelector('[data-panel="notes"]');
    },
  };
})();
