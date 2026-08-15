# STATUS-plugin-market.md — Lane B 状态（插件市场 HTTP API + 插件面板）

> 我只写本文件。任务来源：主控 2026-08-14 第四轮分工指令（Agent B / Lane B）。

## 交付文件（全部新建，未改任何既有文件）

| 文件 | 说明 |
|---|---|
| `crates/owo-agent-server/src/plugin_market_api.rs` | 插件市场 API 模块（独立可编译，不 use crate::/super::） |
| `crates/owo-agent-server/tests/plugin_market_api_tests.rs` | 契约测试（9 个 #[test]，断言场景 ~16） |
| `desktop/web/panels/plugin-market.panel.js` | 插件市场面板（IIFE + OwoPanels 注册） |
| `.coord4/STATUS-plugin-market.md` / `DEPENDENCIES-plugin-market.md` | 本文件 + 依赖说明 |

## 完成清单

1. **路由（前缀 /plugins/market，全部在 plugin_market_api::router 内）**
   - GET /plugins/market：目录（discover_plugins 本地清单 + market.json 合并，含 has_update/risks/source）
   - POST /plugins/market/seed：写 market.json（同 id 合并 min_app_version）
   - GET /plugins/market/versions?id=&app=：VersionsJson 兼容解析（workspace/plugins 与 data_root/plugins 查找）
   - POST /plugins/market/verify {dir}：PluginManager::verify_plugin_dir（签名/扫描/版本门禁）
   - POST /plugins/market/install {dir}：安全前置扫描（高危拒绝）→ install
   - POST /plugins/market/update {id, dir}：先备份、失败回滚（PluginManager 保证），高危新版拒绝且旧版保留
   - POST /plugins/market/uninstall {id}：返回被移除文件列表
   - GET /plugins/market/scan?dir=：风险扫描摘要（不安装）
   - GET /plugins/market/audit?n=：模块内审计尾部（写操作全留痕）
2. **状态与安全**
   - data_root 键控 manager 注册表（Mutex<HashMap<PathBuf, Arc<Mutex<PluginManager>>>>），每次调用同步 env
   - require_signature 默认 true；OWO_PLUGIN_REQUIRE_SIGNATURE=0 关闭（联调/测试）
   - app_version = env!("CARGO_PKG_VERSION")
   - market.json 在 data_root/plugins/market.json
   - 写操作审计：seed/verify/install/update/uninstall 全部记录（含时间戳），GET audit 尾部
3. **测试**（9 个 #[test]，断言场景 ~16）
   - 独立测试 8 个（不依赖 env）：catalog/seed/versions/scan/audit/missing_body/seed 空 id/verify 缺 manifest
   - 串行测试 1 个（signature_install_flow_serial，Runtime::block_on）：缺签名拒绝、签名插件 verify+install、篡改拒绝、
     高危扫描拒绝、签名关闭后可装、update 成功、高危 update 拒绝且旧版保留、uninstall 返回移除文件、未知 id 404、审计含 install
4. **面板**：目录列表（名称/版本/来源/可更新/风险徽标）、扫描/校验/安装/更新/卸载表单、seed 编辑器、审计尾部；
   防御性降级（helpers 缺省时自 fetch）、样式 owo-market- 前缀、XSS 全 esc()

## 门禁结果（实测，PowerShell）

| 门禁 | 结果 |
|---|---|
| `cargo test -p owo-agent-server --test plugin_market_api_tests` | ✅ 9/9（16 断言场景） |
| `cargo clippy -p owo-agent-server --all-targets -- -D warnings` | ✅ 0 警告（仅我的文件有 clippy 错误时已修） |
| `cargo fmt --all -- --check`（我的两文件 rustfmt --check） | ✅ 干净 |
| `node --check desktop/web/panels/plugin-market.panel.js` | ✅ 0 错误 |

## 需主控接线的点

1. `lib.rs`：`mod plugin_market_api;` + 在 build_router 合并 `plugin_market_api::router(state.clone())`。
2. `route_contract_tests.rs`：新增 POST 路由 sample_body（{dir}/{id,dir}/{id}/seed entries）。
3. openapi_spec 登记 /plugins/market* 全部路径。
4. index.html + app.js 挂载 plugin-market 面板。
5. 测试签名常量说明：tests 中 SIGNED_MANIFEST 是 scripts/plugin-sign.py 对
   (id=signed-ok, entry=server.py, 内容=print('signed-ok')) 的 Ed25519 签名（与 core::plugin::plugin_digest 口径一致）。
   若未来更换签名算法/摘要口径，需重新生成该常量。

## 风险 / 遗留

- env（OWO_PLUGIN_REQUIRE_SIGNATURE）是进程级：签名语义测试已合并为单串行测试（协议建议做法）；独立测试不依赖 env。
- update 的"拷贝失败回滚"分支（copy_dir 失败）在 Windows 上难可靠构造，测试覆盖"校验失败旧版保留 + 更新成功先备份"两条路径（core 的 PluginManager 实现含回滚代码路径）。
- seed 的 name/description/url 字段保留（协议字段），当前仅落 id/version/min_app_version（#[allow(dead_code)]）。
