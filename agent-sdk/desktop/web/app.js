// OwO Agent 工作台（v0.4 P1 桌面壳，纯静态，直连本地 HTTP API + SSE）
"use strict";

const state = {
  sessionId: null,
  pendingApproval: null,
  reading: false,
};

const $ = (id) => document.getElementById(id);
// 由 Tauri 壳注入核心服务地址；经核心服务同源托管时为空字符串。
const API_BASE = window.OWO_API_BASE || "";

async function api(path, options = {}) {
  const response = await fetch(API_BASE + path, {
    headers: { "Content-Type": "application/json" },
    ...options,
  });
  if (!response.ok) {
    const body = await response.text();
    throw new Error(`${response.status}: ${body}`);
  }
  return response.status === 204 ? null : response.json();
}

function addMessage(kind, text, meta = "") {
  const div = document.createElement("div");
  div.className = `msg ${kind}`;
  if (meta) {
    const span = document.createElement("span");
    span.className = "meta";
    span.textContent = meta;
    div.appendChild(span);
  }
  div.appendChild(document.createTextNode(text));
  $("messages").appendChild(div);
  $("messages").scrollTop = $("messages").scrollHeight;
  return div;
}

function esc(text) {
  const div = document.createElement("div");
  div.textContent = text;
  return div.innerHTML;
}

// ---------- 头部状态 ----------

async function refreshHealth() {
  try {
    const health = await api("/health");
    $("health").textContent = `API 就绪 ${health.version}`;
  } catch (error) {
    $("health").textContent = `连接失败：${error.message}`;
  }
}

async function refreshPerception() {
  try {
    const snapshot = await api("/context/snapshot");
    $("permission").textContent = `感知：${snapshot.permission_level || "l0_l1"}`;
    $("snapshot").textContent = JSON.stringify(snapshot, null, 2);
  } catch (_) {
    $("snapshot").textContent = "（无法获取情景快照）";
  }
}

async function refreshLearn() {
  try {
    const status = await api("/learn/status");
    $("learnState").textContent = `学习：${status.state}（${status.samples}）`;
  } catch (_) {
    $("learnState").textContent = "学习：—";
  }
}

async function learnControl(action) {
  try {
    await api(`/learn/${action}`, { method: "POST" });
    await refreshLearn();
  } catch (error) {
    addMessage("error", `学习控制失败：${error.message}`);
  }
}

async function sinkSkill() {
  const name = $("sinkName").value.trim();
  const apps = $("sinkApps").value.split(",").map((item) => item.trim()).filter(Boolean);
  const sensitivity = $("sinkSensitivity").value;
  const description = $("sinkDesc").value.trim();
  if (!name || !apps.length) return;
  try {
    const result = await api("/learn/sink", {
      method: "POST",
      body: JSON.stringify({ name, target_apps: apps, sensitivity, description }),
    });
    $("sinkName").value = "";
    $("sinkDesc").value = "";
    addMessage("system", `已沉淀技能包 ${result.name}（变量：${result.variables.join(", ") || "无"}）`);
    await refreshPackages();
  } catch (error) {
    addMessage("error", `沉淀失败：${error.message}`);
  }
}

async function refreshPackages() {
  try {
    const packages = await api("/learn/packages");
    const list = $("packageList");
    list.innerHTML = "";
    for (const package of packages) {
      const li = document.createElement("li");
      li.innerHTML = `<strong>${esc(package.name)}</strong><span class="sub">目标：${esc(package.target_apps.join(","))} ｜ 变量：${esc(package.variables.join(",")) || "无"}</span>`;
      li.addEventListener("click", () => executePackage(package));
      list.appendChild(li);
    }
    if (!packages.length) list.innerHTML = '<li class="sub">暂无流程技能包（先录制再沉淀）</li>';
  } catch (_) {
    $("packageList").innerHTML = '<li class="sub">加载失败</li>';
  }
}

