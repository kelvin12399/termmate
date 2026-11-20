import { invoke } from "@tauri-apps/api/core";
import { MAIN_EN, MAIN_JA } from "./i18n-main";
import { SIDEBAR_EN, SIDEBAR_JA } from "./i18n-sidebar";
import { SFTP_EN, SFTP_JA } from "./i18n-sftp";
import { ZH_TW } from "./i18n-zh-tw";
import { KO } from "./i18n-ko";
import { DE } from "./i18n-de";
import { FR } from "./i18n-fr";
import { ES } from "./i18n-es";
import {
  CONFIG_EN,
  CONFIG_JA,
  CONFIG_ZH_TW,
  CONFIG_KO,
  CONFIG_DE,
  CONFIG_FR,
  CONFIG_ES,
} from "./i18n-config";

export type Locale = "zh-CN" | "zh-TW" | "en" | "ja" | "ko" | "de" | "fr" | "es";

export const APP_NAMES: Record<Locale, string> = {
  "zh-CN": "代敲",
  "zh-TW": "代敲",
  en: "TermMate",
  ja: "タームメイト",
  ko: "텀메이트",
  de: "TermMate",
  fr: "TermMate",
  es: "TermMate",
};

export const LOCALE_NAMES: Record<Locale, string> = {
  "zh-CN": "简体中文",
  "zh-TW": "繁體中文",
  en: "English",
  ja: "日本語",
  ko: "한국어",
  de: "Deutsch",
  fr: "Français",
  es: "Español",
};

