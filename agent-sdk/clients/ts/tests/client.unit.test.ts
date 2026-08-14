import test from "node:test";
import assert from "node:assert/strict";
import { createClient, type TurnEvent } from "../src/index.js";

test("runTurn 复用自定义请求头并处理无换行的最后一个 SSE 事件", async () => {
  const originalFetch = globalThis.fetch;
  let requestUrl = "";
  let requestHeaders: Headers | undefined;

  globalThis.fetch = async (input, init) => {
    requestUrl = String(input);
    requestHeaders = new Headers(init?.headers);
    return new Response('data: {"type":"final","text":"完成"}', {
      status: 200,
      headers: { "Content-Type": "text/event-stream" },
    });
  };

  try {
    const events: TurnEvent[] = [];
    const client = createClient({
      baseUrl: "http://127.0.0.1:4096/",
      headers: { Authorization: "Bearer test-token", "X-Client": "unit" },
    });

    await client.runTurn(
      { id: "session/1", prompt: "测试" },
      { onEvent: (event) => events.push(event) },
    );

    assert.equal(
      requestUrl,
      "http://127.0.0.1:4096/session/session%2F1/turn",
    );
    assert.equal(requestHeaders?.get("Authorization"), "Bearer test-token");
    assert.equal(requestHeaders?.get("X-Client"), "unit");
    assert.equal(requestHeaders?.get("Content-Type"), "application/json");
    assert.deepEqual(events, [{ type: "final", text: "完成" }]);
  } finally {
    globalThis.fetch = originalFetch;
  }
});

test("runTurn 在开始前已取消时透传 AbortSignal", async () => {
  const originalFetch = globalThis.fetch;
  let signalAborted = false;
  const requestedUrls: string[] = [];

  globalThis.fetch = async (input, init) => {
    requestedUrls.push(String(input));
    signalAborted = init?.signal?.aborted ?? false;
    return new Response("", { status: 499 });
  };

  try {
    const controller = new AbortController();
    controller.abort();
    const client = createClient({ baseUrl: "http://127.0.0.1:4096" });

    await assert.rejects(
      client.runTurn(
        { id: "session", prompt: "取消" },
        { onEvent: () => undefined, signal: controller.signal },
      ),
      /HTTP 499/,
    );
    assert.equal(signalAborted, true);
    assert.ok(requestedUrls.some((url) => url.endsWith("/session/session/abort")));
  } finally {
    globalThis.fetch = originalFetch;
  }
});

test("runTurn 在 SSE 没有 final 事件时失败", async () => {
  const originalFetch = globalThis.fetch;
  globalThis.fetch = async () =>
    new Response('data: {"type":"progress","message":"处理中"}', {
      status: 200,
      headers: { "Content-Type": "text/event-stream" },
    });

  try {
    const client = createClient({ baseUrl: "http://127.0.0.1:4096" });
    await assert.rejects(
      client.runTurn(
        { id: "session", prompt: "截断" },
        { onEvent: () => undefined },
      ),
      /未收到 final 事件/,
    );
  } finally {
    globalThis.fetch = originalFetch;
  }
});
