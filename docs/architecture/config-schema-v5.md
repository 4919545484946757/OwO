# OwO 内部配置 Schema v5

Schema v5 在 v4 基础上增加四组可热加载的候选窗导航快捷键：

```text
cursor_left_shortcut_enabled=true
cursor_left_shortcut=Shift+Left
cursor_right_shortcut_enabled=true
cursor_right_shortcut=Shift+Right
previous_page_shortcut_enabled=true
previous_page_shortcut=Shift+Up
next_page_shortcut_enabled=true
next_page_shortcut=Shift+Down
```

启用的快捷键必须使用规范顺序 `Ctrl+Alt+Shift+主键`，并且不能互相重复。
读取器继续接受完整的 v1 至 v4 配置；缺少的导航字段使用以上默认值，下一次保存时统一写为 v5。

光标左右移动只在 OwO 中文模式存在活动拼音缓冲时拦截按键，不会占用应用中的普通导航键。上一页、下一页在折叠候选窗中切换候选页，在展开候选表中滚动一行候选。