// key 就是中文原文。这样静态 HTML 不需要为每个文案维护另一套 id，
// 动态渲染也可以逐步迁移到 t()，不会出现只翻译一半的情况。
const BASE_EN: Record<string, string> = {
  代敲: "TermMate",
  主机: "Hosts",
  新建主机: "New host",
  编辑主机: "Edit host",
  "从 ~/.ssh/config 导入": "Import from ~/.ssh/config",
  名称: "Name",
  地址: "Address",
  端口: "Port",
  用户: "User",
  认证方式: "Authentication",
  密钥: "Key",
  密码: "Password",
  存: "Save",
  清除: "Clear",
  "正在看 agent…": "Checking agent…",
  私钥: "Private key",
  更换: "Change",
  还没选密钥: "No key selected",
  "从 ~/.ssh 选": "Choose from ~/.ssh",
  "选择文件…": "Choose file…",
  粘贴密钥: "Paste key",
  新建一对: "Generate pair",
  "也可以把密钥文件直接拖到这里。": "You can also drop a private key file here.",
  分组: "Group",
  不分组: "Ungrouped",
  备注: "Notes",
  测试连接: "Test connection",
  "私钥和密码都只存在本机（密钥文件 + macOS 钥匙串），AI 只拿得到名字和「存没存」，看不到内容。":
    "Private keys and passwords stay on this Mac (key files + macOS Keychain). AI only sees names and whether a password is saved, never the contents.",
  删除: "Delete",
  保存: "Save",
  已保存: "Saved",
  导入: "Import",
  外观: "Appearance",
  字号: "Font size",
  等宽字体: "Monospace font",
  "填系统里装了的字体族名，留空用内置回退链": "Enter an installed font family, or leave blank for the built-in fallback",
  配色: "Theme",
  行高: "Line height",
  光标: "Cursor",
  方块: "Block",
  竖线: "Bar",
  下划线: "Underline",
  "改完立刻生效，AI 也能用 appearance_set 改。": "Changes apply immediately. AI can also use appearance_set.",
  语言: "Language",
  "连接 AI 客户端": "Connect AI clients",
  "检测中…": "Checking…",
  复制地址: "Copy address",
  "代敲自带 MCP server，跟终端同一个进程。接上的客户端看到的就是这块屏，你随时能接手。":
    "TermMate's MCP server runs in the terminal process. Connected clients see this same screen, and you can take over anytime.",
  复制: "Copy",
  "在终端里跑一次就行": "Run once in a terminal",
  "加到 ~/.codex/config.toml": "Add to ~/.codex/config.toml",
  "加到 ~/.cursor/mcp.json": "Add to ~/.cursor/mcp.json",
  "不支持 HTTP 的客户端走 stdio": "For clients without HTTP support, use stdio",
  "stdio · 命令行": "stdio · Command line",
  "代敲要开着 —— 工具操作的就是这个窗口里的会话": "Keep TermMate running — tools operate sessions in this window",
  "stdio · JSON 配置": "stdio · JSON config",
  "填进客户端的 mcpServers": "Add to the client's mcpServers",
  "多个客户端可以同时连，互不影响。": "Multiple clients can connect at once without interfering.",
  密钥库: "Key vault",
  选择: "Choose",
  粘贴导入: "Paste & import",
  新建: "Generate",
  私钥内容: "Private key contents",
  起个名字: "Name it",
  "存到 ~/.daiqiao/keys/，文件权限 600、目录 700。": "Stored in ~/.daiqiao/keys/ with file mode 600 and directory mode 700.",
  "公钥 —— 贴到服务器的 ~/.ssh/authorized_keys": "Public key — paste into ~/.ssh/authorized_keys on the server",
  名字: "Name",
  类型: "Type",
  "走系统 ssh-keygen 生成，私钥存进代敲库、权限 600，不设口令（要口令就用「粘贴导入」带上）。":
    "Generated with the system ssh-keygen. The private key is stored in TermMate's vault with mode 600 and no passphrase (use Paste & import for passphrase-protected keys).",
  "AI 能列密钥、能指定用哪把，拿不到私钥内容。": "AI can list and select keys, but cannot read private key contents.",
  用这把: "Use this key",
  "双击进目录 · ⌘C/⌘X/⌘V · 拖到文件夹=移动 · 右键打开方式":
    "Double-click to open · ⌘C/⌘X/⌘V · Drop on folder to move · Right-click to open with",
  设置: "Settings",
  关闭: "Close",
  "找主机，或输 user@host": "Find a host or enter user@host",
  "复制 MCP 地址": "Copy MCP address",
  "存进钥匙串": "Save to Keychain",
  "清除已存密码": "Clear saved password",
  "选择已有分组，或输入新分组": "Choose a group or type a new one",
  "选择已有分组": "Choose an existing group",
  上一级: "Parent folder",
  刷新: "Refresh",
  上传文件: "Upload files",
  新建文件夹: "New folder",
  打开: "Open",
  打开方式: "Open with",
  "下载到…": "Download to…",
  重命名: "Rename",
  新建目录: "New folder",
  上传: "Upload",
  下载: "Download",
  剪切: "Cut",
  粘贴: "Paste",
  全选: "Select all",
  取消选择: "Clear selection",
  本地终端: "Local terminal",
  没有会话: "No sessions",
  新建标签页: "New tab",
  关闭标签页: "Close tab",
  重命名标签页: "Rename tab",
  向右分屏: "Split right",
  向下分屏: "Split down",
  关闭窗格: "Close pane",
  连接: "Connect",
  编辑: "Edit",
  删除主机: "Delete host",
  搜索: "Search",
  取消: "Cancel",
  确定: "OK",
  继续: "Continue",
  完成: "Done",
  失败: "Failed",
  成功: "Success",
  重试: "Retry",
  未知: "Unknown",
  未分组: "Ungrouped",
  公司服务器: "Company servers",
  石墨: "Graphite",
  午夜: "Midnight",
  纸白: "Paper",
  "Solarized 深": "Solarized Dark",
  "Solarized 浅": "Solarized Light",
  "代敲默认，暖调深灰": "TermMate default, warm dark gray",
  "偏蓝的深色，和 GitHub 深色一致": "Blue-tinted dark, matching GitHub Dark",
  "浅色，白天强光下用": "Light theme for bright environments",
  "经典低蓝光深色": "Classic low-blue-light dark theme",
  "经典暖浅色": "Classic warm light theme",
  "高对比紫调": "High-contrast purple theme",
  生产网关: "Production gateway",
  "10.0.1.7 或 example.com": "10.0.1.7 or example.com",
  "生产环境 / 测试环境": "Production / Testing",
  "发布机，只走 deploy 脚本": "Deployment host; deploy scripts only",
  "留空 = SF Mono / Menlo / PingFang SC": "Blank = SF Mono / Menlo / PingFang SC",
  深圳跳板机: "Shenzhen bastion",
  代敲生成: "Generated by TermMate",
};