async function executePackage(package) {
  let variables = {};
  if (package.variables && package.variables.length) {
    const raw = prompt(`为技能包填写变量（JSON，如 {"value":"小李"}）：`, "{}");
    if (raw === null) return;
    try {
      variables = JSON.parse(raw);
    } catch (_) {
      addMessage("error", "变量 JSON 解析失败");
      return;
    }
  }
  if (!confirm(`确认执行技能包 ${package.name}？首次执行需要审批。`)) return;
  try {
    const report = await api("/learn/execute-package", {
      method: "POST",
      body: JSON.stringify({ name: package.name, variables, confirm: true }),
    });
    if (report.ok) {
      addMessage("system", `技能包 ${package.name} 执行成功（${report.steps.length} 步）`);
    } else {
      addMessage("error", `技能包 ${package.name} 执行失败：${report.error || ""}`);
    }
    for (const step of report.steps) {
      addMessage("tool", `${step.status.toUpperCase()} ${step.node_id}（${step.action}）：${step.detail || "ok"}`, "执行步骤");
    }
  } catch (error) {
    addMessage("error", `执行失败：${error.message}`);
  }
}

async function refreshSuggestions() {
  try {
    const suggestions = await api("/proactive/suggestions");
    const list = $("suggestionList");
    list.innerHTML = "";
    for (const suggestion of suggestions) {
      const li = document.createElement("li");
      li.innerHTML = `<strong>${esc(suggestion.app_id)}</strong><span class="sub">${esc(suggestion.summary)}</span><span class="sub">${esc(suggestion.sequence.join(" → "))}</span>`;
      const actions = ["learn", "execute", "ignore", "mute"];
      const labels = { learn: "学习", execute: "执行一次", ignore: "忽略", mute: "静默" };
      for (const action of actions) {
        const button = document.createElement("button");
        button.textContent = labels[action];
        button.addEventListener("click", async () => {
          try {
            await api("/proactive/decide", {
              method: "POST",
              body: JSON.stringify({ suggestion_id: suggestion.id, action }),
            });
            await refreshSuggestions();
          } catch (error) {
            addMessage("error", `建议处理失败：${error.message}`);
          }
        });
        li.appendChild(button);
      }
      list.appendChild(li);
    }
    if (!suggestions.length) list.innerHTML = '<li class="sub">暂无建议</li>';
  } catch (_) {
    $("suggestionList").innerHTML = '<li class="sub">加载失败</li>';
  }
}

// ---------- 会话 / 任务 ----------

async function refreshSessions(selectId) {
  const sessions = await api("/sessions");
  const list = $("sessionList");
  list.innerHTML = "";
  for (const session of sessions) {
    const li = document.createElement("li");
    if (session.id === selectId) {
      li.className = "active";
      state.sessionId = session.id;
    }
    li.innerHTML = `<strong>${esc(session.id.slice(0, 12))}</strong><span class="sub">${esc(session.workspace)} ｜ ${esc(session.model)} ｜ ${esc(session.created_at)}</span>`;
    li.addEventListener("click", () => selectSession(session.id));
    list.appendChild(li);
  }
}

async function selectSession(id) {
  state.sessionId = id;
  $("messages").innerHTML = "";
  await refreshSessions(id);
  await refreshDiff(id);
}

async function newSession() {
  const workspace = $("workspace").value.trim();
  if (!workspace) {
    alert("请先填写工作区绝对路径");
    return;
  }
  const session = await api("/session", {
    method: "POST",
    body: JSON.stringify({ workspace }),
  });
  await selectSession(session.id);
  addMessage("system", `已创建会话 ${session.id}`);
}

// ---------- 对话（SSE） ----------

function parseSseBlock(block) {
  let event = "message";
  let data = "";
  for (const line of block.split("\n")) {
    if (line.startsWith("event:")) event = line.slice(6).trim();
    else if (line.startsWith("data:")) data += line.slice(5).trim();
  }
  return { event, data };
}

