//! SSH 凭据存进 macOS 钥匙串：主机的登录密码 + 私钥的解锁口令。
//!
//! 凭据红线：密码/口令只进钥匙串，不进 hosts.json、不进审计日志、不进 MCP 上下文。
//! AI 侧只能问「存没存」（host_list 里的 password_saved、密钥库里的
//! passphrase_saved），拿不到内容。
//!
//! 实现走系统 `security` CLI。有个坑必须绕：`find-generic-password -w`
//! 对非 ASCII 密码读回来的不是原文，而是 UTF-8 字节的十六进制（实测中英混排密码
//! 读回 `e6b58be8...`）。与其猜 CLI 什么时候吐 hex、什么时候吐原文，
//! 不如**存的时候就存 hex** —— hex 是纯 ASCII，读回永远是原样，解码是确定的。
//!
//! 副作用：`-w` 参数会在 ps 里短暂可见（毫秒级、纯 hex、单用户机器）。
//! 要彻底消除得换 Security.framework 绑定，接口不变，以后可以无痛换。

use std::process::Command;

const SERVICE: &str = "daiqiao";

/// 主机密码的 account 用主机 id（不是名字 —— 改名不该把密码搞丢）
fn account(host_id: &str) -> String {
    format!("ssh-host:{host_id}")
}

/// 私钥口令的 account 用密钥 id（密钥库里的稳定 id，换显示名不受影响）。
/// 和 ssh-host: 前缀并列 —— 同一个 id 既是主机又是密钥时两条凭据互不串门。
fn key_account(key_id: &str) -> String {
    format!("ssh-key:{key_id}")
}

fn utf8_hex(s: &str) -> String {
    s.as_bytes().iter().map(|b| format!("{b:02x}")).collect()
}

fn hex_utf8(h: &str) -> Option<String> {
    if h.len() % 2 != 0 || !h.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let bytes: Vec<u8> = (0..h.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&h[i..i + 2], 16).unwrap())
        .collect();
    String::from_utf8(bytes).ok()
}

fn security(args: &[&str]) -> Result<String, String> {
    let out = Command::new("/usr/bin/security")
        .args(args)
        .output()
        .map_err(|e| format!("调不起 security CLI: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout)
            .trim_end_matches('\n')
            .to_string())
    } else {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        Err(err)
    }
}

/// 存（或覆盖）一台主机的密码。
pub fn set(host_id: &str, password: &str) -> Result<(), String> {
    set_entry(&account(host_id), password, "密码")
}

/// 取密码。没存过返回 None（这不是错误）。
pub fn get(host_id: &str) -> Result<Option<String>, String> {
    get_entry(&account(host_id))
}

/// 删掉一台主机的密码。没存过也不算错。
pub fn delete(host_id: &str) -> Result<(), String> {
    delete_entry(&account(host_id))
}

/// 这台主机存没存密码（不暴露内容）。界面和 MCP 都只看这个。
pub fn saved(host_id: &str) -> bool {
    matches!(get(host_id), Ok(Some(_)))
}

// ─────────────── 私钥解锁口令（带 passphrase 的 SSH 私钥）───────────────
// 和主机密码同一套 CLI 封装，只是 account 前缀不同（ssh-key:<id>）。

/// 存（或覆盖）一把私钥的解锁口令。**先验证过口令能解密再调这里**，
/// 别把没验证过的口令写进钥匙串。
pub fn set_key_passphrase(key_id: &str, passphrase: &str) -> Result<(), String> {
    set_entry(&key_account(key_id), passphrase, "口令")
}

/// 取私钥口令。没存过返回 None（这不是错误）。内容只给连接时解锁用，
/// 不进任何日志。
pub fn get_key_passphrase(key_id: &str) -> Result<Option<String>, String> {
    get_entry(&key_account(key_id))
}

/// 删掉一把私钥的口令。没存过也不算错。
pub fn delete_key_passphrase(key_id: &str) -> Result<(), String> {
    delete_entry(&key_account(key_id))
}

/// 这把密钥存没存口令（不暴露内容）。密钥库列表和 MCP 都只看这个。
pub fn key_passphrase_saved(key_id: &str) -> bool {
    matches!(get_key_passphrase(key_id), Ok(Some(_)))
}

// ─────────────── 通用条目操作（账号前缀之上的公共实现）───────────────

fn set_entry(acct: &str, secret: &str, what: &str) -> Result<(), String> {
    if secret.is_empty() {
        return Err(format!("{what}不能为空"));
    }
    let hex = utf8_hex(secret);
    security(&[
        "add-generic-password",
        "-U",
        "-s",
        SERVICE,
        "-a",
        acct,
        "-w",
        &hex,
    ])
    .map(|_| ())
    .map_err(|e| format!("写进钥匙串失败: {e}"))
}

