//! 主机密钥校验（known_hosts）。
//!
//! 这东西防的不是黑客，是**连错机器**：
//!   · 服务器重装过 → 密钥变了 → 该喊停，而不是悄悄连上去让人把命令打进陌生机器
//!   · DHCP 把同一个 IP 分给了别的设备 → 同上
//!
//! ## 安全基调：拿不准就拒连，绝不 TOFU
//!
//! 第一版栽在这上面：读文件失败、哈希行比不了、`@revoked` 解析不出来，
//! 都被压成「没见过这台主机」→ 走 TOFU 放行并记成可信。
//! 等于**已有的信任记录被静默丢弃**，一个非 UTF-8 字节就能让整个信任库作废。
//!
//! 现在分得很清：
//!   `Match`        指纹和记录一致 → 放行
//!   `Unknown`      **确实**没有任何记录 → 首次见面，TOFU 记下并放行
//!   `Changed`      有记录但指纹不符 → 硬拒
//!   `Unverifiable` 有记录/有文件但没法比对（哈希行、CA 行、读不了、解不开）→ **硬拒**
//!   `Revoked`      密钥被 `@revoked` 标记吊销 → **永久拒绝**（OpenSSH 的语义）
//!
//! 只有 `Unknown` 才 TOFU，而 `Unknown` 现在要求「文件都读通了，确实一条都没匹配上」。
//!
//! ## 读写范围
//! 读的时候同时看 `~/.ssh/known_hosts` 和代敲自己的库 —— ssh 命令已经信任过的
//! 主机，代敲不用再问一次。写只写代敲自己的库，绝不改用户的 ssh 配置。

use base64::Engine;
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use std::path::PathBuf;

/// 校验结果
#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    /// 指纹和记录一致
    Match,
    /// 确实没有任何记录（文件都读通了）
    Unknown,
    /// 见过，但指纹变了 —— 危险
    Changed { known: Vec<String> },
    /// 有记录或有文件，但没法完成比对 —— 不能当成「没见过」
    Unverifiable { reason: String },
    /// 这把密钥被 @revoked 吊销了
    Revoked,
}

/// 代敲自己的 known_hosts（不碰 ~/.ssh/known_hosts）
pub fn daiqiao_known_hosts() -> PathBuf {
    crate::keys::daiqiao_dir().join("known_hosts")
}

fn ssh_known_hosts() -> Option<PathBuf> {
    std::env::var("HOME")
        .ok()
        .map(|h| PathBuf::from(h).join(".ssh/known_hosts"))
}

/// 算指纹，格式和 `ssh-keygen -lf` 完全一致：SHA256:<base64 无 padding>
pub fn fingerprint(key_blob: &[u8]) -> String {
    let digest = Sha256::digest(key_blob);
    let b64 = base64::engine::general_purpose::STANDARD_NO_PAD.encode(digest);
    format!("SHA256:{b64}")
}

/// known_hosts 里主机名的写法：标准端口写 `host`，非标准端口写 `[host]:port`
fn host_patterns(host: &str, port: u16) -> Vec<String> {
    if port == 22 {
        vec![host.to_string(), format!("[{host}]:22")]
    } else {
        vec![format!("[{host}]:{port}")]
    }
}

/// 一行里的标记（OpenSSH 的格式是 `@marker hosts keytype key`，**标记在行首**）
#[derive(Debug, Clone, Copy, PartialEq)]
enum Marker {
    None,
    /// 这把密钥被吊销，永久不可信
    Revoked,
    /// 这是签发主机证书的 CA 公钥，不是主机密钥本身
    CertAuthority,
}

/// 解析好的一行
#[derive(Debug, Clone)]
struct Line {
    marker: Marker,
    /// 主机字段原文（可能是逗号分隔、通配符，或 `|1|salt|hash` 哈希形式）
    hosts: String,
    fingerprint: String,
}

