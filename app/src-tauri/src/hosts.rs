//! SSH 主机配置。
//!
//! 存 `~/.daiqiao/hosts.json` —— **只存主机信息，绝不存密码**。
//! 密码走系统钥匙串，AI 只知道配置名（需求里的安全红线）。

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Host {
    pub id: String,
    pub name: String,
    pub hostname: String,
    #[serde(default = "default_port")]
    pub port: u16,
    pub user: String,
    /// "key" | "password" | "agent"
    #[serde(default = "default_auth")]
    pub auth: String,
    /// 密钥库里的密钥 id（auth = key 时用，指向 keys::KeyInfo.id）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_id: Option<String>,
    /// 旧字段，仅为兼容方向 A 之前存的主机：直接给路径，不经过密钥库。
    /// 新建/编辑主机不再写它，但读到的旧主机继续认它。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key_path: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// 所属分组（像 Termius 的 Groups），空则归入「未分组」
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

fn default_port() -> u16 {
    22
}
fn default_auth() -> String {
    "key".to_string()
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HostStore {
    #[serde(default)]
    pub hosts: Vec<Host>,
    #[serde(default)]
    pub seq: u64,
}

impl HostStore {
    pub fn path() -> Option<PathBuf> {
        let home = std::env::var("HOME").ok()?;
        Some(PathBuf::from(home).join(".daiqiao").join("hosts.json"))
    }

    pub fn load() -> Self {
        let Some(p) = Self::path() else {
            return Self::default();
        };
        match std::fs::read_to_string(&p) {
            Ok(s) => serde_json::from_str(&s).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    pub fn save(&self) -> anyhow::Result<()> {
        let p = Self::path().ok_or_else(|| anyhow::anyhow!("找不到 HOME 目录"))?;
        if let Some(dir) = p.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text = serde_json::to_string_pretty(self)?;
        std::fs::write(&p, text)?;
        // 里面可能有内网地址，别让同机其他用户随便看
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600));
        }
        Ok(())
    }

    pub fn next_id(&mut self) -> String {
        self.seq += 1;
        format!("h{}", self.seq)
    }

    pub fn find(&self, key: &str) -> Option<&Host> {
        // id / 显示名优先。其次允许精确地址和 user@host:port，主机改名后
        // Agent 仍然能按它知道的地址找到配置。
        if let Some(h) = self.hosts.iter().find(|h| h.id == key || h.name == key) {
            return Some(h);
        }
        if let Some(h) = self.hosts.iter().find(|h| {
            format!("{}@{}:{}", h.user, h.hostname, h.port) == key
        }) {
            return Some(h);
        }
        // 同一地址可能按不同用户/端口保存了多台。只有唯一匹配时才按
        // hostname 返回，否则宁可报找不到，也绝不猜一台执行命令。
        let mut matches = self.hosts.iter().filter(|h| h.hostname == key);
        let first = matches.next()?;
        if matches.next().is_none() {
            Some(first)
        } else {
            None
        }
    }

}

#[cfg(test)]
mod find_tests {
    use super::*;

    fn host(id: &str, name: &str, hostname: &str, user: &str, port: u16) -> Host {
        Host {
            id: id.into(),
            name: name.into(),
            hostname: hostname.into(),
            port,
            user: user.into(),
            auth: "key".into(),
            key_id: None,
            key_path: None,
            tags: vec![],
            group: None,
            note: None,
        }
    }

    #[test]
    fn 改名后仍能按唯一地址找到() {
        let store = HostStore {
            hosts: vec![host("h1", "生产网关", "203.0.113.10", "root", 22)],
            seq: 1,
        };
        assert_eq!(store.find("生产网关").unwrap().id, "h1");
        assert_eq!(store.find("203.0.113.10").unwrap().id, "h1");
        assert_eq!(store.find("root@203.0.113.10:22").unwrap().id, "h1");
    }

    #[test]
    fn 同地址多配置不猜但完整端点可区分() {
        let store = HostStore {
            hosts: vec![
                host("h1", "root 入口", "example.com", "root", 22),
                host("h2", "deploy 入口", "example.com", "deploy", 22),
            ],
            seq: 2,
        };
        assert!(store.find("example.com").is_none(), "同地址多台时不能猜");
        assert_eq!(store.find("root@example.com:22").unwrap().id, "h1");
        assert_eq!(store.find("deploy@example.com:22").unwrap().id, "h2");
        assert_eq!(store.find("deploy 入口").unwrap().id, "h2");
    }
}

/// 从 `~/.ssh/config` 解析出来的一条候选主机
#[derive(Debug, Clone, Serialize)]
pub struct SshConfigEntry {
    pub alias: String,
    pub hostname: String,
    pub user: String,
    pub port: u16,
    pub identity: Option<String>,
}

fn expand_tilde(p: &str) -> String {
    if let Some(rest) = p.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return format!("{home}/{rest}");
        }
    }
    p.to_string()
}

