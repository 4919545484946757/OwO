# OwO 内部配置 Schema v3

- 状态：当前 Windows 配置格式
- 编码：严格 UTF-8 文本，禁止 BOM
- 大小上限：16 KiB

## 稳定序列化

```text
schema_version=3
candidate_page_size=5
candidate_wrap_length=12
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

`candidate_wrap_length` 控制单个候选词每行最多显示的 UTF-16 字符数，合法范围为
4～64，默认值为 12。它只影响候选窗显示，不改变提交文本、候选排序或点击区域语义。

读取器继续接受完整的 v1 和 v2 配置：v1 会补入默认快捷键和换行长度，v2 会补入
默认换行长度；下一次保存统一写为 v3。原子保存、备份恢复、严格未知字段拒绝和热加载
规则保持不变。

快捷键继续使用 v2 的规范格式。设置中心会清除配置后端 Windows CRLF 中的行尾 `CR`，
并在捕获组合键时等待主键；只有 `Alt` 可单独使用，单独 `Ctrl`、`Shift` 或无主键的
多修饰键会被拒绝。TSF 只截获精确匹配的按键按下事件，按键松开事件始终交还宿主。
