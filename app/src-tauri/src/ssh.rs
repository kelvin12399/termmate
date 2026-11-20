//! SSH / SFTP：把远端机器变成和本地 shell 一样的一个会话，
//! 以及在其上做文件操作。
//!
//! 连接和通道都在一个专用后台线程的 tokio runtime 里跑，
//! 数据回灌进同一个 `TermState` —— 所以对上层（MCP / 前端）来说，
//! 「本地 shell」和「SSH 会话」没有任何区别。

use crate::hosts::Host;
use crate::term::SessionIo;
use russh::client;
use russh::keys::{load_secret_key, PrivateKeyWithHashAlg, PublicKeyOrCertificate};
use russh::ChannelMsg;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};

/// 远端 shell 里注入的命令包装器。
///
/// 注意和本地那份不一样：本地能把输出写到文件再读（干净、不受滚动缓冲限制），
/// 但 SSH 的输出文件在**远端**，本地读不到 —— 只能用可见的起止标记，
/// 由 MCP 侧从屏幕文本里把中间那段抠出来。

/// 主机身份核验没通过。连接失败时要原样报给人看 ——
/// russh 只会给一句干巴巴的 `UnknownKey`，人根本不知道发生了什么。
#[derive(Debug, Clone)]
pub struct HostKeyChanged {
    pub host: String,
    pub port: u16,
    pub got: String,
    /// 记录里存的指纹（指纹变更时非空；无法核验/已吊销时为空）
    pub known: Vec<String>,
    /// 非指纹变更的情形（无法核验、已吊销、首连竞态）在这里说明原因
    pub reason: String,
}

impl std::fmt::Display for HostKeyChanged {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.known.is_empty() {
            // 不是「指纹变了」，而是压根没法核验 / 被吊销 / 首连撞车
            return write!(
                f,
                "没法确认 {}:{} 是你要连的那台机器，所以没有连。\n{}\n\
                 当前拿到的指纹是 {}。",
                self.host, self.port, self.reason, self.got
            );
        }
        let hint = {
            let o = crate::known_hosts::origins(&self.host, self.port);
            match (o.in_daiqiao, o.in_ssh) {
                // 记录只在代敲库里 —— 自己能清
                (d, 0) if d > 0 => "确认是重装/换系统的话，用 host_key_forget 清掉旧记录再连。",
                // 记录来自 ~/.ssh/known_hosts —— 代敲不改用户的 ssh 配置，要人自己清
                (0, s) if s > 0 => {
                    "这条记录在 ~/.ssh/known_hosts 里（代敲不改你的 ssh 配置）。\
                     确认是重装的话，在终端里跑 ssh-keygen -R <主机> 清掉再连。"
                }
                (d, s) if d > 0 && s > 0 => {
                    "两个库里都有记录：代敲库用 host_key_forget 清，\
                     ~/.ssh/known_hosts 要自己跑 ssh-keygen -R <主机>。"
                }
                _ => "确认是重装/换系统的话，清掉旧记录再连。",
            }
        };
        write!(
            f,
            "主机密钥变了！{}:{} 现在的指纹是 {}，但记录里是 {}。\n\
             要么这台机器重装/换了系统，要么你连到了另一台机器上。\n{}",
            self.host,
            self.port,
            self.got,
            self.known.join(" 或 "),
            hint
        )
    }
}

/// 带主机身份校验的连接处理器。
///
/// `check_server_key` 是 SSH 协议里唯一能确认「对面还是原来那台机器」的地方。
/// 之前这里直接 `Ok(true)`，等于把这道门焊开了 —— 服务器重装或 IP 被 DHCP
/// 分给别的设备时，代敲会一声不响连上去，人还以为是原来那台。
pub struct ClientHandler {
    host: String,
    port: u16,
    /// 校验结果往外带：Changed 时上层要拿到详细指纹去报错
    changed: Arc<std::sync::Mutex<Option<HostKeyChanged>>>,
    /// 首次见面记下了新主机 —— 上层要据此提示人
    learned: Arc<std::sync::Mutex<Option<String>>>,
}