/// ssh config 里 IdentityAgent 的显式设置。
#[derive(Debug, Clone, PartialEq)]
pub enum IdentityAgent {
    Socket(String),
    Disabled,
}

fn host_glob(pattern: &str, host: &str) -> bool {
    fn go(p: &[u8], h: &[u8]) -> bool {
        match p.first() {
            None => h.is_empty(),
            Some(b'*') => go(&p[1..], h) || (!h.is_empty() && go(p, &h[1..])),
            Some(b'?') => !h.is_empty() && go(&p[1..], &h[1..]),
            Some(c) => !h.is_empty() && h[0].eq_ignore_ascii_case(c) && go(&p[1..], &h[1..]),
        }
    }
    go(pattern.as_bytes(), host.as_bytes())
}

fn host_section_matches(patterns: &str, host: Option<&str>) -> bool {
    let Some(host) = host else {
        // 没指定主机时，只让全局 Host * 生效，别拿别人的专属配置冒充默认
        return patterns.split_whitespace().any(|p| p == "*");
    };
    let mut positive = false;
    for pattern in patterns.split_whitespace() {
        if let Some(neg) = pattern.strip_prefix('!') {
            if host_glob(neg, host) {
                return false;
            }
        } else if host_glob(pattern, host) {
            positive = true;
        }
    }
    positive
}

fn unquote(value: &str) -> &str {
    let v = value.trim();
    if v.len() >= 2
        && ((v.starts_with('"') && v.ends_with('"')) || (v.starts_with('\'') && v.ends_with('\'')))
    {
        &v[1..v.len() - 1]
    } else {
        v
    }
}

/// 从 ssh config 文本解析某台主机实际该用的 agent。
/// 和 OpenSSH 一样：第一个匹配到的 IdentityAgent 生效，后面的不覆盖。
fn identity_agent_from_config(text: &str, host: Option<&str>) -> Option<IdentityAgent> {
    let mut section_matches = false;
    let mut selected = None;

    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, value) = match line.split_once('=') {
            Some((k, v)) => (k.trim(), v.trim()),
            None => match line.split_once(char::is_whitespace) {
                Some((k, v)) => (k.trim(), v.trim()),
                None => continue,
            },
        };
        if key.eq_ignore_ascii_case("host") {
            section_matches = host_section_matches(value, host);
            continue;
        }
        if section_matches && key.eq_ignore_ascii_case("identityagent") && selected.is_none() {
            let value = unquote(value);
            selected = if value.eq_ignore_ascii_case("none") {
                Some(IdentityAgent::Disabled)
            } else {
                Some(IdentityAgent::Socket(expand_tilde(value)))
            };
        }
    }
    selected
}

/// 读取某台主机的 IdentityAgent；没有显式配置时返回 None。
pub fn configured_identity_agent(host: Option<&str>) -> Option<IdentityAgent> {
    let home = std::env::var("HOME").ok()?;
    let text = std::fs::read_to_string(format!("{home}/.ssh/config")).ok()?;
    identity_agent_from_config(&text, host)
}

/// 实际连接要用的 agent socket：config 优先，环境变量兜底。
pub fn agent_socket_for(host: Option<&str>) -> Option<String> {
    match configured_identity_agent(host) {
        Some(IdentityAgent::Socket(path)) => Some(path),
        Some(IdentityAgent::Disabled) => None,
        None => std::env::var("SSH_AUTH_SOCK").ok().filter(|s| !s.is_empty()),
    }
}

