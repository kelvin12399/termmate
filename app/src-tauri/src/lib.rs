mod appearance;
mod audit;
mod config_io;
mod hosts;
#[cfg(not(test))]
mod keychain;
#[cfg(test)]
mod keychain {
    // 测试里不动真钥匙串：应用代码用的这一份是进程内的 mock。
    // 账号前缀和真实现（keychain.rs）保持一致，主机密码和密钥口令分两个命名空间。
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};

    static PASSWORDS: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();

    fn passwords() -> &'static Mutex<HashMap<String, String>> {
        PASSWORDS.get_or_init(|| Mutex::new(HashMap::new()))
    }

    fn host_account(host_id: &str) -> String {
        format!("ssh-host:{host_id}")
    }

    fn key_account(key_id: &str) -> String {
        format!("ssh-key:{key_id}")
    }

    pub fn set(host_id: &str, password: &str) -> Result<(), String> {
        if password.is_empty() {
            return Err("密码不能为空".into());
        }
        passwords()
            .lock()
            .map_err(|e| e.to_string())?
            .insert(host_account(host_id), password.to_string());
        Ok(())
    }

    pub fn get(host_id: &str) -> Result<Option<String>, String> {
        Ok(passwords()
            .lock()
            .map_err(|e| e.to_string())?
            .get(&host_account(host_id))
            .cloned())
    }

    pub fn delete(host_id: &str) -> Result<(), String> {
        passwords()
            .lock()
            .map_err(|e| e.to_string())?
            .remove(&host_account(host_id));
        Ok(())
    }

    pub fn saved(host_id: &str) -> bool {
        matches!(get(host_id), Ok(Some(_)))
    }

    pub fn set_key_passphrase(key_id: &str, passphrase: &str) -> Result<(), String> {
        if passphrase.is_empty() {
            return Err("口令不能为空".into());
        }
        passwords()
            .lock()
            .map_err(|e| e.to_string())?
            .insert(key_account(key_id), passphrase.to_string());
        Ok(())
    }

    pub fn get_key_passphrase(key_id: &str) -> Result<Option<String>, String> {
        Ok(passwords()
            .lock()
            .map_err(|e| e.to_string())?
            .get(&key_account(key_id))
            .cloned())
    }

    pub fn delete_key_passphrase(key_id: &str) -> Result<(), String> {
        passwords()
            .lock()
            .map_err(|e| e.to_string())?
            .remove(&key_account(key_id));
        Ok(())
    }

    pub fn key_passphrase_saved(key_id: &str) -> bool {
        matches!(get_key_passphrase(key_id), Ok(Some(_)))
    }

    pub fn clear() {
        passwords()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
    }
}
/// 真钥匙串实现再挂一份：上面那份 mock 只在测试构建里给应用代码用，
/// 而 keychain.rs 自己的单元测试要动**真**登录钥匙串（service=daiqiao，
/// 账号带 dqtest- 前缀，跑完自己清）—— 不挂这份的话它们永远不会被跑。
#[cfg(test)]
#[path = "keychain.rs"]
#[allow(dead_code)]
mod keychain_real;
mod keys;
mod known_hosts;
mod mcp;
mod proto;
mod sftp;
mod ssh;
mod term;
mod workspace;

use proto::TermUpdate;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter, State};
use term::Session;
use workspace::{SplitDir, Workspace};

pub const EVENT_TERM_UPDATE: &str = "term://update";
pub const EVENT_TERM_EXIT: &str = "term://exit";
pub const EVENT_WORKSPACE: &str = "workspace://changed";
/// 主机配置变了（不管是人点的还是 AI 调的），侧边栏据此刷新
pub const EVENT_HOSTS: &str = "hosts://changed";
pub const EVENT_KEYS: &str = "keys://changed";
/// SFTP 侧有改动（上传/删除/移动/回传），界面据此刷新当前目录
const EVENT_SFTP: &str = "sftp://changed";
/// 外观变了 —— 前端要重新量字体、重算行列并通知 PTY 新尺寸
const EVENT_APPEARANCE: &str = "appearance://changed";
/// AI 开始 / 结束在某个会话里跑命令 —— 界面据此显示「AI 正在执行」
pub const EVENT_AI_EXEC: &str = "ai://exec";
pub const EVENT_AI_DONE: &str = "ai://done";
/// MCP 的 streamable HTTP 端口（零配置：客户端直接连这个地址）
pub const MCP_PORT: u16 = 7391;

#[derive(Default)]
pub struct AppState {
    pub sessions: Mutex<HashMap<String, Arc<Session>>>,
    /// 标签页 + 分屏布局：人和 AI 共用这一份
    pub workspace: Mutex<Workspace>,
    pub seq: Mutex<u64>,
}

impl AppState {
    pub fn get(&self, id: &str) -> Option<Arc<Session>> {
        self.sessions.lock().ok()?.get(id).cloned()
    }

