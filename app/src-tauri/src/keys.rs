//! SSH 密钥库。
//!
//! 密钥信息（算法、指纹、是否加密）**现场从文件扫出来**，不复制一份存进配置里 ——
//! 这样永远不会跟磁盘上的真实文件状态脱节（文件被换了、权限被改了，下次列表就看得到）。
//!
//! `keys.json` 只存一件事：人手动改过的显示名（没改过就用文件名）。真正的私钥内容
//! 只有两个地方：用户自己的 `~/.ssh/`（代敲只读，绝不碰），和代敲自己的
//! `~/.daiqiao/keys/`（导入/粘贴/生成的密钥落在这，目录 700、文件 600）。

use russh::keys::{HashAlg, PrivateKey};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub fn ssh_dir() -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    Some(PathBuf::from(home).join(".ssh"))
}

/// 代敲自己的目录 `~/.daiqiao`（主机表、密钥库、审计日志都在这下面）
pub fn daiqiao_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(home).join(".daiqiao")
}

pub fn vault_dir() -> Option<PathBuf> {
    std::env::var("HOME").ok()?;
    Some(daiqiao_dir().join("keys"))
}

/// 显示名覆盖表：`~/.daiqiao/keys.json`，只存 `{ id -> 自定义名 }`
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct KeyNameStore {
    #[serde(default)]
    pub names: HashMap<String, String>,
}

impl KeyNameStore {
    pub fn path() -> Option<PathBuf> {
        let home = std::env::var("HOME").ok()?;
        Some(PathBuf::from(home).join(".daiqiao").join("keys.json"))
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
        std::fs::write(&p, serde_json::to_string_pretty(self)?)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600));
        }
        Ok(())
    }
}

/// 一把密钥的现场探测结果，给界面和 MCP 用
#[derive(Debug, Clone, Serialize)]
pub struct KeyInfo {
    /// 用绝对路径算出来的稳定 id
    pub id: String,
    pub name: String,
    pub path: String,
    /// "ssh_dir" | "vault"
    pub source: String,
    /// "ed25519" / "rsa" / "ecdsa" / ...
    pub algo: String,
    pub fingerprint: String,
    pub encrypted: bool,
    pub comment: Option<String>,
    /// 文件权限是不是 0600（私钥权限太松 ssh 会直接拒绝）
    pub perm_ok: bool,
    /// 没加密的话，顺手给出公钥文本（省得再问一次）；加密的也能给，
    /// 因为公钥在 OpenSSH 私钥文件里本来就是明文的一部分
    pub public_key: Option<String>,
    /// 这把密钥被哪些主机引用了（按名字）
    #[serde(default)]
    pub used_by: Vec<String>,
    /// 解锁口令存没存进钥匙串（不暴露内容）。带口令的私钥连接时要用，
    /// 界面和 MCP 只看这个布尔值。
    #[serde(default)]
    pub passphrase_saved: bool,
}

pub fn key_id(path: &Path) -> String {
    use std::hash::{Hash, Hasher};
    let canon = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let mut h = std::collections::hash_map::DefaultHasher::new();
    canon.hash(&mut h);
    format!("k{:x}", h.finish())
}

fn perm_is_safe(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        match std::fs::metadata(path) {
            Ok(m) => m.permissions().mode() & 0o077 == 0,
            Err(_) => false,
        }
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// 读一个私钥文件，探出它的元信息。**不需要口令** —— 算法/指纹/公钥
/// 在 OpenSSH 私钥格式里本来就是明文，只有真正签名才要解密。
fn inspect(path: &Path, source: &str, used_by: Vec<String>) -> Option<KeyInfo> {
    let text = std::fs::read_to_string(path).ok()?;
    if !text.trim_start().starts_with("-----BEGIN") {
        return None;
    }
    let key = PrivateKey::from_openssh(&text).ok()?;
    let algo = key
        .algorithm()
        .as_str()
        .trim_start_matches("ssh-")
        .to_string();
    let comment_bytes: &[u8] = key.comment().as_ref();
    let comment = if comment_bytes.is_empty() {
        None
    } else {
        Some(String::from_utf8_lossy(comment_bytes).to_string())
    };
    let name_store = KeyNameStore::load();
    let id = key_id(path);
    let default_name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| id.clone());
    let name = name_store.names.get(&id).cloned().unwrap_or(default_name);
    // 先查好再进结构体字面量（id 在里面会被 move 走）
    let passphrase_saved = crate::keychain::key_passphrase_saved(&id);

    Some(KeyInfo {
        id,
        name,
        path: path.to_string_lossy().to_string(),
        source: source.to_string(),
        algo,
        fingerprint: key.fingerprint(HashAlg::Sha256).to_string(),
        encrypted: key.is_encrypted(),
        comment,
        perm_ok: perm_is_safe(path),
        public_key: key.public_key().to_openssh().ok(),
        used_by,
        passphrase_saved,
    })
}

