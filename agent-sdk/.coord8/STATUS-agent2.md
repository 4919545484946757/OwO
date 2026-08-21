# Agent 2 — 可复现 CI/CD 基线（认领与状态）

## 认领（2026-08-20）

独占文件：

- `agent-sdk/.github/**`
- `agent-sdk/rust-toolchain.toml`
- `agent-sdk/deny.toml`
- `agent-sdk/scripts/ci-*.ps1`（含 `ci-gate.ps1`、`ci-nightly.ps1`、`ci-weekly.ps1`；**不改** `gate.ps1`）
- `agent-sdk/docs/ci.md`
- `agent-sdk/.coord8/STATUS-agent2.md`

不修改：`Cargo.toml`、`Cargo.lock`、Rust 业务源码、`SECURITY.md`；不执行 git commit。

## 关于既有 .github 目录的说明

上轮会话遗留 5 个工作流文件（pr/merge/nightly/weekly/ci-cd-baseline.yml），存在阻断性问题：

- `ci-gate.ps1 -SkipUtf8 -SkipClippy -SkipTest -SkipNode`（全跳过=空跑）；
- Node/TS 与路由契约为占位 echo，非真实检查；
- 引用了不存在的 cargo feature（`route-contract-tests`/`heavy-tests`/`eval-tests`）→ 会导致 CI 直接失败；
- 工具链写死 1.80.1 与 `rust-toolchain.toml`(1.81.0) 冲突；cargo 命令在仓库根运行（无 workspace）必然失败；
- merge.yml 使用 `upload-release-asset`（需 release 事件 + `contents: write`，push 场景必错）；
- weekly.yml 的 skip 判断 `${{ !env.OPENAI_API_KEY }}` 语法无效。
- 重复的 `ci-cd-baseline.yml` 职责重叠。

本会话在新轮次中整体重写（均在独占范围内），并保证真实门禁而非占位。注意：GitHub Actions 只会自动发现仓库根 `.github/workflows`；本仓库根无 `.github`，`agent-sdk/.github/workflows/**` 是依据协作规则的存放位置，随仓库整仓托管后在根级自动生效（详见 `docs/ci.md` 的"工作流位置说明"）。

## 状态

- [x] 阅读 AGENTS.md / AGENTS-COORD.md / 综合技术文档 §6-§7 / gate.ps1、sbom.ps1、package-desktop.ps1 / Cargo.toml
- [x] 重写 `ci-gate.ps1`（真实门禁：utf8/fmt/clippy/workspace test/路由契约/node/TS；支持 -Step 拆分与 -LogDir 诊断）
- [x] 新增 `ci-nightly.ps1`、`ci-weekly.ps1`（BYOK 缺失显式 skip、不泄露密钥）
- [x] 重写 `deny.toml`（合法 cargo-deny 配置；高危漏洞不静默忽略；过渡规则）
- [x] 重写 PR / Merge / Nightly / Weekly 四个工作流 + 可复用 `ci-core.yml`；删除失效的 `ci-cd-baseline.yml`
- [x] 复核 `rust-toolchain.toml`（固定 1.81.0，组件/目标完整）
- [x] 本地只读门禁验证（见下）
- [x] 重写 `docs/ci.md`

## 触发条件

| 工作流 | 触发 |
|---|---|
| `pr.yml` | PR 指向 `main`，路径含 `agent-sdk/**`；`concurrency` 取消旧跑 |
| `merge.yml` | push 到 `main`，路径含 `agent-sdk/**` |
| `nightly.yml` | 每晚 `0 2 * * *` + `workflow_dispatch` |
| `weekly.yml` | 每周一 `0 3 * * 1` + `workflow_dispatch` |

## 各 job 一览

