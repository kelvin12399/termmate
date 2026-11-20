//! SFTP 文件操作：列目录、上传、下载、删除、重命名、建目录。
//!
//! 每次操作用一条独立连接（简单可靠；等用频繁了再加连接池）。

use crate::hosts::Host;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub struct Entry {
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
}

pub fn human_size(n: u64) -> String {
    const U: [&str; 4] = ["B", "K", "M", "G"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n}{}", U[0])
    } else {
        format!("{v:.1}{}", U[i])
    }
}

pub async fn list(host: &Host, path: &str) -> anyhow::Result<Vec<Entry>> {
    let sftp = crate::ssh::sftp_connect(host).await?;
    let dir = if path.is_empty() { "." } else { path };
    let mut out = Vec::new();
    for e in sftp.read_dir(dir).await? {
        let name = e.file_name();
        if name == "." || name == ".." {
            continue;
        }
        let is_dir = e.file_type().is_dir();
        let size = e.metadata().size.unwrap_or(0);
        out.push(Entry { name, is_dir, size });
    }
    // 目录在前，各自按名字排
    out.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.cmp(&b.name)));
    Ok(out)
}

pub async fn upload(host: &Host, local: &str, remote: &str) -> anyhow::Result<u64> {
    let data = tokio::fs::read(local).await?;
    let sftp = crate::ssh::sftp_connect(host).await?;
    let mut f = sftp.create(remote).await?;
    f.write_all(&data).await?;
    f.flush().await?;
    Ok(data.len() as u64)
}

pub async fn download(host: &Host, remote: &str, local: &str) -> anyhow::Result<u64> {
    let sftp = crate::ssh::sftp_connect(host).await?;
    let mut f = sftp.open(remote).await?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).await?;

    if let Some(dir) = std::path::Path::new(local).parent() {
        if !dir.as_os_str().is_empty() {
            tokio::fs::create_dir_all(dir).await?;
        }
    }
    tokio::fs::write(local, &buf).await?;
    Ok(buf.len() as u64)
}

pub async fn remove(host: &Host, path: &str) -> anyhow::Result<String> {
    let sftp = crate::ssh::sftp_connect(host).await?;
    // 先按目录删，不行再按文件删
    match sftp.remove_dir(path).await {
        Ok(()) => Ok(format!("已删除目录 {path}")),
        Err(_) => {
            sftp.remove_file(path).await?;
            Ok(format!("已删除文件 {path}"))
        }
    }
}

pub async fn rename(host: &Host, from: &str, to: &str) -> anyhow::Result<String> {
    let sftp = crate::ssh::sftp_connect(host).await?;
    sftp.rename(from, to).await?;
    Ok(format!("已把 {from} 移动/重命名为 {to}"))
}

pub async fn mkdir(host: &Host, path: &str) -> anyhow::Result<String> {
    let sftp = crate::ssh::sftp_connect(host).await?;
    sftp.create_dir(path).await?;
    Ok(format!("已创建目录 {path}"))
}

/// 远端复制。SFTP 协议没有 copy 原语，只能「读出来再写回去」。
/// 目录暂不支持（递归复制等真有人要再说）。
pub async fn copy_remote(host: &Host, from: &str, to: &str) -> anyhow::Result<String> {
    let sftp = crate::ssh::sftp_connect(host).await?;
    let mut src = sftp.open(from).await?;
    let mut buf = Vec::new();
    src.read_to_end(&mut buf).await?;
    let mut dst = sftp.create(to).await?;
    dst.write_all(&buf).await?;
    dst.flush().await?;
    Ok(format!("已复制 {from} → {to}（{}）", human_size(buf.len() as u64)))
}

/// 「打开方式」用的本地暂存路径：~/.daiqiao/edit/<主机>/<远端路径>
/// 保留原始文件名和后缀，系统才能挑对应用、编辑器也才有语法高亮。
pub fn edit_stage_path(host: &str, remote: &str) -> std::path::PathBuf {
    let safe_host: String = host
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect();
    let name = remote.rsplit('/').next().unwrap_or("file");
    // 远端目录结构压进一层，避免同名文件互相覆盖
    let parent = remote.trim_end_matches(name).trim_end_matches('/');
    let digest: u32 = parent.bytes().fold(2166136261u32, |h, b| {
        (h ^ b as u32).wrapping_mul(16777619)
    });
    crate::keys::daiqiao_dir()
        .join("edit")
        .join(safe_host)
        .join(format!("{digest:08x}"))
        .join(name)
}