    /// 没指定会话时用「当前聚焦的 pane」，再退到 workspace 里的第一个
    /// 按 id 取会话。**显式给了 id 但不存在就报错** —— 绝不能静默落到
    /// 人正在看的那块屏上（AI 手里拿着陈旧 session id 是常事，悄悄跑到
    /// 别的会话里去，等于在人眼皮底下敲了它没打算敲的机器）。
    pub fn resolve(&self, id: Option<&str>) -> Result<Arc<Session>, String> {
        if let Some(id) = id {
            return self
                .get(id)
                .ok_or_else(|| format!("会话 {id} 不存在，先用 session_list 看看有哪些"));
        }
        self.any_session()
            .ok_or_else(|| "没有可用的终端会话".to_string())
    }

    /// 不挑具体哪个会话时用：焦点会话优先，否则第一个。
    /// 只用于「拿个尺寸当默认值」这类场合。
    pub fn any_session(&self) -> Option<Arc<Session>> {
        let focused = self.workspace.lock().ok().and_then(|w| w.focused.clone());
        if let Some(f) = focused {
            if let Some(s) = self.get(&f) {
                return Some(s);
            }
        }
        let first = self
            .workspace
            .lock()
            .ok()
            .map(|w| w.all_sessions())
            .and_then(|v| v.first().cloned());
        first.and_then(|id| self.get(&id))
    }

    /// 会话 id 列表（按工作区顺序）
    pub fn session_ids(&self) -> Vec<String> {
        self.workspace
            .lock()
            .map(|w| w.all_sessions())
            .unwrap_or_default()
    }
}

pub fn emit_workspace(app: &AppHandle, state: &AppState) {
    if let Ok(w) = state.workspace.lock() {
        let _ = app.emit(EVENT_WORKSPACE, w.clone());
    }
}

/// 主机列表变了就广播一声 —— 人和 AI 改的是同一份数据，界面得跟上
pub fn emit_hosts(app: &AppHandle) {
    let _ = app.emit(EVENT_HOSTS, true);
}

/// 密钥库变了（人改的或 AI 改的都走这里），界面重新拉一次列表
pub fn emit_keys(app: &AppHandle) {
    let _ = app.emit(EVENT_KEYS, true);
}

/// 只起一个终端会话，不碰布局，也不推首屏。
/// 首屏必须在 workspace 更新**之后**推 —— 否则前端还没有对应的 pane，内容会被丢掉。
fn create_session_only(
    app: &AppHandle,
    state: &AppState,
    cols: u16,
    rows: u16,
) -> Result<(String, Arc<Session>), String> {
    let id = {
        let mut seq = state.seq.lock().map_err(|e| e.to_string())?;
        *seq += 1;
        format!("s{}", *seq)
    };

    let app_update = app.clone();
    let on_update = move |u: TermUpdate| {
        let _ = app_update.emit(EVENT_TERM_UPDATE, u);
    };

    let app_exit = app.clone();
    let id_exit = id.clone();
    let on_exit = move || {
        let _ = app_exit.emit(EVENT_TERM_EXIT, id_exit.clone());
    };

    let session = Session::spawn_local(id.clone(), cols.max(2), rows.max(2), on_update, on_exit)
        .map_err(|e| format!("启动终端失败: {e}"))?;
    let session = Arc::new(session);

    state
        .sessions
        .lock()
        .map_err(|e| e.to_string())?
        .insert(id.clone(), session.clone());

    Ok((id, session))
}

/// 新建标签页（新开一个 shell）
pub fn spawn_tab(
    app: &AppHandle,
    state: &AppState,
    cols: u16,
    rows: u16,
    title: Option<String>,
) -> Result<String, String> {
    let (id, session) = create_session_only(app, state, cols, rows)?;

    if let Ok(mut w) = state.workspace.lock() {
        w.new_tab(
            format!("t{}", id),
            id.clone(),
            title.unwrap_or_else(|| "本地终端".to_string()),
        );
    }

    // 顺序要紧：先布局，后内容
    emit_workspace(app, state);
    if let Some(u) = session.force_full(&id) {
        let _ = app.emit(EVENT_TERM_UPDATE, u);
    }
    Ok(id)
}

/// 在 `at` 所在的 pane 上分屏，新 pane 起一个新 shell
pub fn split_pane(
    app: &AppHandle,
    state: &AppState,
    at: &str,
    dir: SplitDir,
    cols: u16,
    rows: u16,
) -> Result<String, String> {
    if state.get(at).is_none() {
        return Err(format!("会话 {at} 不存在"));
    }
    let (new_id, session) = create_session_only(app, state, cols, rows)?;

    let ok = state
        .workspace
        .lock()
        .map(|mut w| w.split(at, &new_id, dir))
        .unwrap_or(false);

    if !ok {
        // 分屏失败就回滚，别留下孤儿会话
        let _ = close_session(app, state, &new_id);
        return Err("找不到要分屏的目标 pane".to_string());
    }

    emit_workspace(app, state);
    if let Some(u) = session.force_full(&new_id) {
        let _ = app.emit(EVENT_TERM_UPDATE, u);
    }
    Ok(new_id)
}

