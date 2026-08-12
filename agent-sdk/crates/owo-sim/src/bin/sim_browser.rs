//! OwO 模拟浏览器站（桌面实验台）：本地搜索页 + 文章页 + 图片下载。
//!
//! 用法：owo-sim-browser --port 18201
//! 页面：
//!   /            模拟搜索引擎首页（表单 + 链接）
//!   /search?q=.. 搜索结果页（3 条结果 + 图片下载链接）
//!   /article/1..3 文章页（正文 + 图片）
//!   /img/1.png|2.png|3.png  生成的 PNG 图片

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::thread;

fn main() {
    let mut port = 18201;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--port" {
            if let Some(value) = args.next() {
                port = value.parse().unwrap_or(18201);
            }
        }
    }
    let images = generate_images();
    let listener = TcpListener::bind(("127.0.0.1", port))
        .unwrap_or_else(|e| panic!("绑定端口 {port} 失败：{e}"));
    println!("owo-sim-browser listening on http://127.0.0.1:{port}");
    let images = Arc::new(images);
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let images = Arc::clone(&images);
                thread::spawn(move || handle_connection(stream, &images));
            }
            Err(_) => continue,
        }
    }
}

fn handle_connection(mut stream: TcpStream, images: &Arc<Vec<(String, Vec<u8>)>>) {
    let mut buffer = [0u8; 8192];
    let read = match stream.read(&mut buffer) {
        Ok(read) => read,
        Err(_) => return,
    };
    if read == 0 {
        return;
    }
    let request = String::from_utf8_lossy(&buffer[..read]).to_string();
    let first_line = request.lines().next().unwrap_or("GET / HTTP/1.1");
    let mut parts = first_line.split_whitespace();
    let _method = parts.next().unwrap_or("GET");
    let target = parts.next().unwrap_or("/");
    let (path, query) = match target.split_once('?') {
        Some((path, query)) => (path, Some(query)),
        None => (target, None),
    };

    let response = match path {
        "/" => html_response(&home_page()),
        "/search" => {
            let q = parse_query(query).unwrap_or_default();
            html_response(&search_page(&q))
        }
        "/article/1" | "/article/2" | "/article/3" => {
            let id = path.rsplit('/').next().unwrap_or("1");
            let q = parse_query(query).unwrap_or_else(|| "默认主题".to_string());
            html_response(&article_page(id, &q))
        }
        "/img/1.png" | "/img/2.png" | "/img/3.png" => {
            let id = path.rsplit('/').next().unwrap_or("1.png");
            let key = id.to_string();
            if let Some((_, bytes)) = images.iter().find(|(name, _)| *name == key) {
                png_response(bytes)
            } else {
                not_found()
            }
        }
        "/health" => json_response("{\"ok\":true}"),
        _ => not_found(),
    };
    let _ = stream.write_all(&response);
    let _ = stream.flush();
}

fn parse_query(query: Option<&str>) -> Option<String> {
    let query = query?;
    for pair in query.split('&') {
        let (key, value) = pair.split_once('=')?;
        if key == "q" {
            return Some(url_decode(value));
        }
    }
    None
}

fn url_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'+' => out.push(b' '),
            b'%' if index + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).ok();
                if let Some(hex) = hex {
                    if let Ok(value) = u8::from_str_radix(hex, 16) {
                        out.push(value);
                        index += 2;
                    }
                }
            }
            byte => out.push(byte),
        }
        index += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

fn home_page() -> String {
    r#"<!doctype html>
<html lang="zh-CN">
<head><meta charset="utf-8"><title>OwO 模拟搜索</title></head>
<body>
  <h1>OwO 模拟搜索</h1>
  <form action="/search" method="get">
    <input type="text" name="q" placeholder="输入关键词">
    <button type="submit">搜索</button>
  </form>
  <p>测试入口：<a href="/article/1?q=%E9%A3%8E%E6%99%AF">示例文章 1</a></p>
</body>
</html>"#
        .to_string()
}