/// 拆一行 known_hosts。认不出就返回 None。
///
/// 真实格式（marker 可选，且在**行首**）：
///   `[@marker] host[,host2,...] keytype base64key [comment]`
fn parse_line(line: &str) -> Option<Line> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let mut parts = line.split_whitespace();
    let mut first = parts.next()?;

    // 标记在行首 —— 第一版错在以为它在主机名后面
    let marker = match first {
        "@revoked" => {
            first = parts.next()?;
            Marker::Revoked
        }
        "@cert-authority" => {
            first = parts.next()?;
            Marker::CertAuthority
        }
        _ => Marker::None,
    };

    let hosts = first.to_string();
    let _keytype = parts.next()?;
    let b64 = parts.next()?;
    let blob = base64::engine::general_purpose::STANDARD.decode(b64).ok()?;
    Some(Line {
        marker,
        hosts,
        fingerprint: fingerprint(&blob),
    })
}

type HmacSha1 = Hmac<sha1::Sha1>;

/// 哈希形式的主机字段：`|1|<base64 salt>|<base64 hmac-sha1(salt, host)>`
///
/// `HashKnownHosts yes` 在不少 Linux 发行版是默认。第一版把这种行当「比不了」
/// 直接丢掉，结果是整个库形同虚设 —— 必须真做 HMAC 比对。
fn hashed_matches(field: &str, candidates: &[String]) -> Option<bool> {
    let rest = field.strip_prefix("|1|")?;
    let (salt_b64, hash_b64) = rest.split_once('|')?;
    let engine = base64::engine::general_purpose::STANDARD;
    let salt = engine.decode(salt_b64).ok()?;
    let want = engine.decode(hash_b64).ok()?;

    for host in candidates {
        let mut mac = HmacSha1::new_from_slice(&salt).ok()?;
        mac.update(host.as_bytes());
        if mac.finalize().into_bytes().as_slice() == want.as_slice() {
            return Some(true);
        }
    }
    Some(false)
}

/// OpenSSH 的通配符：`*` 任意多字符，`?` 单个字符
fn glob_matches(pattern: &str, host: &str) -> bool {
    fn go(p: &[u8], h: &[u8]) -> bool {
        match p.first() {
            None => h.is_empty(),
            Some(b'*') => {
                // 空匹配，或吃掉 h 的一个字符再试
                go(&p[1..], h) || (!h.is_empty() && go(p, &h[1..]))
            }
            Some(b'?') => !h.is_empty() && go(&p[1..], &h[1..]),
            Some(c) => {
                !h.is_empty() && h[0].eq_ignore_ascii_case(c) && go(&p[1..], &h[1..])
            }
        }
    }
    go(pattern.as_bytes(), host.as_bytes())
}

/// 这行的主机字段匹不匹配目标主机。
/// 返回 None = 这行是哈希形式但解不开（既不能算匹配、也不能当它不存在）
fn line_matches_host(hosts_field: &str, patterns: &[String]) -> Option<bool> {
    if hosts_field.starts_with("|1|") {
        return hashed_matches(hosts_field, patterns);
    }

    // OpenSSH 支持 `!pattern` 否定：命中否定项就整行不算
    let mut matched = false;
    for item in hosts_field.split(',') {
        let item = item.trim();
        if item.is_empty() {
            continue;
        }
        if let Some(neg) = item.strip_prefix('!') {
            if patterns.iter().any(|p| glob_matches(neg, p)) {
                return Some(false);
            }
            continue;
        }
        if patterns.iter().any(|p| glob_matches(item, p)) {
            matched = true;
        }
    }
    Some(matched)
}

/// 一个文件的扫描结果
#[derive(Debug, Default)]
struct Scan {
    /// 匹配到这台主机的指纹
    found: Vec<String>,
    /// 匹配到的 @revoked 指纹
    revoked: Vec<String>,
    /// 匹配到 @cert-authority（我们还不做证书链校验）
    has_ca: bool,
    /// 没法比对的原因（哈希行解不开、文件读不了……）
    problems: Vec<String>,
}

/// 扫一个 known_hosts 文件。
///
/// 按**字节**读再逐行 lossy 解码：主机名和 base64 都是 ASCII，
/// 文件里别处混进一个非 UTF-8 字节不该让整个库失效（OpenSSH 也不会）。
/// 但真正的 IO 失败（权限不足等）必须如实报出来，不能当成「没有记录」。
fn scan_file(path: &PathBuf, patterns: &[String], scan: &mut Scan) {
    let raw = match std::fs::read(path) {
        Ok(r) => r,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return, // 没这文件，正常
        Err(e) => {
            scan.problems
                .push(format!("{} 读不了（{}）", path.display(), e));
            return;
        }
    };

    for chunk in raw.split(|b| *b == b'\n') {
        if chunk.is_empty() {
            continue;
        }
        let text = String::from_utf8_lossy(chunk);
        let Some(l) = parse_line(&text) else {
            continue;
        };
        match line_matches_host(&l.hosts, patterns) {
            Some(true) => match l.marker {
                Marker::Revoked => scan.revoked.push(l.fingerprint),
                Marker::CertAuthority => scan.has_ca = true,
                Marker::None => {
                    if !scan.found.contains(&l.fingerprint) {
                        scan.found.push(l.fingerprint);
                    }
                }
            },
            Some(false) => {}
            None => scan
                .problems
                .push(format!("{} 里有解不开的哈希行", path.display())),
        }
    }
}

