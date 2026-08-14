//! M4c 多格式笔记 v1 契约测试：块树操作 / doc.json 往返 / MD 往返 / HTML 消毒 / 画布 / FTS / 零丢失 / 混合样例。

use owo_agent_core::notes::*;
use std::collections::BTreeMap;
use std::path::PathBuf;

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("owo-notes-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn simple_doc() -> NoteDoc {
    let mut doc = new_doc("d1", "测试文档");
    let root = doc.root.clone();
    append_child(
        &mut doc,
        &root,
        BlockKind::Heading {
            level: 1,
            text: "标题".into(),
        },
    )
    .unwrap();
    append_child(
        &mut doc,
        &root,
        BlockKind::Paragraph {
            text: "正文内容".into(),
        },
    )
    .unwrap();
    doc
}

// ---------------- 块树操作 ----------------

#[test]
fn new_doc_has_implicit_root() {
    let doc = new_doc("d", "t");
    assert!(doc.blocks.contains_key(&doc.root));
    assert_eq!(doc.blocks.len(), 1);
}

#[test]
fn add_and_get_blocks() {
    let mut doc = new_doc("d", "t");
    let root = doc.root.clone();
    let a = add_block(
        &mut doc,
        &root,
        BlockKind::Paragraph { text: "A".into() },
        BTreeMap::new(),
    )
    .unwrap();
    let got = get_block(&doc, &a).unwrap();
    assert_eq!(got.kind, BlockKind::Paragraph { text: "A".into() });
    assert_eq!(doc.blocks.get(&root).unwrap().children, vec![a.clone()]);
}

#[test]
fn add_block_rejects_missing_parent() {
    let mut doc = new_doc("d", "t");
    assert!(add_block(
        &mut doc,
        &"nope".into(),
        BlockKind::Paragraph { text: "x".into() },
        BTreeMap::new()
    )
    .is_err());
}

#[test]
fn insert_child_at_position() {
    let mut doc = new_doc("d", "t");
    let root = doc.root.clone();
    let a = append_child(&mut doc, &root, BlockKind::Paragraph { text: "A".into() }).unwrap();
    let b = append_child(&mut doc, &root, BlockKind::Paragraph { text: "B".into() }).unwrap();
    let c = insert_child(
        &mut doc,
        &root,
        1,
        BlockKind::Paragraph { text: "C".into() },
        BTreeMap::new(),
    )
    .unwrap();
    assert_eq!(doc.blocks.get(&root).unwrap().children, vec![a, c, b]);
}

#[test]
fn remove_block_deletes_subtree() {
    let mut doc = new_doc("d", "t");
    let root = doc.root.clone();
    let list = append_child(&mut doc, &root, BlockKind::List { ordered: false }).unwrap();
    let item = append_child(&mut doc, &list, BlockKind::ListItem { text: "x".into() }).unwrap();
    let removed = remove_block(&mut doc, &list).unwrap();
    assert!(removed.contains(&list) && removed.contains(&item));
    assert_eq!(doc.blocks.len(), 1); // 只剩 root
}

#[test]
fn move_block_reorders_and_rejects_cycles() {
    let mut doc = new_doc("d", "t");
    let root = doc.root.clone();
    let a = append_child(&mut doc, &root, BlockKind::Paragraph { text: "A".into() }).unwrap();
    let b = append_child(&mut doc, &root, BlockKind::Paragraph { text: "B".into() }).unwrap();
    let list = append_child(&mut doc, &root, BlockKind::List { ordered: false }).unwrap();
    move_block(&mut doc, &a, &list, None).unwrap();
    assert!(doc.blocks.get(&list).unwrap().children.contains(&a));
    // 环：list 不能移入 a（a 现在是 list 的子块）
    assert!(move_block(&mut doc, &list, &a, None).is_err());
    // 自身
    assert!(move_block(&mut doc, &a, &a, None).is_err());
    // 根保护
    assert!(move_block(&mut doc, &root, &b, None).is_err());
    assert!(remove_block(&mut doc, &root).is_err());
    let _ = b;
}

#[test]
fn doc_title_updates() {
    let mut doc = new_doc("d", "旧标题");
    doc_title(&mut doc, "新标题");
    assert_eq!(doc.title, "新标题");
    assert_ne!(doc.updated_at, "");
}