const BASE_JA: Record<string, string> = {
  代敲: "タームメイト",
  主机: "ホスト",
  新建主机: "新規ホスト",
  编辑主机: "ホストを編集",
  "从 ~/.ssh/config 导入": "~/.ssh/config から読み込む",
  名称: "名前",
  地址: "アドレス",
  端口: "ポート",
  用户: "ユーザー",
  认证方式: "認証方式",
  密钥: "鍵",
  密码: "パスワード",
  存: "保存",
  清除: "消去",
  "正在看 agent…": "Agent を確認中…",
  私钥: "秘密鍵",
  更换: "変更",
  还没选密钥: "鍵が未選択",
  "从 ~/.ssh 选": "~/.ssh から選択",
  "选择文件…": "ファイルを選択…",
  粘贴密钥: "鍵を貼り付け",
  新建一对: "鍵ペアを生成",
  "也可以把密钥文件直接拖到这里。": "秘密鍵ファイルをここにドロップすることもできます。",
  分组: "グループ",
  不分组: "グループなし",
  备注: "メモ",
  测试连接: "接続をテスト",
  "私钥和密码都只存在本机（密钥文件 + macOS 钥匙串），AI 只拿得到名字和「存没存」，看不到内容。":
    "秘密鍵とパスワードはこの Mac（鍵ファイル＋macOS キーチェーン）にのみ保存されます。AI が確認できるのは名前と保存有無だけで、内容は取得できません。",
  删除: "削除",
  保存: "保存",
  已保存: "保存しました",
  导入: "読み込む",
  外观: "外観",
  字号: "フォントサイズ",
  等宽字体: "等幅フォント",
  "填系统里装了的字体族名，留空用内置回退链": "インストール済みのフォント名を入力。空欄なら内蔵フォールバックを使用",
  配色: "テーマ",
  行高: "行の高さ",
  光标: "カーソル",
  方块: "ブロック",
  竖线: "縦線",
  下划线: "下線",
  "改完立刻生效，AI 也能用 appearance_set 改。": "変更は即時反映されます。AI は appearance_set でも変更できます。",
  语言: "言語",
  "连接 AI 客户端": "AI クライアントを接続",
  "检测中…": "確認中…",
  复制地址: "アドレスをコピー",
  "代敲自带 MCP server，跟终端同一个进程。接上的客户端看到的就是这块屏，你随时能接手。":
    "タームメイトの MCP サーバーはターミナルと同じプロセスで動作します。接続したクライアントと同じ画面を共有し、いつでも操作を引き継げます。",
  复制: "コピー",
  "在终端里跑一次就行": "ターミナルで一度実行するだけ",
  "加到 ~/.codex/config.toml": "~/.codex/config.toml に追加",
  "加到 ~/.cursor/mcp.json": "~/.cursor/mcp.json に追加",
  "不支持 HTTP 的客户端走 stdio": "HTTP 非対応のクライアントは stdio を使用",
  "stdio · 命令行": "stdio · コマンドライン",
  "代敲要开着 —— 工具操作的就是这个窗口里的会话": "タームメイトを起動したままにしてください。ツールはこのウィンドウのセッションを操作します",
  "stdio · JSON 配置": "stdio · JSON 設定",
  "填进客户端的 mcpServers": "クライアントの mcpServers に追加",
  "多个客户端可以同时连，互不影响。": "複数のクライアントを同時接続できます。",
  密钥库: "キーボールト",
  选择: "選択",
  粘贴导入: "貼り付けて読み込む",
  新建: "生成",
  私钥内容: "秘密鍵の内容",
  起个名字: "名前を付ける",
  "存到 ~/.daiqiao/keys/，文件权限 600、目录 700。": "~/.daiqiao/keys/ に保存。ファイルは 600、ディレクトリは 700。",
  "公钥 —— 贴到服务器的 ~/.ssh/authorized_keys": "公開鍵 — サーバーの ~/.ssh/authorized_keys に貼り付け",
  名字: "名前",
  类型: "種類",
  "走系统 ssh-keygen 生成，私钥存进代敲库、权限 600，不设口令（要口令就用「粘贴导入」带上）。":
    "OS の ssh-keygen で生成します。秘密鍵は権限 600 でキーボールトに保存し、パスフレーズは設定しません（必要なら貼り付けて読み込んでください）。",
  "AI 能列密钥、能指定用哪把，拿不到私钥内容。": "AI は鍵の一覧と選択はできますが、秘密鍵の内容は取得できません。",
  用这把: "この鍵を使用",
  "双击进目录 · ⌘C/⌘X/⌘V · 拖到文件夹=移动 · 右键打开方式":
    "ダブルクリックで開く · ⌘C/⌘X/⌘V · フォルダーへドロップで移動 · 右クリックでアプリから開く",
  设置: "設定",
  关闭: "閉じる",
  "找主机，或输 user@host": "ホストを検索、または user@host を入力",
  "复制 MCP 地址": "MCP アドレスをコピー",
  "存进钥匙串": "キーチェーンに保存",
  "清除已存密码": "保存済みパスワードを消去",
  "选择已有分组，或输入新分组": "既存のグループを選択、または新しい名前を入力",
  "选择已有分组": "既存のグループを選択",
  上一级: "上の階層",
  刷新: "更新",
  上传文件: "ファイルをアップロード",
  新建文件夹: "新規フォルダー",
  打开: "開く",
  打开方式: "開くアプリ",
  "下载到…": "ダウンロード先…",
  重命名: "名前を変更",
  新建目录: "新規フォルダー",
  上传: "アップロード",
  下载: "ダウンロード",
  剪切: "切り取り",
  粘贴: "貼り付け",
  全选: "すべて選択",
  取消选择: "選択を解除",
  本地终端: "ローカルターミナル",
  没有会话: "セッションはありません",
  新建标签页: "新しいタブ",
  关闭标签页: "タブを閉じる",
  重命名标签页: "タブ名を変更",
  向右分屏: "右に分割",
  向下分屏: "下に分割",
  关闭窗格: "ペインを閉じる",
  连接: "接続",
  编辑: "編集",
  删除主机: "ホストを削除",
  搜索: "検索",
  取消: "キャンセル",
  确定: "OK",
  继续: "続行",
  完成: "完了",
  失败: "失敗",
  成功: "成功",
  重试: "再試行",
  未知: "不明",
  未分组: "グループなし",
  公司服务器: "会社のサーバー",
  石墨: "グラファイト",
  午夜: "ミッドナイト",
  纸白: "ペーパー",
  "Solarized 深": "Solarized ダーク",
  "Solarized 浅": "Solarized ライト",
  "代敲默认，暖调深灰": "タームメイト標準、ウォームダークグレー",
  "偏蓝的深色，和 GitHub 深色一致": "青みのダーク、GitHub Dark と同系色",
  "浅色，白天强光下用": "明るい場所向けのライトテーマ",
  "经典低蓝光深色": "定番のブルーライト低減ダークテーマ",
  "经典暖浅色": "定番のウォームライトテーマ",
  "高对比紫调": "高コントラストのパープル系",
  生产网关: "本番ゲートウェイ",
  "10.0.1.7 或 example.com": "10.0.1.7 または example.com",
  "生产环境 / 测试环境": "本番 / テスト",
  "发布机，只走 deploy 脚本": "デプロイホスト、deploy スクリプトのみ",
  "留空 = SF Mono / Menlo / PingFang SC": "空欄 = SF Mono / Menlo / PingFang SC",
  深圳跳板机: "深圳踏み台サーバー",
  代敲生成: "タームメイトで生成",
};