/// 新开一个标签页连到 SSH 主机
pub fn spawn_ssh_tab(
    app: &AppHandle,
    state: &AppState,
    cols: u16,
    rows: u16,
    host: hosts::Host,
) -> Result<String, String> {
    let id = {
        let mut seq = state.seq.lock().map_err(|e| e.to_string())?;
        *seq += 1;
        format!("s{}", *seq)
    };

    let app_update = app.clone();
    let on_update = move |u: TermUpdate| {
        let _ = app_update.emit(EVENT_TERM_UPDATE, u);
    };

    let app_exit = app.clone();
    let id_exit = id.clone();
    let on_exit = move || {
        let _ = app_exit.emit(EVENT_TERM_EXIT, id_exit.clone());
    };

    let label = host.name.clone();
    let session = Session::spawn_ssh(
        id.clone(),
        cols.max(2),
        rows.max(2),
        host,
        on_update,
        on_exit,
    )
    .map_err(|e| format!("连接 SSH 失败: {e}"))?;
    let session = Arc::new(session);

    state
        .sessions
        .lock()
        .map_err(|e| e.to_string())?
        .insert(id.clone(), session.clone());

    if let Ok(mut w) = state.workspace.lock() {
        w.new_tab(format!("t{}", id), id.clone(), label);
    }

    emit_workspace(app, state);
    if let Some(u) = session.force_full(&id) {
        let _ = app.emit(EVENT_TERM_UPDATE, u);
    }
    Ok(id)
}

/// 关掉一个会话。它是所在 tab 的最后一个 pane 时，整个标签页一起关。
pub fn close_session(app: &AppHandle, state: &AppState, id: &str) -> Result<(), String> {
    let s = state.get(id).ok_or("会话不存在")?;
    s.kill();
    if let Ok(mut m) = state.sessions.lock() {
        m.remove(id);
    }
    if let Ok(mut w) = state.workspace.lock() {
        w.remove_session(id);
    }
    emit_workspace(app, state);
    Ok(())
}

pub fn activate_session(app: &AppHandle, state: &AppState, id: &str) -> Result<(), String> {
    let ok = state
        .workspace
        .lock()
        .map(|mut w| w.activate(id))
        .unwrap_or(false);
    if !ok {
        return Err(format!("会话 {id} 不存在"));
    }
    emit_workspace(app, state);
    Ok(())
}

pub fn rename_session(
    app: &AppHandle,
    state: &AppState,
    id: &str,
    title: &str,
) -> Result<(), String> {
    let ok = state
        .workspace
        .lock()
        .map(|mut w| w.set_title(id, title))
        .unwrap_or(false);
    if !ok {
        return Err(format!("会话 {id} 不存在"));
    }
    emit_workspace(app, state);
    Ok(())
}

// ───────────────────────── Tauri 命令（前端调用）─────────────────────────

#[tauri::command]
fn create_session(
    app: AppHandle,
    state: State<AppState>,
    cols: u16,
    rows: u16,
) -> Result<String, String> {
    spawn_tab(&app, &state, cols, rows, None)
}

#[tauri::command]
fn split_session(
    app: AppHandle,
    state: State<AppState>,
    id: String,
    direction: String,
    cols: u16,
    rows: u16,
) -> Result<String, String> {
    let dir = SplitDir::parse(&direction).ok_or("方向只能是 horizontal 或 vertical")?;
    split_pane(&app, &state, &id, dir, cols, rows)
}

#[tauri::command]
fn activate_session_cmd(app: AppHandle, state: State<AppState>, id: String) -> Result<(), String> {
    activate_session(&app, &state, &id)
}

#[tauri::command]
fn rename_session_cmd(
    app: AppHandle,
    state: State<AppState>,
    id: String,
    title: String,
) -> Result<(), String> {
    rename_session(&app, &state, &id, &title)
}

/// 内置 MCP server 的接入信息。
/// 状态和工具数都从同进程的实际状态读 —— 界面不能跨源 fetch 自己的 server
/// （webview 在 localhost:1420，MCP 在 127.0.0.1:7391，给 server 开 CORS
/// 等于让任何网页都能读这台机器的终端输出，不能干）。
#[tauri::command]
fn mcp_info() -> serde_json::Value {
    serde_json::json!({
        "url": format!("http://127.0.0.1:{MCP_PORT}/mcp"),
        "port": MCP_PORT,
        "listening": mcp::LISTENING.load(std::sync::atomic::Ordering::Relaxed),
        "tools": mcp::tool_count(),
        "bridge": stdio_bridge_path(),
    })
}

#[tauri::command]
fn set_window_title(window: tauri::Window, title: String) -> Result<(), String> {
    window.set_title(&title).map_err(|e| e.to_string())
}