fn scan_all(host: &str, port: u16) -> Scan {
    let patterns = host_patterns(host, port);
    let mut scan = Scan::default();
    for path in [ssh_known_hosts(), Some(daiqiao_known_hosts())]
        .into_iter()
        .flatten()
    {
        scan_file(&path, &patterns, &mut scan);
    }
    scan
}

/// 校验一把服务器密钥
pub fn verify(host: &str, port: u16, key_blob: &[u8]) -> Verdict {
    let fp = fingerprint(key_blob);
    let scan = scan_all(host, port);

    // 吊销优先于一切
    if scan.revoked.contains(&fp) {
        return Verdict::Revoked;
    }
    if scan.found.contains(&fp) {
        return Verdict::Match;
    }
    // 有记录但不匹配 → 变更
    if !scan.found.is_empty() {
        return Verdict::Changed { known: scan.found };
    }
    // 没匹配上，但有没法比对的东西 → 不能当「没见过」
    if !scan.problems.is_empty() {
        return Verdict::Unverifiable {
            reason: scan.problems.join("；"),
        };
    }
    if scan.has_ca {
        return Verdict::Unverifiable {
            reason: format!(
                "{host} 在 known_hosts 里是用 @cert-authority（证书签发）配置的，代敲还不支持校验主机证书"
            ),
        };
    }
    // 这台主机被 @revoked 记过别的密钥，说明人为标记过它有问题，别 TOFU
    if !scan.revoked.is_empty() {
        return Verdict::Unverifiable {
            reason: format!("{host} 有被吊销（@revoked）的密钥记录，这台主机需要人工确认"),
        };
    }
    Verdict::Unknown
}

/// TOFU 写入要串行化。
///
/// 代敲每次 SFTP 操作都新建一条连接，两条连接同时首连同一台主机时，
/// 中间人只要在其中一条里插自己的密钥，就能让**两把**密钥都被记成可信。
/// 所以写之前要在锁里重新校验一次。
static REMEMBER_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// 把新主机记进代敲自己的库（TOFU）。文件权限 600。
///
/// 锁内重新 verify 一次：如果这期间别的连接已经记过一把**不同**的密钥，
/// 这次就不能再追加 —— 返回 Changed 让调用方拒连。
pub fn remember(
    host: &str,
    port: u16,
    key_type: &str,
    key_blob: &[u8],
) -> Result<(), RememberError> {
    use std::io::Write;

    let _guard = REMEMBER_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    // 双重检查：拿到锁之后世界可能已经变了
    match verify(host, port, key_blob) {
        Verdict::Unknown => {}
        Verdict::Match => return Ok(()), // 别人刚记过同一把，等于已完成
        other => return Err(RememberError::Raced(other)),
    }

    let dir = crate::keys::daiqiao_dir();
    std::fs::create_dir_all(&dir).map_err(RememberError::Io)?;
    let path = daiqiao_known_hosts();

    let pattern = if port == 22 {
        host.to_string()
    } else {
        format!("[{host}]:{port}")
    };
    let b64 = base64::engine::general_purpose::STANDARD.encode(key_blob);
    let line = format!("{pattern} {key_type} {b64}\n");

    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(RememberError::Io)?;
    f.write_all(line.as_bytes()).map_err(RememberError::Io)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

#[derive(Debug)]
pub enum RememberError {
    /// 拿到锁之后发现别的连接已经记了别的密钥
    Raced(Verdict),
    Io(std::io::Error),
}

impl std::fmt::Display for RememberError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Raced(v) => write!(f, "另一条连接同时记录了不同的主机密钥（{v:?}）"),
            Self::Io(e) => write!(f, "写入失败: {e}"),
        }
    }
}