/// 看起来像私钥文件名的候选：排掉 known_hosts / config / authorized_keys /
/// 公钥本身(.pub) 这些明显不是私钥的
fn looks_like_key_name(name: &str) -> bool {
    let skip = [
        "known_hosts",
        "known_hosts.old",
        "config",
        "authorized_keys",
    ];
    !name.starts_with('.') && !name.ends_with(".pub") && !skip.contains(&name)
}

/// 扫一个目录，找出里面真正能被 `PrivateKey::from_openssh` 解析的文件
fn scan_dir(dir: &Path, source: &str, used: &HashMap<String, Vec<String>>) -> Vec<KeyInfo> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !looks_like_key_name(name) {
            continue;
        }
        let id = key_id(&path);
        let used_by = used.get(&id).cloned().unwrap_or_default();
        if let Some(info) = inspect(&path, source, used_by) {
            out.push(info);
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// 列出 `~/.ssh` 和 `~/.daiqiao/keys` 里的所有密钥。
/// `used_by`：host_id/名字 -> 引用它的主机名列表，用于「这把密钥被几台主机用」。
pub fn list_keys(used_by_key: &HashMap<String, Vec<String>>) -> Vec<KeyInfo> {
    let mut out = Vec::new();
    if let Some(d) = ssh_dir() {
        out.extend(scan_dir(&d, "ssh_dir", used_by_key));
    }
    if let Some(d) = vault_dir() {
        out.extend(scan_dir(&d, "vault", used_by_key));
    }
    out
}

/// 按 id 或路径找一把密钥的详情（没找到就返回 None）
pub fn find_key(key: &str, used_by_key: &HashMap<String, Vec<String>>) -> Option<KeyInfo> {
    list_keys(used_by_key)
        .into_iter()
        .find(|k| k.id == key || k.path == key || k.name == key)
}

/// 把一个文件复制进代敲库（`~/.daiqiao/keys/`），权限收紧成 600。
/// 用于「选择文件…」—— 原文件在哪儿、是不是优盘上的，导入之后都不依赖它了。
pub fn import_file(src: &Path, name_hint: Option<&str>) -> anyhow::Result<KeyInfo> {
    let text = std::fs::read_to_string(src)?;
    if !text.trim_start().starts_with("-----BEGIN") {
        anyhow::bail!("这个文件看起来不是 OpenSSH 格式的私钥");
    }
    // 校验能解析，不要把坏文件存进库里
    PrivateKey::from_openssh(&text).map_err(|e| anyhow::anyhow!("解析失败: {e}"))?;
    write_into_vault(&text, name_hint, src.file_name().and_then(|n| n.to_str()))
}

/// 把粘贴进来的私钥文本存进代敲库。
///
/// 带口令的私钥要把 `passphrase` 一起给：先用它真解一遍确认没错，再把口令
/// 存进钥匙串（`ssh-key:<id>`，hex 编码，不进日志不进配置）。加密私钥
/// 不给口令会直接报错 —— 与其让它躺在库里连不上，不如当场说清楚。
pub fn import_text(
    text: &str,
    name_hint: Option<&str>,
    passphrase: Option<&str>,
) -> anyhow::Result<KeyInfo> {
    let text = text.trim();
    if !text.starts_with("-----BEGIN") {
        anyhow::bail!("这不是一段 OpenSSH 格式的私钥（应该以 -----BEGIN 开头）");
    }
    // 先解析信封拿元信息（加密的私钥也能解出公钥/算法，只是解不出私钥本体）
    let peek = PrivateKey::from_openssh(text).map_err(|e| anyhow::anyhow!("解析失败: {e}"))?;
    let passphrase = if peek.is_encrypted() {
        let pw = passphrase.ok_or_else(|| {
            anyhow::anyhow!(
                "这把私钥带口令（passphrase）：粘贴导入时把口令一起填上，\
                 代敲会把它存进钥匙串"
            )
        })?;
        // 真解一次：错口令当场报，别等连接的时候才发现
        russh::keys::decode_secret_key(text, Some(pw))
            .map_err(|_| anyhow::anyhow!("口令不对，解不开这把私钥"))?;
        Some(pw)
    } else {
        None
    };
    let mut info = write_into_vault(text, name_hint, None)?;
    if let Some(pw) = passphrase {
        crate::keychain::set_key_passphrase(&info.id, pw).map_err(|e| anyhow::anyhow!("{e}"))?;
        // inspect 里查的时候还没存上，这里补真
        info.passphrase_saved = true;
    }
    Ok(info)
}

/// 老行为：只验格式就入库，带口令的私钥也能存（连接前再去密钥库存口令）。
/// 配置迁移和 MCP 导入走这个 —— 它们没法当场向人要口令。
pub fn import_text_raw(text: &str, name_hint: Option<&str>) -> anyhow::Result<KeyInfo> {
    let text = text.trim();
    if !text.starts_with("-----BEGIN") {
        anyhow::bail!("这不是一段 OpenSSH 格式的私钥（应该以 -----BEGIN 开头）");
    }
    PrivateKey::from_openssh(text).map_err(|e| anyhow::anyhow!("解析失败: {e}"))?;
    write_into_vault(text, name_hint, None)
}

/// 校验一个口令能不能解开这把私钥（存进钥匙串之前先把关）。
/// 三种结果：没加密 / 口令不对 / 通过。
pub fn verify_passphrase(path: &Path, passphrase: &str) -> anyhow::Result<()> {
    let text = std::fs::read_to_string(path)?;
    let peek = PrivateKey::from_openssh(&text).map_err(|e| anyhow::anyhow!("解析失败: {e}"))?;
    if !peek.is_encrypted() {
        anyhow::bail!("这把密钥没有加密，用不着口令");
    }
    russh::keys::decode_secret_key(&text, Some(passphrase))
        .map_err(|_| anyhow::anyhow!("口令不对，解不开这把私钥"))?;
    Ok(())
}

fn write_into_vault(
    text: &str,
    name_hint: Option<&str>,
    file_hint: Option<&str>,
) -> anyhow::Result<KeyInfo> {
    let dir = vault_dir().ok_or_else(|| anyhow::anyhow!("找不到 HOME 目录"))?;
    std::fs::create_dir_all(&dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
    }

    let stem = name_hint
        .or(file_hint)
        .map(sanitize_filename)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| format!("key-{}", unique_suffix()));
    let mut filename = stem.clone();
    let mut path = dir.join(&filename);
    let mut n = 1;
    while path.exists() {
        n += 1;
        filename = format!("{stem}-{n}");
        path = dir.join(&filename);
    }

    std::fs::write(&path, text)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    }

    let mut info =
        inspect(&path, "vault", Vec::new()).ok_or_else(|| anyhow::anyhow!("写入后读取失败"))?;
    if let Some(custom) = name_hint {
        if !custom.is_empty() {
            set_name(&info.id, custom)?;
            info.name = custom.to_string();
        }
    }
    Ok(info)
}

