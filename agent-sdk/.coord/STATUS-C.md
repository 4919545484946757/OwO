# STATUS-C.md — Agent C 状态（回归门禁修复 + 打包发布）

> 我只写本文件。任务来源：主控 2026-08-14 分工指令。

## 认领

- 时间：2026-08-14
- 任务：修复确定性回归套件（sim-regression qq-observe）与技能门禁（skill-gate reportlab），更新打包发布链路，让 ONNX OCR 模型随包分发、开箱即用。
- 白名单文件：`scripts/sim-qq-observe-e2e.py`（必须）、`scripts/sim-regression.py`、`scripts/skill-gate.ps1`、`scripts/package-desktop.ps1`、`scripts/download-onnx-ocr-models.ps1`、`crates/owo-agent-core/src/onnx_ocr.rs`（仅 model_dir() 回退逻辑）、`dist/` 产物、`models/ocr/` 与 onnxruntime.dll 打包所需。
- 备注：`.coord/OWNERSHIP.md` 尚未由 Agent D 冻结；按任务指令声明的文件归属登记，不与 A/B/D 文件域冲突。

## 执行记录（时间戳）

- 2026-08-14 认领登记完成。
- 2026-08-14 sim-qq-observe-e2e.py 修复：`seen_kinds` 仅统计 `kind=="sim_event"` 且 `detail.type` 为字符串的观察项（None 跳过），保留 typed/send_clicked 断言语义；`python -m py_compile` 通过。
- 2026-08-14 sim-regression.py 实测 2/2 PASS（qq-learn/qq-observe，端口 4097/18500，临时数据目录）。
- 2026-08-14 reportlab 检查：运行时 python 已装 reportlab 4.4.9；skill-gate.ps1 实测 12/12 PASS（documents/spreadsheets/pdf/browser × 3 case）。
- 2026-08-14 onnx_ocr.rs model_dir() 改造完成：优先级 环境变量 → 用户数据目录 → exe 同级 models/ocr → 仓库相对路径（向上 4 层）；新增 3 个单元测试；cargo fmt -p owo-agent-core --check 干净；clippy -p owo-agent-core --all-targets -D warnings 0 警告；cargo test -p owo-agent-core 全绿（onnx_ocr 13 项含真实模型推理通过，不再静默跳过）。
- 2026-08-14 打包阻塞：owo-agent-server 被 Agent A 改造中，`cargo check -p owo-agent-server` 当前 6 个编译错误（非我文件，不动）；已预置 target/release/onnxruntime.dll（拷自 debug，避免打包时联网下载）。

## 遗留问题

- 待 Agent A 的 server crate 编译通过后：跑 package-desktop.ps1（debug/release zip）、build-installer.ps1（NSIS setup.exe）、generate-update-manifest.ps1（latest.json）、解包自检三项。
- tauri.conf.json 未声明 models 资源（不在白名单内）：NSIS 安装版侧载 exe 后模型经 model_dir() 回退链（用户数据目录/仓库路径）解析，便携 zip 为开箱即用载体。