/// 系统剪贴板：右键复制/粘贴都走原生实现，WKWebView 权限差异不影响使用。
#[tauri::command]
fn clipboard_write_text(text: String) -> Result<(), String> {
    use std::io::Write as _;
    let mut child = std::process::Command::new("/usr/bin/pbcopy")
        .stdin(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("启动 pbcopy 失败: {e}"))?;
    child
        .stdin
        .take()
        .ok_or_else(|| "拿不到 pbcopy 的输入管道".to_string())?
        .write_all(text.as_bytes())
        .map_err(|e| format!("写入剪贴板失败: {e}"))?;
    let status = child.wait().map_err(|e| format!("pbcopy 等待失败: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("pbcopy 失败: {status}"))
    }
}

#[tauri::command]
fn clipboard_read_text() -> Result<String, String> {
    let out = std::process::Command::new("/usr/bin/pbpaste")
        .output()
        .map_err(|e| format!("启动 pbpaste 失败: {e}"))?;
    if !out.status.success() {
        return Err(format!("pbpaste 失败: {}", out.status));
    }
    String::from_utf8(out.stdout).map_err(|_| "剪贴板里不是 UTF-8 文本".to_string())
}

#[tauri::command]
fn get_appearance() -> serde_json::Value {
    appearance::Appearance::load().to_view()
}

/// 改外观。传进来的字段都是可选，只改给了的那几项。
#[tauri::command]
fn set_appearance(
    app: AppHandle,
    font_size: Option<u8>,
    font_family: Option<String>,
    theme: Option<String>,
    line_height: Option<f32>,
    cursor_style: Option<String>,
    language: Option<String>,
) -> Result<serde_json::Value, String> {
    let mut a = appearance::Appearance::load();
    let before = a.clone();
    if let Some(v) = font_size {
        a.font_size = v;
    }
    if let Some(v) = font_family {
        a.font_family = v;
    }
    if let Some(v) = theme {
        a.theme = v;
    }
    if let Some(v) = line_height {
        a.line_height = v;
    }
    if let Some(v) = cursor_style {
        a.cursor_style = v;
    }
    if let Some(v) = language {
        a.language = v;
    }
    let a = a.clamped();
    a.save()?;

    if a != before {
        audit::human(
            "界面·改外观",
            Some(format!(
                "字号 {} · 主题 {} · 行高 {:.2} · 光标 {}",
                a.font_size, a.theme, a.line_height, a.cursor_style
            )),
        );
    }
    // 所有窗口那边要立即重新量字宽、重算行列
    let view = a.to_view();
    let _ = app.emit(EVENT_APPEARANCE, view.clone());
    Ok(view)
}

/// stdio 桥可执行文件的位置。
///
/// 打包后它和主程序躺在同一个 MacOS/ 目录里；开发时在 target/debug/。
/// 界面要把这个路径原样给人去填进客户端配置，所以必须是**真实存在**的
/// 绝对路径 —— 猜一个填进去，人复制走了才发现跑不起来。
fn stdio_bridge_path() -> String {
    const NAME: &str = "daiqiao-mcp";
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let same_dir = dir.join(NAME);
            if same_dir.is_file() {
                return same_dir.to_string_lossy().into_owned();
            }
        }
    }
    NAME.to_string()
}

#[tauri::command]
fn close_session_cmd(app: AppHandle, state: State<AppState>, id: String) -> Result<(), String> {
    close_session(&app, &state, &id)
}

#[tauri::command]
fn write_session(state: State<AppState>, id: String, data: String) -> Result<(), String> {
    let s = state.get(&id).ok_or("会话不存在")?;
    s.write(data.as_bytes()).map_err(|e| e.to_string())
}

#[tauri::command]
fn resize_session(
    app: AppHandle,
    state: State<AppState>,
    id: String,
    cols: u16,
    rows: u16,
) -> Result<(), String> {
    let s = state.get(&id).ok_or("会话不存在")?;
    s.resize(cols.max(2), rows.max(2))
        .map_err(|e| e.to_string())?;
    if let Some(u) = s.force_full(&id) {
        let _ = app.emit(EVENT_TERM_UPDATE, u);
    }
    Ok(())
}

#[tauri::command]
fn redraw_session(app: AppHandle, state: State<AppState>, id: String) -> Result<(), String> {
    let s = state.get(&id).ok_or("会话不存在")?;
    // 前端刚挂上这个 pane、手上还没有内容时主动要一次整屏，
    // 否则要等到下一次有输出才画得出来（切标签页/重载后会看到空白）
    if let Some(u) = s.force_full(&id) {
        let _ = app.emit(EVENT_TERM_UPDATE, u);
    }
    Ok(())
}

#[tauri::command]
fn get_workspace(state: State<AppState>) -> Workspace {
    state
        .workspace
        .lock()
        .map(|w| w.clone())
        .unwrap_or_default()
}

// ───────────────────── 主机管理的 UI 命令 ─────────────────────
// 前端侧边栏用这些；MCP 那边用对应的 host_* 工具。两边读写同一份 hosts.json。