/// 这台主机的指纹记录分别来自哪里（给「怎么清」的提示用）
pub struct Origins {
    pub in_daiqiao: usize,
    pub in_ssh: usize,
}

pub fn origins(host: &str, port: u16) -> Origins {
    let patterns = host_patterns(host, port);
    let mut d = Scan::default();
    scan_file(&daiqiao_known_hosts(), &patterns, &mut d);
    let mut s = Scan::default();
    if let Some(p) = ssh_known_hosts() {
        scan_file(&p, &patterns, &mut s);
    }
    Origins {
        in_daiqiao: d.found.len() + d.revoked.len(),
        in_ssh: s.found.len() + s.revoked.len(),
    }
}

/// 删掉这台主机在**代敲库**里的记录。返回删了几行。
/// `~/.ssh/known_hosts` 不动 —— 那是 ssh 命令的地盘，要清得用 ssh-keygen -R。
pub fn forget(host: &str, port: u16) -> std::io::Result<usize> {
    let _guard = REMEMBER_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let path = daiqiao_known_hosts();
    let raw = match std::fs::read(&path) {
        Ok(r) => r,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e),
    };
    let patterns = host_patterns(host, port);
    let mut kept: Vec<u8> = Vec::new();
    let mut removed = 0usize;

    for chunk in raw.split(|b| *b == b'\n') {
        if chunk.is_empty() {
            continue;
        }
        let text = String::from_utf8_lossy(chunk);
        let drop = parse_line(&text)
            .and_then(|l| line_matches_host(&l.hosts, &patterns))
            .unwrap_or(false);
        if drop {
            removed += 1;
        } else {
            kept.extend_from_slice(chunk);
            kept.push(b'\n');
        }
    }
    if removed > 0 {
        std::fs::write(&path, kept)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        }
    }
    Ok(removed)
}

