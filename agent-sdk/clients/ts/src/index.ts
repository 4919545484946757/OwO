/**
 * OwO Agent SDK TypeScript 客户端（场景 4：第三方应用集成）。
 *
 * 用法：
 * ```ts
 * import { createClient } from "@owo-agent/client";
 * const api = createClient({ baseUrl: "http://127.0.0.1:4096" });
 * const session = await api.createSession({ workspace });
 * await api.runTurn({ id: session.id, prompt }, { onEvent });
 * ```
 */
import createFetchClient, { type Middleware } from "openapi-fetch";
import type { paths } from "./schema.js";

export interface ClientOptions {
  baseUrl: string;
  /** 每次请求前注入的自定义头（如鉴权令牌）。 */
  headers?: Record<string, string>;
}

export interface TurnStreamOptions {
  onEvent: (event: TurnEvent) => void;
  signal?: AbortSignal;
}

/** SSE 流式事件（对应服务端 TurnEvent JSON）。 */
export interface TurnEvent {
  type: string;
  [key: string]: unknown;
}

export type ApiClient = ReturnType<typeof createFetchClient<paths>> & {
  /** 便捷方法：创建会话。 */
  createSession(input: { workspace: string; prompt?: string }): Promise<{
    id: string;
    [key: string]: unknown;
  }>;
  /** 便捷方法：发起 Agent 回合（SSE 流式，逐事件回调）。 */
  runTurn(
    input: { id: string; prompt: string },
    stream: TurnStreamOptions,
  ): Promise<void>;
  /** 便捷方法：健康检查。 */
  health(): Promise<boolean>;
};

/** 创建类型化 API 客户端。 */
export function createClient(options: ClientOptions): ApiClient {
  const baseUrl = options.baseUrl.replace(/\/+$/, "");
  const client = createFetchClient<paths>({
    baseUrl,
  });

  const auth: Middleware = {
    async onRequest({ request }) {
      if (options.headers) {
        for (const [key, value] of Object.entries(options.headers)) {
          request.headers.set(key, value);
        }
      }
    },
  };
  client.use(auth);

  const api = client as ApiClient;

  api.createSession = async (input) => {
    const { data, error } = await client.POST("/session", {
      body: input as never,
    });
    if (error) throw new Error(`createSession 失败：${JSON.stringify(error)}`);
    return data as { id: string; [key: string]: unknown };
  };

  api.runTurn = async (input, stream) => {
    const controller = new AbortController();
    const requestHeaders = new Headers(options.headers);
    requestHeaders.set("Content-Type", "application/json");
    const abortUrl = `${baseUrl}/session/${encodeURIComponent(input.id)}/abort`;
    const onAbort = () => {
      void fetch(abortUrl, {
        method: "POST",
        headers: new Headers(requestHeaders),
      }).catch(() => undefined);
      controller.abort();
    };
    if (stream.signal) {
      if (stream.signal.aborted) {
        onAbort();
      } else {
        stream.signal.addEventListener("abort", onAbort, { once: true });
      }
    }
    let reader: ReadableStreamDefaultReader<Uint8Array> | undefined;
    let buffer = "";
    let sawFinal = false;
    try {
      const response = await fetch(
        `${baseUrl}/session/${encodeURIComponent(input.id)}/turn`,
        {
          method: "POST",
          headers: requestHeaders,
          body: JSON.stringify({ prompt: input.prompt }),
          signal: controller.signal,
        },
      );
      if (!response.ok || !response.body) {
        throw new Error(`agentTurn 失败：HTTP ${response.status}`);
      }
      const streamReader = response.body.getReader();
      reader = streamReader;
      const decoder = new TextDecoder();
      const onEvent = (event: TurnEvent) => {
        if (event.type === "final") sawFinal = true;
        stream.onEvent(event);
      };
      while (true) {
        const { done, value } = await streamReader.read();
        if (done) {
          buffer += decoder.decode();
          if (buffer) {
            consumeSseLines(`${buffer}\n`, onEvent);
          }
          break;
        }
        buffer += decoder.decode(value, { stream: true });
        buffer = consumeSseLines(buffer, onEvent);
      }
      if (!sawFinal) throw new Error("agentTurn 流结束但未收到 final 事件");
    } finally {
      reader?.releaseLock();
      stream.signal?.removeEventListener("abort", onAbort);
    }
  };

  api.health = async () => {
    const { response } = await client.GET("/health");
    return response.ok;
  };

  return api;
}

function consumeSseLines(
  text: string,
  onEvent: (event: TurnEvent) => void,
): string {
  const lines = text.split("\n");
  const remainder = lines.pop() ?? "";
  for (const line of lines) {
    const trimmed = line.trim();
    if (!trimmed.startsWith("data:")) continue;
    const payload = trimmed.slice(5).trim();
    if (!payload || payload === "[DONE]") continue;
    try {
      onEvent(JSON.parse(payload) as TurnEvent);
    } catch {
      // 只忽略格式错误的事件，保留流的后续事件处理能力。
    }
  }
  return remainder;
}
