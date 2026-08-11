//! 会话分享/导出：生成自包含 Markdown 或 HTML 会话记录。

use crate::gateway::ChatMessage;
use crate::session::Session;

pub fn export_markdown(session: &Session) -> String {
    let mut output = String::new();
    output.push_str(&format!(
        "# OwO Agent 会话分享\n\n- 会话：`{}`\n- 工作区：`{}`\n- 模型：`{}`\n- 创建：{}\n- 更新：{}\n",
        session.id,
        display_workspace(session),
        session.model,
        session.created_at,
        session.updated_at
    ));
    if let Some(parent) = &session.parent_id {
        output.push_str(&format!("- 父会话：`{parent}`\n"));
    }
    output.push('\n');
    for (index, message) in session.messages.iter().enumerate() {
        output.push_str(&render_message_markdown(index, message));
    }
    output
}

fn render_message_markdown(index: usize, message: &ChatMessage) -> String {
    let mut output = String::new();
    let content = message.content.as_deref().unwrap_or_default();
    match message.role.as_str() {
        "user" => {
            output.push_str(&format!("## {index}. 用户\n\n{content}\n\n"));
        }
        "assistant" => {
            if let Some(tool_calls) = &message.tool_calls {
                output.push_str(&format!("## {index}. 助手（工具调用）\n\n"));
                for call in tool_calls {
                    output.push_str(&format!("- `{}`：`{}`\n", call.name, call.arguments));
                }
                output.push('\n');
            } else {
                output.push_str(&format!("## {index}. 助手\n\n{content}\n\n"));
            }
        }
        "tool" => {
            output.push_str(&format!(
                "## {index}. 工具结果\n\n```json\n{content}\n```\n\n"
            ));
        }
        "system" => {
            output.push_str(&format!("## {index}. 系统\n\n{content}\n\n"));
        }
        other => {
            output.push_str(&format!("## {index}. {other}\n\n{content}\n\n"));
        }
    }
    output
}

pub fn export_html(session: &Session) -> String {
    let mut body = String::new();
    body.push_str(&format!(
        "<h1>OwO Agent 会话分享</h1><dl>\
         <dt>会话</dt><dd><code>{}</code></dd>\
         <dt>工作区</dt><dd><code>{}</code></dd>\
         <dt>模型</dt><dd><code>{}</code></dd>\
         <dt>创建</dt><dd>{}</dd>\
         <dt>更新</dt><dd>{}</dd></dl>",
        html_escape(&session.id),
        html_escape(&display_workspace(session)),
        html_escape(&session.model),
        html_escape(&session.created_at),
        html_escape(&session.updated_at)
    ));
    if let Some(parent) = &session.parent_id {
        body.push_str(&format!(
            "<p>父会话：<code>{}</code></p>",
            html_escape(parent)
        ));
    }
    body.push_str("<hr>");
    for (index, message) in session.messages.iter().enumerate() {
        let role = match message.role.as_str() {
            "user" => "用户",
            "assistant" => "助手",
            "tool" => "工具结果",
            "system" => "系统",
            other => other,
        };
        body.push_str(&format!(
            "<h2>{index}. {role}</h2><pre>{}</pre>",
            html_escape(message.content.as_deref().unwrap_or_default())
        ));
        if let Some(tool_calls) = &message.tool_calls {
            for call in tool_calls {
                body.push_str(&format!(
                    "<p><code>{}</code>: <code>{}</code></p>",
                    html_escape(&call.name),
                    html_escape(&call.arguments.to_string())
                ));
            }
        }
    }
    format!(
        "<!DOCTYPE html><html lang=\"zh\"><head><meta charset=\"utf-8\">\
         <title>OwO Agent 会话分享</title>\
         <style>body{{max-width:820px;margin:2rem auto;padding:0 1rem;\
         font-family:system-ui,sans-serif;line-height:1.6}}\
         pre{{background:#f5f5f5;padding:0.8rem;border-radius:6px;overflow-x:auto}}\
         code{{background:#f0f0f0;padding:0.1rem 0.3rem;border-radius:4px}}\
         h2{{border-bottom:1px solid #eee;padding-bottom:0.2rem}}</style></head>\
         <body>{body}</body></html>"
    )
}

fn display_workspace(session: &Session) -> String {
    let raw = session.workspace.to_string_lossy().replace('\\', "/");
    raw.strip_prefix("//?/").unwrap_or(&raw).to_string()
}

fn html_escape(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gateway::ChatMessage;

    #[test]
    fn markdown_export_contains_messages() {
        let mut session = Session::new(".", "mock", None);
        session.push(ChatMessage::user("你好".to_string()));
        session.push(ChatMessage::assistant_text("收到".to_string()));
        session.push(ChatMessage::tool(
            "c1".to_string(),
            r#"{"text":"ok"}"#.to_string(),
        ));

        let markdown = export_markdown(&session);
        assert!(markdown.contains("你好"));
        assert!(markdown.contains("收到"));
        assert!(markdown.contains(r#"{"text":"ok"}"#));
        assert!(markdown.contains("模型"));
    }

    #[test]
    fn html_export_escapes_and_is_self_contained() {
        let mut session = Session::new(".", "mock", None);
        session.push(ChatMessage::user("<script>alert(1)</script>".to_string()));

        let html = export_html(&session);
        assert!(html.starts_with("<!DOCTYPE html>"));
        assert!(html.contains("&lt;script&gt;"));
        assert!(!html.contains("<script>alert"));
    }
}