// ---------------- 持久化往返 ----------------

#[test]
fn save_load_roundtrip_preserves_structure() {
    let dir = scratch("persist1");
    let mut doc = simple_doc();
    let root = doc.root.clone();
    append_child(
        &mut doc,
        &root,
        BlockKind::Table {
            rows: vec![vec!["a".into(), "b".into()]],
        },
    )
    .unwrap();
    save_doc(&doc, &dir).unwrap();
    let loaded = load_doc(&dir).unwrap();
    assert_eq!(loaded, doc, "磁盘往返应完全一致");
    assert!(dir.join("assets").is_dir(), "资源目录应创建");
}

#[test]
fn save_load_ten_cycles_zero_loss() {
    let dir = scratch("persist10");
    let mut doc = generate_mixed_doc(42, "d10", "十次往返");
    for _ in 0..10 {
        save_doc(&doc, &dir).unwrap();
        let loaded = load_doc(&dir).unwrap();
        assert_eq!(loaded, doc, "第 n 次改→存→读应一致");
        // 每次微改后继续
        let root = doc.root.clone();
        append_child(
            &mut doc,
            &root,
            BlockKind::Paragraph {
                text: "追加".into(),
            },
        )
        .unwrap();
    }
}

#[test]
fn load_missing_doc_errors() {
    let dir = scratch("persist-missing");
    assert!(load_doc(&dir).is_err());
}

#[test]
fn corrupted_json_errors_cleanly() {
    let dir = scratch("persist-bad");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("doc.json"), "{ not json").unwrap();
    assert!(load_doc(&dir).is_err());
}

// ---------------- Markdown 导入/导出 ----------------

#[test]
fn md_import_covers_all_common_elements() {
    let md = "# 大标题\n\n## 小节\n\n普通段落，带标点。\n\n- 无序一\n- 无序二\n\n1. 有序一\n2. 有序二\n\n```rust\nlet x = 1;\n```\n\n> 引用内容\n\n| 表头A | 表头B |\n| --- | --- |\n| 值1 | 值2 |\n| 值3 | 值4 |\n\n![说明](assets/x.png)\n\n<p><b>嵌入</b></p>\n";
    let doc = md_to_doc("d", "t", md);
    let kinds: Vec<String> = walk(&doc, &doc.root)
        .iter()
        .map(|b| format!("{:?}", b.kind))
        .collect();
    assert!(kinds.iter().any(|k| k.starts_with("Heading")));
    assert!(kinds.iter().any(|k| k.starts_with("Paragraph")));
    assert!(kinds.iter().any(|k| k.starts_with("List")));
    assert!(kinds.iter().any(|k| k.starts_with("Code")));
    assert!(kinds.iter().any(|k| k.starts_with("Table")));
    assert!(kinds.iter().any(|k| k.starts_with("Quote")));
    assert!(kinds.iter().any(|k| k.starts_with("Image")));
    assert!(kinds.iter().any(|k| k.starts_with("HtmlEmbed")));
}

#[test]
fn md_roundtrip_is_fixed_point() {
    let samples = [
        "# 标题\n\n正文段落。\n",
        "- a\n- b\n- c\n",
        "1. 一\n2. 二\n",
        "> 引用\n\n正文\n",
        "```python\nprint(1)\n```\n",
        "| A | B |\n| --- | --- |\n| 1 | 2 |\n",
        "![alt](img.png)\n",
        "# t\n\n## s\n\n- x\n\n```\ncode\n```\n\n| a |\n| --- |\n| b |\n\n> q\n\n<p>hi</p>\n\n![i](p.png)\n",
    ];
    for (i, md) in samples.iter().enumerate() {
        let doc1 = md_to_doc(format!("d{i}"), "t", md);
        let out = doc_to_md(&doc1);
        let doc2 = md_to_doc(format!("d{i}b"), "t", &out);
        let kinds = |d: &NoteDoc| -> Vec<(String, String)> {
            walk(d, &d.root)
                .iter()
                .map(|b| (format!("{:?}", b.kind), block_text(d, b)))
                .collect()
        };
        assert_eq!(kinds(&doc1), kinds(&doc2), "样例 {i} MD 往返应为不动点");
    }
}

