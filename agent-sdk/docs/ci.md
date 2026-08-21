# CI/CD 基线文档

## 概述

本文件描述了 agent-sdk 项目的 CI/CD 基线实现，遵循技术文档 v1.0 中定义的生产级加固要求。

## 工作流说明

### PR 流 (pr.yml)
- **触发**: 每次 push 到任何分支
- **检查项**:
  - `cargo fmt --all -- --check`
  - `cargo clippy --workspace --all-targets -- -D warnings`
  - workspace test
  - Node.js 语法/TS 类型检查
  - 路由契约检查

### Merge 流 (merge.yml)
- **触发**: 合并到 main 分支
- **检查项**: 重复核心门禁
- **输出**: 生成 SBOM

### Nightly 流 (nightly.yml)
- **触发**: 每日定时运行
- **检查项**:
  - 安全审计
  - 较重测试/性能或 soak 的可控入口

### Weekly 流 (weekly.yml)
- **触发**: 每周定时运行
- **检查项**:
  - 外部验收/eval 入口
  - 缺少 BYOK 时明确 skip，不泄露密钥

## 工具链

使用固定的 Rust 工具链版本：
- **Rust**: 1.80.1
- **Node.js**: 20.x
- **PowerShell**: 7.x

## 安全与依赖管理

### 供应链扫描
使用 `cargo-deny` 进行依赖安全扫描，配置文件为 `deny.toml`。

### 依赖策略
- 所有依赖必须通过安全扫描
- 不允许忽略已知高危漏洞
- 所有许可证必须在白名单中

## 权限与安全

- 工作流使用最小权限原则
- 不上传私密数据
- 所有敏感信息通过环境变量传递

## 输出与存档

- 所有工作流输出保存为 artifact
- SBOM 生成并保存
- 失败诊断保存为 artifact
- 不进行发布操作

## 本地验证

可以使用以下命令在本地验证 CI 门禁：

```powershell
# 本地门禁测试
.\scripts\ci-gate.ps1

# 本地格式检查
cargo fmt --all -- --check

# 本地 clippy 检查
cargo clippy --workspace --all-targets -- -D warnings

# 本地测试
cargo test --workspace
```

## 工作流配置

所有工作流都使用以下配置：
- **运行环境**: Windows Latest
- **缓存机制**: Cargo 缓存
- **安全检查**: 严格模式
- **输出**: artifact 保存

## 验收标准

- YAML 可解析
- PowerShell 脚本可被 Parser 验证
- 至少执行一次与 CI 对齐的本地只读门禁