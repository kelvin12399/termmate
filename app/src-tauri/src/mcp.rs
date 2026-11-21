//! 内置 MCP server。
//!
//! 关键点：它跑在**同一个进程**里，`exec` 直接读内存中的终端 grid，
//! 不需要任何 IPC，也不需要把输出序列化成别的东西再解析一遍。
//!
//! 传输：streamable HTTP（JSON-RPC 2.0 over POST /mcp）。
//! AI 客户端（Claude Code / Cursor / 任何 MCP 客户端）直接连
//! http://127.0.0.1:7391/mcp 即可，零配置。

use crate::{activate_session, close_session, rename_session, split_pane, spawn_tab, AppState};
use crate::workspace::{Layout, Workspace};
use axum::{
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde_json::{json, Value};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager};

const PROTOCOL_VERSION: &str = "2025-06-18";
const SERVER_NAME: &str = "daiqiao";
const SERVER_VERSION: &str = "0.2.0";

/// MCP server 是否真的绑上了端口 —— 界面上的状态读这个，不靠猜
pub static LISTENING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// 对外暴露的工具数量（界面显示用）
pub fn tool_count() -> usize {
    tool_defs().len()
}

pub fn spawn(app: AppHandle, port: u16) {
    std::thread::spawn(move || {
        let rt = match tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(e) => {
                log::error!("创建 MCP runtime 失败: {e}");
                return;
            }
        };

        rt.block_on(async move {
            let router = Router::new()
                .route("/health", get(|| async { "daiqiao mcp ok" }))
                .route("/mcp", post(endpoint))
                .with_state(app);

            match tokio::net::TcpListener::bind(("127.0.0.1", port)).await {
                Ok(listener) => {
                    log::info!("MCP server 已监听 http://127.0.0.1:{port}/mcp");
                    LISTENING.store(true, std::sync::atomic::Ordering::Relaxed);
                    if let Err(e) = axum::serve(listener, router).await {
                        LISTENING.store(false, std::sync::atomic::Ordering::Relaxed);
                        log::error!("MCP server 退出: {e}");
                    }
                }
                Err(e) => {
                    LISTENING.store(false, std::sync::atomic::Ordering::Relaxed);
                    log::error!("MCP server 无法绑定 127.0.0.1:{port}: {e}");
                }
            }
        });
    });
}

async fn endpoint(
    axum::extract::State(app): axum::extract::State<AppHandle>,
    headers: axum::http::HeaderMap,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    // 审计要写「哪个客户端干的」。MCP 协议本身不在每次调用里带身份，
    // 能拿到的就是 HTTP User-Agent —— claude-code / codex / cursor 各不相同。
    let client = client_name(&headers);
    match dispatch(&app, body, &client).await {
        Some(v) => (StatusCode::OK, Json(v)).into_response(),
        // 通知类消息（无 id）不需要响应体
        None => StatusCode::ACCEPTED.into_response(),
    }
}

/// 从 User-Agent 里认出常见客户端；认不出就原样留短名，便于审计对账
fn client_name(headers: &axum::http::HeaderMap) -> String {
    let ua = headers
        .get(axum::http::header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .trim();
    if ua.is_empty() {
        return "未知客户端".into();
    }
    let low = ua.to_lowercase();
    for (needle, name) in [
        ("claude", "claude-code"),
        ("codex", "codex"),
        ("cursor", "cursor"),
        ("python", "python"),
        ("curl", "curl"),
        ("node", "node"),
    ] {
        if low.contains(needle) {
            return name.into();
        }
    }
    ua.chars().take(40).collect()
}

async fn dispatch(app: &AppHandle, body: Value, client: &str) -> Option<Value> {
    // 允许批量请求
    if let Some(arr) = body.as_array() {
        let mut out = Vec::new();
        for item in arr {
            if let Some(v) = Box::pin(dispatch(app, item.clone(), client)).await {
                out.push(v);
            }
        }
        return Some(Value::Array(out));
    }

    let id = body.get("id").cloned();
    let method = body.get("method").and_then(|m| m.as_str()).unwrap_or("");
    let params = body.get("params").cloned().unwrap_or(Value::Null);

    // 通知（没有 id）→ 不回应
    if id.is_none() {
        log::debug!("MCP 通知: {method}");
        return None;
    }
    let id = id.unwrap();

    let result = match method {
        "initialize" => Ok(json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": { "tools": { "listChanged": false } },
            "serverInfo": { "name": SERVER_NAME, "version": SERVER_VERSION }
        })),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tool_defs() })),
        "tools/call" => call_tool(app, &params, client).await,
        other => Err(format!("不支持的方法: {other}")),
    };

    Some(match result {
        Ok(r) => json!({ "jsonrpc": "2.0", "id": id, "result": r }),
        Err(msg) => json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": -32603, "message": msg }
        }),
    })
}

