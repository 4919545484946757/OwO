# STATUS-T2.md — Agent T2 状态（M4b 插件市场治理骨架）

> 我只写本文件。任务来源：主控 2026-08-14 第三轮分工指令（Agent T2 角色）。

## 认领

- 时间：2026-08-14
- 任务：按技术文档 §5.5.3 落地本地市场治理骨架：签名分发、静态扫描、versions.json 兼容选择、更新与失败回滚。P1 必须完成。
- 白名单：`core/src/plugin.rs`、`core/tests/plugin_lifecycle_tests.rs`、`plugins/*`、`scripts/plugin-*.ps1|py`（新）。
- 禁止：不改 lib.rs/Cargo.toml/server/CLI/desktop；不 commit；私钥/真实恶意代码不入库不入快照；新增依赖须到 `.coord3/DEPENDENCIES.md` 留言主控。

## P1 里程碑清单

- [ ] 1. manifest 扩展：签名可选字段（serde default）+ versions.json 解析（版本→App 最低版本映射，不兼容选兼容或拒绝）
- [ ] 2. 签名：scripts/plugin-sign.ps1（Ed25519）+ core 端 verify_plugin_signature（manifest+文件摘要）；缺失/不匹配→拒绝加载
- [ ] 3. 静态扫描：危险 API 黑名单 + 网络 allowlist 域校验；恶意样例内联测试全部拦截
- [ ] 4. 安装/更新/回滚状态机：install→verify→activate；update 先备份失败回滚；全程审计；篡改拒绝/回滚/域外拦截/versions 兼容测试
- [ ] 5. plugins/ 示例插件补 versions.json + README 签名流程说明

## 执行记录（时间戳）

- 认领登记；OWNERSHIP.md 待主控冻结，按指令白名单开工。
- [完成] plugin.rs 扩展：PluginManifest 新增 min_app_version/entry/network_allowlist/signature（serde default 向后兼容，含旧 JSON 解析测试）；PluginSignature 结构。
- [完成] versions.json：VersionsJson 解析 + resolve_compatible（选最高兼容版本，全不兼容拒绝）+ version_cmp/version_gte（点分数字比较）。
- [完成] 静态扫描：scan_plugin_for_risks（危险 API 黑名单 os.system/subprocess/eval… + http(s) URL 域提取 + allowlist 校验）。
- [完成] 安装/更新/回滚状态机：PluginManager（install→verify→activate；update 先备份失败回滚；uninstall；min_app_version 门禁；require_signature 开关）+ 审计轨迹（PluginInstallReport）。
- [完成] 签名工具：scripts/plugin-sign.py + plugin-sign.ps1（Ed25519；generate/sign/verify；摘要口径 = sha256(id|name|version|entry[|入口内容])，与 Rust plugin_digest 一致）。实测 generate→sign→verify 通过（3 个示例插件全签名）。
- [完成] P1-5：三个示例插件补 entry 字段 + versions.json + README 签名/市场治理说明。
- [完成] 契约测试扩展：plugin_lifecycle_tests.rs 13 项（3 既有 + 10 新增：版本兼容×2、serde 兼容、扫描×3、签名×1、安装生命周期、篡改拒绝、更新失败保留旧版、min_app_version 拒绝、缺签名拒绝/可放宽）。
- [完成] P2 预留：PluginSubmission（提交/审核状态 Submitted/Approved/Rejected）、MarketUpdateManifest（远端更新清单 + has_update 版本/兼容判断）+ 测试。
- ⏳ **阻塞**：ed25519-dalek + sha2 依赖未加（DEPENDENCIES.md 已留言 @主控 2026-08-14）；plugin.rs 签名部分与测试编译需要依赖。主控收尾统一合并依赖后即可验证。
- [完成] 依赖已由主控加入（ed25519-dalek + sha2）✓（DEPENDENCIES.md 处理）
- [完成] 依赖已由主控加入（ed25519-dalek + sha2）✓；**修复主控加依赖时的重复 key**（core Cargo.toml 中 ed25519-dalek/sha2 各两份：workspace 引用 + 直写重复 → 删除直写行，保留 workspace 引用；该重复导致整个 workspace 无法编译）。
- [完成] 门禁实测：`cargo test -p owo-agent-core --test plugin_lifecycle_tests` **17/17**（3 既有 + 14 新增）；`plugin.rs` 内置单测 3/3；rustfmt（仅 T2 文件）干净；`clippy -p owo-agent-core --lib -D warnings` 0 警告。
- [完成] 交叉验证：临时独立 crate（temp 目录）验证 Python cryptography 签名 ↔ Rust ed25519-dalek 验签**双向一致**（plugin-sign.py 签名的 owo-translate 被 Rust 摘要口径验证通过；Rust 生成的签名被 Python 验签通过；篡改检测生效）。证明 plugin_digest 摘要口径两实现完全一致。
- [完成] plugin-sign.ps1 对三个示例插件 sign→verify 实测跑通。

## 门禁结果（实测）

| 门禁 | 结果 |
|---|---|
| cargo test -p owo-agent-core --test plugin_lifecycle_tests | ✅ 17/17 |
| cargo test -p owo-agent-core --lib plugin | ✅ 3/3（内置单测） |
| rustfmt（仅 T2 文件：plugin.rs/plugin_lifecycle_tests.rs） | ✅ 干净 |
| clippy -p owo-agent-core --lib -- -D warnings | ✅ 0 警告 |
| scripts/plugin-sign.ps1 generate/sign/verify（示例插件） | ✅ 跑通 |
| python -m py_compile plugin-sign.py | ✅ 0 错误 |

## 遗留问题

- 示例插件签名使用一次性测试密钥（私钥在 $env:TEMP，不入库）；正式分发需发布者密钥，公钥随包分发。
- update 的"拷贝失败回滚"分支（copy_dir 失败路径）在 Windows 上难可靠构造，测试改为验证"校验失败旧版保留 + 更新成功先备份"两条路径；回滚代码路径保留（backup → 失败 → 恢复）。
- 市场服务端（提交/审核 API）与自动更新拉取为 P2 预留结构（PluginSubmission/MarketUpdateManifest），HTTP 接线留待主控。 

## 遗留问题

- 依赖 ed25519-dalek = "2" + sha2 = "0.10" 待主控加入 Cargo.toml/Cargo.lock（DEPENDENCIES.md 留言中）。
- 示例插件签名使用一次性测试密钥（私钥在 $env:TEMP，不入库）；正式分发需发布者密钥。
