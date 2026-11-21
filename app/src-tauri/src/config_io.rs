//! 代敲配置的跨机器导出 / 导入。
//!
//! 默认包只含主机与密钥元数据。凭据包把主机密码和私钥放进一个
//! AES-256-GCM 密文信封，明文不会写入临时文件、日志或 hosts.json。

use crate::hosts::{Host, HostStore};
use crate::keys::{KeyInfo, KeyNameStore};
use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use russh::keys::{HashAlg, PrivateKey};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

const FORMAT: &str = "daiqiao-config";
const VERSION: u32 = 1;
const MAX_FILE_SIZE: u64 = 10 * 1024 * 1024;
const MAX_PRIVATE_KEY_SIZE: u64 = 1024 * 1024;
const MAX_HOSTS: usize = 10_000;
const MAX_GROUPS: usize = 2_000;
const MAX_KEYS: usize = 10_000;
const KDF: &str = "PBKDF2-HMAC-SHA256";
const CIPHER: &str = "AES-256-GCM";
const ITERATIONS: u32 = 600_000;
static OPERATION_LOCK: Mutex<()> = Mutex::new(());

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct KeyMetadata {
    id: String,
    name: String,
    algo: String,
    fingerprint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    public_key: Option<String>,
    source: String,
    path: String,
}