fn tool_defs() -> Vec<Value> {
    vec![
        json!({
            "name": "session_list",
            "description": "列出所有标签页和它们的分屏结构（会话 id、标题、哪个是当前焦点）。exec / send_input / read_screen 都用会话 id 指定目标，不指定则用当前焦点。",
            "inputSchema": { "type": "object", "properties": {} }
        }),
        json!({
            "name": "session_new",
            "description": "新开一个标签页（起一个新 shell）。返回新会话 id。",
            "inputSchema": {
                "type": "object",
                "properties": { "title": { "type": "string", "description": "标签页标题" } }
            }
        }),
        json!({
            "name": "session_close",
            "description": "关闭一个会话。它是所在标签页的最后一个 pane 时，整个标签页一起关掉。",
            "inputSchema": {
                "type": "object",
                "properties": { "session": { "type": "string", "description": "会话 id，省略则关当前焦点" } }
            }
        }),
        json!({
            "name": "session_rename",
            "description": "给标签页改标题。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "session": { "type": "string", "description": "会话 id，省略则用当前焦点" },
                    "title": { "type": "string" }
                },
                "required": ["title"]
            }
        }),
        json!({
            "name": "session_activate",
            "description": "把某个会话切到前台（人看到的画面会跟着切）。",
            "inputSchema": {
                "type": "object",
                "properties": { "session": { "type": "string" } },
                "required": ["session"]
            }
        }),
        json!({
            "name": "pane_split",
            "description": "在指定会话所在的 pane 上分屏，新 pane 里起一个新 shell。direction: horizontal=上下分，vertical=左右分。返回新会话 id。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "session": { "type": "string", "description": "在哪个会话上分屏，省略则用当前焦点" },
                    "direction": { "type": "string", "enum": ["horizontal", "vertical"] }
                },
                "required": ["direction"]
            }
        }),
        json!({
            "name": "exec",
            "description": "在终端会话里执行一条命令，等它跑完，读回完整输出和退出码。输出已去除 ANSI 控制符，是屏幕上真实呈现的纯文本。适合跑构建、测试、git 等命令。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "command": { "type": "string", "description": "要执行的 shell 命令" },
                    "session": { "type": "string", "description": "会话 id，省略则用当前焦点" },
                    "timeout_ms": { "type": "number", "description": "超时毫秒数，默认 30000" }
                },
                "required": ["command"]
            }
        }),
        json!({
            "name": "send_input",
            "description": "给会话里的交互式程序发送输入（比如 y/N 确认、Ctrl-C、方向键）。Ctrl-C 用 \\u0003，方向键上用 \\u001b[A。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "input": { "type": "string", "description": "要发送的原始文本（支持 \\uXXXX 转义）" },
                    "session": { "type": "string" }
                },
                "required": ["input"]
            }
        }),
        json!({
            "name": "read_screen",
            "description": "读取终端当前屏幕上显示的内容（纯文本），可选择带上滚动历史。用于看一个正在运行的交互式程序现在是什么状态。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "session": { "type": "string" },
                    "include_scrollback": { "type": "boolean", "description": "是否包含滚动历史，默认 false" },
                    "tail_lines": { "type": "number", "description": "只要最后 N 行" }
                }
            }
        }),
        json!({
            "name": "host_list",
            "description": "列出所有已配置的 SSH 主机（不包含密码等凭据）。",
            "inputSchema": { "type": "object", "properties": {} }
        }),
        json!({
            "name": "host_add",
            "description": "添加一台 SSH 主机。认证支持 key / password / agent。密码永远不进参数、不进配置：密码认证的主机建好后，人在代敲界面里把密码存进钥匙串（host_list 会显示存没存）；agent 认证不需要任何额外材料。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "给这台机器起的名（后面用它引用）" },
                    "hostname": { "type": "string", "description": "IP 或域名" },
                    "user": { "type": "string" },
                    "port": { "type": "number", "description": "默认 22" },
                    "auth": { "type": "string", "enum": ["key", "password", "agent"], "description": "默认 key" },
                    "key": { "type": "string", "description": "用哪把密钥：密钥库里的 id 或名字（先用 key_list 看有哪些）。auth=key 时用" },
                    "key_path": { "type": "string", "description": "【旧写法，建议改用 key】直接给私钥路径" },
                    "group": { "type": "string", "description": "分组名，界面上按它归类" },
                    "tags": { "type": "array", "items": { "type": "string" } },
                    "note": { "type": "string" }
                },
                "required": ["name", "hostname", "user"]
            }
        }),
        json!({
            "name": "host_update",
            "description": "修改一台已配置主机的字段（只传要改的）。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "host": { "type": "string", "description": "主机 id 或名字" },
                    "name": { "type": "string" },
                    "hostname": { "type": "string" },
                    "port": { "type": "number" },
                    "user": { "type": "string" },
                    "auth": { "type": "string" },
                    "key": { "type": "string", "description": "换成密钥库里的哪把密钥（id 或名字）" },
                    "key_path": { "type": "string", "description": "【旧写法】直接给私钥路径" },
                    "group": { "type": "string", "description": "分组名，空字符串表示移出分组" },
                    "tags": { "type": "array", "items": { "type": "string" } },
                    "note": { "type": "string" }
                },
                "required": ["host"]
            }
        }),
        json!({
            "name": "host_remove",
            "description": "删除一台已配置的 SSH 主机。",
            "inputSchema": {
                "type": "object",
                "properties": { "host": { "type": "string", "description": "主机 id 或名字" } },
                "required": ["host"]
            }
        }),
        json!({
            "name": "host_connect",
            "description": "连到一台已配置的主机，在新标签页里开一个 SSH 会话（之后就能像本地会话一样 exec / read_screen）。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "host": { "type": "string", "description": "主机 id 或名字" },
                    "cols": { "type": "number" },
                    "rows": { "type": "number" }
                },
                "required": ["host"]
            }
        }),
        json!({
            "name": "host_exec",
            "description": "按主机名执行一条任意 shell 命令并读回输出和退出码。自动复用同主机仍在运行的 SSH 会话，没有才新建；会话保留，返回 session id 供后续操作。适合 rsync、管道、循环和批量任务，不必拆成多个 SFTP 调用。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "host": { "type": "string", "description": "主机 id 或名字" },
                    "command": { "type": "string", "description": "要执行的完整 shell 命令" },
                    "timeout_ms": { "type": "integer", "description": "超时毫秒数，默认 30000" },
                    "reuse": { "type": "boolean", "description": "是否复用同主机会话，默认 true；false 则另开新标签页" }
                },
                "required": ["host", "command"]
            }
        }),
        json!({
            "name": "sftp_list",
            "description": "列出远端主机上某个目录的内容。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "host": { "type": "string" },
                    "path": { "type": "string", "description": "远端路径，默认当前目录" }
                },
                "required": ["host"]
            }
        }),
        json!({
            "name": "sftp_upload",
            "description": "把本机文件上传到远端主机。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "host": { "type": "string" },
                    "local_path": { "type": "string", "description": "本机文件路径" },
                    "remote_path": { "type": "string", "description": "远端目标路径" }
                },
                "required": ["host", "local_path", "remote_path"]
            }
        }),
        json!({
            "name": "sftp_download",
            "description": "把远端主机上的文件下载到本机。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "host": { "type": "string" },
                    "remote_path": { "type": "string" },
                    "local_path": { "type": "string", "description": "本机保存路径（目录会自动创建）" }
                },
                "required": ["host", "remote_path", "local_path"]
            }
        }),
        json!({
            "name": "sftp_remove",
            "description": "删除远端主机的文件或空目录。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "host": { "type": "string" },
                    "path": { "type": "string" }
                },
                "required": ["host", "path"]
            }
        }),
        json!({
            "name": "sftp_rename",
            "description": "在远端移动或重命名文件/目录。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "host": { "type": "string" },
                    "from": { "type": "string" },
                    "to": { "type": "string" }
                },
                "required": ["host", "from", "to"]
            }
        }),
        json!({
            "name": "sftp_mkdir",
            "description": "在远端创建目录。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "host": { "type": "string" },
                    "path": { "type": "string" }
                },
                "required": ["host", "path"]
            }
        }),
        json!({
            "name": "sftp_copy",
            "description": "复制远端文件（SFTP 没有 copy 原语，内部是读出来再写回去）。只支持文件，目录还没做。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "host": { "type": "string" },
                    "from": { "type": "string" },
                    "to": { "type": "string" }
                },
                "required": ["host", "from", "to"]
            }
        }),
        json!({
            "name": "key_list",
            "description": "列出密钥库里的所有 SSH 密钥：~/.ssh 里扫到的 + 代敲库里的。给出算法、指纹、有没有口令、口令是否已存进钥匙串、文件权限是否安全、被哪些主机引用。**拿不到私钥内容**。",
            "inputSchema": { "type": "object", "properties": {} }
        }),
        json!({
            "name": "key_import",
            "description": "把一把私钥导入代敲库（~/.daiqiao/keys/，目录 700、文件 600）。两种给法：path=磁盘上的私钥文件，或 text=私钥文本。带口令的私钥也能导入，但口令不经过 AI，连接前请在界面里存一次。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "要导入的私钥文件路径" },
                    "text": { "type": "string", "description": "私钥文本（-----BEGIN OPENSSH PRIVATE KEY----- 开头）" },
                    "name": { "type": "string", "description": "给这把密钥起的名" }
                }
            }
        }),
        json!({
            "name": "key_generate",
            "description": "生成一对新的 SSH 密钥，私钥存进代敲库，并返回公钥文本（可以直接贴到服务器的 authorized_keys）。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "给这把密钥起的名" },
                    "algo": { "type": "string", "enum": ["ed25519", "rsa"], "description": "默认 ed25519" },
                    "comment": { "type": "string", "description": "写进密钥的注释，默认 daiqiao" }
                }
            }
        }),
        json!({
            "name": "key_public",
            "description": "取一把密钥的公钥文本（用来贴到服务器 ~/.ssh/authorized_keys）。加密的私钥也能取，因为公钥本来就是明文部分。",
            "inputSchema": {
                "type": "object",
                "properties": { "key": { "type": "string", "description": "密钥 id 或名字" } },
                "required": ["key"]
            }
        }),
        json!({
            "name": "key_rename",
            "description": "改一把密钥在代敲里的显示名（磁盘上的文件名不动）。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "key": { "type": "string", "description": "密钥 id 或名字" },
                    "name": { "type": "string" }
                },
                "required": ["key", "name"]
            }
        }),
        json!({
            "name": "key_fix_permissions",
            "description": "把私钥文件权限改成 600。权限太松（比如 644）时 SSH 会直接拒绝用这把密钥。",
            "inputSchema": {
                "type": "object",
                "properties": { "key": { "type": "string", "description": "密钥 id 或名字" } },
                "required": ["key"]
            }
        }),
        json!({
            "name": "key_remove",
            "description": "从代敲库里删掉一把密钥（文件一起删）。~/.ssh 里的密钥代敲只读不碰，删不了。",
            "inputSchema": {
                "type": "object",
                "properties": { "key": { "type": "string", "description": "密钥 id 或名字" } },
                "required": ["key"]
            }
        }),
        json!({
            "name": "key_passphrase_forget",
            "description": "清掉一把密钥存在 macOS 钥匙串里的解锁口令（只清不读，口令本身永远不经过 AI）。带口令的私钥连接时要用；存口令请在界面里做。",
            "inputSchema": {
                "type": "object",
                "properties": { "key": { "type": "string", "description": "密钥 id 或名字" } },
                "required": ["key"]
            }
        }),
        json!({
            "name": "appearance_get",
            "description": "读终端外观设置：字号、字体、配色主题、行高、光标形状，并列出有哪些内置主题可选。",
            "inputSchema": { "type": "object", "properties": {} }
        }),
        json!({
            "name": "appearance_set",
            "description": "改终端外观。只传要改的字段，其余保持不动。改完立即生效（界面会重新排版、通知 PTY 新尺寸），不用重启。主题名要用 appearance_get 里列出的那几个。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "font_size": { "type": "integer", "description": "字号 px（9–28，越界会被夹回）" },
                    "font_family": { "type": "string", "description": "等宽字体名，空字符串 = 用内置回退链" },
                    "theme": { "type": "string", "description": "配色主题名" },
                    "line_height": { "type": "number", "description": "行高倍数 1.0–2.0" },
                    "cursor_style": { "type": "string", "description": "block / bar / underline" },
                    "language": { "type": "string", "description": "界面语言：zh-CN / zh-TW / en / ja / ko / de / fr / es" }
                }
            }
        }),
        json!({
            "name": "config_export",
            "description": "把全部主机、分组和密钥引用导出成 .dqconfig 文件，不包含私钥正文或主机密码。带凭据的加密导出请在代敲界面完成，口令不经过 AI。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "保存路径，建议以 .dqconfig 结尾" }
                },
                "required": ["path"]
            }
        }),
        json!({
            "name": "config_inspect",
            "description": "读取一个 .dqconfig 配置的元数据：主机数、分组数、密钥数、是否含加密凭据。不会解密或返回任何秘密。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "配置文件路径" }
                },
                "required": ["path"]
            }
        }),
        json!({
            "name": "config_import",
            "description": "导入 .dqconfig 配置，同名主机更新、其余新增，保留分组并 remap 密钥。加密包需要 passphrase；口令只用于本次解密，不写审计。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "配置文件路径" },
                    "passphrase": { "type": "string", "description": "加密包的口令（不记日志）" }
                },
                "required": ["path"]
            }
        }),
        json!({
            "name": "host_key_list",
            "description": "列出代敲记下的主机指纹（known_hosts）。主机密钥变更导致连不上时，用这个看记录里存的是什么。",
            "inputSchema": { "type": "object", "properties": {} }
        }),
        json!({
            "name": "host_key_forget",
            "description": "删掉某台主机的指纹记录，下次连接时重新记。★ 只有在人已经确认「这台机器确实是我自己重装/换系统了」的时候才能用。指纹变了也可能是连到了别的机器上，这种时候清记录等于自己把门打开——拿不准就先问主人。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "host": { "type": "string", "description": "主机名或 IP（跟主机配置里的地址一致）" },
                    "port": { "type": "integer", "description": "端口，默认 22" }
                },
                "required": ["host"]
            }
        }),
        json!({
            "name": "ssh_config_scan",
            "description": "读 ~/.ssh/config 里的主机条目（只读，不导入）。用来看这台机器上已经配好的 SSH 主机有哪些，再决定导哪几台进代敲。",
            "inputSchema": { "type": "object", "properties": {} }
        }),
        json!({
            "name": "ssh_config_import",
            "description": "把 ~/.ssh/config 里的主机导入代敲（先用 ssh_config_scan 看有哪些别名）。配了 IdentityFile 的走密钥认证，没配的归为 ssh-agent 认证。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "aliases": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "要导入的主机别名（ssh_config_scan 返回里的 alias）"
                    }
                },
                "required": ["aliases"]
            }
        }),
        json!({
            "name": "agent_status",
            "description": "看 ssh-agent / 1Password Agent 的状态：使用哪个 socket、在不在跑、里面加载了几把密钥。会按 ~/.ssh/config 的 IdentityAgent 解析，不只看系统默认 SSH_AUTH_SOCK。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "host": { "type": "string", "description": "主机 id、名字或地址；不传则看全局默认 agent" }
                }
            }
        }),
        json!({
            "name": "mcp_status",
            "description": "代敲自己的 MCP server 状态：监在哪个地址、开了多少个工具。给别的客户端报接入地址时用。",
            "inputSchema": { "type": "object", "properties": {} }
        }),
        json!({
            "name": "sftp_open_candidates",
            "description": "这个文件能用本机哪些应用打开（给 sftp_open_with 选 application 用）。只列真装了的。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "文件名（看后缀定候选）" }
                },
                "required": ["name"]
            }
        }),
        json!({
            "name": "sftp_open_with",
            "description": "用本机的系统应用打开远端文件：先下载到本地暂存区，用指定应用（或默认应用）打开，之后谁在本地改并保存，改动会自动回传到远端（30 分钟内）。注意：这会在主人的屏幕上弹出一个应用窗口。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "host": { "type": "string" },
                    "remote_path": { "type": "string", "description": "远端文件路径" },
                    "application": {
                        "type": "string",
                        "description": "应用名（如 Visual Studio Code / TextEdit），不传则用系统默认应用"
                    }
                },
                "required": ["host", "remote_path"]
            }
        }),
        json!({
            "name": "session_info",
            "description": "读一个会话的现场信息：当前目录、环境变量、这个会话里 AI 跑过的命令。本地会话直接查进程（不往屏幕上打命令，前台跑着程序也安全）；SSH 会话走远端 pwd/env。含凭据的环境变量只报变量名、值打码。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "session": { "type": "string", "description": "会话 id，不传用当前焦点会话" },
                    "include": {
                        "type": "array",
                        "items": { "type": "string", "enum": ["cwd", "env", "history"] },
                        "description": "要读哪几项，默认全读"
                    }
                }
            }
        }),
        json!({
            "name": "audit_list",
            "description": "读审计留痕：谁（AI 哪个客户端 / 人在界面上）、什么时候、在哪个会话、干了什么。按时间正序，最新在最后。凭据不进日志，所以这里看不到密码；send_input 只记字节数。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "limit": { "type": "integer", "description": "读最近多少条，默认 50" },
                    "actor": { "type": "string", "enum": ["ai", "human"], "description": "只看 AI 干的或只看人干的" },
                    "session": { "type": "string", "description": "只看某个会话上的记录" }
                }
            }
        }),
        json!({
            "name": "host_test_connect",
            "description": "试连一台主机并立刻断开，用来验证地址/用户/密钥对不对。不会新建标签页、不占会话。",
            "inputSchema": {
                "type": "object",
                "properties": { "host": { "type": "string", "description": "主机 id 或名字" } },
                "required": ["host"]
            }
        }),
    ]
}

