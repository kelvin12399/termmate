//! 审计留痕。硬指标之一：谁、什么时候、在哪个会话、干了什么，全部落盘。
//!
//! 两边都记：
//!   - AI 侧：每一次 MCP 工具调用（带客户端名、耗时、成功失败）
//!   - 人侧：界面上的配置改动（增删改主机、导入/生成/删除密钥）
//!
//! 凭据绝不进日志。这条比「记全」优先：
//!   - `send_input` 只记字节数，不记内容 —— 那里可能正在回答 sudo 的密码提示
//!   - `exec` 的命令要记（审计的意义就在这），但先过一遍常见密钥参数的打码
//!
//! 文件：~/.daiqiao/audit.jsonl，0600，append-only。超过 5MB 滚一次。

use serde::{Deserialize, Serialize};
use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;

const MAX_BYTES: u64 = 5 * 1024 * 1024;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Entry {
    /// 本地时间 RFC3339
    pub ts: String,
    /// "ai" | "human"
    pub actor: String,
    /// AI 侧是客户端名（claude-code / codex / cursor…），人侧是 "界面"
    pub client: String,
    /// 工具名或界面动作名
    pub action: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    /// 人能读的一句话：跑了什么命令、改了哪台主机
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ms: Option<u64>,
}

pub fn path() -> PathBuf {
    crate::keys::daiqiao_dir().join("audit.jsonl")
}

fn now() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // 不拉 chrono，手算本地时间（够审计读）
    let off = local_offset_secs();
    let t = secs as i64 + off;
    let days = t.div_euclid(86400);
    let rem = t.rem_euclid(86400);
    let (y, m, d) = civil_from_days(days);
    let sign = if off < 0 { '-' } else { '+' };
    let oa = off.abs();
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}{sign}{:02}:{:02}",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60,
        oa / 3600,
        (oa % 3600) / 60
    )
}

/// 本机 UTC 偏移（秒）。问一次系统 `date +%z` 然后缓存 ——
/// 不为了一个时区戳去加 libc/chrono 依赖，也不猜固定 +8。
fn local_offset_secs() -> i64 {
    static OFF: std::sync::OnceLock<i64> = std::sync::OnceLock::new();
    *OFF.get_or_init(|| {
        let out = match std::process::Command::new("/bin/date").arg("+%z").output() {
            Ok(o) => o,
            Err(_) => return 0,
        };
        let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
        // 形如 +0800 / -0500
        if s.len() < 5 {
            return 0;
        }
        let sign: i64 = if s.starts_with('-') { -1 } else { 1 };
        let h: i64 = s[1..3].parse().unwrap_or(0);
        let m: i64 = s[3..5].parse().unwrap_or(0);
        sign * (h * 3600 + m * 60)
    })
}

/// Howard Hinnant 的 civil_from_days
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// 命令里常见的「把密码写在参数上」写法，落盘前打码。
/// 做不到滴水不漏，但该挡的几种写法必须挡住。
///
/// 被对抗验证抛回来的问题（纣进在前面都修正了）：
/// - 打码之前先把引号包住的整段值割出来（否则 `PASSWORD='a b'` 只打第一个单词，后半段泄露）
/// - 名单改成大小写不敏感匹配（`Password=`/`PassWord=` 也得挡），并补上真实世界常见的变体（PWD/PASS/PAT/AUTHORIZATION 等）
/// - URL 里内嵌的 `user:pass@host` 出现很频繁（psql/git clone 连接串），单独来一轮
/// - `Authorization: Bearer xxx` 这类 HTTP 头写法和 KEY=VALUE 形式差很远，叕自己匹配
pub fn redact(cmd: &str) -> String {
    let mut out = cmd.to_string();

    // URL 里内嵌的凭据： scheme://user:pass@host → scheme://user:***@host
    out = redact_url_creds(&out);

    // HTTP 头写法：Authorization: Bearer xxx / Authorization: Basic xxx
    out = redact_header(&out, "Authorization:");
    out = redact_header(&out, "authorization:");

    // KEY=value 形式。名单注量覆盖常见写法，匹配时对 KEY 部分大小写不敏感
    for k in [
        "PASSWORD", "PASSWD", "PASS", "PWD", "TOKEN", "SECRET", "API_KEY", "APIKEY",
        "ACCESS_KEY", "PRIVATE_KEY", "AUTH", "CREDENTIAL", "PAT", "COOKIE", "SESSION_KEY",
    ] {
        out = redact_kv_ci(&out, k);
    }
    // --password=xxx / --token xxx 这类命令行参数
    for k in ["--password", "--passwd", "--token", "--secret", "--api-key"] {
        out = redact_kv(&out, &format!("{k}="), '=');
        out = redact_kv(&out, &format!("{k} "), ' ');
    }
    // mysql -pSECRET（紧贴的 -p）
    out = redact_mysql_p(&out);
    out
}

