# OwO Agent TypeScript SDK

类型化客户端（场景 4：第三方应用集成）。由核心服务 `/openapi.json` 生成类型，
运行时用 `openapi-fetch`，并封装了会话/回合/健康检查的便捷方法。

## 生成

1. 启动核心服务（`owo-agent serve --port 4096`）。
2. `npm install`
3. `npm run generate`（从运行中的服务抓取并生成 `src/schema.d.ts`）
   或 `npm run generate:local`（用仓库内 `openapi.json` 快照）。

## 使用

```ts
import { createClient } from "@owo-agent/client";

const api = createClient({ baseUrl: "http://127.0.0.1:4096" });
const session = await api.createSession({ workspace: process.cwd() });
await api.runTurn({ id: session.id, prompt: "读 README 并汇报" }, {
  onEvent: (event) => console.log(event.type),
});
```

也可直接用生成的类型调用任意端点：

```ts
await api.GET("/sessions");
await api.POST("/session/{id}/turn", { params: { path: { id } }, body: { prompt } });
```

## 验证

```bash
npm run typecheck   # tsc --noEmit
npm run test:unit   # 不依赖运行中的核心服务，验证客户端请求/SSE 行为
npm run build       # 构建发布包并复制 schema.d.ts
npm test            # 对运行中的服务做集成测试（默认 http://127.0.0.1:4097，可用 OWO_TS_SDK_BASE 覆盖）
```