#[test]
fn md_roundtrip_fifty_generated_samples() {
    for seed in 0..50u64 {
        let doc = generate_mixed_doc(seed, format!("g{seed}"), "生成样例");
        let md = doc_to_md(&doc);
        let doc2 = md_to_doc(format!("g{seed}b"), "生成样例", &md);
        // 结构往返：MD 可表达元素（段落/标题/列表/代码/表格/图片/引用/HTML 嵌入）kind 序列一致；
        // Canvas/AiGenerated 块不经 MD 渲染（保真由 doc.json 承担，见零丢失测试）。
        let md_kinds = |d: &NoteDoc| -> Vec<String> {
            walk(d, &d.root)
                .iter()
                .filter(|b| {
                    !matches!(
                        b.kind,
                        BlockKind::Canvas { .. } | BlockKind::AiGenerated { .. }
                    )
                })
                .map(|b| format!("{:?}", b.kind))
                .collect()
        };
        assert_eq!(
            md_kinds(&doc),
            md_kinds(&doc2),
            "seed={seed} 结构往返不一致"
        );
    }
}

#[test]
fn hundred_mixed_docs_roundtrip_and_persist() {
    let dir = scratch("hundred");
    for seed in 0..100u64 {
        let doc = generate_mixed_doc(seed, format!("h{seed}"), "混合样例");
        // 存 → 读 → 导出 → 再导入 → 再存 → 再读
        let sub = dir.join(format!("d{seed}"));
        save_doc(&doc, &sub).unwrap();
        let loaded = load_doc(&sub).unwrap();
        assert_eq!(loaded, doc, "seed={seed} 磁盘往返无损");
        let md = doc_to_md(&doc);
        let reimported = md_to_doc(format!("h{seed}b"), "混合样例", &md);
        save_doc(&reimported, &sub).unwrap();
        let loaded2 = load_doc(&sub).unwrap();
        assert_eq!(loaded2, reimported, "seed={seed} 二次往返无损");
    }
}

// ---------------- HTML 消毒 ----------------

#[test]
fn sanitize_removes_script_style_iframe_and_events() {
    let dirty = r#"<div><script>alert(1)</script><style>body{}</style><iframe src="https://x"></iframe>正文<button onclick="go()">点</button></div>"#;
    let clean = sanitize_html(dirty);
    assert!(!clean.contains("script"));
    assert!(!clean.contains("style"));
    assert!(!clean.contains("iframe"));
    assert!(!clean.contains("button"));
    assert!(!clean.contains("onclick"));
    assert!(clean.contains("正文"), "可见文本应保留");
    assert!(clean.contains("<div>"), "白名单标签保留");
}

#[test]
fn sanitize_blocks_dangerous_urls_and_attrs() {
    let dirty = r#"<a href="javascript:alert(1)">x</a><a href="https://ok.dev/a">ok</a><img src="data:image/gif;base64,xx"><img src="/rel.png" alt="r"><p title="t" style="color:red" data-x="1">p</p>"#;
    let clean = sanitize_html(dirty);
    assert!(!clean.contains("javascript:"));
    assert!(!clean.contains("data:"));
    assert!(clean.contains("https://ok.dev/a"));
    assert!(clean.contains("/rel.png"));
    assert!(!clean.contains("style="));
    assert!(!clean.contains("data-x"), "未知属性剥离");
    assert!(clean.contains("title=\"t\""), "白名单属性保留");
}

#[test]
fn sanitize_preserves_safe_fragment() {
    let safe = r#"<p><strong>加粗</strong>与<em>斜体</em>，<a href="https://docs.example.com">链接</a></p>"#;
    let clean = sanitize_html(safe);
    assert_eq!(clean, safe, "安全片段应原样保留");
}