#[cfg(test)]
mod agent_config_tests {
    use super::*;

    #[test]
    fn host通配符和否定项() {
        assert!(host_section_matches("*", Some("a.example.com")));
        assert!(host_section_matches("*.example.com", Some("a.example.com")));
        assert!(!host_section_matches("*.example.com", Some("example.org")));
        assert!(!host_section_matches("* !bad.example.com", Some("bad.example.com")));
        assert!(host_section_matches("* !bad.example.com", Some("ok.example.com")));
        assert!(!host_section_matches("specific", None));
        assert!(host_section_matches("*", None));
    }

    #[test]
    fn 读带空格的1password路径() {
        let cfg = r#"
Host *
    IdentityAgent "~/Library/Group Containers/2BUA8C4S2C.com.1password/t/agent.sock"
"#;
        let got = identity_agent_from_config(cfg, Some("203.0.113.10"));
        assert_eq!(
            got,
            Some(IdentityAgent::Socket(expand_tilde(
                "~/Library/Group Containers/2BUA8C4S2C.com.1password/t/agent.sock"
            )))
        );
    }

    #[test]
    fn 主机专属配置优先且不读串() {
        let cfg = r#"
Host special
    IdentityAgent /tmp/special.sock
Host *
    IdentityAgent /tmp/default.sock
"#;
        assert_eq!(
            identity_agent_from_config(cfg, Some("special")),
            Some(IdentityAgent::Socket("/tmp/special.sock".into()))
        );
        assert_eq!(
            identity_agent_from_config(cfg, Some("other")),
            Some(IdentityAgent::Socket("/tmp/default.sock".into()))
        );
        assert_eq!(
            identity_agent_from_config(cfg, Some("nomatch")),
            Some(IdentityAgent::Socket("/tmp/default.sock".into())),
            "Host * 本来就应该兜住没有专属配置的主机"
        );
        assert_eq!(
            identity_agent_from_config("Host special\n  IdentityAgent /tmp/special.sock\n", Some("other")),
            None,
            "只有专属块、又不匹配时才没有配置"
        );
    }

    #[test]
    fn 第一个匹配值生效() {
        let cfg = "Host *\n  IdentityAgent /tmp/first.sock\n  IdentityAgent /tmp/second.sock\n";
        assert_eq!(
            identity_agent_from_config(cfg, Some("x")),
            Some(IdentityAgent::Socket("/tmp/first.sock".into()))
        );
    }

    #[test]
    fn 显式none不会退回环境变量配置() {
        let cfg = "Host *\n  IdentityAgent none\n";
        assert_eq!(
            identity_agent_from_config(cfg, Some("x")),
            Some(IdentityAgent::Disabled)
        );
    }
}