fn sanitize_filename(s: &str) -> String {
    let cleaned: String = s
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect();
    // 开头的 `.` 必须去掉：扫描时会跳过点文件（为了躲开 .ssh/config 这类），
    // 真留下来就成了「存在磁盘上、但 key_list 看不到、key_remove 也删不掉」的孤儿私钥。
    // 名字形如 `../../x` 被上面换成 `.._.._x` 时就会踩到这个坑。
    let cleaned = cleaned.trim_start_matches('.').to_string();
    // 全被清空（名字只有点和斜杠）就给个兜底名，别产生空文件名
    if cleaned.is_empty() {
        "key".to_string()
    } else {
        cleaned
    }
}

fn unique_suffix() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{nanos:x}")
}

/// 调系统 `ssh-keygen` 生成一对新密钥，直接落进代敲库
pub fn generate(name: Option<&str>, algo: &str, comment: &str) -> anyhow::Result<KeyInfo> {
    let dir = vault_dir().ok_or_else(|| anyhow::anyhow!("找不到 HOME 目录"))?;
    std::fs::create_dir_all(&dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
    }

    let stem = name
        .map(sanitize_filename)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| format!("daiqiao-{}", unique_suffix()));
    let mut filename = stem.clone();
    let mut path = dir.join(&filename);
    let mut n = 1;
    while path.exists() {
        n += 1;
        filename = format!("{stem}-{n}");
        path = dir.join(&filename);
    }

    let keytype = match algo {
        "rsa" => "rsa",
        _ => "ed25519",
    };
    let mut cmd = std::process::Command::new("ssh-keygen");
    cmd.arg("-t").arg(keytype);
    if keytype == "rsa" {
        cmd.arg("-b").arg("4096");
    }
    cmd.arg("-f")
        .arg(&path)
        .arg("-N")
        .arg("") // 代敲生成的密钥默认不加口令，immediately usable
        .arg("-C")
        .arg(comment)
        .arg("-q");
    let status = cmd
        .status()
        .map_err(|e| anyhow::anyhow!("找不到 ssh-keygen: {e}"))?;
    if !status.success() {
        anyhow::bail!("ssh-keygen 生成失败");
    }
    // ssh-keygen 自己会设对私钥权限，这里兜底一下
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }

    let mut info =
        inspect(&path, "vault", Vec::new()).ok_or_else(|| anyhow::anyhow!("生成后读取失败"))?;
    if let Some(custom) = name {
        if !custom.is_empty() {
            set_name(&info.id, custom)?;
            info.name = custom.to_string();
        }
    }
    Ok(info)
}

