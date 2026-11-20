# 代敲 · TermMate

**一台 macOS 本地终端，内置 MCP server —— 人和 AI 共用同一块终端屏。**

AI 通过 MCP 直接操作这个终端：开标签页、分屏、跑命令、读回输出、收发 SFTP、管理主机与密钥。
人看得见 AI 正在跑什么，随时可以接手同一块屏。

> 和已有 MCP 终端项目的区别：那些项目大多只做「无界面的 MCP server」，AI 在暗处跑命令；
> 代敲做的是**终端本体 + 内置 MCP**，AI 的操作始终发生在一个真实、可见、可接管的终端窗口里。

## 功能

- **终端**：多标签页、上下/左右分屏、标签改名，基于 `portable-pty` + `alacritty_terminal` 做唯一 VT 解析
- **SSH**：私钥 / 密码 / ssh-agent 三种认证；支持 `IdentityAgent`（含 1Password SSH Agent）
- **主机密钥校验**：读 `~/.ssh/known_hosts` 和代敲自己的库；首次连接 TOFU 记录，指纹变更**硬拒**，支持 `@revoked` / `@cert-authority` / 哈希行 / 通配符
- **密钥库**：`~/.daiqiao/keys/`（目录 700、文件 600）；从 `~/.ssh` 引用、导入文件、粘贴导入、系统 `ssh-keygen` 生成
- **SFTP 资源管理器**：列目录、上传、下载、重命名、移动、复制、删除、新建目录；多选、剪贴板、拖拽移动、从 Finder 拖入上传
- **终端文本选择**：拖选、⌘C 复制、⌘A 全选、右键复制/粘贴；中文与 emoji 按真实终端格子提取，不会错列
- **配置备份 / 迁移**：导出全部主机、分组和密钥引用为 `.dqconfig`；默认不含凭据，勾选后以 AES-256-GCM + PBKDF2-HMAC-SHA256 加密私钥和主机密码，另一台 Mac 可导入恢复
- **打开方式**：用本机应用打开远端文件，保存后自动回传（30 分钟内监听变更）
- **MCP 双通道**：HTTP `http://127.0.0.1:7391/mcp`（只监听本机回环）+ stdio 桥（给不支持 HTTP 的客户端）
- **审计**：`~/.daiqiao/audit.jsonl`（0600，5 MB 轮转）；记录谁（人 / 哪个客户端）在哪个会话跑了什么，凭据与敏感环境变量打码
- **外观**：字号 9–28、等宽字体、6 套配色、行高、光标形状，改完立即生效
- **8 种界面语言**：简体中文 / 繁體中文 / English / 日本語 / 한국어 / Deutsch / Français / Español，可在应用内切换并持久化
- **中文/日文/韩文输入**：每个窗格挂隐形输入框承接输入法，普通字符不被全局 `preventDefault()` 截走，组字期间不会把拼音或选词按键送进 shell

## 安全边界

- **凭据不进 AI 上下文、不进日志**：AI 只知道密钥名、指纹，以及「这台主机存没存密码」
- 密码存 macOS 钥匙串（service `daiqiao`），配置文件里不落明文
- AI 执行的每条命令都写审计；`send_input` 只记字节数，不记内容
- 设计上**不做危险命令事前拦截**：防线是「同一块屏 + 全量审计 + 随时接手」。如果你的场景需要事前确认，请在业务侧或 MCP 客户端侧加策略

## 快速开始

环境要求：macOS、Node.js 18+、Rust 1.77+、Xcode Command Line Tools。

```bash
cd app
npm install
npm run tauri dev        # 开发模式
npm run tauri build -- --bundles app   # 打 .app
```

DMG 打包时 Tauri 会用 Finder AppleScript 美化窗口，个别 macOS 版本会超时；遇到时可以只用 `--bundles app`，再用 `hdiutil` 手工制作 DMG。

## 接入 AI 客户端

代敲运行时会启动 MCP server，监听 `127.0.0.1:7391`（仅本机可访问）。

**HTTP（Claude Code）**

```bash
claude mcp add --transport http daiqiao http://127.0.0.1:7391/mcp
```

**HTTP（Codex / Cursor）**

```toml
# ~/.codex/config.toml
[mcp_servers.daiqiao]
url = "http://127.0.0.1:7391/mcp"
```

```json
// ~/.cursor/mcp.json
{ "mcpServers": { "daiqiao": { "url": "http://127.0.0.1:7391/mcp" } } }
```

**stdio（不支持 HTTP 的客户端）**

```json
{
  "mcpServers": {
    "daiqiao": { "command": "/Applications/代敲.app/Contents/MacOS/daiqiao-mcp" }
  }
}
```

stdio 桥只是把 JSON-RPC 转发给运行中的代敲，所以**代敲必须开着** —— 工具操作的就是那个窗口里的会话。

## MCP 工具（45 个）

- **会话/终端**：`session_list` `session_new` `session_close` `session_rename` `session_activate` `pane_split` `exec` `send_input` `read_screen` `session_info`
- **主机**：`host_list` `host_add` `host_update` `host_remove` `host_connect` `host_exec` `host_test_connect` `ssh_config_scan` `ssh_config_import` `host_key_list` `host_key_forget`
- **SFTP**：`sftp_list` `sftp_upload` `sftp_download` `sftp_remove` `sftp_rename` `sftp_mkdir` `sftp_copy` `sftp_open_candidates` `sftp_open_with`
- **密钥**：`key_list` `key_import` `key_generate` `key_public` `key_rename` `key_fix_permissions` `key_remove`
- **自省/审计/外观**：`audit_list` `agent_status` `mcp_status` `appearance_get` `appearance_set`
- **配置迁移**：`config_export` `config_inspect` `config_import`（MCP 导出默认不含凭据；带凭据的加密导出请在界面完成）

`host_exec(host, command)` 按主机名一次执行并读回输出，自动复用同主机仍活着的 SSH 会话；批量任务直接写 shell（`rsync`、管道、循环都可以），不必拆成多个 SFTP 调用。

## 目录结构

```
app/
  index.html            # 单页界面
  src/                  # TypeScript 前端（终端渲染、侧栏、SFTP、密钥库、i18n）
  src-tauri/            # Rust：PTY/SSH/SFTP/MCP server/审计
    src/mcp.rs          # MCP 工具实现
    src/bin/daiqiao-mcp.rs  # stdio 桥
docs/requirements-v0.md # 需求与设计取舍
```

## 致谢

Tauri、alacritty_terminal、portable-pty、russh、russh-sftp、Lucide 图标，以及包内自带的 Symbols Nerd Font（MIT，许可证见 `app/public/fonts/`）。

## License

MIT，见 [LICENSE](LICENSE)。

---

## English

TermMate is a macOS-first terminal emulator with a built-in MCP server. AI agents can drive a real,
visible terminal — sessions, commands, SFTP, hosts and keys — while the human watches the same screen
and can take over at any time.

Credentials never reach the AI: it only sees key names, fingerprints, and whether a password is stored
(macOS Keychain). Every AI command is audited to `~/.daiqiao/audit.jsonl`.

Build: `cd app && npm install && npm run tauri dev`. MCP: `http://127.0.0.1:7391/mcp`, or the stdio
bridge at `Contents/MacOS/daiqiao-mcp` while the app is running. MIT licensed.