/// 解析 `~/.ssh/config`。
///
/// 只认真正能连的条目：跳过带通配符的（`Host *`）和只有别名的块。
/// 这里**不读私钥内容**，只取 `IdentityFile` 的路径。
pub fn parse_ssh_config() -> Vec<SshConfigEntry> {
    let Some(home) = std::env::var("HOME").ok() else {
        return Vec::new();
    };
    let Ok(text) = std::fs::read_to_string(format!("{home}/.ssh/config")) else {
        return Vec::new();
    };

    let current_user = std::env::var("USER").unwrap_or_else(|_| "root".to_string());
    let mut out: Vec<SshConfigEntry> = Vec::new();
    let mut cur: Option<SshConfigEntry> = None;

    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // 支持 "Host a b c" 和 "Key=Value" 两种写法
        let (key, value) = match line.split_once('=') {
            Some((k, v)) => (k.trim(), v.trim()),
            None => match line.split_once(char::is_whitespace) {
                Some((k, v)) => (k.trim(), v.trim()),
                None => continue,
            },
        };
        let key_l = key.to_ascii_lowercase();

        if key_l == "host" {
            if let Some(prev) = cur.take() {
                if !prev.hostname.is_empty() {
                    out.push(prev);
                }
            }
            // 这次编辑走的是新密钥库，旧的直接路径字段作废，
            let alias = value.split_whitespace().next().unwrap_or("").to_string();
            if alias.is_empty() || alias.contains('*') || alias.contains('?') {
                cur = None;
                continue;
            }
            cur = Some(SshConfigEntry {
                hostname: alias.clone(),
                alias,
                user: current_user.clone(),
                port: 22,
                identity: None,
            });
            continue;
        }

        let Some(entry) = cur.as_mut() else { continue };
        match key_l.as_str() {
            "hostname" => entry.hostname = value.to_string(),
            "user" => entry.user = value.to_string(),
            "port" => {
                if let Ok(p) = value.parse::<u16>() {
                    entry.port = p;
                }
            }
            "identityfile" => entry.identity = Some(expand_tilde(value)),
            _ => {}
        }
    }
    if let Some(prev) = cur {
        if !prev.hostname.is_empty() {
            out.push(prev);
        }
    }

    // 去掉已存在的主机（按 hostname:port 判重）
    let existing: Vec<(String, u16)> = HostStore::load()
        .hosts
        .iter()
        .map(|h| (h.hostname.clone(), h.port))
        .collect();
    out.retain(|e| !existing.iter().any(|(hn, p)| hn == &e.hostname && *p == e.port));
    out
}

/// 主机配置的对外视图：**不含任何凭据**
pub fn host_view(h: &Host) -> serde_json::Value {
    serde_json::json!({
        "id": h.id,
        "name": h.name,
        "hostname": h.hostname,
        "port": h.port,
        "user": h.user,
        "auth": h.auth,
        "key_id": h.key_id,
        "key_path": h.key_path,
        "resolved_key_path": resolve_key_path(h),
        // 密码只报「存没存」，内容永远不出钥匙串
        "password_saved": if h.auth == "password" { crate::keychain::saved(&h.id) } else { false },
        "tags": h.tags,
        "group": h.group,
        "note": h.note,
    })
}

/// 新增/修改一台主机（前端表单和 MCP 工具用同一套参数）
pub fn upsert_host(
    store: &mut HostStore,
    key: Option<&str>,
    name: &str,
    hostname: &str,
    user: &str,
    port: u16,
    auth: &str,
    key_id: Option<String>,
    group: Option<String>,
    note: Option<String>,
) -> Result<String, String> {
    match key {
        // 改已有主机
        Some(k) => {
            let idx = store
                .hosts
                .iter()
                .position(|h| h.id == k || h.name == k)
                .ok_or_else(|| format!("找不到主机 {k}"))?;
            let h = &mut store.hosts[idx];
            h.name = name.to_string();
            h.hostname = hostname.to_string();
            h.user = user.to_string();
            h.port = port;
            h.auth = auth.to_string();
            h.key_id = key_id;
            // 这���编辑是走新的密钥库了，旧的直接路径字段作废，
            // 否则 ssh.rs 两个字段都认、谁优先会很混乱
            h.key_path = None;
            h.group = group;
            h.note = note;
            Ok(h.id.clone())
        }
        // 新建
        None => {
            if store.hosts.iter().any(|h| h.name == name) {
                return Err(format!("已经有一台叫「{name}」的主机了"));
            }
            let id = store.next_id();
            store.hosts.push(Host {
                id: id.clone(),
                name: name.to_string(),
                hostname: hostname.to_string(),
                port,
                user: user.to_string(),
                auth: auth.to_string(),
                key_id,
                key_path: None,
                tags: Vec::new(),
                group,
                note,
            });
            Ok(id)
        }
    }
}

/// 一台主机实际应该用哪个私钥路径：新字段 `key_id` 优先（查密钥库拿路径），
/// 没有就退回旧字段 `key_path`（兼容方向 A 之前存的主机）。
pub fn resolve_key_path(h: &Host) -> Option<String> {
    if let Some(id) = &h.key_id {
        if let Some(info) = crate::keys::find_key(id, &std::collections::HashMap::new()) {
            return Some(info.path);
        }
    }
    h.key_path.clone()
}