/// 盯着本地暂存文件，改了就回传。
///
/// 用轮询比上 notify 依赖划算：这里要盯的就一个文件，1 秒一次足够，
/// 而且不用处理编辑器「写临时文件再 rename」导致的 inode 变化（轮询天然免疫）。
/// 30 分钟没动静就收摊，不常驻。
pub fn watch_and_sync_back(
    app: tauri::AppHandle,
    host: String,
    remote: String,
    local: std::path::PathBuf,
) {
    use tauri::Emitter;

    let mtime0 = std::fs::metadata(&local).and_then(|m| m.modified()).ok();
    tauri::async_runtime::spawn(async move {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30 * 60);
        let mut last = mtime0;
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(1000)).await;
            if std::time::Instant::now() > deadline {
                log::info!("「打开方式」回传监听超时收摊: {remote}");
                return;
            }
            let now = match std::fs::metadata(&local).and_then(|m| m.modified()) {
                Ok(t) => Some(t),
                // 文件被编辑器挪走了（保存策略不同），下一轮再看
                Err(_) => continue,
            };
            if now == last {
                continue;
            }
            last = now;

            let h = match crate::hosts::HostStore::load().find(&host).cloned() {
                Some(h) => h,
                None => {
                    log::warn!("回传时找不到主机 {host}，监听结束");
                    return;
                }
            };
            match upload(&h, &local.to_string_lossy(), &remote).await {
                Ok(n) => {
                    crate::audit::human(
                        "回传·保存后自动上传",
                        Some(format!("{host}:{remote}（{}）", human_size(n))),
                    );
                    let _ = app.emit(
                        "sftp://synced",
                        serde_json::json!({
                            "host": host, "remote": remote, "bytes": n, "ok": true
                        }),
                    );
                }
                Err(e) => {
                    let _ = app.emit(
                        "sftp://synced",
                        serde_json::json!({
                            "host": host, "remote": remote, "ok": false, "error": e.to_string()
                        }),
                    );
                }
            }
        }
    });
}

/// 右键「打开方式」的候选应用。
///
/// 走 LaunchServices：先问系统「这个后缀默认用谁打开」，再补几个常用编辑器。
/// 只列真的装了的，避免点了没反应。
pub fn open_candidates(name: &str) -> Vec<serde_json::Value> {
    let mut out: Vec<serde_json::Value> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    // 默认应用（系统认定的那个）
    out.push(serde_json::json!({ "name": "默认应用", "app": "", "default": true }));

    let ext = name.rsplit('.').next().unwrap_or("").to_lowercase();
    let text_like = matches!(
        ext.as_str(),
        "txt" | "md" | "json" | "yaml" | "yml" | "toml" | "conf" | "cfg" | "ini" | "log"
            | "sh" | "zsh" | "bash" | "py" | "rs" | "js" | "ts" | "tsx" | "jsx" | "go"
            | "java" | "c" | "h" | "cpp" | "hpp" | "css" | "html" | "xml" | "sql" | "env"
    ) || !name.contains('.');

    let candidates: &[(&str, &str)] = &[
        ("Visual Studio Code", "/Applications/Visual Studio Code.app"),
        ("Cursor", "/Applications/Cursor.app"),
        ("Sublime Text", "/Applications/Sublime Text.app"),
        ("BBEdit", "/Applications/BBEdit.app"),
        ("Zed", "/Applications/Zed.app"),
        ("TextEdit", "/System/Applications/TextEdit.app"),
        ("Xcode", "/Applications/Xcode.app"),
        ("预览", "/System/Applications/Preview.app"),
    ];
    for (label, path) in candidates {
        if !std::path::Path::new(path).exists() {
            continue;
        }
        // 非文本文件就不推编辑器了，只留预览和默认
        if !text_like && !matches!(*label, "预览") {
            continue;
        }
        if seen.insert((*label).to_string()) {
            out.push(serde_json::json!({ "name": label, "app": label, "default": false }));
        }
    }
    out
}