/// 把一把密钥的私钥文件权限修成 0600（SSH 对权限太松的私钥会直接拒绝）
pub fn fix_permissions(
    id_or_path: &str,
    used_by_key: &HashMap<String, Vec<String>>,
) -> anyhow::Result<()> {
    let info =
        find_key(id_or_path, used_by_key).ok_or_else(|| anyhow::anyhow!("找不到这把密钥"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&info.path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

/// 改显示名（文件本身不动）
pub fn set_name(id: &str, name: &str) -> anyhow::Result<()> {
    let mut store = KeyNameStore::load();
    store.names.insert(id.to_string(), name.to_string());
    store.save()
}

/// 只删代敲库里的密钥（`~/.ssh` 里的只读不碰）。
/// 钥匙串里的解锁口令一并清掉 —— 先清口令再删文件，反过来删一半失败
/// 会留下扫不出来的孤儿口令。
pub fn remove_vault_key(
    id: &str,
    used_by_key: &HashMap<String, Vec<String>>,
) -> anyhow::Result<()> {
    let info = find_key(id, used_by_key).ok_or_else(|| anyhow::anyhow!("找不到这把密钥"))?;
    if info.source != "vault" {
        anyhow::bail!("这把密钥在 ~/.ssh 里，代敲只读不碰，去系统里删");
    }
    crate::keychain::delete_key_passphrase(&info.id).map_err(|e| anyhow::anyhow!("{e}"))?;
    std::fs::remove_file(&info.path)?;
    let mut store = KeyNameStore::load();
    store.names.remove(&info.id);
    store.save()?;
    Ok(())
}

pub fn key_view(k: &KeyInfo) -> serde_json::Value {
    serde_json::json!({
        "id": k.id,
        "name": k.name,
        "path": k.path,
        "source": k.source,
        "algo": k.algo,
        "fingerprint": k.fingerprint,
        "encrypted": k.encrypted,
        "comment": k.comment,
        "perm_ok": k.perm_ok,
        "public_key": k.public_key,
        "used_by": k.used_by,
        "passphrase_saved": k.passphrase_saved,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 对抗验证报的路径穿越名字不再产生孤儿文件() {
        let name = sanitize_filename("../../../../tmp/dqv/EVIL");
        // 关键：不能以 . 开头，否则会被 looks_like_key_name 的点文件过滤器跳过，
        // 变成「写得进去、扫不出来、删不掉」的孤儿密钥
        assert!(!name.starts_with('.'), "文件名不能以 . 开头: {name}");
        assert!(!name.is_empty());
        // 扫描时真的会认得出来
        assert!(
            looks_like_key_name(&name),
            "sanitize 出的名字必须通过扫描过滤器: {name}"
        );
    }

    #[test]
    fn 纯点和斜杠的极端名字有兜底() {
        let name = sanitize_filename("../../.../");
        assert!(!name.is_empty());
        assert!(!name.starts_with('.'));
    }

    #[test]
    fn 正常名字不受影响() {
        assert_eq!(sanitize_filename("我的密钥-1"), "我的密钥-1");
        assert_eq!(sanitize_filename("deploy_key.pem"), "deploy_key.pem");
    }

    // ────── 带口令私钥的导入 / 校验（HOME 沙盒 + mock 钥匙串）──────
    // import 会真写文件进 vault，必须把 HOME 隔离进临时目录；
    // 配合 RUST_TEST_THREADS=1，沙盒期间别的测试动不了 HOME。

    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    struct Sandbox {
        dir: PathBuf,
        old: Option<String>,
        _guard: std::sync::MutexGuard<'static, ()>,
    }

    impl Sandbox {
        fn new(tag: &str) -> Self {
            let guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let dir = std::env::temp_dir().join(format!("dq-keys-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let old = std::env::var("HOME").ok();
            std::env::set_var("HOME", &dir);
            crate::keychain::clear();
            // 隔离真生效了才往下跑（known_hosts 的教训：沙箱没隔离住，
            // 一次写入就落进开发机真实的 ~/.daiqiao 里了）
            assert_eq!(
                vault_dir().as_deref(),
                Some(dir.join(".daiqiao/keys").as_path()),
                "HOME 沙盒没隔离住"
            );
            Self {
                dir,
                old,
                _guard: guard,
            }
        }
    }

    impl Drop for Sandbox {
        fn drop(&mut self) {
            match &self.old {
                Some(h) => std::env::set_var("HOME", h),
                None => std::env::remove_var("HOME"),
            }
            crate::keychain::clear();
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    /// 用系统 ssh-keygen 现场造一把测试私钥文本（带不带口令都行）
    fn gen_key_text(dir: &Path, stem: &str, passphrase: Option<&str>) -> String {
        let path = dir.join(format!("{stem}.key"));
        let st = std::process::Command::new("ssh-keygen")
            .args([
                "-t",
                "ed25519",
                "-N",
                passphrase.unwrap_or(""),
                "-q",
                "-C",
                "dq-keys-test",
            ])
            .arg("-f")
            .arg(&path)
            .status()
            .expect("跑得动 ssh-keygen");
        assert!(st.success(), "ssh-keygen 生成失败");
        std::fs::read_to_string(&path).unwrap()
    }

    #[test]
    fn 粘贴导入带口令私钥_口令验证后进钥匙串() {
        let sb = Sandbox::new("import-enc");
        let text = gen_key_text(&sb.dir, "enc", Some("口令 p@ss 中文"));
        let info =
            import_text(&text, Some("我的加密钥"), Some("口令 p@ss 中文")).expect("带口令导入");
        assert!(info.encrypted);
        assert!(
            info.passphrase_saved,
            "导入时给了口令，passphrase_saved 应为 true"
        );
        assert_eq!(info.name, "我的加密钥");
        assert_eq!(
            crate::keychain::get_key_passphrase(&info.id)
                .expect("mock 钥匙串")
                .as_deref(),
            Some("口令 p@ss 中文")
        );
        assert!(
            Path::new(&info.path).starts_with(&sb.dir),
            "密钥文件应落在沙盒 vault 里: {}",
            info.path
        );
    }

    #[test]
    fn 粘贴导入_加密私钥不给口令_明确报错且不落盘() {
        let sb = Sandbox::new("import-no-pw");
        let text = gen_key_text(&sb.dir, "enc2", Some("s3cret口令"));
        let err = import_text(&text, None, None)
            .expect_err("没给口令必须报错")
            .to_string();
        assert!(err.contains("口令"), "错误信息要提到口令: {err}");
        // 报错就不能把文件落进库里
        let vault = sb.dir.join(".daiqiao/keys");
        let empty = vault
            .read_dir()
            .map(|mut d| d.next().is_none())
            .unwrap_or(true);
        assert!(empty, "失败的导入不该留下文件");
    }

    #[test]
    fn 粘贴导入_口令不对_当场报错() {
        let sb = Sandbox::new("import-bad-pw");
        let text = gen_key_text(&sb.dir, "enc3", Some("对的口令"));
        let err = import_text(&text, None, Some("错的口令"))
            .expect_err("错口令必须报错")
            .to_string();
        assert!(err.contains("口令不对"), "{err}");
    }

    #[test]
    fn 粘贴导入_明文私钥照旧_不存口令() {
        let sb = Sandbox::new("import-plain");
        let text = gen_key_text(&sb.dir, "plain", None);
        let info = import_text(&text, None, None).expect("明文私钥导入不该要口令");
        assert!(!info.encrypted);
        assert!(!info.passphrase_saved);
    }

    #[test]
    fn 明文私钥顺手传了口令_忽略不报错也不存() {
        let sb = Sandbox::new("import-plain-pw");
        let text = gen_key_text(&sb.dir, "plain2", None);
        let info = import_text(&text, None, Some("多余的口令")).expect("明文私钥对口令不敏感");
        assert!(!info.encrypted);
        assert!(!info.passphrase_saved, "明文私钥的口令不该进钥匙串");
        assert_eq!(crate::keychain::get_key_passphrase(&info.id).unwrap(), None);
    }

    #[test]
    fn raw导入_加密私钥不带口令也能入库() {
        let sb = Sandbox::new("import-raw");
        let text = gen_key_text(&sb.dir, "enc4", Some("whatever"));
        let info = import_text_raw(&text, None).expect("raw 导入不问口令");
        assert!(info.encrypted);
        assert!(!info.passphrase_saved);
    }

    #[test]
    fn verify_passphrase三分支() {
        let sb = Sandbox::new("verify");
        let enc_text = gen_key_text(&sb.dir, "v-enc", Some("正确口令"));
        let plain_text = gen_key_text(&sb.dir, "v-plain", None);
        let enc_path = sb.dir.join("v-enc-file");
        std::fs::write(&enc_path, enc_text).unwrap();
        let plain_path = sb.dir.join("v-plain-file");
        std::fs::write(&plain_path, plain_text).unwrap();

        verify_passphrase(&enc_path, "正确口令").expect("对的口令应该通过");
        let err = verify_passphrase(&enc_path, "错误口令")
            .unwrap_err()
            .to_string();
        assert!(err.contains("口令不对"), "{err}");
        let err = verify_passphrase(&plain_path, "任何口令")
            .unwrap_err()
            .to_string();
        assert!(err.contains("没有加密"), "{err}");
    }

    #[test]
    fn 删库里的密钥_顺手清掉钥匙串口令() {
        let sb = Sandbox::new("remove");
        let text = gen_key_text(&sb.dir, "enc5", Some("删我"));
        let info = import_text(&text, None, Some("删我")).expect("导入");
        assert!(crate::keychain::key_passphrase_saved(&info.id));
        remove_vault_key(&info.id, &HashMap::new()).expect("删除");
        assert!(
            !crate::keychain::key_passphrase_saved(&info.id),
            "钥匙串口令应该一并清掉"
        );
        assert!(!Path::new(&info.path).exists());
    }

    #[test]
    fn 列表里passphrase_saved跟着钥匙串走() {
        let sb = Sandbox::new("list-saved");
        let text = gen_key_text(&sb.dir, "enc6", Some("列表口令"));
        let info = import_text(&text, None, Some("列表口令")).expect("导入");
        let listed = list_keys(&HashMap::new());
        let mine = listed
            .iter()
            .find(|k| k.id == info.id)
            .expect("列表里找得到");
        assert!(mine.passphrase_saved);

        crate::keychain::delete_key_passphrase(&info.id).unwrap();
        let listed = list_keys(&HashMap::new());
        let mine = listed.iter().find(|k| k.id == info.id).expect("还在列表里");
        assert!(!mine.passphrase_saved, "清掉口令后列表要跟着变");
    }
}