fn get_entry(acct: &str) -> Result<Option<String>, String> {
    match security(&["find-generic-password", "-s", SERVICE, "-a", acct, "-w"]) {
        Ok(hex) => match hex_utf8(&hex) {
            Some(p) if !p.is_empty() => Ok(Some(p)),
            _ => Ok(None),
        },
        // 「找不到」是正常情况：还没存过
        Err(e) if e.contains("could not be found") || e.contains("未找到") => Ok(None),
        Err(e) => Err(format!("读钥匙串失败: {e}")),
    }
}

fn delete_entry(acct: &str) -> Result<(), String> {
    match security(&["delete-generic-password", "-s", SERVICE, "-a", acct]) {
        Ok(_) => Ok(()),
        Err(e) if e.contains("could not be found") || e.contains("未找到") => Ok(()),
        Err(e) => Err(format!("从钥匙串删除失败: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // 这些测试动的是**登录钥匙串里的专用条目**（service=daiqiao-test），
    // 每次跑完自己清干净。为了复用同一套 security 封装，直接在这里改 SERVICE。
    fn test_host(id: &str) -> String {
        format!("dqtest-{id}")
    }

    #[test]
    fn 存取删全链路_含中文密码() {
        let host = test_host("roundtrip");
        // 中文 + 空格 + 引号 + 换行 —— 最容易在 CLI 传递和编码上出问题的组合
        let pw = "密码 p@ss \"引号\"\n第二行";
        let _ = delete(&host);
        set(&host, pw).expect("存");
        assert_eq!(get(&host).expect("读"), Some(pw.to_string()));
        delete(&host).expect("删");
        assert_eq!(get(&host).expect("读回应为空"), None);
    }

    #[test]
    fn 纯ascii密码也原样() {
        let host = test_host("ascii");
        let _ = delete(&host);
        set(&host, "Hunter2!xyz").expect("存");
        assert_eq!(get(&host).expect("读"), Some("Hunter2!xyz".into()));
        delete(&host).expect("删");
    }

    #[test]
    fn 没存过返回none不是报错() {
        assert_eq!(get(&test_host("never-saved")).expect("读"), None);
    }

    #[test]
    fn 空密码拒绝存() {
        assert!(set(&test_host("empty"), "").is_err());
    }

    #[test]
    fn hex编解码确定性() {
        assert_eq!(utf8_hex("abc"), "616263");
        assert_eq!(hex_utf8("616263"), Some("abc".into()));
        // 非 hex 输入（理论上不该出现）解析失败而不是乱解
        assert_eq!(hex_utf8("zz"), None);
        assert_eq!(hex_utf8("616"), None);
    }

    #[test]
    fn 密钥口令_存取删全链路_含中文口令() {
        let kid = test_host("key-roundtrip");
        let pw = "口令 p@ss \"引号\"\n第二行";
        let _ = delete_key_passphrase(&kid);
        set_key_passphrase(&kid, pw).expect("存");
        assert!(key_passphrase_saved(&kid), "存了之后 saved 应该是 true");
        assert_eq!(get_key_passphrase(&kid).expect("读"), Some(pw.to_string()));
        delete_key_passphrase(&kid).expect("删");
        assert!(!key_passphrase_saved(&kid));
        assert_eq!(get_key_passphrase(&kid).expect("读回应为空"), None);
    }

    #[test]
    fn 密钥口令_和主机密码同id互不串门() {
        // 同一个 id 既当主机 id 又当密钥 id：两条凭据靠 ssh-host:/ssh-key:
        // 前缀分开，绝不能互相读到对方的内容
        let id = test_host("same-id");
        let _ = delete(&id);
        let _ = delete_key_passphrase(&id);
        set(&id, "主机密码").expect("存主机密码");
        set_key_passphrase(&id, "密钥口令").expect("存密钥口令");
        assert_eq!(get(&id).expect("读主机密码"), Some("主机密码".into()));
        assert_eq!(
            get_key_passphrase(&id).expect("读密钥口令"),
            Some("密钥口令".into())
        );
        // 删主机密码不该动密钥口令
        delete(&id).expect("删主机密码");
        assert_eq!(
            get_key_passphrase(&id).expect("密钥口令还在"),
            Some("密钥口令".into())
        );
        delete_key_passphrase(&id).expect("删密钥口令");
    }

    #[test]
    fn 密钥口令_没存过返回none不是报错() {
        assert_eq!(
            get_key_passphrase(&test_host("key-never-saved")).expect("读"),
            None
        );
        assert!(!key_passphrase_saved(&test_host("key-never-saved-2")));
    }

    #[test]
    fn 密钥口令_空口令拒绝存() {
        assert!(set_key_passphrase(&test_host("key-empty"), "").is_err());
    }

    #[test]
    fn 密钥口令_删不存在的也不报错() {
        delete_key_passphrase(&test_host("key-ghost")).expect("删不存在的条目应该算成功");
    }
}