fn search_page(query: &str) -> String {
    let escaped = html_escape(query);
    let mut results = String::new();
    for index in 1..=3 {
        let title = format!("结果{index}：{escaped} 相关介绍");
        let snippet = format!("这是关于“{escaped}”的第 {index} 条模拟搜索结果，内容为本地生成。");
        results.push_str(&format!(
            r#"<li><h3><a href="/article/{index}?q={url}">{title}</a></h3><p>{snippet}</p></li>"#,
            url = url_encode(query)
        ));
    }
    format!(
        r#"<!doctype html>
<html lang="zh-CN">
<head><meta charset="utf-8"><title>搜索：{escaped}</title></head>
<body>
  <h1>搜索结果：{escaped}</h1>
  <ul>{results}</ul>
  <p>图片下载测试：<a href="/img/1.png" download>下载示例图 1</a></p>
  <p><a href="/">返回首页</a></p>
</body>
</html>"#
    )
}

fn article_page(id: &str, query: &str) -> String {
    let escaped = html_escape(query);
    let image_id = match id {
        "2" => "2.png",
        "3" => "3.png",
        _ => "1.png",
    };
    format!(
        r#"<!doctype html>
<html lang="zh-CN">
<head><meta charset="utf-8"><title>文章 {id}：{escaped}</title></head>
<body>
  <h1>文章 {id}：{escaped}</h1>
  <p>这里是模拟文章正文。关键词：{escaped}。图片见下方。</p>
  <img src="/img/{image_id}" alt="示例图片 {id}" width="320" height="240">
  <p><a href="/img/{image_id}" download>下载高清图（{image_id}）</a></p>
  <p><a href="/search?q={url}">继续搜索 {escaped}</a></p>
</body>
</html>"#,
        url = url_encode(query)
    )
}

fn html_escape(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn url_encode(input: &str) -> String {
    let mut out = String::new();
    for byte in input.as_bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(*byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

fn generate_images() -> Vec<(String, Vec<u8>)> {
    let colors: [(u8, u8, u8); 3] = [(220, 70, 70), (70, 180, 90), (70, 110, 220)];
    colors
        .iter()
        .enumerate()
        .map(|(index, color)| {
            let name = format!("{}.png", index + 1);
            let bytes = make_png(320, 240, *color);
            (name, bytes)
        })
        .collect()
}

fn make_png(width: u32, height: u32, base: (u8, u8, u8)) -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, width, height);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().expect("PNG 头写入失败");
        let mut data = vec![0u8; (width * height * 3) as usize];
        for y in 0..height {
            for x in 0..width {
                let offset = ((y * width + x) * 3) as usize;
                let factor = (x as f32 / width as f32) * 0.4;
                data[offset] = (base.0 as f32 * (1.0 - factor) + 40.0 * factor) as u8;
                data[offset + 1] = (base.1 as f32 * (1.0 - factor) + 40.0 * factor) as u8;
                data[offset + 2] = (base.2 as f32 * (1.0 - factor) + 40.0 * factor) as u8;
            }
        }
        writer.write_image_data(&data).expect("PNG 像素写入失败");
    }
    out
}

fn html_response(body: &str) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(
        b"HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: ",
    );
    out.extend_from_slice(body.len().to_string().as_bytes());
    out.extend_from_slice(b"\r\nConnection: close\r\n\r\n");
    out.extend_from_slice(body.as_bytes());
    out
}

fn png_response(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: ");
    out.extend_from_slice(bytes.len().to_string().as_bytes());
    out.extend_from_slice(b"\r\nConnection: close\r\n\r\n");
    out.extend_from_slice(bytes);
    out
}

fn json_response(body: &str) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: ");
    out.extend_from_slice(body.len().to_string().as_bytes());
    out.extend_from_slice(b"\r\nConnection: close\r\n\r\n");
    out.extend_from_slice(body.as_bytes());
    out
}

fn not_found() -> Vec<u8> {
    b"HTTP/1.1 404 Not Found\r\nContent-Length: 9\r\nConnection: close\r\n\r\nnot found".to_vec()
}