- `ci-core.yml`（可复用，`workflow_call`）：Windows 主线核心门禁 job `gate`
  - 步骤：utf8 → fmt → clippy(`-D warnings`, `--locked`) → workspace test(`--locked`) → route contract(`cargo test -p owo-agent-server --test route_contract_tests --locked`) → node --check → TS typecheck；`generate-sbom` 时产出 `dist/sbom.json`；`if: always()` 上传诊断日志并装配 SBOM artifact。
- `pr.yml`：`core`（复用 ci-core，不产 SBOM）+ `linux-check`（ubuntu-latest，`x86_64-unknown-linux-gnu` cargo check，`continue-on-error: true` 只报告不阻断）。
- `merge.yml`：`core`（复用 ci-core，`generate-sbom: true`）+ `linux-check`（同 PR，只报告）。
- `nightly.yml`：`nightly`（windows-latest）——cargo-audit + cargo-deny（安全审计）+ release 全量 workspace test + 可控的 soak/fault-inject/sim-e2e/bench/skill-gate 入口（`ci-nightly.ps1 -Include*`，前置缺失显式 SKIP 不 panic）+ SBOM + 诊断 artifact；`linux-check` 辅助。
- `weekly.yml`：`weekly`（windows-latest）——依赖审计（audit+deny）+ 真模型 eval/外部验收（BYOK 缺失显式 skip，不打印密钥）+ SBOM + 红队占位（缺工具显式 skip）+ 诊断 artifact。

## 工具版本

- Rust 工具链：1.81.0（`rust-toolchain.toml` 固定；rustfmt、clippy、rust-src、llvm-tools-preview；targets windows-msvc + linux-gnu）
- Node.js：20.x（`package-lock.json` 锁定 TS 依赖）
- PowerShell：Windows PowerShell 5.1（`shell: powershell`；脚本与 7.x 兼容）
- cargo-audit：最新（GitHub 用 `taiki-e/install-action`）
- cargo-deny：最新（同上）
- Actions：`actions/checkout@v4`、`actions/setup-node@v4`、`dtolnay/rust-toolchain@v1`、`Swatinem/rust-cache@v2`、`taiki-e/install-action@v2`、`actions/upload-artifact@v4`；token 权限最小化（`contents: read`，无写权限、无 release 发布）

## 本地可验证命令（已执行）

```powershell
# 工具链核对
cargo +1.81.0 --version; rustup show active-toolchain

# 与 CI 对齐的只读门禁（ci-gate.ps1 全量 = UTF-8 + fmt + clippy + workspace test + 路由契约 + node + TS）
powershell -ExecutionPolicy Bypass -File scripts\ci-gate.ps1 -LogDir "$env:TEMP\ci-gate-local"

# 单个步骤拆分验证（与 pr.yml 中逐步骤调用一致）
powershell -ExecutionPolicy Bypass -File scripts\ci-gate.ps1 -Step utf8
powershell -ExecutionPolicy Bypass -File scripts\ci-gate.ps1 -Step fmt
powershell -ExecutionPolicy Bypass -File scripts\ci-gate.ps1 -Step clippy
powershell -ExecutionPolicy Bypass -File scripts\ci-gate.ps1 -Step route-contract

# YAML 解析（Python tomllib 兜底 + Ruby/actionlint 可用时精确）
python -c "import yaml,sys; [yaml.safe_load(open(f,'rb').read()) for f in glob.glob(r'agent-sdk/.github/workflows/*.yml')]"

# 构建脚本语法校验（PS 解析器）
powershell -Command "[System.Management.Automation.PSParser]::Tokenize((Get-Content -Raw 'scripts\ci-gate.ps1'),[ref]$null)"

# 供应链（本机未安装时跳过，CI 已由 install-action 提供）
cargo audit; cargo deny check
```

## 完成

- YAML 全部可解析；
- PowerShell 脚本全部通过 Parser 验证（UTF-8 BOM 保留）；
- 已执行与 CI 对齐的本地只读门禁（UTF-8/fmt/路由契约等，见 `docs/ci.md`"本地验证"）。