impl ClientHandler {
    pub fn new(host: &str, port: u16) -> Self {
        Self {
            host: host.to_string(),
            port,
            changed: Arc::new(std::sync::Mutex::new(None)),
            learned: Arc::new(std::sync::Mutex::new(None)),
        }
    }
}

impl client::Handler for ClientHandler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        use russh::keys::PublicKey;

        // 证书身份走 CA 信任链，不是 known_hosts 的范畴，这里放行
        let key: PublicKey = match server_public_key {
            PublicKeyOrCertificate::PublicKey { key, .. } => key.clone(),
            PublicKeyOrCertificate::Certificate(_) => return Ok(true),
        };
        let blob = key.to_bytes().map_err(|_| russh::Error::Inconsistent)?;
        let algo = key.algorithm().to_string();

        match crate::known_hosts::verify(&self.host, self.port, &blob) {
            crate::known_hosts::Verdict::Match => Ok(true),
            crate::known_hosts::Verdict::Unknown => {
                // 首次见面：记下来并放行（TOFU，和 ssh 首次连接一个道理），
                // 但要留痕 + 让上层提示人，不能完全无声。
                let fp = crate::known_hosts::fingerprint(&blob);
                match crate::known_hosts::remember(&self.host, self.port, &algo, &blob) {
                    Ok(()) => {}
                    // 写之前的二次校验发现别的连接刚记了另一把密钥 —— 拒连。
                    // 两条连接同时首连时，这是唯一能拦住「两把密钥都变可信」的地方。
                    Err(crate::known_hosts::RememberError::Raced(v)) => {
                        let detail = HostKeyChanged {
                            host: self.host.clone(),
                            port: self.port,
                            got: fp,
                            known: Vec::new(),
                            reason: format!(
                                "另一条连接同时记录了不同的主机密钥（{v:?}）—— \
                                 同一时刻出现两把不同的密钥，这台主机现在不可信"
                            ),
                        };
                        crate::audit::human(
                            "拒绝连接·首连竞态",
                            Some(format!("{}:{} {}", self.host, self.port, detail.reason)),
                        );
                        if let Ok(mut g) = self.changed.lock() {
                            *g = Some(detail);
                        }
                        return Ok(false);
                    }
                    Err(e) => {
                        // 记不下来就不能放行：下次连接会再判一次 Unknown，
                        // 等于这台主机永远处在「没有记录」状态，校验形同虚设。
                        let detail = HostKeyChanged {
                            host: self.host.clone(),
                            port: self.port,
                            got: fp,
                            known: Vec::new(),
                            reason: format!("主机指纹记不下来（{e}），为安全起见不放行"),
                        };
                        if let Ok(mut g) = self.changed.lock() {
                            *g = Some(detail);
                        }
                        return Ok(false);
                    }
                }
                crate::audit::human(
                    "首次连接·记下主机指纹",
                    Some(format!("{}:{} {}", self.host, self.port, fp)),
                );
                if let Ok(mut g) = self.learned.lock() {
                    *g = Some(fp);
                }
                Ok(true)
            }
            // 有记录/有文件但没法比对 —— 绝不能当「没见过」去 TOFU。
            // 第一版正是在这里失守：哈希行、CA 行、读不了的文件全被压成 Unknown。
            crate::known_hosts::Verdict::Unverifiable { reason } => {
                let detail = HostKeyChanged {
                    host: self.host.clone(),
                    port: self.port,
                    got: crate::known_hosts::fingerprint(&blob),
                    known: Vec::new(),
                    reason,
                };
                crate::audit::human(
                    "拒绝连接·主机身份无法核验",
                    Some(format!("{}:{} {}", self.host, self.port, detail.reason)),
                );
                if let Ok(mut g) = self.changed.lock() {
                    *g = Some(detail);
                }
                Ok(false)
            }
            // @revoked 是人为标记「这把密钥已经不可信」，OpenSSH 永久拒绝，我们一样
            crate::known_hosts::Verdict::Revoked => {
                let fp = crate::known_hosts::fingerprint(&blob);
                let detail = HostKeyChanged {
                    host: self.host.clone(),
                    port: self.port,
                    got: fp.clone(),
                    known: Vec::new(),
                    reason: format!(
                        "这把主机密钥在 known_hosts 里被标记为已吊销（@revoked）。\
                         清掉吊销标记之前不会连 —— 吊销通常意味着这把密钥泄露过"
                    ),
                };
                crate::audit::human(
                    "拒绝连接·主机密钥已吊销",
                    Some(format!("{}:{} {fp}", self.host, self.port)),
                );
                if let Ok(mut g) = self.changed.lock() {
                    *g = Some(detail);
                }
                Ok(false)
            }
            crate::known_hosts::Verdict::Changed { known } => {
                // 硬拒。这是唯一需要人明确动手才能继续的情况。
                let detail = HostKeyChanged {
                    host: self.host.clone(),
                    port: self.port,
                    got: crate::known_hosts::fingerprint(&blob),
                    known,
                    reason: String::new(),
                };
                crate::audit::human(
                    "拒绝连接·主机密钥变更",
                    Some(format!(
                        "{}:{} 指纹 {} 与记录不符",
                        self.host, self.port, detail.got
                    )),
                );
                if let Ok(mut g) = self.changed.lock() {
                    *g = Some(detail);
                }
                Ok(false)
            }
        }
    }
}