/// 列出代敲库里记了哪些主机（给界面和 MCP 自省用）
pub fn list() -> Vec<(String, String)> {
    let Ok(raw) = std::fs::read(daiqiao_known_hosts()) else {
        return Vec::new();
    };
    raw.split(|b| *b == b'\n')
        .filter(|c| !c.is_empty())
        .filter_map(|c| parse_line(&String::from_utf8_lossy(c)))
        .map(|l| {
            let tag = match l.marker {
                Marker::Revoked => "（已吊销）",
                Marker::CertAuthority => "（CA）",
                Marker::None => "",
            };
            (format!("{}{tag}", l.hosts), l.fingerprint)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blob_a() -> Vec<u8> {
        base64::engine::general_purpose::STANDARD
            .decode("AAAAC3NzaC1lZDI1NTE5AAAAIHEwRG1DtUswg6gumzm52hIlaVjwVN9pfT0xNZ6Qo5Zx")
            .unwrap()
    }
    fn blob_b() -> Vec<u8> {
        base64::engine::general_purpose::STANDARD
            .decode("AAAAC3NzaC1lZDI1NTE5AAAAIGvQ2VxVkHKZXKmJYzqLJ8VYxF5pZ1qQ9kLmNoPqRsTu")
            .unwrap()
    }
    fn b64(blob: &[u8]) -> String {
        base64::engine::general_purpose::STANDARD.encode(blob)
    }

    /// 改 HOME 是**进程级**的，而 cargo 默认并行跑测试 —— 几个沙盒会互相踩，
    /// A 的 verify() 会读到 B 的目录。所以所有碰 HOME 的测试用这把锁串起来。
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// 隔离 HOME：产品代码通过 HOME 找两个 known_hosts，测试必须在自己的沙盒里跑
    struct Sandbox {
        dir: PathBuf,
        old: Option<String>,
        // 守卫活到 Sandbox 被 drop，期间别的测试动不了 HOME
        _guard: std::sync::MutexGuard<'static, ()>,
    }
    impl Sandbox {
        fn new(tag: &str) -> Self {
            let guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let dir = std::env::temp_dir().join(format!("dq-kh-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(dir.join(".ssh")).unwrap();
            let old = std::env::var("HOME").ok();
            std::env::set_var("HOME", &dir);

            // ★ 确认隔离真生效了，再让测试往下跑。
            // 代价很实在：加锁之前那次并行跑，HOME 被别的测试踩掉，
            // 一次 remember() 落进了开发机真实的 ~/.daiqiao/known_hosts。
            // 测试污染用户数据这种事，宁可在这里炸掉。
            let resolved = daiqiao_known_hosts();
            assert!(
                resolved.starts_with(&dir),
                "沙盒没隔离住：daiqiao_known_hosts() = {}，应该在 {} 里面",
                resolved.display(),
                dir.display()
            );

            Self {
                dir,
                old,
                _guard: guard,
            }
        }
        fn write_ssh(&self, content: &[u8]) {
            std::fs::write(self.dir.join(".ssh/known_hosts"), content).unwrap();
        }
    }
    impl Drop for Sandbox {
        fn drop(&mut self) {
            match &self.old {
                Some(h) => std::env::set_var("HOME", h),
                None => std::env::remove_var("HOME"),
            }
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    #[test]
    fn 指纹格式和sshkeygen一致() {
        let fp = fingerprint(&blob_a());
        assert!(fp.starts_with("SHA256:"), "{fp}");
        assert!(!fp.contains('='), "{fp}");
        assert_eq!(fp.len(), "SHA256:".len() + 43, "{fp}");
    }

    #[test]
    fn 不同密钥指纹不同() {
        assert_ne!(fingerprint(&blob_a()), fingerprint(&blob_b()));
    }

    #[test]
    fn 解析普通行() {
        let line = format!("198.51.100.29 ssh-ed25519 {}", b64(&blob_a()));
        let l = parse_line(&line).unwrap();
        assert_eq!(l.hosts, "198.51.100.29");
        assert_eq!(l.marker, Marker::None);
        assert_eq!(l.fingerprint, fingerprint(&blob_a()));
    }

    #[test]
    fn 标记在行首才是真实格式() {
        // OpenSSH 产出的是 `@revoked host keytype key`
        let line = format!("@revoked victim.example.com ssh-ed25519 {}", b64(&blob_a()));
        let l = parse_line(&line).unwrap();
        assert_eq!(l.marker, Marker::Revoked, "@revoked 必须认出来");
        assert_eq!(l.hosts, "victim.example.com");

        let ca = format!("@cert-authority *.example.com ssh-ed25519 {}", b64(&blob_b()));
        let l2 = parse_line(&ca).unwrap();
        assert_eq!(l2.marker, Marker::CertAuthority);
        assert_eq!(l2.hosts, "*.example.com");
    }

    #[test]
    fn 通配符匹配() {
        assert!(glob_matches("*.example.com", "a.example.com"));
        assert!(glob_matches("*.example.com", "deep.sub.example.com"));
        assert!(!glob_matches("*.example.com", "example.com"));
        assert!(glob_matches("host?.lan", "host1.lan"));
        assert!(!glob_matches("host?.lan", "host12.lan"));
        assert!(glob_matches("203.0.113.*", "203.0.113.10"));
        // 大小写不敏感
        assert!(glob_matches("A.Example.COM", "a.example.com"));
    }

    #[test]
    fn 否定模式整行作废() {
        let f = "*.example.com,!bad.example.com";
        assert_eq!(
            line_matches_host(f, &["bad.example.com".into()]),
            Some(false),
            "命中否定项就不算匹配"
        );
        assert_eq!(
            line_matches_host(f, &["ok.example.com".into()]),
            Some(true)
        );
    }

    #[test]
    fn 哈希行真做hmac比对() {
        // 用已知 salt 自己算一遍，构造出真实的哈希行
        let engine = base64::engine::general_purpose::STANDARD;
        let salt = b"0123456789abcdefghij"; // HMAC-SHA1 的 salt 是 20 字节
        let host = "victim.example.com";
        let mut mac = HmacSha1::new_from_slice(salt).unwrap();
        mac.update(host.as_bytes());
        let digest = mac.finalize().into_bytes();
        let field = format!("|1|{}|{}", engine.encode(salt), engine.encode(digest));

        assert_eq!(
            line_matches_host(&field, &[host.to_string()]),
            Some(true),
            "★ 哈希行必须能匹配上对应主机（第一版直接丢弃，等于信任库失效）"
        );
        assert_eq!(
            line_matches_host(&field, &["other.example.com".into()]),
            Some(false),
            "不是这台主机就该不匹配"
        );
    }

    #[test]
    fn 哈希行解不开要报problem而不是当没记录() {
        assert_eq!(
            line_matches_host("|1|这不是base64!!|也不是", &["x".into()]),
            None,
            "解不开必须返回 None（调用方据此判 Unverifiable），不能返回 false"
        );
    }

    #[test]
    fn 注释和空行跳过() {
        assert!(parse_line("").is_none());
        assert!(parse_line("   ").is_none());
        assert!(parse_line("# 这是注释").is_none());
    }

    #[test]
    fn 非标准端口用方括号写法() {
        let p = host_patterns("example.com", 2222);
        assert_eq!(p, vec!["[example.com]:2222"]);
        let p22 = host_patterns("example.com", 22);
        assert!(p22.contains(&"example.com".to_string()));
        assert!(p22.contains(&"[example.com]:22".to_string()));
    }

    #[test]
    fn 坏base64不炸() {
        assert!(parse_line("host ssh-ed25519 这不是base64!!!").is_none());
        assert!(parse_line("host ssh-ed25519").is_none());
        assert!(parse_line("onlyhost").is_none());
    }

    /// 对抗验证报的 6 条，逐条回归。这些是真漏洞，不是风格问题。
    #[test]
    fn 对抗验证报的漏洞全部挡住() {
        let sb = Sandbox::new("adv");
        let host = "victim.example.com";
        let fp_a = fingerprint(&blob_a());

        // ① 文件里混进非 UTF-8 字节，不该让整个库失效
        let mut bad = Vec::new();
        bad.extend_from_slice(b"# comment with bad byte: \xff\xfe\n");
        bad.extend_from_slice(format!("{host} ssh-ed25519 {}\n", b64(&blob_a())).as_bytes());
        sb.write_ssh(&bad);
        assert_eq!(
            verify(host, 22, &blob_a()),
            Verdict::Match,
            "★ 一个坏字节不该让整个信任库作废"
        );
        match verify(host, 22, &blob_b()) {
            Verdict::Changed { known } => assert!(known.contains(&fp_a)),
            other => panic!("★ 坏字节文件里换密钥必须判 Changed，实际 {other:?}"),
        }

        // ② 哈希行：有记录就必须比对得上，换密钥要判 Changed 而不是 Unknown
        let engine = base64::engine::general_purpose::STANDARD;
        let salt = b"abcdefghij0123456789";
        let mut mac = HmacSha1::new_from_slice(salt).unwrap();
        mac.update(host.as_bytes());
        let hashed = format!(
            "|1|{}|{} ssh-ed25519 {}\n",
            engine.encode(salt),
            engine.encode(mac.finalize().into_bytes()),
            b64(&blob_a())
        );
        sb.write_ssh(hashed.as_bytes());
        assert_eq!(
            verify(host, 22, &blob_a()),
            Verdict::Match,
            "★ 哈希行记录的主机要能匹配"
        );
        match verify(host, 22, &blob_b()) {
            Verdict::Changed { .. } => {}
            other => panic!("★ 哈希行记录下换密钥必须 Changed，实际 {other:?}（这是绕过信任库）"),
        }

        // ③ @revoked：被吊销的密钥永久拒绝，绝不 TOFU
        sb.write_ssh(format!("@revoked {host} ssh-ed25519 {}\n", b64(&blob_a())).as_bytes());
        assert_eq!(
            verify(host, 22, &blob_a()),
            Verdict::Revoked,
            "★ 被吊销的密钥必须判 Revoked"
        );
        // 同一台主机的别的密钥也不能当「首次见面」直接放行
        match verify(host, 22, &blob_b()) {
            Verdict::Unverifiable { .. } => {}
            other => panic!("★ 有吊销记录的主机不该 TOFU，实际 {other:?}"),
        }

        // ④ @cert-authority：我们不做证书校验，要拒连而不是 TOFU
        sb.write_ssh(
            format!("@cert-authority *.example.com ssh-ed25519 {}\n", b64(&blob_a())).as_bytes(),
        );
        match verify("host.example.com", 22, &blob_b()) {
            Verdict::Unverifiable { reason } => {
                assert!(reason.contains("cert-authority"), "{reason}")
            }
            other => panic!("★ CA 配置的主机不该 TOFU，实际 {other:?}"),
        }

        // ⑤ 通配符行：被它覆盖的主机换了密钥要判 Changed
        sb.write_ssh(format!("*.example.com ssh-ed25519 {}\n", b64(&blob_a())).as_bytes());
        assert_eq!(verify("a.example.com", 22, &blob_a()), Verdict::Match);
        match verify("a.example.com", 22, &blob_b()) {
            Verdict::Changed { .. } => {}
            other => panic!("★ 通配符记录被绕过了，实际 {other:?}"),
        }

        // ⑥ 真·空库才 TOFU
        sb.write_ssh(b"# nothing here\n");
        assert_eq!(
            verify("brand-new.example.com", 22, &blob_a()),
            Verdict::Unknown,
            "确实没记录时才允许 TOFU"
        );
    }

    #[test]
    fn 读不了的文件要拒连而不是放行() {
        let sb = Sandbox::new("perm");
        let p = sb.dir.join(".ssh/known_hosts");
        sb.write_ssh(b"# whatever\n");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o000)).unwrap();
            match verify("any.example.com", 22, &blob_a()) {
                Verdict::Unverifiable { reason } => assert!(reason.contains("读不了"), "{reason}"),
                other => panic!("★ 文件读不了必须拒连，实际 {other:?}（这是 fail-open）"),
            }
            // 收尾：改回可读，Drop 才删得掉
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
    }

    #[test]
    fn tofu写入与遗忘的完整链路() {
        let sb = Sandbox::new("tofu");
        sb.write_ssh(b"");
        let host = "fresh.example.com";

        assert_eq!(verify(host, 22, &blob_a()), Verdict::Unknown);
        remember(host, 22, "ssh-ed25519", &blob_a()).unwrap();
        assert_eq!(verify(host, 22, &blob_a()), Verdict::Match);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(daiqiao_known_hosts())
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600, "known_hosts 权限应为 600，实际 {mode:o}");
        }

        // 记过之后换密钥 → Changed
        match verify(host, 22, &blob_b()) {
            Verdict::Changed { .. } => {}
            other => panic!("记过的主机换密钥要 Changed，实际 {other:?}"),
        }

        // ★ 竞态：已经记了 A，再 remember B 必须被拒，不能追加成第二把可信密钥
        match remember(host, 22, "ssh-ed25519", &blob_b()) {
            Err(RememberError::Raced(Verdict::Changed { .. })) => {}
            other => panic!("★ 重复 TOFU 不同密钥必须被拒，实际 {other:?}"),
        }
        // 同一把再记一次是幂等的（别的连接刚好记过）
        remember(host, 22, "ssh-ed25519", &blob_a()).unwrap();

        // forget 只删代敲库，删完回到 Unknown
        assert_eq!(forget(host, 22).unwrap(), 1);
        assert_eq!(verify(host, 22, &blob_a()), Verdict::Unknown);

        // 同主机不同端口互不干扰
        remember(host, 2222, "ssh-ed25519", &blob_a()).unwrap();
        assert_eq!(verify(host, 2222, &blob_a()), Verdict::Match);
        assert_eq!(verify(host, 22, &blob_a()), Verdict::Unknown);

        // 自己清干净 —— Sandbox 的 Drop 会删目录，但万一隔离失效，
        // 这一步能把残留限制在一条而不是留在用户文件里
        forget(host, 2222).unwrap();
    }

    #[test]
    fn origins能分清记录来自哪个库() {
        let sb = Sandbox::new("orig");
        let host = "both.example.com";
        sb.write_ssh(format!("{host} ssh-ed25519 {}\n", b64(&blob_a())).as_bytes());
        let o = origins(host, 22);
        assert_eq!(o.in_ssh, 1, "应认出 ~/.ssh 里那条");
        assert_eq!(o.in_daiqiao, 0, "代敲库里还没有");
    }

    #[test]
    fn forget不碰用户的ssh配置() {
        let sb = Sandbox::new("noclobber");
        let host = "sshonly.example.com";
        let content = format!("{host} ssh-ed25519 {}\n", b64(&blob_a()));
        sb.write_ssh(content.as_bytes());

        assert_eq!(forget(host, 22).unwrap(), 0, "代敲库里没有，应删 0 行");
        let after = std::fs::read_to_string(sb.dir.join(".ssh/known_hosts")).unwrap();
        assert_eq!(after, content, "★ ~/.ssh/known_hosts 必须原样不动");
    }
}