fn tool_appearance_get() -> Result<String, String> {
    let a = crate::appearance::Appearance::load();
    let (bg, fg) = crate::appearance::theme_colors(&a.theme);
    let mut out = format!(
        "字号 {}px（可调 {}–{}）\n字体 {}\n主题 {}（背景 {bg} 前景 {fg}）\n行高 {:.2}\n光标 {}\n语言 {}\n\n内置主题：\n",
        a.font_size,
        crate::appearance::MIN_FONT,
        crate::appearance::MAX_FONT,
        if a.font_family.is_empty() {
            "（内置回退链：SF Mono / Menlo / PingFang SC）".to_string()
        } else {
            a.font_family.clone()
        },
        a.theme,
        a.line_height,
        a.cursor_style,
        a.language
    );
    for (name, b, f, desc) in crate::appearance::THEMES {
        out.push_str(&format!(
            "- {name}  {b}/{f}  {desc}{}\n",
            if *name == a.theme { "  ← 当前" } else { "" }
        ));
    }
    Ok(out.trim_end().to_string())
}

fn tool_appearance_set(app: &AppHandle, args: &Value) -> Result<String, String> {
    let mut a = crate::appearance::Appearance::load();
    let before = a.clone();
    let mut touched = Vec::new();

    if let Some(v) = args.get("font_size").and_then(|v| v.as_u64()) {
        a.font_size = v.min(255) as u8;
        touched.push("字号");
    }
    if let Some(v) = args.get("font_family").and_then(|v| v.as_str()) {
        a.font_family = v.to_string();
        touched.push("字体");
    }
    if let Some(v) = args.get("theme").and_then(|v| v.as_str()) {
        // 主题名写错会被 clamped() 静默退回默认 —— 对 AI 来说这是坏事：
        // 它以为改成功了，实际没。这里直接报错并把可选值列出来。
        if !crate::appearance::THEMES.iter().any(|(n, _, _, _)| n == &v) {
            let names: Vec<&str> = crate::appearance::THEMES
                .iter()
                .map(|(n, _, _, _)| *n)
                .collect();
            return Err(format!(
                "没有「{v}」这个主题。可选：{}",
                names.join("、")
            ));
        }
        a.theme = v.to_string();
        touched.push("主题");
    }
    if let Some(v) = args.get("line_height").and_then(|v| v.as_f64()) {
        a.line_height = v as f32;
        touched.push("行高");
    }
    if let Some(v) = args.get("cursor_style").and_then(|v| v.as_str()) {
        if !matches!(v, "block" | "bar" | "underline") {
            return Err(format!("光标形状只能是 block / bar / underline，收到「{v}」"));
        }
        a.cursor_style = v.to_string();
        touched.push("光标");
    }
    if let Some(v) = args.get("language").and_then(|v| v.as_str()) {
        if !crate::appearance::LANGUAGES.iter().any(|(code, _)| code == &v) {
            let names: Vec<&str> = crate::appearance::LANGUAGES
                .iter()
                .map(|(code, _)| *code)
                .collect();
            return Err(format!("不支持语言「{v}」。可选：{}", names.join("、")));
        }
        a.language = v.to_string();
        touched.push("语言");
    }

    if touched.is_empty() {
        return Err("没传任何要改的字段。可改：font_size / font_family / theme / line_height / cursor_style / language".into());
    }

    let a = a.clamped();
    a.save()?;
    let view = a.to_view();
    use tauri::Emitter;
    let _ = app.emit("appearance://changed", view);

    // 字号被夹过的话要说清——不能让 AI 以为改成了 200
    let clamp_note = if let Some(req) = args.get("font_size").and_then(|v| v.as_u64()) {
        if req as u8 != a.font_size {
            format!("（请求的 {req} 超出范围，已夹到 {}）", a.font_size)
        } else {
            String::new()
        }
    } else {
        String::new()
    };

    Ok(format!(
        "已改 {}{}。现在：字号 {}px·主题 {}·行高 {:.2}·光标 {}·语言 {}。{}",
        touched.join("、"),
        clamp_note,
        a.font_size,
        a.theme,
        a.line_height,
        a.cursor_style,
        a.language,
        if a == before { "（值没变）" } else { "界面已立即生效。" }
    ))
}

fn tool_config_export(args: &Value) -> Result<String, String> {
    let path = need(args, "path")?.to_string();
    // MCP 只开放无凭据导出：需要口令的加密包由人在界面里完成。
    crate::config_io::export_config(path, false, None)
}

fn tool_config_inspect(args: &Value) -> Result<String, String> {
    let path = need(args, "path")?.to_string();
    let v = crate::config_io::inspect_config(path)?;
    let hosts = v.get("hosts").and_then(|x| x.as_u64()).unwrap_or(0);
    let groups = v.get("groups").and_then(|x| x.as_u64()).unwrap_or(0);
    let keys = v.get("keys").and_then(|x| x.as_u64()).unwrap_or(0);
    let encrypted = v
        .get("includes_credentials")
        .and_then(|x| x.as_bool())
        .unwrap_or(false);
    Ok(format!(
        "主机 {hosts} 台 · 分组 {groups} 个 · 密钥 {keys} 把 · {}",
        if encrypted { "含加密凭据" } else { "不含凭据" }
    ))
}

fn tool_config_import(args: &Value) -> Result<String, String> {
    let path = need(args, "path")?.to_string();
    let passphrase = args
        .get("passphrase")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    crate::config_io::import_config(path, passphrase)
}

fn tool_host_key_list() -> Result<String, String> {
    let list = crate::known_hosts::list();
    if list.is_empty() {
        return Ok(format!(
            "代敲还没记下任何主机指纹（{}）。\n\
             首次连接一台主机时会自动记下。~/.ssh/known_hosts 里已有的记录也会被沉用，不用重复确认。",
            crate::known_hosts::daiqiao_known_hosts().display()
        ));
    }
    let mut out = String::new();
    for (host, fp) in &list {
        out.push_str(&format!("- {host}  {fp}\n"));
    }
    out.push_str(&format!("\n共 {} 条（{}）", list.len(), crate::known_hosts::daiqiao_known_hosts().display()));
    Ok(out)
}

fn tool_host_key_forget(args: &Value) -> Result<String, String> {
    let host = need(args, "host")?;
    let port = args.get("port").and_then(|v| v.as_u64()).unwrap_or(22) as u16;
    let n = crate::known_hosts::forget(host, port).map_err(|e| e.to_string())?;
    if n == 0 {
        return Ok(format!(
            "代敲的记录里没有 {host}:{port}。\n如果指纹冲突来自 ~/.ssh/known_hosts，要用 ssh-keygen -R {host} 自己清（代敲不改你的 ssh 配置）。"
        ));
    }
    crate::audit::human(
        "AI·清除主机指纹记录",
        Some(format!("{host}:{port} 删了 {n} 条")),
    );
    Ok(format!(
        "已删掉 {host}:{port} 的 {n} 条指纹记录，下次连接会重新记。"
    ))
}

fn tool_ssh_config_scan() -> Result<String, String> {
    let entries = crate::hosts::parse_ssh_config();
    if entries.is_empty() {
        // 空有两种原因，必须说清是哪种 —— 否则人会以为代敲读不到文件。
        // 最常见的是「Host 条目只写了别名、没写 HostName/User」，
        // 这种条目 ssh 命令自己能用（靠 Host 名当主机名），但代敲需要显式字段。
        let home = std::env::var("HOME").unwrap_or_default();
        let path = format!("{home}/.ssh/config");
        return Ok(match std::fs::read_to_string(&path) {
            Err(_) => format!("{path} 读不到（文件不存在或没权限）。"),
            Ok(text) => {
                let host_lines = text
                    .lines()
                    .filter(|l| l.trim().to_ascii_lowercase().starts_with("host "))
                    .count();
                if host_lines == 0 {
                    format!("{path} 里没有 Host 条目。")
                } else {
                    format!(
                        "{path} 里有 {host_lines} 条 Host，但都不含代敲需要的字段（HostName / User），\
                         所以没有可导入的。\n\
                         ssh 命令本身能用这种条目（拿 Host 名当主机名、用当前登录名），\
                         代敲要显式写明。用 host_add 直接加更快。"
                    )
                }
            }
        });
    }
    let known: std::collections::HashSet<String> = crate::hosts::HostStore::load()
        .hosts
        .iter()
        .map(|h| h.name.clone())
        .collect();
    let mut out = String::new();
    for e in &entries {
        out.push_str(&format!(
            "- {}  {}@{}:{}  {}{}\n",
            e.alias,
            e.user,
            e.hostname,
            e.port,
            if e.identity.is_some() { "有 IdentityFile（密钥认证）" } else { "无 IdentityFile（归 ssh-agent）" },
            if known.contains(&e.alias) { "  [已在代敲里]" } else { "" }
        ));
    }
    out.push_str(&format!(
        "\n共 {} 条。用 ssh_config_import 传 aliases 导入。",
        entries.len()
    ));
    Ok(out)
}

fn tool_ssh_config_import(app: &AppHandle, args: &Value) -> Result<String, String> {
    let aliases: Vec<String> = args
        .get("aliases")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    if aliases.is_empty() {
        return Err("缺 aliases（先用 ssh_config_scan 看有哪些别名）".into());
    }
    let n = crate::import_ssh_config_inner(app, aliases.clone())?;
    Ok(format!(
        "已导入 {n} 台主机：{}",
        aliases.join("、")
    ))
}