impl KeyMetadata {
    fn from_info(info: &KeyInfo) -> Self {
        Self {
            id: info.id.clone(),
            name: info.name.clone(),
            algo: info.algo.clone(),
            fingerprint: info.fingerprint.clone(),
            public_key: info.public_key.clone(),
            source: info.source.clone(),
            path: portable_path(&info.path),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EncryptedCredentials {
    kdf: String,
    iterations: u32,
    salt: String,
    nonce: String,
    cipher: String,
    data: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CredentialsPayload {
    hosts: BTreeMap<String, String>,
    keys: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigFile {
    format: String,
    version: u32,
    created_at: String,
    hosts: Vec<Host>,
    groups: Vec<String>,
    keys: Vec<KeyMetadata>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    credentials: Option<EncryptedCredentials>,
}

#[derive(Debug)]
enum KeyResolution {
    Id(String),
    Import(KeyMetadata, String),
    Alias(String),
    Reference(String),
    Missing(String),
}

#[derive(Debug)]
struct PasswordBackup {
    host_id: String,
    old: Option<String>,
}

fn lock_operations() -> Result<std::sync::MutexGuard<'static, ()>, String> {
    OPERATION_LOCK
        .lock()
        .map_err(|_| "配置导入导出锁已失效".to_string())
}

fn random_bytes<const N: usize>() -> Result<[u8; N], String> {
    let mut bytes = [0u8; N];
    File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut bytes))
        .map_err(|e| format!("读取系统随机数失败: {e}"))?;
    Ok(bytes)
}

fn derive_key(passphrase: &str, salt: &[u8; 16]) -> [u8; 32] {
    let mut key = [0u8; 32];
    pbkdf2::pbkdf2_hmac::<pbkdf2::sha2::Sha256>(passphrase.as_bytes(), salt, ITERATIONS, &mut key);
    key
}

fn encrypt_credentials(
    payload: &CredentialsPayload,
    passphrase: &str,
) -> Result<EncryptedCredentials, String> {
    if passphrase.is_empty() {
        return Err("包含凭据时口令不能为空".to_string());
    }
    let plaintext = serde_json::to_vec(payload).map_err(|e| format!("序列化凭据失败: {e}"))?;
    let salt = random_bytes::<16>()?;
    let nonce = random_bytes::<12>()?;
    let key = Key::<Aes256Gcm>::from(derive_key(passphrase, &salt));
    let cipher = Aes256Gcm::new(&key);
    let nonce = Nonce::from(nonce);
    let data = cipher
        .encrypt(&nonce, plaintext.as_ref())
        .map_err(|_| "凭据加密失败".to_string())?;
    Ok(EncryptedCredentials {
        kdf: KDF.to_string(),
        iterations: ITERATIONS,
        salt: BASE64.encode(salt),
        nonce: BASE64.encode(nonce),
        cipher: CIPHER.to_string(),
        data: BASE64.encode(data),
    })
}

fn decode_fixed<const N: usize>(value: &str, label: &str) -> Result<[u8; N], String> {
    let bytes = BASE64
        .decode(value)
        .map_err(|_| format!("凭据信封的 {label} 不是有效 Base64"))?;
    bytes
        .try_into()
        .map_err(|v: Vec<u8>| format!("凭据信封的 {label} 长度应为 {N} 字节，实际为 {}", v.len()))
}

fn decrypt_credentials(
    envelope: &EncryptedCredentials,
    passphrase: Option<&str>,
) -> Result<CredentialsPayload, String> {
    let passphrase = passphrase
        .filter(|p| !p.is_empty())
        .ok_or_else(|| "这个配置包包含加密凭据，导入时必须提供非空口令".to_string())?;
    validate_envelope(envelope)?;
    let salt = decode_fixed::<16>(&envelope.salt, "salt")?;
    let nonce = decode_fixed::<12>(&envelope.nonce, "nonce")?;
    let data = BASE64
        .decode(&envelope.data)
        .map_err(|_| "凭据密文不是有效 Base64".to_string())?;
    let key = Key::<Aes256Gcm>::from(derive_key(passphrase, &salt));
    let cipher = Aes256Gcm::new(&key);
    let nonce = Nonce::from(nonce);
    let plaintext = cipher
        .decrypt(&nonce, data.as_ref())
        .map_err(|_| "口令错误或凭据密文已被篡改，未导入任何配置".to_string())?;
    serde_json::from_slice(&plaintext)
        .map_err(|_| "解密后的凭据结构无效，未导入任何配置".to_string())
}

fn validate_envelope(envelope: &EncryptedCredentials) -> Result<(), String> {
    if envelope.kdf != KDF || envelope.cipher != CIPHER || envelope.iterations != ITERATIONS {
        return Err(format!(
            "不支持的凭据加密参数（要求 {KDF} / {ITERATIONS} / {CIPHER}）"
        ));
    }
    decode_fixed::<16>(&envelope.salt, "salt")?;
    decode_fixed::<12>(&envelope.nonce, "nonce")?;
    let data = BASE64
        .decode(&envelope.data)
        .map_err(|_| "凭据密文不是有效 Base64".to_string())?;
    if data.len() < 16 {
        return Err("凭据密文长度无效".to_string());
    }
    Ok(())
}

fn read_limited(path: &Path, limit: u64) -> Result<Vec<u8>, String> {
    let metadata =
        std::fs::metadata(path).map_err(|e| format!("无法读取配置文件 {}: {e}", path.display()))?;
    if !metadata.is_file() {
        return Err(format!("配置路径不是普通文件: {}", path.display()));
    }
    if metadata.len() > limit {
        return Err(format!("文件超过大小上限 {limit} 字节"));
    }
    let file = File::open(path).map_err(|e| format!("打开文件失败: {e}"))?;
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("读取文件失败: {e}"))?;
    if bytes.len() as u64 > limit {
        return Err(format!("文件超过大小上限 {limit} 字节"));
    }
    if bytes.is_empty() {
        return Err("配置文件不能为空".to_string());
    }
    Ok(bytes)
}

fn read_package(path: &str) -> Result<ConfigFile, String> {
    if path.trim().is_empty() {
        return Err("配置路径不能为空".to_string());
    }
    let bytes = read_limited(Path::new(path), MAX_FILE_SIZE)?;
    let package: ConfigFile =
        serde_json::from_slice(&bytes).map_err(|e| format!("配置 JSON 无效: {e}"))?;
    validate_package(&package)?;
    Ok(package)
}

fn validate_package(package: &ConfigFile) -> Result<(), String> {
    if package.format != FORMAT {
        return Err(format!("配置格式必须是 {FORMAT}"));
    }
    if package.version != VERSION {
        return Err(format!(
            "不支持的配置版本 {}，当前支持 {VERSION}",
            package.version
        ));
    }
    if package.created_at.is_empty() || package.created_at.len() > 64 {
        return Err("created_at 缺失或过长".to_string());
    }
    if package.hosts.len() > MAX_HOSTS {
        return Err(format!("主机数量超过上限 {MAX_HOSTS}"));
    }
    if package.groups.len() > MAX_GROUPS {
        return Err(format!("分组数量超过上限 {MAX_GROUPS}"));
    }
    if package.keys.len() > MAX_KEYS {
        return Err(format!("密钥数量超过上限 {MAX_KEYS}"));
    }

    let mut host_names = HashSet::new();
    let mut host_ids = HashSet::new();
    for host in &package.hosts {
        if host.id.is_empty()
            || host.name.trim().is_empty()
            || host.hostname.trim().is_empty()
            || host.user.trim().is_empty()
            || host.port == 0
        {
            return Err("主机 id、名称、地址、用户和端口必须有效".to_string());
        }
        if !matches!(host.auth.as_str(), "key" | "password" | "agent") {
            return Err(format!("主机「{}」的认证方式无效", host.name));
        }
        if !host_names.insert(host.name.as_str()) {
            return Err(format!("配置包内主机名称重复: {}", host.name));
        }
        if !host_ids.insert(host.id.as_str()) {
            return Err(format!("配置包内主机 id 重复: {}", host.id));
        }
        if host.name.len() > 1024
            || host.hostname.len() > 4096
            || host.user.len() > 1024
            || host.id.len() > 256
        {
            return Err(format!("主机「{}」的字段过长", host.name));
        }
    }

    let mut groups = HashSet::new();
    for group in &package.groups {
        if group.is_empty() || group.len() > 1024 {
            return Err("分组名称不能为空且不能超过 1024 字节".to_string());
        }
        if !groups.insert(group.as_str()) {
            return Err(format!("分组重复: {group}"));
        }
    }

    let mut key_ids = HashSet::new();
    for key in &package.keys {
        if key.id.is_empty()
            || key.name.is_empty()
            || key.fingerprint.is_empty()
            || key.path.is_empty()
            || !matches!(key.source.as_str(), "ssh_dir" | "vault")
        {
            return Err("密钥元数据缺少 id/name/fingerprint/path 或 source 无效".to_string());
        }
        if key.id.len() > 256 || key.name.len() > 1024 || key.path.len() > 4096 {
            return Err(format!("密钥「{}」的元数据字段过长", key.name));
        }
        if !key_ids.insert(key.id.as_str()) {
            return Err(format!("配置包内密钥 id 重复: {}", key.id));
        }
    }

    if let Some(envelope) = &package.credentials {
        validate_envelope(envelope)?;
    }
    Ok(())
}

fn validate_credentials(package: &ConfigFile, payload: &CredentialsPayload) -> Result<(), String> {
    if payload.hosts.len() > package.hosts.len() || payload.keys.len() > package.keys.len() {
        return Err("凭据条目数量超过配置记录数量".to_string());
    }
    let hosts: HashMap<&str, &Host> = package.hosts.iter().map(|h| (h.id.as_str(), h)).collect();
    for (host_id, password) in &payload.hosts {
        let host = hosts
            .get(host_id.as_str())
            .ok_or_else(|| "凭据引用了配置包中不存在的主机 id".to_string())?;
        if host.auth != "password" {
            return Err(format!(
                "主机「{}」不是密码认证，拒绝写入孤立密码",
                host.name
            ));
        }
        if password.is_empty() {
            return Err(format!("主机「{}」的导入密码为空", host.name));
        }
    }

    let keys: HashMap<&str, &KeyMetadata> =
        package.keys.iter().map(|k| (k.id.as_str(), k)).collect();
    let mut plaintext_by_fingerprint: HashMap<&str, &str> = HashMap::new();
    for (key_id, text) in &payload.keys {
        let metadata = keys
            .get(key_id.as_str())
            .ok_or_else(|| "凭据引用了配置包中不存在的密钥 id".to_string())?;
        if text.len() as u64 > MAX_PRIVATE_KEY_SIZE {
            return Err(format!("密钥「{}」的私钥正文过大", metadata.name));
        }
        let private = PrivateKey::from_openssh(text)
            .map_err(|_| format!("密钥「{}」的私钥正文无效", metadata.name))?;
        let actual = private.fingerprint(HashAlg::Sha256).to_string();
        if actual != metadata.fingerprint {
            return Err(format!("密钥「{}」的私钥与元数据指纹不一致", metadata.name));
        }
        if let Some(previous) = plaintext_by_fingerprint.insert(&metadata.fingerprint, text) {
            if previous != text {
                return Err("同一指纹对应了不同私钥正文，拒绝导入".to_string());
            }
        }
    }
    Ok(())
}

fn portable_path(path: &str) -> String {
    let Ok(home) = std::env::var("HOME") else {
        return path.to_string();
    };
    let path_obj = Path::new(path);
    for relative in [".ssh", ".daiqiao/keys"] {
        let base = Path::new(&home).join(relative);
        if let Ok(rest) = path_obj.strip_prefix(&base) {
            let suffix = rest
                .components()
                .map(|c| c.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");
            return if suffix.is_empty() {
                format!("~/{relative}")
            } else {
                format!("~/{relative}/{suffix}")
            };
        }
    }
    path.to_string()
}

fn localized_path(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return Path::new(&home).join(rest).to_string_lossy().into_owned();
        }
    }
    path.to_string()
}