pub type SshHandle = client::Handle<ClientHandler>;

/// 统一的认证入口：shell 会话和 SFTP 都走这里，凭证只在内存里过一遍
pub async fn authenticate(session: &mut SshHandle, host: &Host) -> anyhow::Result<()> {
    match host.auth.as_str() {
        "key" => {
            // key_id 优先查密钥库；没有就退回旧的直接路径字段（兼容老主机）
            let key_path = crate::hosts::resolve_key_path(host)
                .ok_or_else(|| anyhow::anyhow!("这台主机没有配置密钥"))?;
            let key = load_secret_key(&key_path, None)?;
            let hash = session.best_supported_rsa_hash().await?.flatten();
            let res = session
                .authenticate_publickey(
                    host.user.clone(),
                    PrivateKeyWithHashAlg::new(Arc::new(key), hash),
                )
                .await?;
            if !res.success() {
                anyhow::bail!("公钥认证被拒绝");
            }
            Ok(())
        }
        "password" => {
            // 密码从钥匙串来。AI 只知道「存没存」，内容永远不经过它 ——
            // 所以这条分支不接受任何调用方传入的密码。
            let pw = crate::keychain::get(&host.id)
                .map_err(|e| anyhow::anyhow!("{e}"))?
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "这台主机还没存密码：在代敲的主机编辑里存进钥匙串（密码不经过 AI）"
                    )
                })?;
            let res = session
                .authenticate_password(host.user.clone(), pw)
                .await?;
            if !res.success() {
                anyhow::bail!("密码认证被拒绝（密码可能不对，或服务器禁用了密码登录）");
            }
            Ok(())
        }
        "agent" => {
            authenticate_with_agent(session, host).await
        }
        other => anyhow::bail!("不认识的认证方式: {other}（支持 key / password / agent）"),
    }
}

/// ssh-agent 认证：把 agent 里加载的每把密钥都试一遍。
///
/// russh 0.65 的 AgentClient 没实现自己的 Signer（上游就没给），
/// 这里补一个：签名请求直接转发给 agent —— 私钥从头到尾不出 agent。
struct AgentSigner {
    client: russh::keys::agent::client::AgentClient<tokio::net::UnixStream>,
}