const EN: Record<string, string> = {
  ...BASE_EN,
  ...MAIN_EN,
  ...SIDEBAR_EN,
  ...SFTP_EN,
};
const JA: Record<string, string> = {
  ...BASE_JA,
  ...MAIN_JA,
  ...SIDEBAR_JA,
  ...SFTP_JA,
};

const DICTS: Partial<Record<Locale, Record<string, string>>> = {
  en: { ...EN, ...CONFIG_EN },
  ja: { ...JA, ...CONFIG_JA },
  "zh-TW": { ...ZH_TW, ...CONFIG_ZH_TW },
  ko: { ...KO, ...CONFIG_KO },
  de: { ...DE, ...CONFIG_DE },
  fr: { ...FR, ...CONFIG_FR },
  es: { ...ES, ...CONFIG_ES },
};

let current: Locale = "zh-CN";
const originalText = new WeakMap<Text, string>();
const originalAttrs = new WeakMap<Element, Map<string, string>>();

export function getLocale(): Locale {
  return current;
}

export function t(source: string, vars?: Record<string, string | number>): string {
  const dict = DICTS[current];
  let value = dict?.[source] ?? source;
  if (vars) {
    for (const [key, val] of Object.entries(vars)) {
      value = value.replaceAll(`{${key}}`, String(val));
    }
  }
  return value;
}