fn private_key_for_export(info: &KeyInfo) -> Result<String, String> {
    let bytes = read_limited(Path::new(&info.path), MAX_PRIVATE_KEY_SIZE)
        .map_err(|_| format!("读取密钥「{}」的私钥失败", info.name))?;
    let text = String::from_utf8(bytes)
        .map_err(|_| format!("密钥「{}」的私钥不是 UTF-8 文本", info.name))?;
    let private = PrivateKey::from_openssh(&text)
        .map_err(|_| format!("密钥「{}」的私钥正文无效", info.name))?;
    if private.fingerprint(HashAlg::Sha256).to_string() != info.fingerprint {
        return Err(format!("密钥「{}」在导出时发生变化，请重试", info.name));
    }
    Ok(text)
}

fn write_atomic(path: &Path, data: &[u8]) -> Result<(), String> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| "输出路径缺少父目录".to_string())?;
    if !parent.is_dir() {
        return Err(format!("输出目录不存在: {}", parent.display()));
    }
    let filename = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| "输出路径缺少有效文件名".to_string())?;
    let random = random_bytes::<16>()?;
    let suffix = random
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    let temporary = parent.join(format!(".{filename}.{suffix}.tmp"));

    let result = (|| -> Result<(), String> {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&temporary)
            .map_err(|e| format!("创建输出文件失败: {e}"))?;
        file.write_all(data)
            .map_err(|e| format!("写入输出文件失败: {e}"))?;
        file.sync_all()
            .map_err(|e| format!("同步输出文件失败: {e}"))?;
        drop(file);
        std::fs::rename(&temporary, path).map_err(|e| format!("完成输出文件失败: {e}"))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

fn save_hosts_atomic(store: &HostStore, path: &Path) -> Result<(), String> {
    let text = serde_json::to_vec_pretty(store).map_err(|e| format!("序列化主机失败: {e}"))?;
    write_atomic(path, &text)
}

fn save_names_atomic(store: &KeyNameStore, path: &Path) -> Result<(), String> {
    let text = serde_json::to_vec_pretty(store).map_err(|e| format!("序列化密钥名称失败: {e}"))?;
    write_atomic(path, &text)
}

fn used_key_map() -> HashMap<String, Vec<String>> {
    let mut map = HashMap::new();
    for host in HostStore::load().hosts {
        if let Some(id) = host.key_id {
            map.entry(id).or_insert_with(Vec::new).push(host.name);
        }
    }
    map
}

/// 导出全部主机、由主机归并出的分组和密钥元数据。
/// 建议前端使用 `.dqconfig` 扩展名；后端不强制扩展名。
#[tauri::command]
pub fn export_config(
    path: String,
    include_credentials: bool,
    passphrase: Option<String>,
) -> Result<String, String> {
    let _guard = lock_operations()?;
    if path.trim().is_empty() {
        return Err("导出路径不能为空".to_string());
    }
    if include_credentials && passphrase.as_deref().unwrap_or("").is_empty() {
        return Err("包含私钥和密码时必须提供非空口令".to_string());
    }

    let store = HostStore::load();
    let key_infos = crate::keys::list_keys(&used_key_map());
    let groups = store
        .hosts
        .iter()
        .filter_map(|h| h.group.clone())
        .filter(|g| !g.is_empty())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let hosts = store
        .hosts
        .iter()
        .cloned()
        .map(|mut h| {
            if let Some(key_path) = &h.key_path {
                h.key_path = Some(portable_path(key_path));
            }
            h
        })
        .collect();
    let keys = key_infos
        .iter()
        .map(KeyMetadata::from_info)
        .collect::<Vec<_>>();
    let mut package = ConfigFile {
        format: FORMAT.to_string(),
        version: VERSION,
        created_at: utc_now(),
        hosts,
        groups,
        keys,
        credentials: None,
    };
    validate_package(&package)?;

    if include_credentials {
        let mut payload = CredentialsPayload::default();
        for host in &store.hosts {
            if host.auth == "password" {
                if let Some(password) = crate::keychain::get(&host.id)? {
                    payload.hosts.insert(host.id.clone(), password);
                }
            }
        }
        for info in &key_infos {
            payload
                .keys
                .insert(info.id.clone(), private_key_for_export(info)?);
        }
        let encrypted = encrypt_credentials(&payload, passphrase.as_deref().unwrap_or(""))?;
        package.credentials = Some(encrypted);
    }

    let text = serde_json::to_vec_pretty(&package).map_err(|e| format!("序列化配置失败: {e}"))?;
    if text.len() as u64 > MAX_FILE_SIZE {
        return Err(format!("导出后配置超过大小上限 {MAX_FILE_SIZE} 字节"));
    }
    write_atomic(Path::new(&path), &text)?;
    Ok(format!(
        "已导出 {} 台主机、{} 个分组、{} 把密钥{}",
        package.hosts.len(),
        package.groups.len(),
        package.keys.len(),
        if include_credentials {
            "（凭据已加密）"
        } else {
            "（不含凭据）"
        }
    ))
}

/// 只检查外层结构和数量；绝不解密或返回凭据内容。
#[tauri::command]
pub fn inspect_config(path: String) -> Result<serde_json::Value, String> {
    let package = read_package(&path)?;
    Ok(serde_json::json!({
        "format_version": package.version,
        "hosts": package.hosts.len(),
        "groups": package.groups.len(),
        "keys": package.keys.len(),
        "includes_credentials": package.credentials.is_some(),
    }))
}

fn rollback(
    imported: &[KeyInfo],
    passwords: &[PasswordBackup],
    original_names: &KeyNameStore,
    names_path: &Path,
    names_committed: bool,
) -> bool {
    let mut clean = true;
    for backup in passwords {
        let result = match &backup.old {
            Some(old) => crate::keychain::set(&backup.host_id, old),
            None => crate::keychain::delete(&backup.host_id),
        };
        clean &= result.is_ok();
    }
    if names_committed {
        clean &= save_names_atomic(original_names, names_path).is_ok();
    }
    for info in imported {
        clean &= std::fs::remove_file(&info.path).is_ok();
    }
    clean
}

/// 按主机名称合并；新主机重新分配 id，密钥按指纹复用并重映射引用。
#[tauri::command]
pub fn import_config(path: String, passphrase: Option<String>) -> Result<String, String> {
    let _guard = lock_operations()?;
    let package = read_package(&path)?;
    let payload = match &package.credentials {
        Some(envelope) => {
            let payload = decrypt_credentials(envelope, passphrase.as_deref())?;
            validate_credentials(&package, &payload)?;
            payload
        }
        None => CredentialsPayload::default(),
    };

    let mut store = HostStore::load();
    let hosts_path = HostStore::path().ok_or_else(|| "找不到 HOME 目录".to_string())?;
    let names_path = KeyNameStore::path().ok_or_else(|| "找不到 HOME 目录".to_string())?;
    let config_dir = hosts_path
        .parent()
        .ok_or_else(|| "主机配置路径无效".to_string())?;
    std::fs::create_dir_all(config_dir).map_err(|e| format!("创建配置目录失败: {e}"))?;

    let original_names = KeyNameStore::load();
    let existing_keys = crate::keys::list_keys(&used_key_map());
    let existing_by_fingerprint: HashMap<String, String> = existing_keys
        .iter()
        .map(|k| (k.fingerprint.clone(), k.id.clone()))
        .collect();
    let metadata_by_id: HashMap<&str, &KeyMetadata> =
        package.keys.iter().map(|k| (k.id.as_str(), k)).collect();
    let metadata_by_path: HashMap<&str, &KeyMetadata> =
        package.keys.iter().map(|k| (k.path.as_str(), k)).collect();

    let mut resolutions: HashMap<String, KeyResolution> = HashMap::new();
    let mut by_fingerprint: HashMap<String, String> = existing_by_fingerprint;
    let mut planned_imports: HashMap<String, String> = HashMap::new();
    for metadata in &package.keys {
        if let Some(id) = by_fingerprint.get(&metadata.fingerprint) {
            resolutions.insert(metadata.id.clone(), KeyResolution::Id(id.clone()));
            continue;
        }
        if let Some(text) = payload.keys.get(&metadata.id) {
            if let Some(canonical_id) = planned_imports.get(&metadata.fingerprint) {
                resolutions.insert(
                    metadata.id.clone(),
                    KeyResolution::Alias(canonical_id.clone()),
                );
            } else {
                planned_imports.insert(metadata.fingerprint.clone(), metadata.id.clone());
                resolutions.insert(
                    metadata.id.clone(),
                    KeyResolution::Import(metadata.clone(), text.clone()),
                );
            }
            continue;
        }
        if metadata.source == "ssh_dir" {
            resolutions.insert(
                metadata.id.clone(),
                KeyResolution::Reference(localized_path(&metadata.path)),
            );
        } else {
            let mut missing_id = metadata.id.clone();
            let mut suffix = 1usize;
            while existing_keys.iter().any(|k| k.id == missing_id) {
                suffix += 1;
                missing_id = format!("{}-imported-{suffix}", metadata.id);
            }
            resolutions.insert(metadata.id.clone(), KeyResolution::Missing(missing_id));
        }
    }

    let mut inserted = 0usize;
    let mut updated = 0usize;
    let mut affected_hosts = HashSet::new();
    let mut target_passwords = BTreeMap::new();
    for imported in &package.hosts {
        let mut host = imported.clone();
        let referenced = host
            .key_id
            .as_deref()
            .and_then(|id| metadata_by_id.get(id).copied())
            .or_else(|| {
                host.key_path
                    .as_deref()
                    .and_then(|path| metadata_by_path.get(path).copied())
            });
        if let Some(metadata) = referenced {
            match resolutions.get(&metadata.id) {
                Some(KeyResolution::Id(id)) => {
                    host.key_id = Some(id.clone());
                    host.key_path = None;
                }
                Some(KeyResolution::Import(key, _)) => {
                    host.key_id = Some(key.id.clone());
                    host.key_path = None;
                }
                Some(KeyResolution::Alias(alias)) => {
                    host.key_id = Some(alias.clone());
                    host.key_path = None;
                }
                Some(KeyResolution::Reference(path)) => {
                    host.key_id = None;
                    host.key_path = Some(path.clone());
                }
                Some(KeyResolution::Missing(id)) => {
                    host.key_id = Some(id.clone());
                    host.key_path = None;
                }
                None => return Err("内部错误：密钥引用没有解析结果".to_string()),
            }
        } else {
            if let Some(path) = &host.key_path {
                host.key_path = Some(localized_path(path));
            }
        }

        let target_id = if let Some(index) = store.hosts.iter().position(|h| h.name == host.name) {
            let id = store.hosts[index].id.clone();
            host.id = id.clone();
            store.hosts[index] = host;
            updated += 1;
            id
        } else {
            let id = loop {
                let candidate = store.next_id();
                if !store.hosts.iter().any(|h| h.id == candidate) {
                    break candidate;
                }
            };
            host.id = id.clone();
            store.hosts.push(host);
            inserted += 1;
            id
        };
        if let Some(password) = payload.hosts.get(&imported.id) {
            target_passwords.insert(target_id.clone(), password.clone());
        }
        affected_hosts.insert(target_id);
    }

    let resulting_groups = store
        .hosts
        .iter()
        .filter_map(|h| h.group.as_ref())
        .collect::<HashSet<_>>();
    if store.hosts.len() > MAX_HOSTS || resulting_groups.len() > MAX_GROUPS {
        return Err("合并后的主机或分组数量超过上限".to_string());
    }

    let mut password_backups = Vec::new();
    for host_id in target_passwords.keys() {
        password_backups.push(PasswordBackup {
            host_id: host_id.clone(),
            old: crate::keychain::get(host_id)?,
        });
    }

    let mut imported_keys: Vec<KeyInfo> = Vec::new();
    let mut imported_by_old_id: HashMap<String, String> = HashMap::new();
    let mut new_names = original_names.clone();
    for metadata in &package.keys {
        let old_id = &metadata.id;
        let resolution = resolutions
            .get(old_id)
            .ok_or_else(|| "内部错误：密钥导入计划缺失".to_string())?;
        let KeyResolution::Import(metadata, text) = resolution else {
            continue;
        };
        if by_fingerprint.contains_key(&metadata.fingerprint) {
            continue;
        }
        // 配置迁移没法当场向人要口令，走 raw 导入（和 import_file 同语义）：
        // 带口令的私钥也能迁进来，连之前去密钥库存口令
        match crate::keys::import_text_raw(text, None) {
            Ok(info) => {
                if info.fingerprint != metadata.fingerprint {
                    let _ = std::fs::remove_file(&info.path);
                    let clean = rollback(&imported_keys, &[], &original_names, &names_path, false);
                    return Err(if clean {
                        "导入密钥后的指纹校验失败，已回滚".to_string()
                    } else {
                        "导入密钥后的指纹校验失败，部分回滚未完成".to_string()
                    });
                }
                imported_by_old_id.insert(old_id.clone(), info.id.clone());
                by_fingerprint.insert(metadata.fingerprint.clone(), info.id.clone());
                new_names
                    .names
                    .insert(info.id.clone(), metadata.name.clone());
                imported_keys.push(info);
            }
            Err(error) => {
                let clean = rollback(&imported_keys, &[], &original_names, &names_path, false);
                return Err(format!(
                    "写入密钥「{}」失败，未导入主机配置{}: {error}",
                    metadata.name,
                    if clean {
                        "，已回滚密钥"
                    } else {
                        "，密钥回滚不完整"
                    }
                ));
            }
        }
    }

    for host in &mut store.hosts {
        if !affected_hosts.contains(&host.id) {
            continue;
        }
        let Some(key_id) = &host.key_id else {
            continue;
        };
        if let Some(new_id) = imported_by_old_id.get(key_id) {
            host.key_id = Some(new_id.clone());
        }
    }

    let mut applied_passwords: Vec<PasswordBackup> = Vec::new();
    for backup in password_backups {
        let password = target_passwords
            .get(&backup.host_id)
            .ok_or_else(|| "内部错误：密码计划缺失".to_string())?;
        applied_passwords.push(PasswordBackup {
            host_id: backup.host_id.clone(),
            old: backup.old,
        });
        if let Err(error) = crate::keychain::set(&backup.host_id, password) {
            let clean = rollback(
                &imported_keys,
                &applied_passwords,
                &original_names,
                &names_path,
                false,
            );
            return Err(format!(
                "写入钥匙串失败，未导入主机配置{}: {error}",
                if clean {
                    "，已回滚"
                } else {
                    "，回滚不完整"
                }
            ));
        }
    }

    let names_changed = !imported_keys.is_empty();
    if names_changed {
        if let Err(error) = save_names_atomic(&new_names, &names_path) {
            let clean = rollback(
                &imported_keys,
                &applied_passwords,
                &original_names,
                &names_path,
                false,
            );
            return Err(format!(
                "保存密钥名称失败，未导入主机配置{}: {error}",
                if clean {
                    "，已回滚"
                } else {
                    "，回滚不完整"
                }
            ));
        }
    }

    if let Err(error) = save_hosts_atomic(&store, &hosts_path) {
        let clean = rollback(
            &imported_keys,
            &applied_passwords,
            &original_names,
            &names_path,
            names_changed,
        );
        return Err(format!(
            "保存主机配置失败{}: {error}",
            if clean {
                "，已回滚"
            } else {
                "，回滚不完整"
            }
        ));
    }

    Ok(format!(
        "导入完成：新增 {inserted} 台主机，更新 {updated} 台主机；新导入 {} 把密钥，写入 {} 个密码",
        imported_keys.len(),
        applied_passwords.len()
    ))
}

fn utc_now() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let days = seconds.div_euclid(86_400);
    let day_seconds = seconds.rem_euclid(86_400);
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let mut year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    year += if month <= 2 { 1 } else { 0 };
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        day_seconds / 3600,
        (day_seconds % 3600) / 60,
        day_seconds % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::MutexGuard;

    static ENV_LOCK: Mutex<()> = Mutex::new(());
    static NEXT_SANDBOX: AtomicU64 = AtomicU64::new(1);

    struct Sandbox {
        root: PathBuf,
        old_home: Option<String>,
        _guard: MutexGuard<'static, ()>,
    }

    impl Sandbox {
        fn new() -> Self {
            let guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let serial = NEXT_SANDBOX.fetch_add(1, Ordering::Relaxed);
            let root =
                std::env::temp_dir().join(format!("dq-config-io-{}-{serial}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            let home = root.join("source");
            std::fs::create_dir_all(home.join(".ssh")).unwrap();
            let old_home = std::env::var("HOME").ok();
            std::env::set_var("HOME", &home);
            crate::keychain::clear();
            assert_eq!(HostStore::path().unwrap(), home.join(".daiqiao/hosts.json"));
            Self {
                root,
                old_home,
                _guard: guard,
            }
        }

        fn use_home(&self, name: &str) -> PathBuf {
            let home = self.root.join(name);
            std::fs::create_dir_all(home.join(".ssh")).unwrap();
            std::env::set_var("HOME", &home);
            crate::keychain::clear();
            home
        }

        fn source_home(&self) -> PathBuf {
            self.root.join("source")
        }
    }

    impl Drop for Sandbox {
        fn drop(&mut self) {
            match &self.old_home {
                Some(home) => std::env::set_var("HOME", home),
                None => std::env::remove_var("HOME"),
            }
            crate::keychain::clear();
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn host(id: &str, name: &str, auth: &str) -> Host {
        Host {
            id: id.to_string(),
            name: name.to_string(),
            hostname: format!("{name}.example.com"),
            port: 22,
            user: "deploy".to_string(),
            auth: auth.to_string(),
            key_id: None,
            key_path: None,
            tags: vec!["迁移".to_string()],
            group: Some("生产 / 华东".to_string()),
            note: Some("中文备注".to_string()),
        }
    }

    fn generate_key(home: &Path, vault: bool, name: &str) -> KeyInfo {
        let dir = if vault {
            home.join(".daiqiao/keys")
        } else {
            home.join(".ssh")
        };
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        let status = std::process::Command::new("ssh-keygen")
            .args(["-q", "-t", "ed25519", "-N", "", "-f"])
            .arg(&path)
            .status()
            .unwrap();
        assert!(status.success());
        crate::keys::find_key(&crate::keys::key_id(&path), &HashMap::new()).unwrap()
    }

    fn sample_payload() -> CredentialsPayload {
        CredentialsPayload {
            hosts: BTreeMap::from([("h源".to_string(), "密码：海棠未眠 🔐\n第二行".to_string())]),
            keys: BTreeMap::from([(
                "k源".to_string(),
                "私钥正文占位，仅用于加密往返".to_string(),
            )]),
        }
    }

    #[test]
    fn 无凭据导出不包含私钥或主机密码() {
        let sandbox = Sandbox::new();
        let home = sandbox.source_home();
        let info = generate_key(&home, false, "id_ed25519");
        let private = std::fs::read_to_string(&info.path).unwrap();
        let mut key_host = host("h1", "密钥机", "key");
        key_host.key_id = Some(info.id.clone());
        let password_host = host("h2", "密码机", "password");
        HostStore {
            hosts: vec![key_host, password_host],
            seq: 2,
        }
        .save()
        .unwrap();
        crate::keychain::set("h2", "绝不能落盘的密码-密碼-🔐").unwrap();
        let output = sandbox.root.join("plain.dqconfig");

        export_config(
            output.to_string_lossy().into_owned(),
            false,
            Some("这个口令必须被忽略".to_string()),
        )
        .unwrap();

        let text = std::fs::read_to_string(&output).unwrap();
        assert!(!text.contains("绝不能落盘的密码-密碼-🔐"));
        assert!(!text.contains(&private));
        assert!(!text.contains("PRIVATE KEY"));
        let package: ConfigFile = serde_json::from_str(&text).unwrap();
        assert!(package.credentials.is_none());
        assert_eq!(package.groups, vec!["生产 / 华东"]);
        assert!(package.keys[0].path.starts_with("~/.ssh/"));
        assert_eq!(
            inspect_config(output.to_string_lossy().into_owned()).unwrap(),
            serde_json::json!({
                "format_version": 1,
                "hosts": 2,
                "groups": 1,
                "keys": 1,
                "includes_credentials": false,
            })
        );
    }

    #[test]
    fn aes_gcm中文往返() {
        let payload = sample_payload();
        let envelope = encrypt_credentials(&payload, "口令-🔐-passphrase").unwrap();
        assert_eq!(envelope.kdf, KDF);
        assert_eq!(envelope.iterations, 600_000);
        assert_eq!(
            decode_fixed::<16>(&envelope.salt, "salt").unwrap().len(),
            16
        );
        assert_eq!(
            decode_fixed::<12>(&envelope.nonce, "nonce").unwrap().len(),
            12
        );
        assert!(!envelope.data.contains("海棠未眠"));
        let decrypted = decrypt_credentials(&envelope, Some("口令-🔐-passphrase")).unwrap();
        assert_eq!(decrypted.hosts, payload.hosts);
        assert_eq!(decrypted.keys, payload.keys);
    }

    #[test]
    fn 错误口令和密文篡改都失败且不半导入() {
        let sandbox = Sandbox::new();
        let imported = host("h-old", "不应出现", "password");
        let mut package = ConfigFile {
            format: FORMAT.to_string(),
            version: VERSION,
            created_at: utc_now(),
            hosts: vec![imported.clone()],
            groups: vec!["生产 / 华东".to_string()],
            keys: vec![],
            credentials: None,
        };
        let payload = CredentialsPayload {
            hosts: BTreeMap::from([("h-old".to_string(), "中文密码".to_string())]),
            keys: BTreeMap::new(),
        };
        package.credentials = Some(encrypt_credentials(&payload, "正确口令").unwrap());
        let path = sandbox.root.join("encrypted.dqconfig");
        std::fs::write(&path, serde_json::to_vec_pretty(&package).unwrap()).unwrap();

        let error = import_config(
            path.to_string_lossy().into_owned(),
            Some("错误口令".to_string()),
        )
        .unwrap_err();
        assert!(error.contains("口令错误或"));
        assert!(HostStore::load().hosts.is_empty());

        let mut tampered = package;
        let envelope = tampered.credentials.as_mut().unwrap();
        let mut bytes = BASE64.decode(&envelope.data).unwrap();
        let last = bytes.last_mut().unwrap();
        *last ^= 1;
        envelope.data = BASE64.encode(bytes);
        std::fs::write(&path, serde_json::to_vec_pretty(&tampered).unwrap()).unwrap();
        let error = import_config(
            path.to_string_lossy().into_owned(),
            Some("正确口令".to_string()),
        )
        .unwrap_err();
        assert!(error.contains("口令错误或"));
        assert!(HostStore::load().hosts.is_empty());
        assert_eq!(crate::keychain::get("h-old").unwrap(), None);
    }

    #[test]
    fn 导入按名称合并并重映射主机和密钥id() {
        let sandbox = Sandbox::new();
        let source_home = sandbox.source_home();
        let source_key = generate_key(&source_home, true, "生产密钥");
        let mut source_key_host = host("h-source-key", "同名主机", "key");
        source_key_host.key_id = Some(source_key.id.clone());
        source_key_host.hostname = "new.example.com".to_string();
        let source_password_host = host("h1", "新增密码机", "password");
        HostStore {
            hosts: vec![source_key_host, source_password_host],
            seq: 99,
        }
        .save()
        .unwrap();
        crate::keychain::set("h1", "跨机密码-🔐").unwrap();
        let output = sandbox.root.join("with-credentials.dqconfig");
        export_config(
            output.to_string_lossy().into_owned(),
            true,
            Some("迁移口令".to_string()),
        )
        .unwrap();
        let encrypted_text = std::fs::read_to_string(&output).unwrap();
        assert!(!encrypted_text.contains("跨机密码-🔐"));
        assert!(!encrypted_text.contains("PRIVATE KEY"));

        sandbox.use_home("target");
        let mut existing = host("h1", "同名主机", "agent");
        existing.hostname = "old.example.com".to_string();
        existing.group = Some("旧分组".to_string());
        let mut unrelated = host("h9", "未参与导入", "key");
        unrelated.key_id = Some(source_key.id.clone());
        HostStore {
            hosts: vec![existing, unrelated],
            seq: 9,
        }
        .save()
        .unwrap();

        let summary = import_config(
            output.to_string_lossy().into_owned(),
            Some("迁移口令".to_string()),
        )
        .unwrap();
        assert!(summary.contains("新增 1 台"), "{summary}");
        assert!(summary.contains("更新 1 台"), "{summary}");
        assert!(summary.contains("新导入 1 把密钥"), "{summary}");
        let store = HostStore::load();
        assert_eq!(store.hosts.len(), 3);
        let merged = store.hosts.iter().find(|h| h.name == "同名主机").unwrap();
        assert_eq!(merged.id, "h1", "同名更新必须保留目标主机 id");
        assert_eq!(merged.hostname, "new.example.com");
        assert_eq!(merged.group.as_deref(), Some("生产 / 华东"));
        assert_ne!(merged.key_id.as_deref(), Some(source_key.id.as_str()));
        let added = store.hosts.iter().find(|h| h.name == "新增密码机").unwrap();
        assert_ne!(added.id, "h1", "源 h1 与目标其他主机冲突，必须分配新 id");
        assert_eq!(
            crate::keychain::get(&added.id).unwrap().as_deref(),
            Some("跨机密码-🔐")
        );
        assert_eq!(
            crate::keychain::get("h1").unwrap(),
            None,
            "密码不能写错到源 id 冲突的主机"
        );
        let imported_key =
            crate::keys::find_key(merged.key_id.as_deref().unwrap(), &HashMap::new()).unwrap();
        assert_eq!(imported_key.fingerprint, source_key.fingerprint);
        assert_eq!(
            store
                .hosts
                .iter()
                .find(|h| h.name == "未参与导入")
                .unwrap()
                .key_id
                .as_deref(),
            Some(source_key.id.as_str()),
            "本次导入之外的原有主机不能被重映射"
        );

        let second = import_config(
            output.to_string_lossy().into_owned(),
            Some("迁移口令".to_string()),
        )
        .unwrap();
        assert!(second.contains("新增 0 台"), "{second}");
        assert!(second.contains("更新 2 台"), "{second}");
        assert!(second.contains("新导入 0 把密钥"), "{second}");
        assert_eq!(HostStore::load().hosts.len(), 3);
        assert_eq!(
            crate::keys::list_keys(&HashMap::new())
                .iter()
                .filter(|k| k.fingerprint == source_key.fingerprint)
                .count(),
            1,
            "重复导入必须按指纹复用"
        );
    }

    #[test]
    fn 同id不同指纹的密钥会生成新id且不覆盖目标密钥() {
        let sandbox = Sandbox::new();
        let home = sandbox.source_home();
        let source_key = generate_key(&home, true, "same-path");
        let original_private = std::fs::read_to_string(&source_key.path).unwrap();
        let mut source_host = host("h1", "冲突密钥主机", "key");
        source_host.key_id = Some(source_key.id.clone());
        HostStore {
            hosts: vec![source_host],
            seq: 1,
        }
        .save()
        .unwrap();
        let output = sandbox.root.join("key-id-conflict.dqconfig");
        export_config(
            output.to_string_lossy().into_owned(),
            true,
            Some("口令".to_string()),
        )
        .unwrap();

        std::fs::remove_file(&source_key.path).unwrap();
        let _ = std::fs::remove_file(format!("{}.pub", source_key.path));
        let target_key = generate_key(&home, true, "same-path");
        assert_eq!(target_key.id, source_key.id, "测试必须制造同路径同 id");
        assert_ne!(target_key.fingerprint, source_key.fingerprint);
        let target_private = std::fs::read_to_string(&target_key.path).unwrap();
        assert_ne!(target_private, original_private);
        HostStore::default().save().unwrap();

        import_config(
            output.to_string_lossy().into_owned(),
            Some("口令".to_string()),
        )
        .unwrap();
        let store = HostStore::load();
        let imported_id = store.hosts[0].key_id.as_deref().unwrap();
        assert_ne!(imported_id, target_key.id);
        let imported = crate::keys::find_key(imported_id, &HashMap::new()).unwrap();
        assert_eq!(imported.fingerprint, source_key.fingerprint);
        assert!(
            std::fs::read_to_string(imported.path).unwrap() == original_private.trim(),
            "导入的必须是源私钥"
        );
        assert!(
            std::fs::read_to_string(&target_key.path).unwrap() == target_private,
            "目标机原有同 id 密钥不能被覆盖"
        );
    }

    #[test]
    fn 无凭据ssh目录引用会迁移到目标home且缺钥不伪造导入() {
        let sandbox = Sandbox::new();
        let source_home = sandbox.source_home();
        let source_key = generate_key(&source_home, false, "id_ed25519");
        let mut source_host = host("h1", "引用主机", "key");
        source_host.key_id = Some(source_key.id.clone());
        HostStore {
            hosts: vec![source_host],
            seq: 1,
        }
        .save()
        .unwrap();
        let output = sandbox.root.join("reference-only.dqconfig");
        export_config(output.to_string_lossy().into_owned(), false, None).unwrap();

        let target_home = sandbox.use_home("target-without-key");
        import_config(output.to_string_lossy().into_owned(), None).unwrap();
        let store = HostStore::load();
        let imported = &store.hosts[0];
        assert_eq!(imported.key_id, None);
        assert_eq!(
            imported.key_path.as_deref(),
            Some(
                target_home
                    .join(".ssh/id_ed25519")
                    .to_string_lossy()
                    .as_ref()
            )
        );
        assert!(crate::keys::list_keys(&HashMap::new()).is_empty());
        assert!(!target_home.join(".ssh/id_ed25519").exists());
    }

    #[test]
    fn 格式版本数量与凭据引用在写入前拒绝() {
        let sandbox = Sandbox::new();
        let path = sandbox.root.join("bad.dqconfig");
        let mut package = ConfigFile {
            format: FORMAT.to_string(),
            version: 2,
            created_at: utc_now(),
            hosts: vec![],
            groups: vec![],
            keys: vec![],
            credentials: None,
        };
        std::fs::write(&path, serde_json::to_vec(&package).unwrap()).unwrap();
        assert!(inspect_config(path.to_string_lossy().into_owned()).is_err());

        package.version = VERSION;
        package.hosts = vec![host("h1", "重复", "agent"), host("h2", "重复", "agent")];
        std::fs::write(&path, serde_json::to_vec(&package).unwrap()).unwrap();
        assert!(import_config(path.to_string_lossy().into_owned(), None).is_err());
        assert!(HostStore::load().hosts.is_empty());

        package.hosts = vec![host("h1", "密码机", "password")];
        package.credentials = Some(encrypt_credentials(&sample_payload(), "口令").unwrap());
        std::fs::write(&path, serde_json::to_vec(&package).unwrap()).unwrap();
        assert!(import_config(
            path.to_string_lossy().into_owned(),
            Some("口令".to_string())
        )
        .is_err());
        assert!(HostStore::load().hosts.is_empty());
    }
}