fn tool_agent_status(args: &Value) -> Result<String, String> {
    let hostname = args.get("host").and_then(|v| v.as_str()).map(|key| {
        crate::hosts::HostStore::load()
            .find(key)
            .map(|h| h.hostname.clone())
            .unwrap_or_else(|| key.to_string())
    });
    let v = crate::agent_status_inner(hostname.as_deref())?;
    let count = v.get("count").and_then(|c| c.as_u64()).unwrap_or(0);
    let socket = v.get("socket").and_then(|s| s.as_str()).unwrap_or("");
    let kind = if socket.contains("1password") || socket.contains("1Password") {
        "1Password Agent"
    } else {
        "ssh-agent"
    };
    if count > 0 {
        Ok(format!(
            "{kind} 在跑，里面加载了 {count} 把密钥。\nsocket: {socket}"
        ))
    } else {
        Ok(format!(
            "{kind} 在跑，但没加载任何密钥 —— agent 认证的主机现在连不上，要人先加载。\nsocket: {socket}"
        ))
    }
}

fn tool_mcp_status() -> Result<String, String> {
    Ok(format!(
        "MCP server {}\n地址: http://127.0.0.1:{}/mcp\n工具数: {}",
        if LISTENING.load(std::sync::atomic::Ordering::Relaxed) {
            "运行中"
        } else {
            "没在跑"
        },
        crate::MCP_PORT,
        tool_count()
    ))
}

fn tool_sftp_open_candidates(args: &Value) -> Result<String, String> {
    let name = need(args, "name")?;
    let list = crate::sftp::open_candidates(name);
    let mut out = String::new();
    for c in &list {
        let app_name = c.get("name").and_then(|v| v.as_str()).unwrap_or("");
        let arg = c.get("app").and_then(|v| v.as_str()).unwrap_or("");
        out.push_str(&format!(
            "- {app_name}{}\n",
            if arg.is_empty() { "（application 不传）".to_string() } else { format!("（application=\"{arg}\"）") }
        ));
    }
    Ok(out.trim_end().to_string())
}

async fn tool_sftp_open_with(app: &AppHandle, args: &Value) -> Result<String, String> {
    let host = need(args, "host")?.to_string();
    let remote = need(args, "remote_path")?.to_string();
    let application = args
        .get("application")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    crate::sftp_open_with_inner(app.clone(), host, remote, application).await
}

fn tool_audit_list(args: &Value) -> Result<String, String> {
    let limit = args
        .get("limit")
        .and_then(|v| v.as_u64())
        .unwrap_or(50)
        .clamp(1, 500) as usize;
    let actor = args.get("actor").and_then(|v| v.as_str());
    let session = args.get("session").and_then(|v| v.as_str());

    let rows = crate::audit::tail(limit, actor, session);
    if rows.is_empty() {
        return Ok("审计日志里还没有记录。".into());
    }
    let mut out = String::new();
    for e in &rows {
        let who = if e.actor == "ai" {
            format!("AI/{}", e.client)
        } else {
            "人/界面".to_string()
        };
        out.push_str(&format!("{}  {:<12}  {}", e.ts, who, e.action));
        if let Some(sid) = &e.session {
            out.push_str(&format!("  @{sid}"));
        }
        if let Some(d) = &e.detail {
            out.push_str(&format!("  {d}"));
        }
        if !e.ok {
            out.push_str("  [失败]");
        }
        if let Some(ms) = e.ms {
            out.push_str(&format!("  {ms}ms"));
        }
        out.push('\n');
    }
    out.push_str(&format!("\n共 {} 条（文件 {}）", rows.len(), crate::audit::path().display()));
    Ok(out)
}

/// 主机实际用了哪把密钥，列表里显示成人看得懂的样子
fn describe_host_key(h: &crate::hosts::Host) -> String {
    if let Some(id) = &h.key_id {
        return match crate::keys::find_key(id, &std::collections::HashMap::new()) {
            Some(k) => format!("  密钥=「{}」({})", k.name, k.algo),
            None => format!("  密钥={id}(已失效，密钥库里找不到)"),
        };
    }
    match &h.key_path {
        Some(p) => format!("  密钥路径={p}"),
        None => String::new(),
    }
}

/// host_add/host_update 的 `key` 参数 → 密钥库 id
fn resolve_key_arg(args: &Value) -> Result<Option<String>, String> {
    let Some(v) = args.get("key").and_then(|v| v.as_str()) else {
        return Ok(None);
    };
    if v.is_empty() {
        return Ok(None);
    }
    let info = crate::keys::find_key(v, &std::collections::HashMap::new())
        .ok_or_else(|| format!("密钥库里找不到「{v}」，先用 key_list 看有哪些"))?;
    Ok(Some(info.id))
}

/// 哪些主机在用哪把密钥：key_id -> 主机名列表
fn hosts_by_key() -> std::collections::HashMap<String, Vec<String>> {
    let mut map: std::collections::HashMap<String, Vec<String>> = std::collections::HashMap::new();
    for h in crate::hosts::HostStore::load().hosts {
        // 新字段直接用；旧的 key_path 换算成同一个 id，这样两种主机都能统计到
        let id = match (&h.key_id, &h.key_path) {
            (Some(id), _) => Some(id.clone()),
            (None, Some(p)) => Some(crate::keys::key_id(std::path::Path::new(p))),
            _ => None,
        };
        if let Some(id) = id {
            map.entry(id).or_default().push(h.name.clone());
        }
    }
    map
}

fn tool_key_list() -> Result<String, String> {
    let keys = crate::keys::list_keys(&hosts_by_key());
    if keys.is_empty() {
        return Ok("密钥库是空的。~/.ssh 里没扫到私钥，代敲库里也还没有。\n可以用 key_generate 生成一对，或 key_import 导入。".to_string());
    }
    let mut out = String::new();
    for k in &keys {
        let where_ = if k.source == "vault" { "代敲库" } else { "~/.ssh" };
        out.push_str(&format!(
            "- {}  「{}」  {}  {}  [{}]{}{}{}\n    {}\n",
            k.id,
            k.name,
            k.algo,
            k.fingerprint,
            where_,
            if k.encrypted {
                if k.passphrase_saved {
                    "  有口令·已存"
                } else {
                    "  有口令·未存"
                }
            } else {
                ""
            },
            if k.perm_ok { "" } else { "  ⚠权限太松(需 key_fix_permissions)" },
            if k.used_by.is_empty() {
                String::new()
            } else {
                format!("  被{}台主机用: {}", k.used_by.len(), k.used_by.join("、"))
            },
            k.path,
        ));
    }
    Ok(out.trim_end().to_string())
}

fn tool_key_import(app: &AppHandle, args: &Value) -> Result<String, String> {
    let name = args.get("name").and_then(|v| v.as_str());
    let info = if let Some(text) = args.get("text").and_then(|v| v.as_str()) {
        // MCP 这边没法当场向人要口令，走 raw 导入：带口令的也能入库，
        // 连接前去密钥库存口令（口令不经过 AI）
        crate::keys::import_text_raw(text, name).map_err(|e| e.to_string())?
    } else if let Some(path) = args.get("path").and_then(|v| v.as_str()) {
        crate::keys::import_file(std::path::Path::new(path), name).map_err(|e| e.to_string())?
    } else {
        return Err("要么给 path（私钥文件），要么给 text（私钥文本）".to_string());
    };
    crate::emit_keys(app);
    Ok(format!(
        "已导入「{}」 {} {}\n存到 {}（权限 600）{}",
        info.name,
        info.algo,
        info.fingerprint,
        info.path,
        if info.encrypted {
            "\n这把密钥有口令，连接时需要先解锁。AI 不能读也不能存口令；请在界面里存一次。"
        } else {
            ""
        }
    ))
}

fn tool_key_generate(app: &AppHandle, args: &Value) -> Result<String, String> {
    let name = args.get("name").and_then(|v| v.as_str());
    let algo = args.get("algo").and_then(|v| v.as_str()).unwrap_or("ed25519");
    let comment = args
        .get("comment")
        .and_then(|v| v.as_str())
        .unwrap_or("daiqiao");
    let info = crate::keys::generate(name, algo, comment).map_err(|e| e.to_string())?;
    crate::emit_keys(app);
    Ok(format!(
        "已生成「{}」 {} {}\n私钥: {}（权限 600）\n公钥（贴到服务器 ~/.ssh/authorized_keys）:\n{}",
        info.name,
        info.algo,
        info.fingerprint,
        info.path,
        info.public_key.unwrap_or_default()
    ))
}

fn tool_key_public(args: &Value) -> Result<String, String> {
    let key = args.get("key").and_then(|v| v.as_str()).ok_or("缺少 key")?;
    let info = crate::keys::find_key(key, &hosts_by_key())
        .ok_or_else(|| format!("密钥库里找不到「{key}」"))?;
    info.public_key
        .ok_or_else(|| "这把密钥读不出公钥".to_string())
}

fn tool_key_rename(app: &AppHandle, args: &Value) -> Result<String, String> {
    let key = args.get("key").and_then(|v| v.as_str()).ok_or("缺少 key")?;
    let name = args.get("name").and_then(|v| v.as_str()).ok_or("缺少 name")?;
    let info = crate::keys::find_key(key, &hosts_by_key())
        .ok_or_else(|| format!("密钥库里找不到「{key}」"))?;
    crate::keys::set_name(&info.id, name).map_err(|e| e.to_string())?;
    crate::emit_keys(app);
    Ok(format!("已把「{}」改名为「{}」", info.name, name))
}

fn tool_key_fix_permissions(app: &AppHandle, args: &Value) -> Result<String, String> {
    let key = args.get("key").and_then(|v| v.as_str()).ok_or("缺少 key")?;
    let used = hosts_by_key();
    let info = crate::keys::find_key(key, &used)
        .ok_or_else(|| format!("密钥库里找不到「{key}」"))?;
    crate::keys::fix_permissions(&info.id, &used).map_err(|e| e.to_string())?;
    crate::emit_keys(app);
    Ok(format!("已把「{}」的文件权限改成 600", info.name))
}

fn tool_key_passphrase_forget(app: &AppHandle, args: &Value) -> Result<String, String> {
    let key = args
        .get("key")
        .and_then(|v| v.as_str())
        .ok_or("缺少 key")?;
    let used = hosts_by_key();
    let info = crate::keys::find_key(key, &used)
        .ok_or_else(|| format!("密钥库里找不到「{key}」"))?;
    let was_saved = info.passphrase_saved;
    crate::keychain::delete_key_passphrase(&info.id).map_err(|e| e.to_string())?;
    crate::emit_keys(app);
    Ok(if was_saved {
        format!("已清掉「{}」存在钥匙串里的口令", info.name)
    } else {
        format!("「{}」本来就没存口令", info.name)
    })
}

