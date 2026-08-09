# OwO 内部配置 Schema v2（已由 v3 取代，仍可读取）

- 状态：P3 后体验优化内部契约，可迁移但不是公共 SDK
- 编码：严格 UTF-8 文本、禁止 BOM
- 大小上限：16 KiB

## 稳定序列化

```text
schema_version=2
candidate_page_size=5
user_learning_enabled=true
model_ranking_enabled=false
model_timeout_ms=50
correction_shortcut_enabled=true
correction_shortcut=Alt
language_shortcut_enabled=true
language_shortcut=Ctrl+Space
raw_input_shortcut_enabled=true
raw_input_shortcut=Enter
```

v2 沿用 v1 的原子保存、备份恢复、热加载和严格字段规则。读取器接受完整 v1，并为其补入上述快捷键默认值；下一次保存会写成完整 v2，不会丢失 v1 的候选、学习或模型设置。

## 快捷键字段

| 操作 | 开关 | 默认快捷键 | 语义 |
| --- | --- | --- | --- |
| 拼音纠错切换 | `correction_shortcut_enabled` | `Alt` | 切换后续候选请求是否生成单次编辑纠错路径；当前组合文本会立即刷新 |
| 中英文切换 | `language_shortcut_enabled` | `Ctrl+Space` | 切到英文前将已有拼音按 ASCII 原样上屏；英文模式不截获普通字母 |
| 拼音原样上屏 | `raw_input_shortcut_enabled` | `Enter` | 将当前组合栏原始拼音直接提交，不写入用户中文词频 |

快捷键使用规范顺序 `Ctrl+Alt+Shift+主键`。支持字母、数字、F1～F24、常用编辑/导航键和常用 OEM 符号键；为兼容默认纠错开关，只有 `Alt` 可作为独立修饰键，单独 `Ctrl`、`Shift` 或多修饰键无主键会被拒绝，避免吞掉复制、粘贴等宿主快捷键。启用的三个快捷键必须互不相同。设置中心在修饰键按下时继续等待主键，只有单独 `Alt` 在松开时确认；配置后端仍执行最终类型、规范化和冲突校验。

TSF 最多每 500 ms 在按键边界检查一次配置变化，失败时保留上一份有效快捷键快照。Core 继续通过 `ConfigMonitor` 使用同一文件的候选、学习和模型字段。