#[derive(Debug, thiserror::Error)]
enum AgentSignError {
    #[error("{0}")]
    Agent(#[from] russh::keys::Error),
    #[error("通道已关闭")]
    Send(#[from] russh::SendError),
}

impl russh::Signer for AgentSigner {
    type Error = AgentSignError;

    async fn auth_sign(
        &mut self,
        key: &russh::keys::agent::AgentIdentity,
        hash_alg: Option<russh::keys::HashAlg>,
        to_sign: Vec<u8>,
    ) -> Result<Vec<u8>, Self::Error> {
        Ok(self.client.sign_request(key, hash_alg, to_sign).await?)
    }
}

async fn authenticate_with_agent(session: &mut SshHandle, host: &Host) -> anyhow::Result<()> {
    let socket = crate::hosts::agent_socket_for(Some(&host.hostname)).ok_or_else(|| {
        anyhow::anyhow!(
            "这台主机没有可用的 ssh-agent（可能在 ~/.ssh/config 里设了 IdentityAgent none）"
        )
    })?;
    // 必须走这台主机在 ssh config 里指定的 socket。直接 connect_env()
    // 会漏掉 1Password 的 IdentityAgent，误报「agent 没密钥」。
    let mut agent = russh::keys::agent::client::AgentClient::connect_uds(&socket)
        .await
        .map_err(|e| anyhow::anyhow!("连不上 ssh-agent（{socket}）: {e}"))?;

    let ids = agent
        .request_identities()
        .await
        .map_err(|e| anyhow::anyhow!("问 ssh-agent 要密钥列表失败: {e}"))?;
    if ids.is_empty() {
        anyhow::bail!("ssh-agent 里没有加载任何密钥（socket: {socket}）");
    }
    log::info!("ssh-agent 里有 {} 把密钥，逐把试", ids.len());

    let mut signer = AgentSigner { client: agent };
    let rsa_hash = session.best_supported_rsa_hash().await?.flatten();
    let mut tried = 0usize;

    for id in &ids {
        // 证书身份取它底下的公钥（服务器不认证书时至少裸钥匙还有机会）
        let owned;
        let key: &russh::keys::PublicKey = match id {
            russh::keys::agent::AgentIdentity::PublicKey { key, .. } => key,
            russh::keys::agent::AgentIdentity::Certificate { certificate, .. } => {
                owned = russh::keys::PublicKey::from(certificate.public_key().clone());
                &owned
            }
        };
        // RSA 才需要协商 hash；别的算法传 None
        let hash = if matches!(
            key.algorithm(),
            russh::keys::Algorithm::Rsa { .. }
        ) {
            rsa_hash
        } else {
            None
        };
        tried += 1;
        let res = session
            .authenticate_publickey_with(host.user.clone(), key.clone(), hash, &mut signer)
            .await;
        match res {
            Ok(r) if r.success() => {
                let comment = match id {
                    russh::keys::agent::AgentIdentity::PublicKey { comment, .. } => {
                        comment.clone()
                    }
                    _ => String::new(),
                };
                log::info!("agent 密钥认证成功（{tried}/{})「{comment}」", ids.len());
                return Ok(());
            }
            Ok(_) => continue, // 这把不行，换下一把
            Err(e) => {
                // agent 挂了或签名失败，继续试下一把没意义
                anyhow::bail!("agent 签名失败: {e}");
            }
        }
    }
    anyhow::bail!("ssh-agent 里的 {tried} 把密钥服务器都不认（authorized_keys 里加了吗？）");
}

/// 建立一条新的 SSH 连接并完成认证。
///
/// 主机密钥校验在 `ClientHandler::check_server_key` 里做：指纹变了会在
/// 握手阶段就断掉，这里要把那个「为什么断」的详情翻译成人能看懂的错误 ——
/// russh 只会给一个干巴巴的 `UnknownKey`，直接抛出去人根本不知道发生了什么。
pub async fn connect(host: &Host) -> anyhow::Result<SshHandle> {
    let config = Arc::new(client::Config::default());
    let handler = ClientHandler::new(&host.hostname, host.port);
    let changed = handler.changed.clone();
    let learned = handler.learned.clone();

    let result = client::connect(config, (host.hostname.clone(), host.port), handler).await;

    // 先看是不是被我们自己的校验拦下来的
    if let Some(detail) = changed.lock().ok().and_then(|g| g.clone()) {
        anyhow::bail!("{detail}");
    }
    let mut session = result?;

    if let Some(fp) = learned.lock().ok().and_then(|g| g.clone()) {
        log::info!("首次连接 {}:{}，已记下指纹 {fp}", host.hostname, host.port);
    }

    authenticate(&mut session, host).await?;
    Ok(session)
}

/// 开一条 SFTP 通道（每次操作用一条独立连接，简单可靠）
pub async fn sftp_connect(host: &Host) -> anyhow::Result<russh_sftp::client::SftpSession> {
    let session = connect(host).await?;
    let channel = session.channel_open_session().await?;
    channel.request_subsystem(true, "sftp").await?;
    let sftp = russh_sftp::client::SftpSession::new(channel.into_stream()).await?;
    Ok(sftp)
}

enum SshCmd {
    Data(Vec<u8>),
    Resize(u16, u16),
    Close,
}

/// SSH 会话的控制端：写入 / 改尺寸 / 关闭都通过它转发给后台线程
pub struct SshIo {
    tx: UnboundedSender<SshCmd>,
    alive: Arc<AtomicBool>,
}

impl SessionIo for SshIo {
    fn write(&self, data: &[u8]) -> anyhow::Result<()> {
        self.tx
            .send(SshCmd::Data(data.to_vec()))
            .map_err(|_| anyhow::anyhow!("SSH 连接已关闭"))
    }

    fn resize(&self, cols: u16, rows: u16) -> anyhow::Result<()> {
        let _ = self.tx.send(SshCmd::Resize(cols, rows));
        Ok(())
    }

    fn is_alive(&self) -> bool {
        self.alive.load(Ordering::Relaxed)
    }

    fn kill(&self) {
        let _ = self.tx.send(SshCmd::Close);
    }
}

/// 起后台线程连远端。`on_data` 收到的就是 SSH 通道里的原始字节，
/// 直接喂给终端仿真器即可。
pub fn spawn_ssh_worker<F, G>(
    host: Host,
    cols: u16,
    rows: u16,
    on_data: F,
    on_exit: G,
) -> anyhow::Result<Arc<SshIo>>
where
    F: Fn(&[u8]) + Send + 'static,
    G: Fn() + Send + 'static,
{
    let (tx, rx) = unbounded_channel();
    let alive = Arc::new(AtomicBool::new(true));
    let alive_bg = alive.clone();

    std::thread::spawn(move || {
        let rt = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(e) => {
                log::error!("SSH runtime 创建失败: {e}");
                alive_bg.store(false, Ordering::Relaxed);
                on_exit();
                return;
            }
        };

        let result = rt.block_on(run_ssh(host, cols, rows, rx, &on_data));
        match result {
            Ok(()) => log::info!("SSH 会话正常结束"),
            Err(e) => on_data(format!("\r\n\x1b[31m[SSH 连接结束: {e}]\x1b[0m\r\n").as_bytes()),
        }
        alive_bg.store(false, Ordering::Relaxed);
        on_exit();
    });

    Ok(Arc::new(SshIo { tx, alive }))
}

async fn run_ssh<F>(
    host: Host,
    cols: u16,
    rows: u16,
    mut rx: UnboundedReceiver<SshCmd>,
    on_data: &F,
) -> anyhow::Result<()>
where
    F: Fn(&[u8]),
{
    log::info!("连接 SSH {}@{}:{}", host.user, host.hostname, host.port);
    let session = connect(&host).await?;

    on_data(
        format!(
            "\x1b[32m[已连接 {}@{}:{}]\x1b[0m\r\n",
            host.user, host.hostname, host.port
        )
        .as_bytes(),
    );

    let mut channel = session.channel_open_session().await?;
    channel
        .request_pty(true, "xterm-256color", cols as u32, rows as u32, 0, 0, &[])
        .await?;
    channel.request_shell(true).await?;

    // exec 包装器不在连上时注入（那样会在屏幕上回显一整行）：
    // 第一次 exec 时由 MCP 在静默窗口里送进去，见 mcp.rs::exec_ssh。

    loop {
        tokio::select! {
            msg = channel.wait() => {
                match msg {
                    Some(ChannelMsg::Data { data }) => on_data(&data),
                    Some(ChannelMsg::ExtendedData { data, .. }) => on_data(&data),
                    Some(ChannelMsg::Eof) | Some(ChannelMsg::Close) | None => break,
                    _ => {}
                }
            }
            cmd = rx.recv() => {
                match cmd {
                    Some(SshCmd::Data(d)) => { channel.data_bytes(d).await?; }
                    Some(SshCmd::Resize(c, r)) => {
                        let _ = channel.window_change(c as u32, r as u32, 0, 0).await;
                    }
                    Some(SshCmd::Close) | None => {
                        let _ = channel.eof().await;
                        break;
                    }
                }
            }
        }
    }

    let _ = session
        .disconnect(russh::Disconnect::ByApplication, "", "en")
        .await;
    Ok(())
}