#[tauri::command]
fn list_hosts() -> Vec<serde_json::Value> {
    let store = hosts::HostStore::load();
    store.hosts.iter().map(hosts::host_view).collect()
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
fn save_host(
    app: AppHandle,
    key: Option<String>,
    name: String,
    hostname: String,
    user: String,
    port: u16,
    auth: String,
    key_id: Option<String>,
    password: Option<String>,
    group: Option<String>,
    note: Option<String>,
) -> Result<String, String> {
    let mut store = hosts::HostStore::load();
    let id = hosts::upsert_host(
        &mut store,
        key.as_deref(),
        &name,
        &hostname,
        &user,
        port,
        &auth,
        key_id,
        group,
        note,
    )?;
    store.save().map_err(|e| e.to_string())?;
    audit::human(
        if key.is_some() {
            "界面·修改主机"
        } else {
            "界面·新建主机"
        },
        Some(format!("主机「{name}」 {user}@{hostname}:{port}")),
    );
    // 密码走钥匙串，不进 hosts.json；内容不进审计
    if auth == "password" {
        if let Some(pw) = password.filter(|p| !p.is_empty()) {
            keychain::set(&id, &pw)?;
            audit::human(
                "界面·存主机密码",
                Some(format!("主机「{name}」（内容不落日志）")),
            );
        }
    }
    emit_hosts(&app);
    Ok(id)
}

#[tauri::command]
fn set_host_password(app: AppHandle, host_id: String, password: String) -> Result<(), String> {
    keychain::set(&host_id, &password)?;
    audit::human(
        "界面·存主机密码",
        Some(format!("主机 {host_id} 的密码已存进钥匙串（内容不落日志）")),
    );
    emit_hosts(&app);
    Ok(())
}

#[tauri::command]
fn clear_host_password(app: AppHandle, host_id: String) -> Result<(), String> {
    keychain::delete(&host_id)?;
    audit::human("界面·清除主机密码", Some(format!("主机 {host_id}")));
    emit_hosts(&app);
    Ok(())
}

/// 界面和 MCP 共用一份实现，两边绝不能各自写一遍。
pub fn agent_status_inner(host: Option<&str>) -> Result<serde_json::Value, String> {
    let socket = hosts::agent_socket_for(host).ok_or_else(|| {
        if matches!(
            hosts::configured_identity_agent(host),
            Some(hosts::IdentityAgent::Disabled)
        ) {
            "这台主机在 ~/.ssh/config 里设了 IdentityAgent none，agent 认证被显式禁用".to_string()
        } else {
            "没找到可用的 ssh-agent socket（~/.ssh/config 和 SSH_AUTH_SOCK 都没有）".to_string()
        }
    })?;

    // 只给这个子进程设环境变量，不改整个 App 的 SSH_AUTH_SOCK，
    // 否则多台主机用不同 agent 时会互相踩。
    let out = std::process::Command::new("ssh-add")
        .arg("-l")
        .env("SSH_AUTH_SOCK", &socket)
        .output()
        .map_err(|e| format!("找不到 ssh-add: {e}"))?;
    // 0 = 有密钥；1 = agent 在跑但没密钥；其他 = 连不上 agent
    if out.status.success() {
        let n = String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter(|l| !l.trim().is_empty())
            .count();
        Ok(serde_json::json!({ "running": true, "count": n, "socket": socket }))
    } else if out.status.code() == Some(1) {
        Ok(serde_json::json!({ "running": true, "count": 0, "socket": socket }))
    } else {
        Err(format!(
            "连不上 agent（{socket}）。它可能没在运行，或 1Password 的 SSH Agent 没启用"
        ))
    }
}

#[tauri::command]
fn agent_status(host: Option<String>) -> Result<serde_json::Value, String> {
    agent_status_inner(host.as_deref())
}

#[tauri::command]
fn delete_host(app: AppHandle, key: String) -> Result<(), String> {
    let mut store = hosts::HostStore::load();
    let before = store.hosts.len();
    let gone: Vec<String> = store
        .hosts
        .iter()
        .filter(|h| h.id == key || h.name == key)
        .map(|h| h.name.clone())
        .collect();
    store.hosts.retain(|h| h.id != key && h.name != key);
    if store.hosts.len() == before {
        return Err(format!("找不到主机 {key}"));
    }
    store.save().map_err(|e| e.to_string())?;
    audit::human(
        "界面·删除主机",
        Some(format!(
            "主机「{}」",
            if gone.is_empty() {
                key.clone()
            } else {
                gone.join("、")
            }
        )),
    );
    emit_hosts(&app);
    Ok(())
}

// ─────────────────────── 密钥库 ───────────────────────

/// 哪些主机在用哪把密钥：key_id -> 主机名列表。
/// 旧的 key_path 主机也换算成同一个 id，这样两种都能统计到。
fn hosts_by_key() -> HashMap<String, Vec<String>> {
    let mut map: HashMap<String, Vec<String>> = HashMap::new();
    for h in hosts::HostStore::load().hosts {
        let id = match (&h.key_id, &h.key_path) {
            (Some(id), _) => Some(id.clone()),
            (None, Some(p)) => Some(keys::key_id(std::path::Path::new(p))),
            _ => None,
        };
        if let Some(id) = id {
            map.entry(id).or_default().push(h.name.clone());
        }
    }
    map
}

#[tauri::command]
fn list_keys_cmd() -> Vec<serde_json::Value> {
    keys::list_keys(&hosts_by_key())
        .iter()
        .map(keys::key_view)
        .collect()
}

#[tauri::command]
fn import_key_file(
    app: AppHandle,
    path: String,
    name: Option<String>,
) -> Result<serde_json::Value, String> {
    let info = keys::import_file(std::path::Path::new(&path), name.as_deref())
        .map_err(|e| e.to_string())?;
    audit::human(
        "界面·导入密钥文件",
        Some(format!("密钥「{}」 {}", info.name, info.algo)),
    );
    emit_keys(&app);
    Ok(keys::key_view(&info))
}

#[tauri::command]
fn import_key_text(
    app: AppHandle,
    text: String,
    name: Option<String>,
    passphrase: Option<String>,
) -> Result<serde_json::Value, String> {
    // 空串当没填，免得前端没处理干净就传过来
    let passphrase = passphrase.filter(|p| !p.is_empty());
    let info = keys::import_text(&text, name.as_deref(), passphrase.as_deref())
        .map_err(|e| e.to_string())?;
    audit::human(
        "界面·粘贴导入密钥",
        Some(format!(
            "密钥「{}」 {}{}",
            info.name,
            info.algo,
            if info.encrypted {
                "（带口令，口令已存钥匙串）"
            } else {
                ""
            }
        )),
    );
    emit_keys(&app);
    Ok(keys::key_view(&info))
}

/// 给一把密钥存 / 清「解锁口令」（带 passphrase 的私钥连接时要用）。
/// `passphrase` 传 None（或空串）= 清除。存之前先拿它解一遍私钥，
/// 错口令当场报出来，绝不把没验证过的口令写进钥匙串；口令内容不落日志。
#[tauri::command]
fn set_key_passphrase(
    app: AppHandle,
    id: String,
    passphrase: Option<String>,
) -> Result<(), String> {
    let used = hosts_by_key();
    let info = keys::find_key(&id, &used).ok_or("找不到这把密钥")?;
    match passphrase.as_deref().filter(|p| !p.is_empty()) {
        Some(pw) => {
            keys::verify_passphrase(std::path::Path::new(&info.path), pw)
                .map_err(|e| e.to_string())?;
            keychain::set_key_passphrase(&id, pw)?;
            audit::human(
                "界面·存密钥口令",
                Some(format!("密钥「{}」（内容不落日志）", info.name)),
            );
        }
        None => {
            keychain::delete_key_passphrase(&id)?;
            audit::human("界面·清除密钥口令", Some(format!("密钥「{}」", info.name)));
        }
    }
    emit_keys(&app);
    Ok(())
}

#[tauri::command]
fn generate_key_cmd(
    app: AppHandle,
    name: Option<String>,
    algo: String,
    comment: Option<String>,
) -> Result<serde_json::Value, String> {
    let info = keys::generate(
        name.as_deref(),
        &algo,
        comment.as_deref().unwrap_or("daiqiao"),
    )
    .map_err(|e| e.to_string())?;
    audit::human(
        "界面·生成密钥",
        Some(format!("密钥「{}」 {}", info.name, info.algo)),
    );
    emit_keys(&app);
    Ok(keys::key_view(&info))
}

#[tauri::command]
fn rename_key_cmd(app: AppHandle, id: String, name: String) -> Result<(), String> {
    keys::set_name(&id, &name).map_err(|e| e.to_string())?;
    emit_keys(&app);
    Ok(())
}

#[tauri::command]
fn fix_key_permissions(app: AppHandle, id: String) -> Result<(), String> {
    keys::fix_permissions(&id, &hosts_by_key()).map_err(|e| e.to_string())?;
    emit_keys(&app);
    Ok(())
}

#[tauri::command]
fn remove_key_cmd(app: AppHandle, id: String) -> Result<(), String> {
    let used = hosts_by_key();
    let info = keys::find_key(&id, &used).ok_or("找不到这把密钥")?;
    if !info.used_by.is_empty() {
        return Err(format!(
            "还有 {} 台主机在用它（{}），先把它们改成别的密钥",
            info.used_by.len(),
            info.used_by.join("、")
        ));
    }
    keys::remove_vault_key(&id, &used).map_err(|e| e.to_string())?;
    audit::human("界面·删除密钥", Some(format!("密钥「{}」", info.name)));
    emit_keys(&app);
    Ok(())
}

// ─────────────────── SFTP：给界面用的桥接 ───────────────────
// MCP 侧已有同名工具，这里是人用界面的入口。两边走的是同一套 sftp.rs。

fn host_by_key(key: &str) -> Result<hosts::Host, String> {
    hosts::HostStore::load()
        .find(key)
        .cloned()
        .ok_or_else(|| format!("找不到主机 {key}"))
}

#[tauri::command]
async fn sftp_ls(host: String, path: String) -> Result<Vec<serde_json::Value>, String> {
    let h = host_by_key(&host)?;
    let entries = sftp::list(&h, &path).await.map_err(|e| e.to_string())?;
    Ok(entries
        .into_iter()
        .map(|e| {
            serde_json::json!({
                "name": e.name,
                "is_dir": e.is_dir,
                "size": e.size,
                "size_h": if e.is_dir { "-".to_string() } else { sftp::human_size(e.size) },
            })
        })
        .collect())
}

#[tauri::command]
async fn sftp_put(
    app: AppHandle,
    host: String,
    local: String,
    remote: String,
) -> Result<u64, String> {
    let h = host_by_key(&host)?;
    let n = sftp::upload(&h, &local, &remote)
        .await
        .map_err(|e| e.to_string())?;
    audit::human("界面·SFTP 上传", Some(format!("{local} → {host}:{remote}")));
    let _ = app.emit(
        EVENT_SFTP,
        serde_json::json!({ "host": host, "changed": remote }),
    );
    Ok(n)
}

#[tauri::command]
async fn sftp_get(host: String, remote: String, local: String) -> Result<u64, String> {
    let h = host_by_key(&host)?;
    let n = sftp::download(&h, &remote, &local)
        .await
        .map_err(|e| e.to_string())?;
    audit::human("界面·SFTP 下载", Some(format!("{host}:{remote} → {local}")));
    Ok(n)
}

/// Finder 拖进来的文件在 WKWebView 里拿不到本地路径，前端读完内容转字节数组传过来。
#[tauri::command]
async fn sftp_put_bytes(
    app: AppHandle,
    host: String,
    remote: String,
    bytes: Vec<u8>,
) -> Result<u64, String> {
    let h = host_by_key(&host)?;
    let sftp = ssh::sftp_connect(&h).await.map_err(|e| e.to_string())?;
    use tokio::io::AsyncWriteExt;
    let mut f = sftp.create(&remote).await.map_err(|e| e.to_string())?;
    f.write_all(&bytes).await.map_err(|e| e.to_string())?;
    f.flush().await.map_err(|e| e.to_string())?;
    audit::human("界面·SFTP 上传（拖拽）", Some(format!("{host}:{remote}")));
    let _ = app.emit(
        EVENT_SFTP,
        serde_json::json!({ "host": host, "changed": remote }),
    );
    Ok(bytes.len() as u64)
}

#[tauri::command]
async fn sftp_rm(app: AppHandle, host: String, path: String) -> Result<String, String> {
    let h = host_by_key(&host)?;
    let msg = sftp::remove(&h, &path).await.map_err(|e| e.to_string())?;
    audit::human("界面·SFTP 删除", Some(format!("{host}:{path}")));
    let _ = app.emit(
        EVENT_SFTP,
        serde_json::json!({ "host": host, "changed": path }),
    );
    Ok(msg)
}

#[tauri::command]
async fn sftp_mv(app: AppHandle, host: String, from: String, to: String) -> Result<String, String> {
    let h = host_by_key(&host)?;
    let msg = sftp::rename(&h, &from, &to)
        .await
        .map_err(|e| e.to_string())?;
    audit::human("界面·SFTP 移动", Some(format!("{host}:{from} → {to}")));
    let _ = app.emit(
        EVENT_SFTP,
        serde_json::json!({ "host": host, "changed": to }),
    );
    Ok(msg)
}

#[tauri::command]
async fn sftp_mkdir_cmd(app: AppHandle, host: String, path: String) -> Result<String, String> {
    let h = host_by_key(&host)?;
    let msg = sftp::mkdir(&h, &path).await.map_err(|e| e.to_string())?;
    audit::human("界面·SFTP 新建目录", Some(format!("{host}:{path}")));
    let _ = app.emit(
        EVENT_SFTP,
        serde_json::json!({ "host": host, "changed": path }),
    );
    Ok(msg)
}

/// 远端复制。SFTP 协议本身没有 copy，走「下载到临时文件再上传」。
#[tauri::command]
async fn sftp_copy(
    app: AppHandle,
    host: String,
    from: String,
    to: String,
) -> Result<String, String> {
    let h = host_by_key(&host)?;
    let msg = sftp::copy_remote(&h, &from, &to)
        .await
        .map_err(|e| e.to_string())?;
    audit::human("界面·SFTP 复制", Some(format!("{host}:{from} → {to}")));
    let _ = app.emit(
        EVENT_SFTP,
        serde_json::json!({ "host": host, "changed": to }),
    );
    Ok(msg)
}

/// 用系统应用打开远端文件：下载到本地暂存目录 → open（可指定 App）→ 盯着它改动自动回传。
/// 界面和 MCP 共用这一份实现。
pub async fn sftp_open_with_inner(
    app: AppHandle,
    host: String,
    remote: String,
    application: Option<String>,
) -> Result<String, String> {
    let h = host_by_key(&host)?;
    let local = sftp::edit_stage_path(&host, &remote);
    sftp::download(&h, &remote, &local.to_string_lossy())
        .await
        .map_err(|e| format!("下载失败: {e}"))?;

    let mut cmd = std::process::Command::new("/usr/bin/open");
    if let Some(a) = application.as_deref().filter(|a| !a.is_empty()) {
        cmd.arg("-a").arg(a);
    }
    cmd.arg(&local);
    let out = cmd.output().map_err(|e| format!("open 跑不起来: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "打开失败: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }

    audit::human(
        "界面·SFTP 打开方式",
        Some(format!(
            "{host}:{remote} 用 {} 打开（改动会自动回传）",
            application.as_deref().unwrap_or("默认应用")
        )),
    );
    sftp::watch_and_sync_back(app, host, remote.clone(), local);
    Ok(format!("已打开 {remote}，在本地保存后会自动回传"))
}

#[tauri::command]
async fn sftp_open_with(
    app: AppHandle,
    host: String,
    remote: String,
    application: Option<String>,
) -> Result<String, String> {
    sftp_open_with_inner(app, host, remote, application).await
}

/// 列出能打开这个文件的系统应用（给右键「打开方式」用）。
#[tauri::command]
fn sftp_open_candidates(name: String) -> Vec<serde_json::Value> {
    sftp::open_candidates(&name)
}

/// 试连一台主机就断开，用来验证配置对不对。返回耗时（秒）。
#[tauri::command]
async fn test_host_connect(key: String) -> Result<f32, String> {
    let host = hosts::HostStore::load()
        .find(&key)
        .cloned()
        .ok_or_else(|| format!("找不到主机 {key}"))?;
    let t0 = std::time::Instant::now();
    ssh::connect(&host).await.map_err(|e| e.to_string())?;
    Ok(t0.elapsed().as_secs_f32())
}

/// 代敲记下的主机指纹（给界面列表用）
#[tauri::command]
fn host_keys() -> Vec<serde_json::Value> {
    known_hosts::list()
        .into_iter()
        .map(|(host, fp)| serde_json::json!({ "host": host, "fingerprint": fp }))
        .collect()
}

/// 清掉某台主机的指纹记录。只动代敲自己的库。
#[tauri::command]
fn forget_host_key(host: String, port: Option<u16>) -> Result<usize, String> {
    let port = port.unwrap_or(22);
    let n = known_hosts::forget(&host, port).map_err(|e| e.to_string())?;
    if n > 0 {
        audit::human(
            "界面·清除主机指纹记录",
            Some(format!("{host}:{port} 删了 {n} 条")),
        );
    }
    Ok(n)
}

/// 扫一遍 ~/.ssh/config，返回可导入的主机（已存在的会被滤掉）
#[tauri::command]
fn scan_ssh_config() -> Vec<hosts::SshConfigEntry> {
    hosts::parse_ssh_config()
}

/// 把选中的条目批量导入 hosts.json。界面和 MCP 共用这一份实现。
pub fn import_ssh_config_inner(app: &AppHandle, aliases: Vec<String>) -> Result<usize, String> {
    let candidates = hosts::parse_ssh_config();
    let mut store = hosts::HostStore::load();
    let mut n = 0usize;

    for e in candidates
        .into_iter()
        .filter(|e| aliases.contains(&e.alias))
    {
        let auth = if e.identity.is_some() { "key" } else { "agent" };
        hosts::upsert_host(
            &mut store,
            None,
            &e.alias,
            &e.hostname,
            &e.user,
            e.port,
            auth,
            e.identity,
            None,
            Some("从 ~/.ssh/config 导入".to_string()),
        )?;
        n += 1;
    }

    if n > 0 {
        store.save().map_err(|e| e.to_string())?;
        emit_hosts(app);
    }
    Ok(n)
}

#[tauri::command]
fn import_ssh_config(app: AppHandle, aliases: Vec<String>) -> Result<usize, String> {
    import_ssh_config_inner(&app, aliases)
}

/// 侧边栏点主机 → 新标签页连上
#[tauri::command]
fn connect_host(app: AppHandle, state: State<AppState>, key: String) -> Result<String, String> {
    let store = hosts::HostStore::load();
    let host = store
        .find(&key)
        .cloned()
        .ok_or_else(|| format!("找不到主机 {key}"))?;
    let (cols, rows) = state.any_session().map(|s| s.size()).unwrap_or((120, 34));
    spawn_ssh_tab(&app, &state, cols, rows, host)
}

pub fn run() {
    env_logger::init();

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![
            create_session,
            split_session,
            activate_session_cmd,
            rename_session_cmd,
            close_session_cmd,
            mcp_info,
            set_window_title,
            clipboard_write_text,
            clipboard_read_text,
            get_appearance,
            set_appearance,
            write_session,
            resize_session,
            redraw_session,
            get_workspace,
            list_hosts,
            save_host,
            delete_host,
            set_host_password,
            agent_status,
            clear_host_password,
            connect_host,
            scan_ssh_config,
            import_ssh_config,
            config_io::export_config,
            config_io::inspect_config,
            config_io::import_config,
            list_keys_cmd,
            import_key_file,
            import_key_text,
            set_key_passphrase,
            generate_key_cmd,
            rename_key_cmd,
            fix_key_permissions,
            remove_key_cmd,
            test_host_connect,
            host_keys,
            forget_host_key,
            sftp_ls,
            sftp_put,
            sftp_put_bytes,
            sftp_get,
            sftp_rm,
            sftp_mv,
            sftp_mkdir_cmd,
            sftp_copy,
            sftp_open_with,
            sftp_open_candidates
        ])
        .setup(|app| {
            let handle = app.handle().clone();
            // 内置 MCP server：和终端状态同进程，AI 直接读内存里的 grid
            mcp::spawn(handle, MCP_PORT);
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