#[test]
fn html_embed_block_never_executes() {
    let mut doc = new_doc("d", "t");
    let root = doc.root.clone();
    let id = append_child(
        &mut doc,
        &root,
        BlockKind::HtmlEmbed {
            html: sanitize_html(r#"<img src="x" onerror="alert(1)"><script>evil()</script>"#),
        },
    )
    .unwrap();
    let block = get_block(&doc, &id).unwrap();
    if let BlockKind::HtmlEmbed { html } = block.kind {
        assert!(!html.contains("onerror"));
        assert!(!html.contains("script"));
    } else {
        panic!("应为 HtmlEmbed 块");
    }
}

// ---------------- 画布数据往返 ----------------

#[test]
fn canvas_block_data_roundtrips() {
    let mut doc = new_doc("d", "t");
    let root = doc.root.clone();
    let data = CanvasBlockData {
        rects: vec![
            CanvasRect {
                id: "r1".into(),
                x: 10.5,
                y: 20.0,
                w: 100.0,
                h: 60.0,
                layer: "bg".into(),
            },
            CanvasRect {
                id: "r2".into(),
                x: 0.0,
                y: 0.0,
                w: 1.0,
                h: 1.0,
                layer: "fg".into(),
            },
        ],
        notes: vec![CanvasNote {
            id: "n1".into(),
            x: 5.0,
            y: 5.0,
            text: "便签文本".into(),
        }],
        layers: vec!["bg".into(), "fg".into()],
    };
    let id = append_child(&mut doc, &root, BlockKind::Canvas { data: data.clone() }).unwrap();
    // serde 往返
    let json = serde_json::to_string(&doc).unwrap();
    let restored: NoteDoc = serde_json::from_str(&json).unwrap();
    let block = get_block(&restored, &id).unwrap();
    assert_eq!(block.kind, BlockKind::Canvas { data });
    // 磁盘往返
    let dir = scratch("canvas");
    save_doc(&doc, &dir).unwrap();
    let loaded = load_doc(&dir).unwrap();
    assert_eq!(loaded, doc);
}

// ---------------- 全文索引 ----------------

#[test]
fn in_memory_index_finds_blocks() {
    let mut doc = generate_mixed_doc(7, "idx", "索引文档");
    let root = doc.root.clone();
    let target = append_child(
        &mut doc,
        &root,
        BlockKind::Paragraph {
            text: "量子计算量子纠缠 检索词".into(),
        },
    )
    .unwrap();
    let mut index = NoteIndexer::in_memory();
    index.index_doc(&doc).unwrap();
    let hits = search_notes(&index, "量子");
    assert!(!hits.is_empty(), "应命中量子关键词");
    assert!(hits.iter().any(|h| h.block_id == target), "应命中目标块");
    let miss = search_notes(&index, "不存在的词xyz");
    assert!(miss.is_empty() || !miss.iter().any(|h| h.block_id == target));
}

#[test]
fn in_memory_index_reindex_replaces_old() {
    let mut doc = new_doc("d", "t");
    let root = doc.root.clone();
    append_child(
        &mut doc,
        &root,
        BlockKind::Paragraph {
            text: "alpha beta".into(),
        },
    )
    .unwrap();
    let mut index = NoteIndexer::in_memory();
    index.index_doc(&doc).unwrap();
    assert!(!search_notes(&index, "alpha").is_empty());
    // 清掉 alpha 重建
    let root = doc.root.clone();
    let removed_ids = {
        let root_block = doc.blocks.get(&root).unwrap().clone();
        root_block.children
    };
    for id in removed_ids {
        remove_block(&mut doc, &id).unwrap();
    }
    append_child(
        &mut doc,
        &root,
        BlockKind::Paragraph {
            text: "gamma".into(),
        },
    )
    .unwrap();
    index.index_doc(&doc).unwrap();
    assert!(search_notes(&index, "alpha").is_empty(), "重建后旧词应消失");
    assert!(!search_notes(&index, "gamma").is_empty());
}

#[test]
fn fts5_index_searches_blocks() {
    let dir = scratch("fts");
    let mut doc = generate_mixed_doc(9, "fts-doc", "FTS 文档");
    let root = doc.root.clone();
    let target = append_child(
        &mut doc,
        &root,
        BlockKind::Paragraph {
            text: "数据库索引全文检索测试语料".into(),
        },
    )
    .unwrap();
    let mut index = NoteIndexer::fts(&dir.join("index.db")).unwrap();
    index.index_doc(&doc).unwrap();
    let hits = search_notes(&index, "全文检索");
    assert!(!hits.is_empty(), "FTS 应命中");
    assert!(hits.iter().any(|h| h.block_id == target));
}

#[test]
fn fts5_reindex_updates() {
    let dir = scratch("fts2");
    let mut doc = new_doc("d", "t");
    let root = doc.root.clone();
    append_child(
        &mut doc,
        &root,
        BlockKind::Paragraph {
            text: "旧词 unique-word-abc".into(),
        },
    )
    .unwrap();
    let mut index = NoteIndexer::fts(&dir.join("index.db")).unwrap();
    index.index_doc(&doc).unwrap();
    assert!(!search_notes(&index, "unique-word-abc").is_empty());
    // 重建
    let root = doc.root.clone();
    let ids: Vec<String> = doc.blocks.get(&root).unwrap().children.clone();
    for id in ids {
        remove_block(&mut doc, &id).unwrap();
    }
    append_child(
        &mut doc,
        &root,
        BlockKind::Paragraph {
            text: "新词".into(),
        },
    )
    .unwrap();
    index.index_doc(&doc).unwrap();
    assert!(
        search_notes(&index, "unique-word-abc").is_empty(),
        "FTS 重建后旧词应消失"
    );
    assert!(!search_notes(&index, "新词").is_empty());
}

// ---------------- AI 生成块 / 文件块 ----------------

#[test]
fn ai_and_file_blocks_roundtrip() {
    let mut doc = new_doc("d", "t");
    let root = doc.root.clone();
    let ai = append_child(
        &mut doc,
        &root,
        BlockKind::AiGenerated {
            model: "m1".into(),
            prompt: "写摘要".into(),
            text: "摘要内容".into(),
        },
    )
    .unwrap();
    let file = append_child(
        &mut doc,
        &root,
        BlockKind::File {
            path: "assets/report.pdf".into(),
            mime: "application/pdf".into(),
        },
    )
    .unwrap();
    let dir = scratch("ai-file");
    save_doc(&doc, &dir).unwrap();
    let loaded = load_doc(&dir).unwrap();
    assert_eq!(loaded, doc);
    assert_eq!(
        get_block(&loaded, &ai).unwrap().kind,
        BlockKind::AiGenerated {
            model: "m1".into(),
            prompt: "写摘要".into(),
            text: "摘要内容".into(),
        }
    );
    assert_eq!(
        get_block(&loaded, &file).unwrap().kind,
        BlockKind::File {
            path: "assets/report.pdf".into(),
            mime: "application/pdf".into(),
        }
    );
}

#[test]
fn md_export_excludes_ai_blocks() {
    let mut doc = new_doc("d", "t");
    let root = doc.root.clone();
    append_child(
        &mut doc,
        &root,
        BlockKind::AiGenerated {
            model: "m".into(),
            prompt: "总结要点".into(),
            text: "要点内容".into(),
        },
    )
    .unwrap();
    let md = doc_to_md(&doc);
    assert!(!md.contains("要点内容"), "AI 块内容不进 MD 渲染");
    assert!(!md.contains("ai:"), "MD 不应含 AI 标记");
    // 保真由 doc.json 承担
    let dir = scratch("ai-md");
    save_doc(&doc, &dir).unwrap();
    let loaded = load_doc(&dir).unwrap();
    assert_eq!(loaded, doc);
}

// ---------------- 零丢失验收模型 ----------------

#[test]
fn zero_loss_ten_edit_cycles_hash_stable() {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let dir = scratch("zeroloss");
    let mut doc = generate_mixed_doc(123, "zl", "零丢失");
    let hash_of = |d: &NoteDoc| -> u64 {
        let mut hasher = DefaultHasher::new();
        d.blocks.len().hash(&mut hasher);
        let md = doc_to_md(d);
        md.hash(&mut hasher);
        hasher.finish()
    };
    let mut last = hash_of(&doc);
    for cycle in 0..10 {
        save_doc(&doc, &dir).unwrap();
        let loaded = load_doc(&dir).unwrap();
        assert_eq!(loaded, doc, "cycle {cycle} 读回应一致");
        let h = hash_of(&loaded);
        assert_eq!(h, last, "cycle {cycle} 哈希稳定");
        // 改一步
        let root = doc.root.clone();
        append_child(
            &mut doc,
            &root,
            BlockKind::Paragraph {
                text: format!("cycle-{cycle}"),
            },
        )
        .unwrap();
        last = hash_of(&doc);
    }
}
