import test from "node:test";
import assert from "node:assert/strict";
import { createClient } from "../src/index.js";

const BASE = process.env.OWO_TS_SDK_BASE ?? "http://127.0.0.1:4097";
const client = createClient({ baseUrl: BASE });

test("health 返回可用", async () => {
  const ok = await client.health();
  assert.equal(ok, true);
});

test("创建会话并列出（场景 4 集成）", async () => {
  const session = await client.createSession({
    workspace: process.cwd(),
    prompt: "SDK 集成测试",
  });
  assert.ok(session.id, "应返回会话 id");
  const list = await client.GET("/sessions");
  assert.ok(list.response.ok);
  const sessions = (list.data ?? []) as Array<{ id: string }>;
  assert.ok(
    sessions.some((s) => s.id === session.id),
    "新会话应出现在列表",
  );
});
