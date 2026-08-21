//! auth_token 契约测试（R7 X03）：生成/持久化/复用/恒定时间校验/ACL/引导端点。
//!
//! 独立编译目标：`auth_token.rs` 不引用 crate::/super::，本文件用 #[path] 挂载。
//! 存储全部落在 tempfile 临时目录。

#[path = "../src/auth_token.rs"]
mod auth_token;

use auth_token::AuthToken;
use std::path::Path;

/// 测试临时 data_root。
fn temp_root() -> (tempfile::TempDir, std::path::PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().to_path_buf();
    (temp, root)
}

#[test]
fn generates_random_long_tokens() {
    let a = AuthToken::generate();
    let b = AuthToken::generate();
    assert_ne!(a.token(), b.token());
    assert!(a.token().len() >= 64, "256 位 hex 至少 64 字符");
}

#[test]
fn persists_token_to_file_and_reloads() {
    let (_temp, root) = temp_root();
    let created = AuthToken::load_or_create(&root);
    let path = AuthToken::file_path(&root);
    assert!(path.is_file(), "token 文件应已创建");
    let on_disk = std::fs::read_to_string(&path).unwrap();
    assert_eq!(on_disk.trim(), created.token());

    let reloaded = AuthToken::load_or_create(&root);
    assert_eq!(reloaded.token(), created.token(), "重启应复用同一 token");
    assert!(reloaded.acl_warning().is_none());
}

#[test]
fn reuses_existing_valid_token_file() {
    let (_temp, root) = temp_root();
    let path = AuthToken::file_path(&root);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "pre-existing-32-char-token-abcdef123456").unwrap();
    let loaded = AuthToken::load_or_create(&root);
    assert_eq!(loaded.token(), "pre-existing-32-char-token-abcdef123456");
}

#[test]
fn overwrites_corrupt_or_empty_token_file() {
    let (_temp, root) = temp_root();
    let path = AuthToken::file_path(&root);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "   ").unwrap();
    let loaded = AuthToken::load_or_create(&root);
    assert!(loaded.token().len() >= 64, "空文件应重新生成");
}

#[test]
fn falls_back_to_memory_token_when_unwritable() {
    // data_root 指向一个“文件”而非目录 → 目录创建/写入必然失败 → 内存 token 兜底。
    let (_temp, root) = temp_root();
    let blocker = root.join("blocker");
    std::fs::write(&blocker, "file-not-dir").unwrap();
    let loaded = AuthToken::load_or_create(&blocker);
    assert!(loaded.token().len() >= 64, "降级也应返回可用 token");
    assert!(loaded.acl_warning().is_some(), "应携带降级警告");
}

#[test]
fn verify_accepts_exact_token_rejects_others() {
    let token = AuthToken::generate();
    assert!(token.verify(token.token()));
    assert!(!token.verify("wrong-token"));
    assert!(!token.verify(""));
    assert!(!token.verify(&token.token()[..token.token().len() - 1]));
}

#[test]
fn verify_header_parses_bearer_prefix() {
    let token = AuthToken::generate();
    let ok = axum::http::HeaderValue::from_str(&format!("Bearer {}", token.token())).unwrap();
    let bad = axum::http::HeaderValue::from_str("Bearer nope").unwrap();
    let plain = axum::http::HeaderValue::from_str(token.token()).unwrap();
    assert!(token.verify_header(Some(&ok)));
    assert!(!token.verify_header(Some(&bad)));
    assert!(!token.verify_header(Some(&plain)), "无 Bearer 前缀应拒绝");
    assert!(!token.verify_header(None));
}

#[test]
fn public_and_sse_path_classification() {
    assert!(auth_token::is_public_path("/health"));
    assert!(auth_token::is_public_path("/openapi.json"));
    assert!(auth_token::is_public_path("/auth/token"));
    assert!(!auth_token::is_public_path("/session"));
    assert!(auth_token::is_sse_path("/cloud/tasks/x/events"));
    assert!(auth_token::is_sse_path("/events/stream"));
    assert!(auth_token::is_sse_path("/workflow/run/r/events"));
    assert!(!auth_token::is_sse_path("/workflow/run/r"));
}

/// Windows：token 文件 ACL 应仅含当前用户（继承移除后无 BUILTIN\Users 授权）。
#[cfg(windows)]
#[test]
fn token_file_acl_is_user_only() {
    let (_temp, root) = temp_root();
    let created = AuthToken::load_or_create(&root);
    assert!(
        created.acl_warning().is_none(),
        "ACL 应用不应失败：{:?}",
        created.acl_warning()
    );
    let path = AuthToken::file_path(&root);
    let output = std::process::Command::new("icacls")
        .arg(&path)
        .output()
        .unwrap();
    assert!(output.status.success());
    let text = String::from_utf8_lossy(&output.stdout).to_lowercase();
    assert!(
        !text.contains("buil\\users"),
        "BUILTIN\\Users 不应有权限：{text}"
    );
    assert!(text.contains("(f)"), "当前用户应有完全控制：{text}");
}

/// ACL 函数本身：对临时文件执行收紧应成功。
#[cfg(windows)]
#[test]
fn apply_acl_on_arbitrary_file_succeeds() {
    let temp = tempfile::tempdir().unwrap();
    let file = temp.path().join("secret.txt");
    std::fs::write(&file, "s").unwrap();
    auth_token::apply_user_only_acl(Path::new(&file)).expect("icacls 收紧应成功");
}