/// 值的边界：如果值以 `'` 或 `"` 开头，跟到对应的配对引号为止（包含引号本身，一并打码）；
/// 没有引号就做到第一个空格/分号/管道/连接符为止
/// （和原来行为一致——没引号时谁知道哪里算值结束，只是现在带引号时不再被引号里的空格邝断）。
fn value_end(rest: &str) -> usize {
    match rest.chars().next() {
        Some(q @ ('\'' | '"')) => {
            // 找到配对的收尾引号；找不到就当整段都是值
            match rest[1..].find(q) {
                Some(j) => j + 2,
                None => rest.len(),
            }
        }
        _ => rest
            .find(|c: char| c.is_whitespace() || c == ';' || c == '|' || c == '&')
            .unwrap_or(rest.len()),
    }
}

fn redact_kv(s: &str, needle: &str, _sep: char) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find(needle) {
        out.push_str(&rest[..i + needle.len()]);
        rest = &rest[i + needle.len()..];
        let end = value_end(rest);
        if end > 0 {
            out.push_str("***");
        }
        rest = &rest[end..];
    }
    out.push_str(rest);
    out
}

/// `KEY=value` 形式，对 KEY 部分大小写不敏感（`Password=`/`PASSWORD=`/`PassWord=` 都挡）。
/// 通过小写全文找匹配位置再回原文取真实长度，避免字节长度不一致的问题。
fn redact_kv_ci(s: &str, key: &str) -> String {
    let low_s = s.to_lowercase();
    let low_needle = format!("{}=", key.to_lowercase());
    let mut out = String::with_capacity(s.len());
    let mut pos = 0usize;
    while let Some(rel) = low_s[pos..].find(&low_needle) {
        let i = pos + rel;
        let kv_end = i + low_needle.len();
        out.push_str(&s[pos..kv_end]);
        let rest = &s[kv_end..];
        let end = value_end(rest);
        if end > 0 {
            out.push_str("***");
        }
        pos = kv_end + end;
    }
    out.push_str(&s[pos..]);
    out
}

/// `scheme://user:pass@host` → `scheme://user:***@host`（psql / git clone 连接串常见写法）
fn redact_url_creds(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    loop {
        // 找 `://`，再在其后面找 `user:pass@`
        let Some(scheme_at) = rest.find("://") else {
            out.push_str(rest);
            break;
        };
        let after_scheme = scheme_at + 3;
        let tail = &rest[after_scheme..];
        let Some(at) = tail.find('@') else {
            out.push_str(&rest[..after_scheme]);
            rest = tail;
            continue;
        };
        let userinfo = &tail[..at];
        if let Some(colon) = userinfo.find(':') {
            out.push_str(&rest[..after_scheme]);
            out.push_str(&userinfo[..colon]);
            out.push_str(":***");
            out.push('@');
            rest = &tail[at + 1..];
        } else {
            // 没有密码部分，原样保留
            out.push_str(&rest[..after_scheme + at + 1]);
            rest = &tail[at + 1..];
        }
    }
    out
}

/// `Authorization: Bearer xxx` 这类 HTTP 头：冒号后面到行末/分号/管道为止整段打码
fn redact_header(s: &str, needle: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find(needle) {
        out.push_str(&rest[..i + needle.len()]);
        rest = &rest[i + needle.len()..];
        // 跳过头部空格，打码值部分
        let ws = rest.len() - rest.trim_start().len();
        out.push_str(&rest[..ws]);
        rest = &rest[ws..];
        let end = rest
            .find(|c: char| c == '\'' || c == ';' || c == '\n')
            .unwrap_or(rest.len());
        if end > 0 {
            out.push_str("***");
        }
        rest = &rest[end..];
    }
    out.push_str(rest);
    out
}

/// `mysql -pSECRET` → `mysql -p***`（`-p` 后面紧跟非空白才算密码）
fn redact_mysql_p(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find("-p") {
        out.push_str(&rest[..i + 2]);
        rest = &rest[i + 2..];
        // 和 KEY=value 用同一套边界规则：带引号时跟到配对引号，别被引号里的空格邙断
        let end = value_end(rest);
        // 只有紧贴着内容才打码；`-p` 单独一个词是「提示输入密码」，不动
        if end > 0 && !rest.starts_with('-') {
            out.push_str("***");
            rest = &rest[end..];
        }
    }
    out.push_str(rest);
    out
}

/// 写一条。审计失败不能拖垮正常操作，所以只记日志不返回错误。
pub fn write(e: Entry) {
    if let Err(err) = append(&e) {
        log::warn!("审计写入失败: {err}");
    }
}