async function sendPrompt() {
  if (state.reading || !state.sessionId) {
    if (!state.sessionId) addMessage("system", "请先新建或选择一个会话");
    return;
  }
  const prompt = $("prompt").value.trim();
  if (!prompt) return;
  $("prompt").value = "";
  addMessage("user", prompt);
  const streaming = addMessage("assistant", "");

  state.reading = true;
  try {
    const response = await fetch(`${API_BASE}/session/${state.sessionId}/turn`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ prompt }),
    });
    if (!response.ok || !response.body) {
      throw new Error(await response.text());
    }
    const reader = response.body.getReader();
    const decoder = new TextDecoder();
    let buffer = "";
    let assistantText = "";
    let finished = false;

    while (true) {
      const { done, value } = await reader.read();
      if (done) break;
      buffer += decoder.decode(value, { stream: true });
      const blocks = buffer.split("\n\n");
      buffer = blocks.pop() || "";
      for (const block of blocks) {
        const { event, data } = parseSseBlock(block);
        if (!data) continue;
        let payload;
        try { payload = JSON.parse(data); } catch (_) { continue; }
        switch (event) {
          case "token_delta":
            assistantText += payload.delta || "";
            streaming.textContent = assistantText;
            $("messages").scrollTop = $("messages").scrollHeight;
            break;
          case "progress":
            streaming.textContent = (streaming.textContent || "") + `\n[${payload.message}]`;
            break;
          case "tool_use":
            addMessage("tool", `▶ ${payload.tool}`, "工具调用");
            break;
          case "tool_result":
            addMessage("tool", payload.ok ? `✔ ${payload.tool}` : `✘ ${payload.tool}：${payload.error || ""}`, "工具结果");
            break;
          case "permission_request":
            showApproval(payload);
            break;
          case "final":
            assistantText = payload.text || assistantText;
            streaming.textContent = assistantText;
            finished = true;
            break;
          case "compaction":
            addMessage("system", `上下文已压缩：${payload.summary}`);
            break;
        }
      }
    }
    if (!finished && !assistantText) streaming.remove();
    hideApproval();
    await refreshSessions(state.sessionId);
    await refreshDiff(state.sessionId);
  } catch (error) {
    addMessage("error", `回合失败：${error.message}`);
  } finally {
    state.reading = false;
  }
}

// ---------- 审批条 ----------

function showApproval(payload) {
  state.pendingApproval = payload.request_id;
  $("approvalText").textContent = `需要审批：${payload.tool}（${payload.reason || ""}）`;
  $("approvalBar").classList.remove("hidden");
}

function hideApproval() {
  state.pendingApproval = null;
  $("approvalBar").classList.add("hidden");
}

async function respondApproval(allow) {
  if (!state.pendingApproval) return;
  const requestId = state.pendingApproval;
  hideApproval();
  try {
    await api(`/session/${state.sessionId}/permission/${requestId}`, {
      method: "POST",
      body: JSON.stringify({ allow }),
    });
    addMessage("system", allow ? "已允许该操作" : "已拒绝该操作");
  } catch (error) {
    addMessage("error", `审批失败：${error.message}`);
  }
}

// ---------- diff 审阅 ----------

async function refreshDiff(sessionId) {
  const list = $("diffList");
  list.innerHTML = "";
  if (!sessionId) return;
  try {
    const diffs = await api(`/session/${sessionId}/diff`);
    for (const diff of diffs) {
      const li = document.createElement("li");
      li.className = "diff-item";
      const changed = diff.before != null && diff.after != null ? "修改" : diff.after != null ? "新增" : "删除";
      li.innerHTML = `<strong>${esc(diff.path)}</strong><span class="sub">${changed}</span>`;
      list.appendChild(li);
    }
    if (!diffs.length) list.innerHTML = '<li class="sub">暂无改动</li>';
  } catch (_) {
    list.innerHTML = '<li class="sub">无会话或读取失败</li>';
  }
}