fn tool_key_remove(app: &AppHandle, args: &Value) -> Result<String, String> {
    let key = args.get("key").and_then(|v| v.as_str()).ok_or("缺少 key")?;
    let used = hosts_by_key();
    let info = crate::keys::find_key(key, &used)
        .ok_or_else(|| format!("密钥库里找不到「{key}」"))?;
    if !info.used_by.is_empty() {
        return Err(format!(
            "还有 {} 台主机在用这把密钥（{}），先把它们改成别的密钥",
            info.used_by.len(),
            info.used_by.join("、")
        ));
    }
    crate::keys::remove_vault_key(&info.id, &used).map_err(|e| e.to_string())?;
    crate::emit_keys(app);
    Ok(format!("已从代敲库删除「{}」", info.name))
}

async fn tool_host_test_connect(args: &Value) -> Result<String, String> {
    let key = args.get("host").and_then(|v| v.as_str()).ok_or("缺少 host")?;
    let store = crate::hosts::HostStore::load();
    let host = store
        .find(key)
        .cloned()
        .ok_or_else(|| format!("找不到主机 {key}"))?;
    let t0 = std::time::Instant::now();
    match crate::ssh::connect(&host).await {
        Ok(_) => Ok(format!(
            "连通了：{}@{}:{} 认证通过，耗时 {:.2}s",
            host.user,
            host.hostname,
            host.port,
            t0.elapsed().as_secs_f32()
        )),
        Err(e) => Err(format!(
            "连不上 {}@{}:{} —— {e}",
            host.user, host.hostname, host.port
        )),
    }
}

async fn call_tool(app: &AppHandle, params: &Value, client: &str) -> Result<Value, String> {
    let name = params
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or("缺少工具名")?;
    let args = params.get("arguments").cloned().unwrap_or(json!({}));

    let started = Instant::now();
    let out = call_tool_body(app, name, &args).await;
    // 审计：读类工具不记（会把日志冲爆、也没有审计价值），有副作用的全记
    if !matches!(
        name,
        // 纯读类工具不记审计：会把日志冲爆，而且没有审计价值
        //（审计要回答的是「谁动了什么」，不是「谁看了什么」）。
        // 新增读类工具时记得加进来 —— 漏了就会刷日志。
        "session_list"
            | "host_list"
            | "key_list"
            | "read_screen"
            | "sftp_list"
            | "audit_list"
            | "key_public"
            | "session_info"
            | "ssh_config_scan"
            | "host_key_list"
            | "appearance_get"
            | "config_inspect"
            | "agent_status"
            | "mcp_status"
            | "sftp_open_candidates"
    ) {
        crate::audit::ai(
            client,
            name,
            audit_session(&args),
            audit_detail(name, &args),
            out.is_ok(),
            started.elapsed().as_millis() as u64,
        );
    }
    out
}

