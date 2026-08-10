# OwO 内部配置 Schema v4

- 状态：当前 Windows 配置格式
- 编码：严格 UTF-8 文本，禁止 BOM
- 大小上限：16 KiB

## 稳定序列化

```text
schema_version=4
candidate_page_size=5
candidate_wrap_length=12
user_learning_enabled=true
user_learning_sensitivity=7
model_ranking_enabled=false
model_timeout_ms=50
correction_shortcut_enabled=true
correction_shortcut=Alt
language_shortcut_enabled=true
language_shortcut=Ctrl+Space
raw_input_shortcut_enabled=true
raw_input_shortcut=Enter
```

`user_learning_sensitivity` 的合法范围为 1～10，默认值为 7。它只调整读取学习计数时的排序增益，不改写已经存储的计数，因此调低后立即可逆。`user_learning_enabled=false` 会停止新增学习记录，但不会删除已有数据。

`model_ranking_enabled` 在 v4 中启用本地上下文自适应排序：Core 使用“规范化拼音 → 已选候选”以及“最近一次 OwO 上屏片段（最多 16 个字符）+ 拼音 → 候选”的本地计数补充全局词频分数。该功能不读取应用名称、窗口标题或完整正文，也不上传数据。若配置了通过质量和许可证门禁的外部 ModelHost，它可以在基础响应之后继续做可降级的二次重排；基础候选不等待外部模型。

读取器继续接受完整的 v1、v2、v3 配置，并为旧版本补入灵敏度默认值 7；下一次保存统一写为 v4。原子保存、备份恢复、严格拒绝未知字段和热加载规则不变。