async function revertAll() {
  if (!state.sessionId) return;
  if (!confirm("确定回滚当前会话全部写操作？")) return;
  await api(`/session/${state.sessionId}/revert`, { method: "POST" });
  await refreshDiff(state.sessionId);
  addMessage("system", "已回滚全部改动");
}

// ---------- 技能中心 ----------

async function refreshSkills() {
  try {
    const skills = await api("/skills");
    const list = $("skillList");
    list.innerHTML = "";
    for (const skill of skills) {
      const li = document.createElement("li");
      li.innerHTML = `<strong>${esc(skill.name)}</strong><span class="sub">${esc(skill.description || "")}</span>`;
      list.appendChild(li);
    }
    if (!skills.length) list.innerHTML = '<li class="sub">暂无技能</li>';
  } catch (_) {
    $("skillList").innerHTML = '<li class="sub">加载失败</li>';
  }
}

// ---------- 白名单 ----------

async function refreshWhitelist() {
  try {
    const entries = await api("/whitelist");
    const list = $("whitelistList");
    list.innerHTML = "";
    for (const entry of entries) {
      const li = document.createElement("li");
      li.innerHTML = `<strong>${esc(entry.name)}</strong><span class="sub">${esc(entry.app_id)} ｜ ${esc(entry.tier)} ｜ 操作:${entry.auto_ops_allowed ? "开" : "关"}</span>`;
      li.addEventListener("dblclick", async () => {
        await api("/whitelist/manage", {
          method: "POST",
          body: JSON.stringify({ action: "remove", app_id: entry.app_id }),
        });
        refreshWhitelist();
      });
      list.appendChild(li);
    }
  } catch (_) {
    $("whitelistList").innerHTML = '<li class="sub">加载失败</li>';
  }
}

// ---------- 事件绑定 ----------

$("newSession").addEventListener("click", newSession);
$("chatForm").addEventListener("submit", (event) => {
  event.preventDefault();
  sendPrompt();
});
$("approveBtn").addEventListener("click", () => respondApproval(true));
$("denyBtn").addEventListener("click", () => respondApproval(false));
$("revertBtn").addEventListener("click", revertAll);
$("learnStart").addEventListener("click", () => learnControl("start"));
$("learnPause").addEventListener("click", () => learnControl("pause"));
$("learnResume").addEventListener("click", () => learnControl("resume"));
$("learnStop").addEventListener("click", () => learnControl("stop"));
$("learnClear").addEventListener("click", () => learnControl("clear"));
$("sinkForm").addEventListener("submit", (event) => {
  event.preventDefault();
  sinkSkill();
});
$("whitelistForm").addEventListener("submit", async (event) => {
  event.preventDefault();
  const appId = $("wlAppId").value.trim();
  if (!appId) return;
  await api("/whitelist/manage", {
    method: "POST",
    body: JSON.stringify({
      action: "upsert",
      entry: {
        app_id: appId,
        name: appId,
        tier: "productivity",
        learn_allowed: true,
        auto_ops_allowed: true,
        chat_authorized: false,
        sensitive: false,
      },
    }),
  });
  $("wlAppId").value = "";
  refreshWhitelist();
});
$("prompt").addEventListener("keydown", (event) => {
  if (event.key === "Enter" && !event.shiftKey) {
    event.preventDefault();
    sendPrompt();
  }
});

// ---------- 启动 ----------

async function boot() {
  await refreshHealth();
  await Promise.all([
    refreshSessions(),
    refreshSkills(),
    refreshPackages(),
    refreshSuggestions(),
    refreshWhitelist(),
    refreshPerception(),
    refreshLearn(),
  ]);
  setInterval(refreshPerception, 3000);
  setInterval(refreshLearn, 5000);
  setInterval(refreshPackages, 10000);
  setInterval(refreshSuggestions, 10000);
  setInterval(refreshHealth, 15000);
}

boot();
