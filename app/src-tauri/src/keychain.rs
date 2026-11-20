//! SSH 密码存进 macOS 钥匙串。
//!
//! 凭据红线：密码只进钥匙串，不进 hosts.json、不进审计日志、不进 MCP 上下文。
//! AI 侧只能问「这台主机存没存密码」（host_list 里的 password_saved），拿不到内容。
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

/// account 用主机 id（不是名字 —— 改名不该把密码搞丢）
fn account(host_id: &str) -> String {
    format!("ssh-host:{host_id}")
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
        Ok(String::from_utf8_lossy(&out.stdout).trim_end_matches('\n').to_string())
    } else {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        Err(err)
    }
}

/// 存（或覆盖）一台主机的密码。
pub fn set(host_id: &str, password: &str) -> Result<(), String> {
    if password.is_empty() {
        return Err("密码不能为空".into());
    }
    let acct = account(host_id);
    let hex = utf8_hex(password);
    security(&[
        "add-generic-password",
        "-U",
        "-s",
        SERVICE,
        "-a",
        &acct,
        "-w",
        &hex,
    ])
    .map(|_| ())
    .map_err(|e| format!("写进钥匙串失败: {e}"))
}

/// 取密码。没存过返回 None（这不是错误）。
pub fn get(host_id: &str) -> Result<Option<String>, String> {
    let acct = account(host_id);
    match security(&[
        "find-generic-password",
        "-s",
        SERVICE,
        "-a",
        &acct,
        "-w",
    ]) {
        Ok(hex) => match hex_utf8(&hex) {
            Some(p) if !p.is_empty() => Ok(Some(p)),
            _ => Ok(None),
        },
        // 「找不到」是正常情况：还没存过
        Err(e) if e.contains("could not be found") || e.contains("未找到") => Ok(None),
        Err(e) => Err(format!("读钥匙串失败: {e}")),
    }
}

/// 删掉一台主机的密码。没存过也不算错。
pub fn delete(host_id: &str) -> Result<(), String> {
    let acct = account(host_id);
    match security(&[
        "delete-generic-password",
        "-s",
        SERVICE,
        "-a",
        &acct,
    ]) {
        Ok(_) => Ok(()),
        Err(e) if e.contains("could not be found") || e.contains("未找到") => Ok(()),
        Err(e) => Err(format!("从钥匙串删除失败: {e}")),
    }
}

/// 这台主机存没存密码（不暴露内容）。界面和 MCP 都只看这个。
pub fn saved(host_id: &str) -> bool {
    matches!(get(host_id), Ok(Some(_)))
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
}
