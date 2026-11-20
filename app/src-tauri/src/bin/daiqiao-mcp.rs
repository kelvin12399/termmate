//! 代敲的 stdio MCP 桥。
//!
//! 为什么需要它：Claude Code / Codex / Cursor 的标准 MCP 配置是
//! 「起一个进程，用 stdin/stdout 说 JSON-RPC」。代敲的 MCP server 是
//! HTTP 的（跑在 App 进程里），有些客户端不支持 HTTP 传输，或者配起来麻烦。
//!
//! 为什么是「桥」而不是独立实现：MCP 工具要操作**正在运行的那个代敲窗口**里的
//! 会话、面板、终端缓冲 —— 这些状态在 App 进程里，独立进程拿不到。
//! 所以这个进程只做一件事：把 stdin 的 JSON-RPC 原样 POST 给
//! 127.0.0.1:7391/mcp，再把回应原样写回 stdout。
//!
//! 约定：
//!   · 一行一条 JSON（MCP stdio 传输的标准：newline-delimited）
//!   · 日志只能走 stderr —— stdout 是协议通道，掺一个字节就坏
//!   · App 没开时给结构化的 JSON-RPC 错误，不是崩掉（客户端要看得懂）
//!
//! 用法（Claude Code 的 mcp 配置）：
//!   { "command": "/path/to/daiqiao-mcp" }
//! 端口非默认时用环境变量 DAIQIAO_MCP_PORT。

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::time::Duration;

const DEFAULT_PORT: u16 = 7391;

fn port() -> u16 {
    std::env::var("DAIQIAO_MCP_PORT")
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(DEFAULT_PORT)
}

/// 往 App 的 HTTP 端点转发一条 JSON-RPC。
///
/// 没用 reqwest：这是本机回环的一问一答，手写 HTTP 比拖一个
/// 异步 HTTP 栈进来更省事，也让这个桥的编译产物保持很小。
fn forward(body: &str, port: u16) -> Result<String, String> {
    let mut stream = TcpStream::connect(("127.0.0.1", port))
        .map_err(|e| format!("连不上代敲（127.0.0.1:{port}）: {e}"))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(120)))
        .map_err(|e| e.to_string())?;
    stream
        .set_write_timeout(Some(Duration::from_secs(30)))
        .map_err(|e| e.to_string())?;

    // Connection: close 让服务端答完就断，这样读到 EOF 就是读完了，
    // 不用解析 Content-Length 或处理 chunked。
    let req = format!(
        "POST /mcp HTTP/1.1\r\n\
         Host: 127.0.0.1:{port}\r\n\
         Content-Type: application/json\r\n\
         Accept: application/json\r\n\
         User-Agent: daiqiao-stdio-bridge\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         \r\n\
         {body}",
        body.len()
    );
    stream
        .write_all(req.as_bytes())
        .map_err(|e| format!("发送失败: {e}"))?;
    stream.flush().ok();

    let mut raw = Vec::new();
    stream
        .read_to_end(&mut raw)
        .map_err(|e| format!("读取回应失败: {e}"))?;
    let text = String::from_utf8_lossy(&raw);

    // 掐掉 HTTP 头，只要 body
    match text.find("\r\n\r\n") {
        Some(i) => Ok(text[i + 4..].to_string()),
        None => Err("回应不是合法的 HTTP".into()),
    }
}

/// App 没开时回一条客户端看得懂的错误，而不是让桥崩掉
fn offline_error(id: &serde_json::Value, detail: &str) -> String {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": -32603,
            "message": format!(
                "代敲没在运行，连不上它的 MCP server。\
                 先把代敲打开（它启动时会自动开 MCP），然后重试。\n详情: {detail}"
            )
        }
    })
    .to_string()
}

fn main() {
    let port = port();
    eprintln!("代敲 stdio 桥启动，转发到 127.0.0.1:{port}/mcp");

    let stdin = BufReader::new(std::io::stdin());
    let mut stdout = std::io::stdout();

    for line in stdin.lines() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                eprintln!("读 stdin 失败: {e}");
                break;
            }
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        // 通知（没有 id）不需要回应 —— 但仍要转发过去，
        // 否则 notifications/initialized 这类握手信号会丢。
        let parsed: serde_json::Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("stdin 里不是合法 JSON: {e}");
                let err = serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": serde_json::Value::Null,
                    "error": { "code": -32700, "message": format!("JSON 解析失败: {e}") }
                });
                let _ = writeln!(stdout, "{err}");
                let _ = stdout.flush();
                continue;
            }
        };
        let is_notification = parsed.get("id").is_none();

        match forward(trimmed, port) {
            Ok(resp) => {
                // 通知转发过去就完了，服务端不会回内容
                if is_notification {
                    continue;
                }
                let resp = resp.trim();
                if resp.is_empty() {
                    continue;
                }
                let _ = writeln!(stdout, "{resp}");
                let _ = stdout.flush();
            }
            Err(e) => {
                eprintln!("转发失败: {e}");
                if is_notification {
                    continue;
                }
                let id = parsed.get("id").cloned().unwrap_or(serde_json::Value::Null);
                let _ = writeln!(stdout, "{}", offline_error(&id, &e));
                let _ = stdout.flush();
            }
        }
    }
    eprintln!("stdin 关闭，桥退出");
}