function translated(source: string): string {
  return t(source);
}

/** 翻译静态 HTML 和尚未迁移到 t() 的动态文案。用户数据只有命中字典才会改。 */
export function translateDom(root: ParentNode = document): void {
  const doc = root.ownerDocument ?? document;
  const walker = doc.createTreeWalker(root, NodeFilter.SHOW_TEXT);
  const nodes: Text[] = [];
  while (walker.nextNode()) nodes.push(walker.currentNode as Text);

  for (const node of nodes) {
    const parent = node.parentElement;
    if (!parent || parent.closest("script,style,[data-no-i18n]")) continue;
    const raw = node.nodeValue ?? "";
    if (!originalText.has(node) && /[\u3400-\u9fff]/.test(raw)) originalText.set(node, raw);
    const original = originalText.get(node);
    if (!original) continue;
    const trimmed = original.trim();
    if (!trimmed) continue;
    const next = translated(trimmed);
    const leading = original.match(/^\s*/)?.[0] ?? "";
    const trailing = original.match(/\s*$/)?.[0] ?? "";
    const value = `${leading}${next}${trailing}`;
    if (node.nodeValue !== value) node.nodeValue = value;
  }

  const elements =
    root instanceof Element
      ? [root, ...root.querySelectorAll("*")]
      : Array.from(root.querySelectorAll("*"));
  for (const el of elements) {
    for (const attr of ["placeholder", "title", "aria-label"]) {
      const raw = el.getAttribute(attr);
      if (!raw) continue;
      let attrs = originalAttrs.get(el);
      if (!attrs) {
        attrs = new Map();
        originalAttrs.set(el, attrs);
      }
      if (!attrs.has(attr) && /[\u3400-\u9fff]/.test(raw)) attrs.set(attr, raw);
      const original = attrs.get(attr);
      if (original) el.setAttribute(attr, translated(original));
    }
  }
}

export function setLocale(next: string): Locale {
  current = next in APP_NAMES ? (next as Locale) : "zh-CN";
  document.documentElement.lang = current;
  translateDom();
  const name = APP_NAMES[current];
  document.title = name;
  // 窗口标题随 App 内语言立即切换；Dock/Finder 名称由 macOS 按系统语言选择。
  void invoke("set_window_title", { title: name }).catch(() => {});
  window.dispatchEvent(new CustomEvent("dq:locale", { detail: current }));
  return current;
}