fn append(e: &Entry) -> std::io::Result<()> {
    let p = path();
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir)?;
    }
    rotate_if_big(&p);

    let line = serde_json::to_string(e).unwrap_or_default();
    let mut f = OpenOptions::new().create(true).append(true).open(&p)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = f.set_permissions(std::fs::Permissions::from_mode(0o600));
    }
    writeln!(f, "{line}")
}

fn rotate_if_big(p: &std::path::Path) {
    if let Ok(md) = std::fs::metadata(p) {
        if md.len() > MAX_BYTES {
            let _ = std::fs::rename(p, p.with_extension("jsonl.1"));
        }
    }
}

/// 读最近 n 条（最新在最后）。可按 actor / 会话过滤。
pub fn tail(n: usize, actor: Option<&str>, session: Option<&str>) -> Vec<Entry> {
    let p = path();
    let f = match std::fs::File::open(&p) {
        Ok(f) => f,
        Err(_) => return Vec::new(),
    };
    let mut all: Vec<Entry> = BufReader::new(f)
        .lines()
        .map_while(Result::ok)
        .filter_map(|l| serde_json::from_str::<Entry>(&l).ok())
        .filter(|e| actor.is_none_or(|a| e.actor == a))
        .filter(|e| session.is_none_or(|s| e.session.as_deref() == Some(s)))
        .collect();
    if all.len() > n {
        all.drain(..all.len() - n);
    }
    all
}

/// AI 侧：一次 MCP 工具调用
pub fn ai(client: &str, action: &str, session: Option<String>, detail: Option<String>, ok: bool, ms: u64) {
    write(Entry {
        ts: now(),
        actor: "ai".into(),
        client: client.to_string(),
        action: action.to_string(),
        session,
        detail,
        ok,
        ms: Some(ms),
    });
}

/// 人侧：界面上的配置改动
pub fn human(action: &str, detail: Option<String>) {
    write(Entry {
        ts: now(),
        actor: "human".into(),
        client: "界面".into(),
        action: action.to_string(),
        session: None,
        detail,
        ok: true,
        ms: None,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 打码挡住常见的密码写法() {
        assert_eq!(redact("mysql -uroot -pHunter2 db"), "mysql -uroot -p*** db");
        assert_eq!(
            redact("curl -H x --token=abc123 https://x"),
            "curl -H x --token=*** https://x"
        );
        assert_eq!(
            redact("PASSWORD=s3cret ./deploy.sh"),
            "PASSWORD=*** ./deploy.sh"
        );
        assert_eq!(redact("export api_key=zzz"), "export api_key=***");
    }

    #[test]
    fn 单独的_p_不是密码不该打码() {
        assert_eq!(redact("mysql -u root -p"), "mysql -u root -p");
        assert_eq!(redact("mkdir -p /tmp/x"), "mkdir -p /tmp/x");
    }

    #[test]
    fn 对抗验证报的绕过写法全部挡住() {
        // 带空格的引号值：以前只打到第一个空白，后半段泄漏
        assert_eq!(redact("PASSWORD='a b' echo done"), "PASSWORD=*** echo done");
        assert_eq!(redact("mysql -p\"with space\" --help"), "mysql -p*** --help");
        // 混合大小写
        assert_eq!(redact("Password=MixedCaseLeak"), "Password=***");
        assert_eq!(redact("PassWord=WeIrDcAsE"), "PassWord=***");
        // 名单覆盖面
        assert_eq!(redact("export MYSQL_PWD=leaky"), "export MYSQL_PWD=***");
        assert_eq!(redact("export DB_PASS=leaky"), "export DB_PASS=***");
        assert_eq!(redact("export GH_PAT=leaky"), "export GH_PAT=***");
        // HTTP 头
        assert_eq!(
            redact("curl -H 'Authorization: Bearer sk-live-LEAK' https://x"),
            "curl -H 'Authorization: ***' https://x"
        );
        // URL 内嵌凭据
        assert_eq!(
            redact("psql 'postgres://user:Pa55w0rd@host/db'"),
            "psql 'postgres://user:***@host/db'"
        );
        assert_eq!(
            redact("git clone https://user:ghp_LEAK@github.com/a/b"),
            "git clone https://user:***@github.com/a/b"
        );
        // 没有密码的 URL 不该被动
        assert_eq!(
            redact("git clone https://github.com/a/b"),
            "git clone https://github.com/a/b"
        );
    }

    #[test]
    fn 普通命令原样留痕() {
        assert_eq!(redact("ls -la /etc"), "ls -la /etc");
        assert_eq!(redact("git push origin main"), "git push origin main");
    }

    #[test]
    fn 时间戳是能解析的_rfc3339() {
        let t = now();
        assert!(t.len() >= 25, "{t}");
        assert_eq!(&t[4..5], "-");
        assert_eq!(&t[10..11], "T");
    }
}