/// 审计里这条记在哪个会话上
fn audit_session(args: &Value) -> Option<String> {
    args.get("session")
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

/// 一句人能读的描述。凭据不进日志：send_input 只记长度。
fn audit_detail(name: &str, args: &Value) -> Option<String> {
    let s = |k: &str| args.get(k).and_then(|v| v.as_str()).unwrap_or("");
    match name {
        "exec" => Some(crate::audit::redact(s("command"))),
        "config_export" | "config_import" | "config_inspect" => {
            Some(format!("配置文件 {}", s("path")))
        }
        "host_exec" => Some(format!(
            "主机「{}」·{}",
            s("host"),
            crate::audit::redact(s("command"))
        )),
        // 这里可能正在回答 sudo 的密码提示 —— 只记长度，内容一个字都不留
        "send_input" => Some(format!(
            "发送输入 {} 字节",
            args.get("input").and_then(|v| v.as_str()).map(str::len).unwrap_or(0)
        )),
        "host_add" | "host_update" | "host_remove" | "host_connect" | "host_test_connect" => {
            let h = if s("host").is_empty() { s("name") } else { s("host") };
            Some(format!("主机「{h}」"))
        }
        "key_import"
            | "key_generate"
            | "key_remove"
            | "key_rename"
            | "key_fix_permissions"
            | "key_passphrase_forget" => {
            let k = if s("key").is_empty() { s("name") } else { s("key") };
            Some(format!("密钥「{k}」"))
        }
        // 参数名是 local_path/remote_path，写成 local/remote 会让审计里的路径全是空的
        "sftp_upload" => Some(format!(
            "{} → {}:{}",
            s("local_path"),
            s("host"),
            s("remote_path")
        )),
        "sftp_download" => Some(format!(
            "{}:{} → {}",
            s("host"),
            s("remote_path"),
            s("local_path")
        )),
        "sftp_remove" | "sftp_mkdir" => Some(format!("{}:{}", s("host"), s("path"))),
        "sftp_copy" => Some(format!("{}:{} → {}", s("host"), s("from"), s("to"))),
        "sftp_rename" => Some(format!("{}:{} → {}", s("host"), s("from"), s("to"))),
        "session_rename" => Some(format!("改名为「{}」", s("title"))),
        _ => None,
    }
}

async fn call_tool_body(app: &AppHandle, name: &str, args: &Value) -> Result<Value, String> {
    let args = args.clone();
    let text = match name {
        "session_list" => tool_session_list(app)?,
        "session_new" => tool_session_new(app, &args)?,
        "session_close" => tool_session_close(app, &args)?,
        "session_rename" => tool_session_rename(app, &args)?,
        "session_activate" => tool_session_activate(app, &args)?,
        "pane_split" => tool_pane_split(app, &args)?,
        "host_list" => tool_host_list()?,
        "host_add" => tool_host_add(app, &args)?,
        "host_update" => tool_host_update(app, &args)?,
        "host_remove" => tool_host_remove(app, &args)?,
        "host_connect" => tool_host_connect(app, &args)?,
        "host_exec" => tool_host_exec(app, &args).await?,
        "sftp_list" => tool_sftp_list(&args).await?,
        "sftp_upload" => tool_sftp_upload(&args).await?,
        "sftp_download" => tool_sftp_download(&args).await?,
        "sftp_remove" => tool_sftp_remove(&args).await?,
        "sftp_rename" => tool_sftp_rename(&args).await?,
        "sftp_mkdir" => tool_sftp_mkdir(&args).await?,
        "sftp_copy" => tool_sftp_copy(&args).await?,
        "exec" => tool_exec(app, &args).await?,
        "send_input" => tool_send_input(app, &args)?,
        "read_screen" => tool_read_screen(app, &args)?,
        "key_list" => tool_key_list()?,
        "key_import" => tool_key_import(app, &args)?,
        "key_generate" => tool_key_generate(app, &args)?,
        "key_public" => tool_key_public(&args)?,
        "key_rename" => tool_key_rename(app, &args)?,
        "key_fix_permissions" => tool_key_fix_permissions(app, &args)?,
        "key_remove" => tool_key_remove(app, &args)?,
        "key_passphrase_forget" => tool_key_passphrase_forget(app, &args)?,
        "host_test_connect" => tool_host_test_connect(&args).await?,
        "audit_list" => tool_audit_list(&args)?,
        "appearance_get" => tool_appearance_get()?,
        "appearance_set" => tool_appearance_set(app, &args)?,
        "config_export" => tool_config_export(&args)?,
        "config_inspect" => tool_config_inspect(&args)?,
        "config_import" => tool_config_import(&args)?,
        "host_key_list" => tool_host_key_list()?,
        "host_key_forget" => tool_host_key_forget(&args)?,
        "ssh_config_scan" => tool_ssh_config_scan()?,
        "ssh_config_import" => tool_ssh_config_import(app, &args)?,
        "agent_status" => tool_agent_status(&args)?,
        "mcp_status" => tool_mcp_status()?,
        "sftp_open_candidates" => tool_sftp_open_candidates(&args)?,
        "sftp_open_with" => tool_sftp_open_with(app, &args).await?,
        "session_info" => tool_session_info(app, &args).await?,
        other => return Err(format!("未知工具: {other}")),
    };

    Ok(json!({
        "content": [{ "type": "text", "text": text }],
        "isError": false
    }))
}

fn tool_session_list(app: &AppHandle) -> Result<String, String> {
    let state = app.state::<AppState>();
    let ws: Workspace = state
        .workspace
        .lock()
        .map(|w| w.clone())
        .map_err(|_| "读取布局失败")?;

    if ws.tabs.is_empty() {
        return Ok("当前没有标签页。".to_string());
    }

    let mut out = String::new();
    for (i, t) in ws.tabs.iter().enumerate() {
        out.push_str(&format!(
            "标签页 {}{}  {}\n",
            i + 1,
            if ws.active_tab.as_deref() == Some(t.id.as_str()) {
                "（当前）"
            } else {
                ""
            },
            t.title
        ));
        layout_tree(&t.layout, &ws, 1, &mut out);
    }
    Ok(out.trim_end().to_string())
}

fn layout_tree(l: &Layout, ws: &Workspace, depth: usize, out: &mut String) {
    let pad = "  ".repeat(depth);
    match l {
        Layout::Leaf { session } => {
            let focused = ws.focused.as_deref() == Some(session.as_str());
            out.push_str(&format!(
                "{pad}└─ {session}{}\n",
                if focused { "   ← 焦点" } else { "" }
            ));
        }
        Layout::Split { dir, a, b, .. } => {
            out.push_str(&format!("{pad}├─ 分屏（{}）\n", dir.as_str()));
            layout_tree(a, ws, depth + 1, out);
            layout_tree(b, ws, depth + 1, out);
        }
    }
}

fn tool_session_new(app: &AppHandle, args: &Value) -> Result<String, String> {
    let state = app.state::<AppState>();
    let title = args.get("title").and_then(|v| v.as_str()).map(String::from);
    // 新标签页沿用当前焦点会话的尺寸（前端随后会按真实布局纠正）
    let (cols, rows) = state
        .any_session()
        .map(|s| s.size())
        .unwrap_or((120, 34));
    let id = spawn_tab(app, &state, cols, rows, title.clone())?;
    Ok(format!(
        "已新建标签页 {}（会话 {id}）",
        title.unwrap_or_else(|| "本地终端".into())
    ))
}

fn tool_session_close(app: &AppHandle, args: &Value) -> Result<String, String> {
    let state = app.state::<AppState>();
    let id = match args.get("session").and_then(|v| v.as_str()) {
        Some(s) => s.to_string(),
        None => state
            .any_session()
            .map(|s| s.id.clone())
            .ok_or("没有可关闭的会话")?,
    };
    close_session(app, &state, &id)?;
    Ok(format!("已关闭 {id}"))
}

fn tool_session_rename(app: &AppHandle, args: &Value) -> Result<String, String> {
    let state = app.state::<AppState>();
    let title = args
        .get("title")
        .and_then(|v| v.as_str())
        .ok_or("缺少 title")?;
    let id = match args.get("session").and_then(|v| v.as_str()) {
        Some(s) => s.to_string(),
        None => state
            .any_session()
            .map(|s| s.id.clone())
            .ok_or("没有可改名的会话")?,
    };
    rename_session(app, &state, &id, title)?;
    Ok(format!("已把 {id} 改名为「{title}」"))
}

fn tool_session_activate(app: &AppHandle, args: &Value) -> Result<String, String> {
    let state = app.state::<AppState>();
    let id = args
        .get("session")
        .and_then(|v| v.as_str())
        .ok_or("缺少 session")?;
    activate_session(app, &state, id)?;
    Ok(format!("已切到 {id}"))
}

fn tool_pane_split(app: &AppHandle, args: &Value) -> Result<String, String> {
    let state = app.state::<AppState>();
    let dir_raw = args
        .get("direction")
        .and_then(|v| v.as_str())
        .ok_or("缺少 direction")?;
    let dir = crate::workspace::SplitDir::parse(dir_raw)
        .ok_or("direction 只能是 horizontal 或 vertical")?;

    let target = match args.get("session").and_then(|v| v.as_str()) {
        Some(s) => s.to_string(),
        None => state
            .any_session()
            .map(|s| s.id.clone())
            .ok_or("没有可分屏的会话")?,
    };
    let (cols, rows) = state
        .resolve(Some(&target))
        .ok()
        .map(|s| s.size())
        .unwrap_or((60, 17));

    let new_id = split_pane(app, &state, &target, dir, cols / 2, rows)?;
    Ok(format!(
        "已在 {target} 上{}分屏，新会话 {new_id}",
        if dir == crate::workspace::SplitDir::Horizontal {
            "上下"
        } else {
            "左右"
        }
    ))
}

fn tool_send_input(app: &AppHandle, args: &Value) -> Result<String, String> {
    let state = app.state::<AppState>();
    let session = state.resolve(args.get("session").and_then(|v| v.as_str()))?;
    let input = args
        .get("input")
        .and_then(|v| v.as_str())
        .ok_or("缺少 input")?;
    session.write(input.as_bytes()).map_err(|e| e.to_string())?;
    Ok(format!("已向会话 {} 发送 {} 字节", session.id, input.len()))
}

fn tool_read_screen(app: &AppHandle, args: &Value) -> Result<String, String> {
    let state = app.state::<AppState>();
    let session = state.resolve(args.get("session").and_then(|v| v.as_str()))?;
    let snap = session.snapshot().ok_or("读取终端状态失败")?;

    let include_sb = args
        .get("include_scrollback")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let mut lines: Vec<String> = Vec::new();
    if include_sb {
        lines.extend(snap.scrollback.iter().cloned());
    }
    lines.extend(snap.visible.iter().cloned());

    // 屏幕下方通常是大片留白，先去掉尾部空行，否则 tail_lines 只会拿到空白
    while lines.last().map(|l| l.trim().is_empty()).unwrap_or(false) {
        lines.pop();
    }

    if let Some(n) = args.get("tail_lines").and_then(|v| v.as_u64()) {
        let n = n as usize;
        if lines.len() > n {
            lines = lines.split_off(lines.len() - n);
        }
    }

    if lines.is_empty() {
        return Ok("(屏幕上没有内容)".to_string());
    }

    Ok(lines.join("\n"))
}

/// `env` 的输出进 AI 上下文有真实风险：AWS_SECRET_ACCESS_KEY、GITHUB_TOKEN 这类
/// 变量经常就摆在环境变量里。只报「这个变量存在」，值一律打码。
/// 密钥/口令类变量名单跟 audit.rs::redact 保持一致。
fn is_sensitive_env_name(name: &str) -> bool {
    let up = name.to_uppercase();
    // 对抗验证抛回来的漏网写法都补进来了：
    // MYSQL_PWD（只含 PWD）、DB_PASS（只含 PASS，不是 PASSWD）、
    // OPENAI_KEY（只含 KEY，不是 API_KEY）、GH_PAT（PAT 本来不在名单）。
    // 宁可误打一些无关变量，也不能漏真密钥。
    [
        "PASSWORD", "PASSWD", "PASS", "PWD", "TOKEN", "SECRET", "KEY", "CREDENTIAL",
        "AUTH", "COOKIE", "SESSION", "PAT", "SIGNATURE", "PRIVATE", "LICENSE",
    ]
    .iter()
    .any(|k| up.contains(k))
}

async fn tool_session_info(app: &AppHandle, args: &Value) -> Result<String, String> {
    let state = app.state::<AppState>();
    let session = state.resolve(args.get("session").and_then(|v| v.as_str()))?;
    let want: Vec<&str> = args
        .get("include")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str()).collect())
        .filter(|v: &Vec<&str>| !v.is_empty())
        .unwrap_or_else(|| vec!["cwd", "env", "history"]);

    let pid = session.pid();
    let mut out = String::new();

    if want.contains(&"cwd") {
        out.push_str("── 当前目录 ──
");
        match pid {
            Some(p) => match local_cwd(p) {
                Ok(dir) => out.push_str(&format!("{dir}
")),
                Err(e) => out.push_str(&format!("读不到：{e}
")),
            },
            None => {
                // SSH 会话没有本地进程可查，只能问远端
                match exec_body(&json!({ "command": "pwd", "timeout_ms": 10_000 }), &session).await {
                    Ok(dir) => out.push_str(&format!("{}
", first_line(&dir))),
                    Err(e) => out.push_str(&format!("读不到：{e}
")),
                }
            }
        }
        out.push('\n');
    }

    if want.contains(&"env") {
        out.push_str("── 环境变量（含凭据的值已打码；只看存不存在）──
");
        // 这里必须问 shell 自己：`ps eww` 只有进程 exec 那一刻的环境快照，
        // shell 起来之后 export 的变量它永远看不到（实测 MY_API_KEY 读不出来）。
        //
        // 关键：**打码必须发生在 shell 侧**。裸跑 `env` 会把明文打到屏幕上，
        // 紧跟一个 read_screen 就能原样读走 —— 对抗验证抓到的就是这条，
        // 下面这层 Rust 打码只是第二道防线，挡不住屏幕缓冲区。
        // 所以用 awk 在值离开 shell 之前就替换掉，明文根本不上屏。
        let masker = concat!(
            "env | awk -F= '{k=toupper($1); ",
            "if (k ~ /PASS|PWD|TOKEN|SECRET|KEY|CREDENTIAL|AUTH|COOKIE|SESSION|PAT|SIGNATURE|PRIVATE|LICENSE/) ",
            "print $1 \"=***\"; else print $0}'"
        );
        let raw = exec_body(
            &json!({ "command": masker, "timeout_ms": 10_000 }),
            &session,
        )
        .await;
        match raw {
            Ok(text) => {
                let mut lines: Vec<String> = text
                    .lines()
                    .filter_map(|l| {
                        let (k, v) = l.split_once('=')?;
                        if k.is_empty() {
                            return None;
                        }
                        Some(if is_sensitive_env_name(k) {
                            format!("{k}=***")
                        } else {
                            format!("{k}={v}")
                        })
                    })
                    .collect();
                lines.sort();
                out.push_str(&lines.join("
"));
                out.push('\n');
            }
            Err(e) => out.push_str(&format!("读不到：{e}
")),
        }
        out.push('\n');
    }

    if want.contains(&"history") {
        out.push_str(
            "── 这个会话里 AI 跑过的命令（来自审计日志）──\n\
             注意：人自己在终端里敲的命令代敲读不到（没有往 shell 里注入 history 钩子），\n\
             要看人敲了什么用 read_screen 翻屏幕。\n",
        );
        let rows = crate::audit::tail(50, Some("ai"), Some(&session.id));
        let cmds: Vec<&crate::audit::Entry> =
            rows.iter().filter(|e| e.action == "exec").collect();
        if cmds.is_empty() {
            out.push_str("（这个会话里 AI 还没跑过命令）\n");
        } else {
            for e in cmds {
                let ok = if e.ok { "" } else { "  [失败]" };
                out.push_str(&format!(
                    "{}  {}{}
",
                    e.ts,
                    e.detail.as_deref().unwrap_or(""),
                    ok
                ));
            }
        }
    }

    Ok(out.trim_end().to_string())
}

/// 本地进程的当前目录。macOS 上 lsof 直接给，比在 shell 里跑 pwd 干净
/// （不用管前台是不是有程序在跑，也不往屏幕上留痕迹）。
fn local_cwd(pid: u32) -> Result<String, String> {
    let out = std::process::Command::new("lsof")
        .args(["-a", "-p", &pid.to_string(), "-d", "cwd", "-Fn"])
        .output()
        .map_err(|e| format!("lsof 跑不起来: {e}"))?;
    if !out.status.success() {
        return Err("进程可能已经退出".into());
    }
    // lsof -Fn 的输出里，路径那行以 'n' 开头
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .find_map(|l| l.strip_prefix('n'))
        .map(str::to_string)
        .ok_or_else(|| "lsof 没给出路径".into())
}

/// exec 的返回尾部会带 `[退出码 N]`，取第一行就是干净结果
fn first_line(s: &str) -> &str {
    s.lines().next().unwrap_or("").trim()
}


// ─────────────────────── SSH 主机管理 ───────────────────────
// 红线：hosts.json 里只存主机信息和密钥路径，**不存密码**；
// 这些工具的返回也从不回显任何凭据。

fn tool_host_list() -> Result<String, String> {
    let store = crate::hosts::HostStore::load();
    if store.hosts.is_empty() {
        return Ok("还没有配置任何 SSH 主机，用 host_add 添加。".to_string());
    }
    let mut out = String::new();
    for h in &store.hosts {
        // 密码认证的主机：AI 永远拿不到密码，但必须知道「存没存」——
        // 没存就得提醒人在界面里存，否则连接必败
        let auth_extra = if h.auth == "password" {
            if crate::keychain::saved(&h.id) {
                "  密码=已存钥匙串".to_string()
            } else {
                "  密码=没存！要在代敲界面里存，密码不经过你".to_string()
            }
        } else {
            String::new()
        };
        out.push_str(&format!(
            "- {}  「{}」  {}@{}:{}  认证={}{}{}{}{}\n",
            h.id,
            h.name,
            h.user,
            h.hostname,
            h.port,
            h.auth,
            describe_host_key(h),
            auth_extra,
            h.group
                .as_ref()
                .map(|g| format!("  分组={g}"))
                .unwrap_or_default(),
            h.note
                .as_ref()
                .map(|n| format!("  备注={n}"))
                .unwrap_or_default()
        ));
    }
    Ok(out.trim_end().to_string())
}

fn tool_host_add(app: &AppHandle, args: &Value) -> Result<String, String> {
    let name = args
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or("缺少 name")?;
    let hostname = args
        .get("hostname")
        .and_then(|v| v.as_str())
        .ok_or("缺少 hostname")?;
    let user = args
        .get("user")
        .and_then(|v| v.as_str())
        .ok_or("缺少 user")?;

    let mut store = crate::hosts::HostStore::load();
    if store.hosts.iter().any(|h| h.name == name) {
        return Err(format!("已经有一台叫「{name}」的主机了"));
    }

    let id = store.next_id();
    store.hosts.push(crate::hosts::Host {
        id: id.clone(),
        name: name.to_string(),
        hostname: hostname.to_string(),
        port: args.get("port").and_then(|v| v.as_u64()).unwrap_or(22) as u16,
        user: user.to_string(),
        auth: args
            .get("auth")
            .and_then(|v| v.as_str())
            .unwrap_or("key")
            .to_string(),
        key_id: resolve_key_arg(args)?,
        key_path: args
            .get("key_path")
            .and_then(|v| v.as_str())
            .map(String::from),
        tags: args
            .get("tags")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default(),
        group: args.get("group").and_then(|v| v.as_str()).map(String::from),
        note: args.get("note").and_then(|v| v.as_str()).map(String::from),
    });
    store.save().map_err(|e| e.to_string())?;
    crate::emit_hosts(app);
    Ok(format!("已添加主机 {id}「{name}」 → {user}@{hostname}"))
}

fn tool_host_update(app: &AppHandle, args: &Value) -> Result<String, String> {
    let key = args
        .get("host")
        .and_then(|v| v.as_str())
        .ok_or("缺少 host（id 或名字）")?;

    let mut store = crate::hosts::HostStore::load();
    let idx = store
        .hosts
        .iter()
        .position(|h| h.id == key || h.name == key)
        .ok_or_else(|| format!("找不到主机 {key}"))?;

    let mut changed: Vec<&str> = Vec::new();
    {
        let h = &mut store.hosts[idx];
        if let Some(v) = args.get("name").and_then(|v| v.as_str()) {
            h.name = v.to_string();
            changed.push("name");
        }
        if let Some(v) = args.get("hostname").and_then(|v| v.as_str()) {
            h.hostname = v.to_string();
            changed.push("hostname");
        }
        if let Some(v) = args.get("port").and_then(|v| v.as_u64()) {
            h.port = v as u16;
            changed.push("port");
        }
        if let Some(v) = args.get("user").and_then(|v| v.as_str()) {
            h.user = v.to_string();
            changed.push("user");
        }
        if let Some(v) = args.get("auth").and_then(|v| v.as_str()) {
            h.auth = v.to_string();
            changed.push("auth");
        }
        if let Some(v) = args.get("key").and_then(|v| v.as_str()) {
            let info = crate::keys::find_key(v, &std::collections::HashMap::new())
                .ok_or_else(|| format!("密钥库里找不到「{v}」，先用 key_list 看有哪些"))?;
            h.key_id = Some(info.id);
            h.key_path = None;
            changed.push("key");
        }
        if let Some(v) = args.get("key_path").and_then(|v| v.as_str()) {
            h.key_path = Some(v.to_string());
            h.key_id = None;
            changed.push("key_path");
        }
        if let Some(v) = args.get("note").and_then(|v| v.as_str()) {
            h.note = Some(v.to_string());
            changed.push("note");
        }
        if let Some(arr) = args.get("tags").and_then(|v| v.as_array()) {
            h.tags = arr
                .iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect();
            changed.push("tags");
        }
        if let Some(v) = args.get("group").and_then(|v| v.as_str()) {
            h.group = if v.is_empty() { None } else { Some(v.to_string()) };
            changed.push("group");
        }
    }

    if changed.is_empty() {
        return Err("没有给出任何要修改的字段".to_string());
    }
    store.save().map_err(|e| e.to_string())?;
    crate::emit_hosts(app);
    Ok(format!("已更新主机 {key}：{}", changed.join("、")))
}

fn tool_host_remove(app: &AppHandle, args: &Value) -> Result<String, String> {
    let key = args
        .get("host")
        .and_then(|v| v.as_str())
        .ok_or("缺少 host")?;

    let mut store = crate::hosts::HostStore::load();
    let before = store.hosts.len();
    store.hosts.retain(|h| h.id != key && h.name != key);
    if store.hosts.len() == before {
        return Err(format!("找不到主机 {key}"));
    }
    store.save().map_err(|e| e.to_string())?;
    crate::emit_hosts(app);
    Ok(format!("已删除主机 {key}"))
}

fn tool_host_connect(app: &AppHandle, args: &Value) -> Result<String, String> {
    let key = args
        .get("host")
        .and_then(|v| v.as_str())
        .ok_or("缺少 host")?;

    let store = crate::hosts::HostStore::load();
    let host = store
        .find(key)
        .ok_or_else(|| format!("找不到主机 {key}"))?
        .clone();

    let state = app.state::<AppState>();
    let (dc, dr) = state.any_session().map(|s| s.size()).unwrap_or((120, 34));
    let cols = args.get("cols").and_then(|v| v.as_u64()).unwrap_or(dc as u64) as u16;
    let rows = args.get("rows").and_then(|v| v.as_u64()).unwrap_or(dr as u64) as u16;

    let title = host.name.clone();
    let id = crate::spawn_ssh_tab(app, &state, cols, rows, host)?;
    Ok(format!("已连上「{title}」，新标签页会话 {id}。现在可以对它用 exec / read_screen。"))
}

/// 远端 shell 里的命令包装器（POSIX sh 语法，bash/zsh/dash/ksh 都能用）。
/// 输出先落远端临时文件再 cat，起止信号走 OSC 标题 —— 屏幕上看不见任何标记。
const SSH_WRAPPER: &str = r#"__dq(){ __dq_t="$1"; { eval "$2"; } >"${TMPDIR:-/tmp}/.dq.$__dq_t" 2>&1; __dq_r=$?; printf '\033]0;__DQB_%s\007' "$__dq_t"; cat "${TMPDIR:-/tmp}/.dq.$__dq_t" 2>/dev/null; rm -f "${TMPDIR:-/tmp}/.dq.$__dq_t"; printf '\033]0;__DQD_%s_%d\007' "$__dq_t" "$__dq_r"; return $__dq_r; }"#;

/// SSH 会话的 exec。
///
/// 第一次用时把 `__dq` 送进远端 shell —— 送的那一刻起到远端回报「定义好了」为止，
/// 远端回来的字节（回显那一长串定义）都不进屏幕。之后每次 exec 屏幕上只看到
/// `__dq <token> '命令'` 和输出，和本地会话一致。输出从原始字节流里按不可见标记切出。
async fn exec_ssh(
    session: &std::sync::Arc<crate::term::Session>,
    token: &str,
    command: &str,
    timeout_ms: u64,
) -> Result<String, String> {
    use crate::term::{EXEC_FAILED, EXEC_INJECTING, EXEC_NONE, EXEC_READY};

    if session.exec_state() == EXEC_NONE {
        let it = unique_token();
        let line = format!(
            "\u{15} {SSH_WRAPPER}; printf '\\033]0;__DQI_%s\\007' {it}\n"
        );
        session
            .begin_inject(&it, line.as_bytes(), Duration::from_millis(4000))
            .map_err(|e| e.to_string())?;
    }
    let ready_deadline = Instant::now() + Duration::from_millis(5000);
    loop {
        match session.exec_state() {
            EXEC_READY => break,
            EXEC_FAILED => {
                session.reset_exec_state();
                return Err("远端 shell 不支持 exec 包装器（不是 bash/zsh/sh 一类，例如 fish 或设备 CLI）。请改用 send_input + read_screen。".to_string());
            }
            EXEC_INJECTING | _ => {}
        }
        if !session.is_alive() {
            return Ok("[SSH 连接已断开]".to_string());
        }
        if Instant::now() >= ready_deadline {
            session.reset_exec_state();
            return Err("远端 shell 没有响应（可能正在运行别的前台程序）。".to_string());
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
        session.poll_tap();
    }

    session.start_capture();
    let script = format!("\u{15}__dq {token} {}\n", shell_quote(command));
    session.write(script.as_bytes()).map_err(|e| e.to_string())?;

    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    loop {
        tokio::time::sleep(Duration::from_millis(25)).await;
        if let Some(code) = session.take_done(token) {
            let raw = session.take_capture().unwrap_or_default();
            let begin = format!("\x1b]0;__DQB_{token}\x07").into_bytes();
            let end = format!("\x1b]0;__DQD_{token}_").into_bytes();
            let body: &[u8] = match crate::term::find_bytes(&raw, &begin) {
                Some(b) => {
                    let rest = &raw[b + begin.len()..];
                    match crate::term::find_bytes(rest, &end) {
                        Some(e) => &rest[..e],
                        None => rest,
                    }
                }
                None => &[],
            };
            let output = strip_ansi(&String::from_utf8_lossy(body))
                .trim_matches('\n')
                .to_string();
            let mut text = if output.is_empty() { "(无输出)".to_string() } else { output };
            text.push_str(&format!("\n[退出码 {code}]"));
            return Ok(text);
        }
        if !session.is_alive() {
            let _ = session.take_capture();
            return Ok("[SSH 连接已断开]".to_string());
        }
        if Instant::now() >= deadline {
            let _ = session.take_capture();
            return Err(format!(
                "命令在 {timeout_ms} ms 内没有结束（SSH 会话）。需要交互输入的话用 send_input，看进度用 read_screen。"
            ));
        }
    }
}

// ─────────────────────── SFTP 文件操作 ───────────────────────

fn remote_host(args: &Value) -> Result<crate::hosts::Host, String> {
    let key = args
        .get("host")
        .and_then(|v| v.as_str())
        .ok_or("缺少 host")?;
    let store = crate::hosts::HostStore::load();
    store
        .find(key)
        .cloned()
        .ok_or_else(|| format!("找不到主机 {key}"))
}

fn need<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| format!("缺少 {key}"))
}

async fn tool_sftp_list(args: &Value) -> Result<String, String> {
    let host = remote_host(args)?;
    let path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
    let entries = crate::sftp::list(&host, path)
        .await
        .map_err(|e| format!("列目录失败: {e}"))?;

    if entries.is_empty() {
        return Ok(format!("{path} 是空的"));
    }
    let mut out = String::new();
    for e in entries {
        out.push_str(&format!(
            "{} {:>9}  {}\n",
            if e.is_dir { "d" } else { "-" },
            if e.is_dir {
                "-".to_string()
            } else {
                crate::sftp::human_size(e.size)
            },
            e.name
        ));
    }
    Ok(out.trim_end().to_string())
}

async fn tool_sftp_upload(args: &Value) -> Result<String, String> {
    let host = remote_host(args)?;
    let local = need(args, "local_path")?;
    let remote = need(args, "remote_path")?;
    let n = crate::sftp::upload(&host, local, remote)
        .await
        .map_err(|e| format!("上传失败: {e}"))?;
    Ok(format!(
        "已上传 {local} → {remote}（{}）",
        crate::sftp::human_size(n)
    ))
}

async fn tool_sftp_download(args: &Value) -> Result<String, String> {
    let host = remote_host(args)?;
    let remote = need(args, "remote_path")?;
    let local = need(args, "local_path")?;
    let n = crate::sftp::download(&host, remote, local)
        .await
        .map_err(|e| format!("下载失败: {e}"))?;
    Ok(format!(
        "已下载 {remote} → {local}（{}）",
        crate::sftp::human_size(n)
    ))
}

async fn tool_sftp_remove(args: &Value) -> Result<String, String> {
    let host = remote_host(args)?;
    let path = need(args, "path")?;
    crate::sftp::remove(&host, path)
        .await
        .map_err(|e| format!("删除失败: {e}"))
}

async fn tool_sftp_rename(args: &Value) -> Result<String, String> {
    let host = remote_host(args)?;
    let from = need(args, "from")?;
    let to = need(args, "to")?;
    crate::sftp::rename(&host, from, to)
        .await
        .map_err(|e| format!("重命名失败: {e}"))
}

async fn tool_sftp_mkdir(args: &Value) -> Result<String, String> {
    let host = remote_host(args)?;
    let path = need(args, "path")?;
    crate::sftp::mkdir(&host, path)
        .await
        .map_err(|e| format!("建目录失败: {e}"))
}

async fn tool_sftp_copy(args: &Value) -> Result<String, String> {
    let host = remote_host(args)?;
    let from = need(args, "from")?;
    let to = need(args, "to")?;
    crate::sftp::copy_remote(&host, from, to)
        .await
        .map_err(|e| format!("复制失败: {e}"))
}

/// 唯一标记，避免与命令自身输出混淆
fn unique_token() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{nanos:x}")
}

/// 安全地放进 shell 单引号里
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// 剥掉 ANSI 转义序列，得到可直接喂给模型的纯文本
fn strip_ansi(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut it = input.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            '\u{1b}' => match it.peek() {
                // CSI：ESC [ ... 终止符 0x40..=0x7E
                Some('[') => {
                    it.next();
                    for c2 in it.by_ref() {
                        if ('\u{40}'..='\u{7e}').contains(&c2) {
                            break;
                        }
                    }
                }
                // OSC：ESC ] ... BEL 或 ESC \
                Some(']') => {
                    it.next();
                    while let Some(c2) = it.next() {
                        if c2 == '\u{7}' {
                            break;
                        }
                        if c2 == '\u{1b}' {
                            let _ = it.next();
                            break;
                        }
                    }
                }
                // 其它双字节转义
                Some(_) => {
                    it.next();
                }
                None => {}
            },
            '\r' | '\u{8}' => {}
            _ => out.push(c),
        }
    }
    out
}

/// exec 的外壳：只负责把「AI 正在这个会话里跑命令」广播给界面。
/// 人能看到 AI 在跑什么、什么时候跑完 —— 这是「人在场」的那一半。
async fn tool_exec(app: &AppHandle, args: &Value) -> Result<String, String> {
    let command = args
        .get("command")
        .and_then(|v| v.as_str())
        .ok_or("缺少 command")?;
    let timeout_ms = args
        .get("timeout_ms")
        .and_then(|v| v.as_u64())
        .unwrap_or(30_000);
    let session = {
        let state = app.state::<AppState>();
        state.resolve(args.get("session").and_then(|v| v.as_str()))?
    };
    exec_with_events(app, &session, command, timeout_ms).await
}

/// 在指定会话执行并广播执行态。exec 和 host_exec 共用，界面表现一致。
async fn exec_with_events(
    app: &AppHandle,
    session: &std::sync::Arc<crate::term::Session>,
    command: &str,
    timeout_ms: u64,
) -> Result<String, String> {
    // 每次执行一个独立 run id。多个 AI 客户端可以同时连，同一个会话上
    // 很可能有两条 exec 排队；没有 run id 的话先返回的那条会把还在跑的
    // 那条的执行态一并抹掉，界面就谎报「没人在跑」。
    let run = unique_token();
    let _ = app.emit(
        crate::EVENT_AI_EXEC,
        json!({ "run": run, "session": session.id.clone(), "command": command }),
    );
    let args = json!({ "command": command, "timeout_ms": timeout_ms });
    let out = exec_body(&args, session).await;
    let _ = app.emit(
        crate::EVENT_AI_DONE,
        json!({ "run": run, "session": session.id.clone(), "ok": out.is_ok() }),
    );
    out
}

/// 按主机名一键执行：复用同主机活着的 SSH 会话，没有才新建。
async fn tool_host_exec(app: &AppHandle, args: &Value) -> Result<String, String> {
    let key = args.get("host").and_then(|v| v.as_str()).ok_or("缺少 host")?;
    let command = args
        .get("command")
        .and_then(|v| v.as_str())
        .ok_or("缺少 command")?;
    let timeout_ms = args
        .get("timeout_ms")
        .and_then(|v| v.as_u64())
        .unwrap_or(30_000);
    let reuse = args.get("reuse").and_then(|v| v.as_bool()).unwrap_or(true);

    let store = crate::hosts::HostStore::load();
    let host = store
        .find(key)
        .ok_or_else(|| format!("找不到主机 {key}"))?
        .clone();
    let endpoint = format!("{}@{}:{}", host.user, host.hostname, host.port);

    // 复用只认「同一 user@host:port 且仍活着」的 SSH 会话。
    // 不能只按标题找：标题可以改，改完就可能把命令打到另一台机器上。
    let existing = if reuse {
        let state = app.state::<AppState>();
        state.session_ids().iter().find_map(|id| {
            let session = state.get(id)?;
            let same_host = matches!(
                &session.kind,
                crate::term::SessionKind::Ssh { host: addr, .. } if addr == &endpoint
            );
            if same_host && session.is_alive() {
                Some(session)
            } else {
                None
            }
        })
    } else {
        None
    };

    let (session, created) = if let Some(session) = existing {
        (session, false)
    } else {
        let state = app.state::<AppState>();
        let (dc, dr) = state.any_session().map(|s| s.size()).unwrap_or((120, 34));
        let id = crate::spawn_ssh_tab(app, &state, dc, dr, host.clone())?;
        let session = state
            .get(&id)
            .ok_or_else(|| "SSH 会话创建后没有找到它".to_string())?;
        (session, true)
    };

    // 复用时也切到前台：人应该看见 AI 正在哪台机器上跑命令。
    crate::activate_session(app, &app.state::<AppState>(), &session.id)?;
    let session_id = session.id.clone();
    let out = exec_with_events(app, &session, command, timeout_ms)
        .await
        .map_err(|e| format!("主机「{}」·会话 {session_id}：{e}", host.name))?;
    Ok(format!(
        "主机「{}」·会话 {}{}\n{}",
        host.name,
        session_id,
        if created { "（新连接）" } else { "（复用）" },
        out
    ))
}

async fn exec_body(
    args: &Value,
    session: &std::sync::Arc<crate::term::Session>,
) -> Result<String, String> {
    let command = args
        .get("command")
        .and_then(|v| v.as_str())
        .ok_or("缺少 command")?;
    let timeout_ms = args
        .get("timeout_ms")
        .and_then(|v| v.as_u64())
        .unwrap_or(30_000);

    if !session.is_alive() {
        return Err("这个会话的 shell 已经退出了，请先新建一个会话".to_string());
    }

    let token = unique_token();

    // SSH 会话的输出落在远端，本地读不到 —— 只能从屏幕文本里抠。
    if matches!(session.kind, crate::term::SessionKind::Ssh { .. }) {
        return exec_ssh(session, &token, command, timeout_ms).await;
    }

    // 本地会话：输出先落文件再读，干净且不受滚动缓冲长度限制
    let out_path = std::path::PathBuf::from(format!("/tmp/daiqiao.{token}"));

    // 屏幕上只会出现 `__dq <token> '命令'` 这一行；
    // 完成信号走 OSC 设置标题（终端不显示），所以屏幕上不会留任何内部痕迹。
    //
    // 开头的 ^U（Ctrl-U）是防御：终端对能力查询的响应偶尔会晚到，
    // 落进 shell 的输入行里（实测出现过 `390- __dq ...` 被当成一条命令执行）。
    // Ctrl-U 清掉当前输入行，保证我们的命令从干净的行开始。
    let script = format!("\u{15}__dq {token} {}\n", shell_quote(command));
    session.write(script.as_bytes()).map_err(|e| e.to_string())?;

    let deadline = Instant::now() + Duration::from_millis(timeout_ms);

    loop {
        tokio::time::sleep(Duration::from_millis(25)).await;

        if let Some(exit_code) = session.take_done(&token) {
            // 从文件读原始输出，再剥掉 ANSI
            let raw = std::fs::read(&out_path).unwrap_or_default();
            let _ = std::fs::remove_file(&out_path);
            let output = strip_ansi(&String::from_utf8_lossy(&raw));
            let output = output.trim_end_matches('\n').to_string();

            let mut text = String::new();
            if output.is_empty() {
                text.push_str("(无输出)\n");
            } else {
                text.push_str(&output);
                text.push('\n');
            }
            text.push_str(&format!("[退出码 {exit_code}]"));
            return Ok(text);
        }

        // 命令把 shell 干掉了（最常见的是命令里带了 exit）——立刻返回，不要干等超时
        if !session.is_alive() {
            let raw = std::fs::read(&out_path).unwrap_or_default();
            let _ = std::fs::remove_file(&out_path);
            let output = strip_ansi(&String::from_utf8_lossy(&raw));
            let output = output.trim_end_matches('\n').to_string();
            let mut text = String::new();
            if !output.is_empty() {
                text.push_str(&output);
                text.push('\n');
            }
            text.push_str("[这条命令让 shell 退出了（多半是命令里带了 exit），会话已结束]");
            return Ok(text);
        }

        if Instant::now() >= deadline {
            let _ = std::fs::remove_file(&out_path);
            return Err(format!(
                "命令在 {timeout_ms} ms 内没有结束。如果它需要交互输入，请改用 send_input；如果它本来就该长时间运行，请用 read_screen 查看进度。"
            ));
        }
    }
}
