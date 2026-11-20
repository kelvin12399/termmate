import { invoke } from "@tauri-apps/api/core";
import { SftpPanel } from "./sftp";
import { listen } from "@tauri-apps/api/event";
import { TerminalView, loadSymbolFont, type TermUpdate } from "./renderer";
import {
  Sidebar,
  closeOverlays,
  closeTopOverlay,
  anyOverlayOpen,
  type Host,
  type SideView,
} from "./sidebar";
import { Palette, type Cmd } from "./palette";
import { setLocale, t } from "./i18n";
import "./styles.css";

// ───────────────────────── 布局模型（与 Rust 侧一致）─────────────────────────

type SplitDir = "horizontal" | "vertical";

type Layout =
  | { kind: "leaf"; session: string }
  | { kind: "split"; dir: SplitDir; ratio: number; a: Layout; b: Layout };

interface Tab {
  id: string;
  title: string;
  layout: Layout;
}

interface Workspace {
  tabs: Tab[];
  active_tab: string | null;
  focused: string | null;
}

let workspace: Workspace = { tabs: [], active_tab: null, focused: null };

// ───────────────────────── 每个 pane 的终端视图 ─────────────────────────

interface Pane {
  el: HTMLDivElement;
  canvas: HTMLCanvasElement;
  view: TerminalView;
  /**
   * 贴在光标位置的隐形 textarea，专门承接输入法。
   *
   * 中文打不进终端的根因就在这：原来整个应用没任何可编辑元素，
   * macOS 输入法找不到落脚处，根本不会开始组字。canvas 加 tabIndex
   * 只能拿焦点，它不是输入区，候选词窗也无处可弹。
   */
  ime: HTMLTextAreaElement;
  dirty: Set<number>;
  full: boolean;
  queued: boolean;
  cols: number;
  rows: number;
  /** 是否已经收到过内容；没收到就主动要一次整屏 */
  hasData: boolean;
}

const panes = new Map<string, Pane>();

/** AI 正在跑的每一条命令 —— 「人在场」靠的就是这份状态。
 *
 * 按 run id 存，不是按会话：多个 AI 客户端可以同时连，同一个会话上
 * 很可能有两条 exec 排队。按会话存的话，先返回的那条会把还在跑的那条
 * 的执行态一并抹掉，界面就谎报「没人在跑」。 */
interface AiRun {
  session: string;
  command: string;
  startedAt: number;
  /** 当前挂载着的胶囊里的计时文本。窗格没挂载时为空 —— 状态不依赖 DOM。 */
  clock?: HTMLElement;
}
const aiRuns = new Map<string, AiRun>();
/** 一个全局 ticker 更新所有计时，不给每条 run 开定时器（省得漏） */
let aiTicker = 0;

function runsOf(session: string): AiRun[] {
  return [...aiRuns.values()].filter((r) => r.session === session);
}
let mcpUrl = "";
let mcpListening = false;
let mcpTools = 0;
/** stdio 桥的可执行文件路径，由后端给（要原样填进客户端配置） */
let mcpBridge = "daiqiao-mcp";
/** 启动时读到的外观。新建窗格要立刻套上，不能等用户去开面板。 */
let bootLook: LookView | null = null;
const stageEl = document.getElementById("stage") as HTMLElement;
const tabsEl = document.getElementById("tabs") as HTMLElement;
const statusLeftEl = document.getElementById("status-left") as HTMLElement;
const statusRightEl = document.getElementById("status-right") as HTMLElement;
const mcpPill = document.getElementById("mcp-pill") as HTMLButtonElement;
const mcpTextEl = document.getElementById("mcp-text") as HTMLElement;
const aiPanel = document.getElementById("ai-panel") as HTMLElement;
const lookPanel = document.getElementById("look-panel") as HTMLElement;

/** 后端 appearance 的视图结构（Rust 侧 Appearance::to_view） */
interface LookView {
  font_size: number;
  font_family: string;
  theme: string;
  line_height: number;
  cursor_style: string;
  language: string;
  languages: { code: string; name: string }[];
  bg: string;
  fg: string;
  themes: { name: string; bg: string; fg: string; desc: string }[];
  min_font: number;
  max_font: number;
}


/** 取 index.html 里内联的 Lucide 精灵 */
function icon(name: string, cls = "i"): SVGSVGElement {
  const svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
  svg.setAttribute("class", cls);
  const use = document.createElementNS("http://www.w3.org/2000/svg", "use");
  use.setAttribute("href", `#i-${name}`);
  svg.appendChild(use);
  return svg;
}

// ───────────────────────── SFTP 资源管理器 ─────────────────────────
// 和 MCP 的 sftp_* 工具共用后端：AI 传的文件人能看到，人移动的 AI 也看得到。
const sftpPanel = new SftpPanel((msg) => sidebar.notify(msg));

// ───────────────────────── 侧边栏（主机管理）─────────────────────────

const sidebar = new Sidebar(
  {
    body: document.getElementById("side-body") as HTMLElement,
    search: document.getElementById("side-search") as HTMLInputElement,
    newBtn: document.getElementById("host-new") as HTMLElement,
    importBtn: document.getElementById("host-import") as HTMLElement,
    views: document.getElementById("side-views") as HTMLElement,
    drawer: document.getElementById("drawer") as HTMLElement,
    drawerTitle: document.getElementById("drawer-title") as HTMLElement,
    drawerDelete: document.getElementById("drawer-delete") as HTMLButtonElement,
    drawerSave: document.getElementById("drawer-save") as HTMLButtonElement,
    drawerClose: document.getElementById("drawer-close") as HTMLElement,
    fName: document.getElementById("f-name") as HTMLInputElement,
    fHost: document.getElementById("f-host") as HTMLInputElement,
    fPort: document.getElementById("f-port") as HTMLInputElement,
    fUser: document.getElementById("f-user") as HTMLInputElement,
    fAuth: document.getElementById("f-auth") as HTMLSelectElement,
    fKeyRow: document.getElementById("f-key-row") as HTMLElement,
    keyCard: document.getElementById("f-key-card") as HTMLElement,
    keyName: document.getElementById("f-key-name") as HTMLElement,
    keyBadge: document.getElementById("f-key-badge") as HTMLElement,
    keyFp: document.getElementById("f-key-fp") as HTMLElement,
    keySwap: document.getElementById("f-key-swap") as HTMLElement,
    keyEmpty: document.getElementById("f-key-empty") as HTMLElement,
    keyPick: document.getElementById("f-key-pick") as HTMLElement,
    keyFile: document.getElementById("f-key-file") as HTMLElement,
    keyPaste: document.getElementById("f-key-paste") as HTMLElement,
    keyGen: document.getElementById("f-key-gen") as HTMLElement,
    keyAlert: document.getElementById("f-key-alert") as HTMLElement,
    keyAlertText: document.getElementById("f-key-alert-text") as HTMLElement,
    keyAlertAct: document.getElementById("f-key-alert-act") as HTMLButtonElement,
    keyUsed: document.getElementById("f-key-used") as HTMLElement,
    testBtn: document.getElementById("f-test") as HTMLButtonElement,
    testOut: document.getElementById("f-test-out") as HTMLElement,
    fGroup: document.getElementById("f-group") as HTMLInputElement,
    fNote: document.getElementById("f-note") as HTMLInputElement,
    importPanel: document.getElementById("import-panel") as HTMLElement,
    importList: document.getElementById("import-list") as HTMLElement,
    importClose: document.getElementById("import-close") as HTMLElement,
    importDo: document.getElementById("import-do") as HTMLButtonElement,
    ctxMenu: document.getElementById("ctx-menu") as HTMLElement,
    authSeg: document.getElementById("f-auth-seg") as HTMLElement,
    passRow: document.getElementById("f-pass-row") as HTMLElement,
    passInput: document.getElementById("f-pass") as HTMLInputElement,
    passSave: document.getElementById("f-pass-save") as HTMLButtonElement,
    passClear: document.getElementById("f-pass-clear") as HTMLButtonElement,
    passStatus: document.getElementById("f-pass-status") as HTMLElement,
    agentRow: document.getElementById("f-agent-row") as HTMLElement,
    agentStatus: document.getElementById("f-agent-status") as HTMLElement,
  },
  // 点主机 → 新标签页连上（走的是和 MCP 的 host_connect 同一个后端函数）
  (h: Host) => {
    setStatus(t("连接 {name}…", { name: h.name }));
    invoke<string>("connect_host", { key: h.id }).catch((err) => setStatus(`${err}`));
  },
  (_v: SideView) => {
    /* 侧栏切到 SFTP 页时只换列表；面板由点主机触发（见 sidebar 的 onSftp） */
  },
  // SFTP 页点主机 → 开资源管理器面板
  (h: Host) => {
    void sftpPanel.open(h.name);
  }
);

/** 左边：当前焦点会话在哪台机器上。右边：真实的终端尺寸。 */
function setStatus(text: string): void {
  statusLeftEl.replaceChildren();
  statusLeftEl.appendChild(icon("terminal", "i"));
  const t = document.createElement("span");
  t.className = "path";
  t.textContent = text;
  statusLeftEl.appendChild(t);
}

function syncStatusRight(): void {
  const sid = workspace.focused;
  const pane = sid ? panes.get(sid) : undefined;
  statusRightEl.textContent = pane ? `UTF-8 · ${pane.cols}×${pane.rows}` : "UTF-8";
}

/**
 * 把隐形 textarea 接成终端的输入口，让输入法能用。
 *
 * 分工（这是中文能不能打进去的关键）：
 *   · 组字期间（compositionstart→end）一个字节都不发给 shell，
 *     让输入法在 textarea 里自己折腾拼音和候选词；
 *   · 上屏（compositionend）那一下，把定下来的整段文字一次写给 shell；
 *   · 非组字状态下的普通按键仍然走 encodeKey 那条老路（Ctrl-C、方向键等照旧）。
 *
 * 为什么 keydown 里必须看 isComposing：组字时输入法也会发 keydown，
 * 回车/空格/数字键都是在「选词」，直接转发就会把 \r 和数字打进 shell，
 * 变成一边选词一边执行命令。
 */
function wireIme(session: string, pane: Pane): void {
  const ime = pane.ime;

  const write = (data: string) => {
    if (!data) return;
    invoke("write_session", { id: session, data }).catch((err) => setStatus(`${err}`));
  };

  ime.addEventListener("compositionstart", () => {
    pane.el.classList.add("composing");
  });

  ime.addEventListener("compositionend", (e) => {
    pane.el.classList.remove("composing");
    const text = (e as CompositionEvent).data ?? "";
    ime.value = "";
    write(text);
  });

  // 输入法上屏之外，还有两种文字会落到这里：
  //   · 直接粘贴（paste 事件单独处理，这里兜底）
  //   · 某些输入法不发 compositionend 只发 input（比如部分第三方输入法的英文模式）
  ime.addEventListener("input", (e) => {
    const ev = e as InputEvent;
    if (ev.isComposing) return; // 组字中间态，等 compositionend
    const text = ime.value;
    if (!text) return;
    ime.value = "";
    write(text);
  });

  // 让输入法窗口贴着光标：每帧结束后把 textarea 挪到光标处
  ime.addEventListener("focus", () => placeIme(pane));
}

/** 把隐形 textarea 挪到光标位置，候选词窗口才会跟着光标弹 */
function placeIme(pane: Pane): void {
  const box = pane.view.caretBox();
  pane.ime.style.left = `${box.x}px`;
  pane.ime.style.top = `${box.y}px`;
  pane.ime.style.height = `${box.h}px`;
}

function createPane(session: string): Pane {
  const el = document.createElement("div");
  el.className = "pane";
  el.dataset.session = session;

  const canvas = document.createElement("canvas");
  el.appendChild(canvas);

  // 真正接键盘的是这个 textarea，不是 canvas。
  // 它必须是「能聚焦的真输入框」输入法才愿意工作；做成 1×1 像素、完全透明，
  // 跟着光标跑，这样候选词窗就贴在光标下方。caret-color 设透明，
  // 不要让它自己的光标和终端画的光标打打。
  const ime = document.createElement("textarea");
  ime.className = "ime";
  ime.setAttribute("autocapitalize", "off");
  ime.setAttribute("autocomplete", "off");
  ime.setAttribute("spellcheck", "false");
  ime.tabIndex = 0;
  el.appendChild(ime);

  const view = new TerminalView(canvas);
  const pane: Pane = {
    el,
    canvas,
    ime,
    view,
    dirty: new Set(),
    full: true,
    queued: false,
    cols: 80,
    rows: 24,
    hasData: false,
  };

  // 新窗格立刻套上当前外观（启动时读到的，或运行中改过的）
  if (bootLook) {
    view.applyAppearance(bootLook);
    ime.style.fontSize = `${bootLook.font_size}px`;
  }

  wireIme(session, pane);

  // 点哪个 pane，焦点就归谁（人也在操作同一份布局）。
  // 焦点要落在 ime 上，不是 canvas —— 否则输入法没处可去。
  el.addEventListener("mousedown", (e) => {
    // 选中文本的拖选不该被打断，所以只在没按住拖动时抢焦点
    if (e.button !== 0) return;
    ime.focus({ preventScroll: true });
    if (workspace.focused !== session) {
      invoke("activate_session_cmd", { id: session }).catch(() => {});
    }
  });

  const ro = new ResizeObserver(() => scheduleFit(session));
  ro.observe(el);

  return pane;
}

function scheduleFrame(pane: Pane): void {
  if (pane.queued) return;
  pane.queued = true;
  requestAnimationFrame(() => {
    pane.queued = false;
    if (!pane.el.isConnected) return;
    if (pane.full) {
      pane.full = false;
      pane.dirty.clear();
      pane.view.render();
    } else if (pane.dirty.size > 0) {
      const rows = [...pane.dirty];
      pane.dirty.clear();
      pane.view.renderRows(rows);
    }
  });
}

// ───────────────────────── 尺寸同步 ─────────────────────────

const fitTimers = new Map<string, number>();

function scheduleFit(session: string): void {
  window.clearTimeout(fitTimers.get(session));
  fitTimers.set(
    session,
    window.setTimeout(() => fitPane(session), 60)
  );
}

function fitPane(session: string, force = false): void {
  const pane = panes.get(session);
  if (!pane || !pane.el.isConnected) return;
  const rect = pane.el.getBoundingClientRect();
  if (rect.width < 8 || rect.height < 8) return;

  const m = pane.view.fit(rect.width - 8, rect.height - 6);
  // force：外观改了字号/行高，行列数可能没变但每个格子的尺寸变了，
  // 必须整屏重画，否则画面还是老字号的位图。
  if (!force && m.cols === pane.cols && m.rows === pane.rows) return;
  pane.cols = m.cols;
  pane.rows = m.rows;

  // 换了尺寸就先把已有内容整屏重画一遍（pane 刚挂到 DOM 时尤其必要），
  // 再让后端按新尺寸重排 —— 不等后者，免得出现一片空白
  pane.full = true;
  scheduleFrame(pane);
  if (session === workspace.focused) syncStatusRight();
  invoke("resize_session", { id: session, cols: m.cols, rows: m.rows }).catch(() => {});
}

// ───────────────────────── 渲染 ─────────────────────────

function collectSessions(l: Layout, out: Set<string>): void {
  if (l.kind === "leaf") out.add(l.session);
  else {
    collectSessions(l.a, out);
    collectSessions(l.b, out);
  }
}

function buildLayout(l: Layout, into: HTMLElement): void {
  if (l.kind === "leaf") {
    let pane = panes.get(l.session);
    if (!pane) {
      pane = createPane(l.session);
      panes.set(l.session, pane);
    }
    into.appendChild(pane.el);
    return;
  }

  const wrap = document.createElement("div");
  wrap.className = `split ${l.dir}`;

  const ca = document.createElement("div");
  ca.className = "cell";
  ca.style.flex = `${l.ratio} 1 0`;
  const cb = document.createElement("div");
  cb.className = "cell";
  cb.style.flex = `${1 - l.ratio} 1 0`;

  buildLayout(l.a, ca);
  buildLayout(l.b, cb);
  wrap.appendChild(ca);
  wrap.appendChild(cb);
  into.appendChild(wrap);
}

/** 内置默认标题随界面语言显示；用户改过的标题和后端原值不动。 */
function displayTitle(title: string): string {
  return title === "本地终端" ? t("本地终端") : title;
}

function renderTabs(): void {
  tabsEl.replaceChildren();
  for (const tab of workspace.tabs) {
    const btn = document.createElement("button");
    btn.className = "tab" + (tab.id === workspace.active_tab ? " active" : "");
    btn.title = t("{title}（双击改名）", { title: displayTitle(tab.title) });

    // 标签页标题就是主机名；是已知主机就用服务器图标，否则是本地 shell
    btn.appendChild(icon(sidebar.isHostName(tab.title) ? "server" : "terminal", "i"));
    if (tabHasAi(tab.layout)) {
      const mark = document.createElement("span");
      mark.className = "ai-mark";
      mark.title = t("AI 正在这个标签页里跑命令");
      btn.appendChild(mark);
    }

    const name = document.createElement("span");
    name.className = "tab-name";
    name.textContent = displayTitle(tab.title);
    btn.appendChild(name);

    const close = document.createElement("span");
    close.className = "tab-close";
    close.title = t("关闭标签页");
    close.appendChild(icon("x", "i"));
    close.addEventListener("click", (e) => {
      e.stopPropagation();
      void closeTab(tab);
    });
    btn.appendChild(close);

    btn.addEventListener("click", () => {
      const first = firstSession(tab.layout);
      if (first) invoke("activate_session_cmd", { id: first }).catch(() => {});
    });
    // 双击标签改名 —— 后端一直有 rename_session_cmd，界面之前没有入口
    btn.addEventListener("dblclick", (e) => {
      e.preventDefault();
      const first = firstSession(tab.layout);
      if (!first) return;
      const next = window.prompt(t("标签页名称"), tab.title);
      if (next === null) return;
      const title = next.trim();
      if (!title || title === tab.title) return;
      invoke("rename_session_cmd", { id: first, title }).catch((err) => setStatus(`${err}`));
    });

    tabsEl.appendChild(btn);
  }

  const plus = document.createElement("button");
  plus.className = "tab-add";
  plus.title = t("新建标签页");
  plus.appendChild(icon("plus", "i"));
  plus.addEventListener("click", () => {
    invoke("create_session", { cols: 120, rows: 34 }).catch(() => {});
  });
  tabsEl.appendChild(plus);
}

function firstSession(l: Layout): string | null {
  if (l.kind === "leaf") return l.session;
  return firstSession(l.a) ?? firstSession(l.b);
}

function renderWorkspace(): void {
  renderTabs();

  // 收集当前需要的会话
  const needed = new Set<string>();
  for (const t of workspace.tabs) collectSessions(t.layout, needed);

  // 回收已经关掉的 pane
  for (const [sid, p] of [...panes]) {
    if (!needed.has(sid)) {
      aiStopAllOf(sid);
      p.el.remove();
      panes.delete(sid);
    }
  }

  // 重新拼装当前标签页的布局（pane 元素是搬过去，不重建，所以不丢渲染状态）
  // 注意：新挂载的窗格上还没有胶囊，拼装完要 syncAi() 补回来。
  stageEl.replaceChildren();
  const active =
    workspace.tabs.find((t) => t.id === workspace.active_tab) ?? workspace.tabs[0];
  if (!active) {
    setStatus(t("没有会话"));
    statusRightEl.textContent = "";
    return;
  }
  buildLayout(active.layout, stageEl);

  // 挂回 DOM 之后再量尺寸；还没拿到过内容的 pane 主动要一次整屏
  for (const sid of needed) {
    scheduleFit(sid);
    const p = panes.get(sid);
    if (p && !p.hasData) {
      invoke("redraw_session", { id: sid }).catch(() => {});
    }
  }

  setStatus(displayTitle(active.title));
  syncStatusRight();
  // 新挂载的窗格上还没有胶囊 —— 切回来要补上
  syncAi();
}

// ───────────────────────── AI 执行态 ─────────────────────────

function elapsed(ms: number): string {
  const s = Math.floor(ms / 1000);
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}`;
}

function aiStart(run: string, session: string, command: string): void {
  if (aiRuns.has(run)) return;
  // 不要求窗格已挂载 —— AI 可能在后台标签里跑，那时窗格还没建，
  // 但标签上的绿点必须亮，否则人根本不知道它在动。
  aiRuns.set(run, { session, command, startedAt: Date.now() });
  syncAi();
  renderTabs();
}

function aiStop(run: string): void {
  if (!aiRuns.delete(run)) return;
  syncAi();
  renderTabs();
}

/** 会话关掉时连带收掉它上面的 run */
function aiStopAllOf(session: string): void {
  let hit = false;
  for (const [id, r] of [...aiRuns]) {
    if (r.session === session) {
      aiRuns.delete(id);
      hit = true;
    }
  }
  if (hit) {
    syncAi();
    renderTabs();
  }
}

function capFor(r: AiRun): HTMLElement {
  const cap = document.createElement("div");
  cap.className = "ai-cap";
  const dot = document.createElement("span");
  dot.className = "dot";
  const label = document.createElement("span");
  label.textContent = t("AI 正在执行");
  const cmd = document.createElement("span");
  cmd.className = "cmd";
  cmd.textContent = r.command;
  const sep = document.createElement("span");
  sep.className = "sep";
  sep.textContent = "·";
  const clock = document.createElement("span");
  clock.className = "cmd";
  clock.textContent = elapsed(Date.now() - r.startedAt);
  const sep2 = document.createElement("span");
  sep2.className = "sep";
  sep2.textContent = "·";
  const take = document.createElement("span");
  take.className = "take";
  take.textContent = t("接管 ⌃C");
  cap.append(dot, label, cmd, sep, clock, sep2, take);
  r.clock = clock;
  return cap;
}

/** 按 aiRuns 重建所有胶囊。窗格切换回来时会自然补上。 */
function syncAi(): void {
  for (const p of panes.values()) {
    for (const el of Array.from(p.el.querySelectorAll(".ai-cap"))) el.remove();
    p.el.classList.remove("ai-run");
  }
  for (const r of aiRuns.values()) r.clock = undefined;

  for (const [sid, pane] of panes) {
    const runs = runsOf(sid).sort((a, b) => a.startedAt - b.startedAt);
    if (runs.length === 0) continue;
    pane.el.classList.add("ai-run");
    runs.forEach((r, i) => {
      const cap = capFor(r);
      cap.style.top = `${10 + i * 30}px`;
      pane.el.appendChild(cap);
    });
  }

  if (aiRuns.size > 0 && aiTicker === 0) {
    aiTicker = window.setInterval(tickAi, 500);
  } else if (aiRuns.size === 0 && aiTicker !== 0) {
    window.clearInterval(aiTicker);
    aiTicker = 0;
  }
}

function tickAi(): void {
  const now = Date.now();
  for (const r of aiRuns.values()) {
    if (r.clock) r.clock.textContent = elapsed(now - r.startedAt);
  }
}

function tabHasAi(l: Layout): boolean {
  const set = new Set<string>();
  collectSessions(l, set);
  return [...aiRuns.values()].some((r) => set.has(r.session));
}

// ───────────────────────── ⌘K 命令面板 ─────────────────────────

const palette = new Palette(buildCommands);

window.addEventListener("dq:locale", () => {
  renderWorkspace();
  palette.refresh();
  mcpPill.title = t("连接 AI 客户端");
  if (!aiPanel.hidden) syncAiPanelState();
  if (bootLook && !lookPanel.hidden) syncLookPanel(bootLook);
});

function focusedTab(): Tab | undefined {
  return workspace.tabs.find((t) => t.id === workspace.active_tab) ?? workspace.tabs[0];
}

function buildCommands(): Cmd[] {
  const out: Cmd[] = [];
  const target = workspace.focused;

  const hosts = [...sidebar.allHosts()].sort((a, b) => {
    const ga = a.group?.trim() || "未分组";
    const gb = b.group?.trim() || "未分组";
    if (ga !== gb) return ga === "未分组" ? 1 : gb === "未分组" ? -1 : ga.localeCompare(gb);
    return a.name.localeCompare(b.name);
  });
  for (const h of hosts) {
    out.push({
      id: `host:${h.id}`,
      section: t("主机"),
      icon: "server",
      label: h.name,
      desc: `${h.user}@${h.hostname}:${h.port}`,
      hint: `${h.user}@${h.hostname} ${h.group ?? ""}`,
      run: () => {
        setStatus(t("连接 {name}…", { name: h.name }));
        invoke<string>("connect_host", { key: h.id }).catch((e) => setStatus(`${e}`));
      },
    });
  }

  out.push(
    {
      id: "tab:new",
      section: t("标签与窗格"),
      icon: "plus",
      label: t("新建标签页"),
      desc: t("在当前目录开一个本地 shell"),
      keys: "⌘T",
      run: () => void invoke("create_session", { cols: 120, rows: 34 }).catch(() => {}),
    },
    {
      id: "pane:v",
      section: t("标签与窗格"),
      icon: "columns",
      label: t("左右分屏"),
      desc: t("把当前窗格纵向切开"),
      keys: "⌘D",
      run: () => void splitFocused("vertical"),
    },
    {
      id: "pane:h",
      section: t("标签与窗格"),
      icon: "rows",
      label: t("上下分屏"),
      desc: t("把当前窗格横向切开"),
      keys: "⇧⌘D",
      run: () => void splitFocused("horizontal"),
    },
    {
      id: "tab:close",
      section: t("标签与窗格"),
      icon: "x",
      label: t("关闭标签页"),
      desc: t("关掉当前标签及其所有窗格"),
      keys: "⌘W",
      run: () => {
        const t = focusedTab();
        const first = t && firstSession(t.layout);
        if (first) void invoke("close_session_cmd", { id: first }).catch(() => {});
      },
    },
    {
      id: "tab:rename",
      section: t("标签与窗格"),
      icon: "pencil",
      label: t("重命名标签页"),
      desc: t("也可以双击标签直接改"),
      run: () => renameFocusedTab(),
    },
    {
      id: "host:new",
      section: t("主机管理"),
      icon: "plus",
      label: t("新建主机…"),
      desc: t("打开主机检查器"),
      keys: "⌘N",
      run: () => sidebar.openNew(),
    },
    {
      id: "host:import",
      section: t("主机管理"),
      icon: "download",
      label: t("从 ~/.ssh/config 导入"),
      desc: t("勾选要导入的条目"),
      run: () => void sidebar.openImportPanel(),
    },
    {
      id: "ai:panel",
      section: "AI",
      icon: "sparkles",
      label: t("连接 AI 客户端…"),
      desc: t("地址、状态和三家客户端的配置片段"),
      run: () => void openAiPanel(),
    },
    {
      id: "ai:copy",
      section: "AI",
      icon: "copy",
      label: t("复制 MCP 地址"),
      desc: mcpUrl || t("内置 MCP server"),
      keys: "⌘⇧C",
      run: () => void copyMcp(),
    }
  );

  if (target && runsOf(target).length > 0) {
    out.push({
      id: "ai:take",
      section: "AI",
      icon: "hand",
      label: t("接管当前窗格"),
      desc: t("给 AI 正在跑的命令发 Ctrl-C"),
      keys: "⌃C",
      run: () => void invoke("write_session", { id: target, data: "\u0003" }).catch(() => {}),
    });
  }
  return out;
}

/** 关掉一个标签页里的所有窗格 —— ⌘K 面板里写的就是「关掉当前标签及其所有窗格」 */
async function closeTab(tab: Tab): Promise<void> {
  const ids = new Set<string>();
  collectSessions(tab.layout, ids);
  for (const id of ids) {
    await invoke("close_session_cmd", { id }).catch(() => {});
  }
}

async function closeFocusedTab(): Promise<void> {
  const t = focusedTab();
  if (t) await closeTab(t);
}

async function splitFocused(direction: SplitDir): Promise<void> {
  const target = workspace.focused;
  if (!target) return;
  await invoke("split_session", { id: target, direction, cols: 80, rows: 24 }).catch(() => {});
}

function renameFocusedTab(): void {
  const tab = focusedTab();
  if (!tab) return;
  const first = firstSession(tab.layout);
  if (!first) return;
  const next = window.prompt(t("标签页名称"), tab.title);
  if (next === null) return;
  const title = next.trim();
  if (!title || title === tab.title) return;
  void invoke("rename_session_cmd", { id: first, title }).catch((e) => setStatus(`${e}`));
}

async function copyMcp(): Promise<void> {
  if (!mcpUrl) return;
  try {
    await navigator.clipboard.writeText(mcpUrl);
    sidebar.notify(t("已复制 {url}", { url: mcpUrl }));
  } catch {
    sidebar.notify(mcpUrl);
  }
}

// ─────────────── 连接 AI 客户端（只给地址和片段，不动别人的配置文件）───────────────

function snippets(url: string, bridge: string): Record<string, string> {
  return {
    // 本机实测：Claude Code 2.1.x 用 --transport http
    claude: `claude mcp add --transport http daiqiao ${url}`,
    // Codex 0.44+ 原生支持 streamable HTTP；0.77 起不再需要 experimental_use_rmcp_client
    codex: `[mcp_servers.daiqiao]\nurl = "${url}"`,
    cursor: `{\n  "mcpServers": {\n    "daiqiao": { "url": "${url}" }\n  }\n}`,
    // stdio 通道：给不支持 HTTP 传输的客户端用。
    // 桥只是把 stdin/stdout 的 JSON-RPC 转给上面那个 HTTP 端点，
    // 所以代敲必须开着 —— 工具要操作的就是这个窗口里的会话。
    stdio: `claude mcp add daiqiao ${bridge}`,
    stdioJson: `{\n  "mcpServers": {\n    "daiqiao": { "command": "${bridge}" }\n  }\n}`,
  };
}

// ───────────────────────── 外观 ─────────────────────────

/** 把外观套到所有窗格上：重新量字宽 → 整屏重画 → 通知 PTY 新尺寸 */
function applyLook(v: LookView): void {
  setLocale(v.language);
  // 记下来：之后新开的标签页要用这份，不能退回默认
  bootLook = v;

  for (const [sid, pane] of panes) {
    pane.view.applyAppearance(v);
    // 隐形输入框的字号要跟终端一致 —— 输入法按元素字号决定候选窗大小
    pane.ime.style.fontSize = `${v.font_size}px`;
    // 字号/行高变了，行列数和格子尺寸都可能变，强制重排重画
    fitPane(sid, true);
  }
  // 终端区整体底色（分屏缝隙、状态条上方那条）
  document.documentElement.style.setProperty("--term-bg", v.bg);
}

/** 把当前值回填到面板控件上 */
function syncLookPanel(v: LookView): void {
  const font = document.getElementById("look-font") as HTMLInputElement | null;
  const fontVal = document.getElementById("look-font-val") as HTMLElement | null;
  const family = document.getElementById("look-family") as HTMLInputElement | null;
  const lh = document.getElementById("look-lh") as HTMLInputElement | null;
  const lhVal = document.getElementById("look-lh-val") as HTMLElement | null;
  const themesEl = document.getElementById("look-themes");
  const cursorEl = document.getElementById("look-cursor");
  const languageEl = document.getElementById("look-language") as HTMLSelectElement | null;

  if (languageEl) {
    languageEl.replaceChildren();
    for (const language of v.languages) {
      const option = document.createElement("option");
      option.value = language.code;
      option.textContent = language.name;
      languageEl.appendChild(option);
    }
    languageEl.value = v.language;
  }

  if (font) {
    font.min = String(v.min_font);
    font.max = String(v.max_font);
    font.value = String(v.font_size);
  }
  if (fontVal) fontVal.textContent = `${v.font_size}px`;
  if (family) family.value = v.font_family;
  if (lh) lh.value = String(v.line_height);
  if (lhVal) lhVal.textContent = v.line_height.toFixed(2);

  if (themesEl && v.themes?.length) {
    themesEl.replaceChildren();
    for (const theme of v.themes) {
      const btn = document.createElement("button");
      btn.className = "look-theme" + (theme.name === v.theme ? " on" : "");
      btn.dataset.theme = theme.name;
      btn.title = t(theme.desc);
      // 色块预览：用主题的背景色打底、前景色写「Aa」——
      // 对比度是选配色唯一要看的东西，一个小色点看不出来。
      const sw = document.createElement("span");
      sw.className = "look-sw";
      sw.style.background = theme.bg;
      sw.style.color = theme.fg;
      sw.textContent = "Aa";
      const name = document.createElement("span");
      name.className = "look-theme-name";
      name.textContent = t(theme.name);
      btn.append(sw, name);
      btn.addEventListener("click", () => void saveLook({ theme: theme.name }));
      themesEl.appendChild(btn);
    }
  }

  if (cursorEl) {
    for (const b of Array.from(cursorEl.querySelectorAll<HTMLElement>("button"))) {
      b.classList.toggle("on", b.dataset.cursor === v.cursor_style);
    }
  }
}

/** 存一项外观改动。后端会广播 appearance://changed，套用由监听那边做。 */
async function saveLook(patch: Record<string, unknown>): Promise<void> {
  try {
    const v = await invoke<LookView>("set_appearance", patch);
    applyLook(v);
    syncLookPanel(v);
  } catch (err) {
    sidebar.notify(t("保存外观设置失败: {error}", { error: String(err) }));
  }
}

async function openLookPanel(): Promise<void> {
  closeOverlays("look-panel");
  lookPanel.hidden = false;
  try {
    const v = await invoke<LookView>("get_appearance");
    syncLookPanel(v);
  } catch (err) {
    sidebar.notify(t("读外观设置失败: {error}", { error: String(err) }));
  }
}

function syncAiPanelState(): void {
  const state = document.getElementById("ai-state") as HTMLElement;
  const tools = document.getElementById("ai-tools") as HTMLElement;
  state.className = `ai-state ${mcpListening ? "on" : "off"}`;
  state.textContent = mcpListening ? t("运行中") : t("没监听");
  tools.textContent =
    mcpListening && mcpTools > 0
      ? t(mcpTools === 1 ? "{count} 个工具" : "共 {count} 个工具", { count: mcpTools })
      : "";
}

async function openAiPanel(): Promise<void> {
  // 先让面板露面、再去问后端 —— 反过来的话，这几毫秒里按 Esc
  // 会被当成「没有浮层开着」而漏进 shell。
  const state = document.getElementById("ai-state") as HTMLElement;
  const tools = document.getElementById("ai-tools") as HTMLElement;
  state.className = "ai-state";
  state.textContent = t("检测中…");
  tools.textContent = "";
  closeOverlays("ai-panel");
  aiPanel.hidden = false;

  // 每次打开都重新问一次后端（server 掉了要能看出来）
  try {
    const info = await invoke<{
      url: string;
      port: number;
      listening: boolean;
      tools: number;
      bridge: string;
    }>("mcp_info");
    mcpUrl = info.url;
    mcpListening = info.listening;
    mcpTools = info.tools;
    if (info.bridge) mcpBridge = info.bridge;
  } catch {
    mcpListening = false;
  }

  const url = mcpUrl || "http://127.0.0.1:7391/mcp";
  (document.getElementById("ai-url") as HTMLElement).textContent = url;

  const snips = snippets(url, mcpBridge);
  for (const el of Array.from(document.querySelectorAll<HTMLElement>("[data-snip]"))) {
    el.textContent = snips[el.dataset.snip ?? ""] ?? "";
  }

  syncAiPanelState();
}

function updateFocusRing(): void {
  for (const [sid, p] of panes) {
    p.el.classList.toggle("focused", sid === workspace.focused);
  }
  syncStatusRight();
}

// ───────────────────────── 事件接入 ─────────────────────────

async function boot(): Promise<void> {
  // 先把存盘的外观读出来套上 —— 必须在建窗格之前，
  // 否则第一屏按默认字号画完，改过设置的人每次启动都要闪一下回弹。
  // （之前漏了这一步：外观存了盘但启动不读，重启就白改。）
  try {
    const look = await invoke<LookView>("get_appearance");
    bootLook = look;
    setLocale(look.language);
    document.documentElement.style.setProperty("--term-bg", look.bg);
  } catch (err) {
    console.warn(t("读外观设置失败，用默认值"), err);
  }

  await listen<Workspace>("workspace://changed", (e) => {
    workspace = e.payload;
    renderWorkspace();
    updateFocusRing();
    // 标签页标题就是主机名，拿来点亮侧边栏的状态点
    sidebar.markConnected(new Set(workspace.tabs.map((t) => t.title)));
  });

  await listen<TermUpdate>("term://update", (e) => {
    const u = e.payload;
    const pane = panes.get(u.session);
    if (!pane) return;
    pane.view.applyUpdate(u);
    pane.hasData = true;
    if (u.full) pane.full = true;
    else for (const l of u.lines) pane.dirty.add(l.row);
    scheduleFrame(pane);
  });

  await listen<string>("term://exit", (e) => {
    const pane = panes.get(e.payload);
    if (pane) pane.el.classList.add("exited");
  });

  // AI 开始 / 结束跑命令 —— 窗格顶部流光线 + 右上胶囊 + 标签上的小绿点
  await listen<{ run: string; session: string; command: string }>("ai://exec", (e) => {
    aiStart(e.payload.run, e.payload.session, e.payload.command);
  });
  await listen<{ run: string }>("ai://done", (e) => {
    aiStop(e.payload.run);
  });

  // 外观变了（人在面板里改的，或 AI 调 appearance_set）—— 所有窗格立刻套用
  await listen<LookView>("appearance://changed", (e) => {
    applyLook(e.payload);
    syncLookPanel(e.payload);
  });

  // 主机列表变了（无论是人点的还是 AI 调的）就刷新侧边栏
  await listen("hosts://changed", () => {
    void sidebar.refresh();
  });

  // 侧栏齿轮 → 外观；MCP 胶囊 → 连接 AI 客户端
  (document.getElementById("app-settings") as HTMLElement).addEventListener("click", () => {
    void openLookPanel();
  });
  (document.getElementById("look-close") as HTMLElement).addEventListener("click", () => {
    lookPanel.hidden = true;
  });

  // 字号/行高拖动时实时预览，松手才落盘 —— 拖一次写几十遍文件没必要
  const lookFont = document.getElementById("look-font") as HTMLInputElement;
  const lookFontVal = document.getElementById("look-font-val") as HTMLElement;
  lookFont.addEventListener("input", () => {
    lookFontVal.textContent = `${lookFont.value}px`;
  });
  lookFont.addEventListener("change", () => {
    void saveLook({ fontSize: Number(lookFont.value) });
  });

  const lookLh = document.getElementById("look-lh") as HTMLInputElement;
  const lookLhVal = document.getElementById("look-lh-val") as HTMLElement;
  lookLh.addEventListener("input", () => {
    lookLhVal.textContent = Number(lookLh.value).toFixed(2);
  });
  lookLh.addEventListener("change", () => {
    void saveLook({ lineHeight: Number(lookLh.value) });
  });

  const lookFamily = document.getElementById("look-family") as HTMLInputElement;
  lookFamily.addEventListener("change", () => {
    void saveLook({ fontFamily: lookFamily.value.trim() });
  });

  const lookLanguage = document.getElementById("look-language") as HTMLSelectElement | null;
  lookLanguage?.addEventListener("change", () => {
    void saveLook({ language: lookLanguage.value });
  });

  for (const b of Array.from(
    document.querySelectorAll<HTMLElement>("#look-cursor button[data-cursor]")
  )) {
    b.addEventListener("click", () => {
      void saveLook({ cursorStyle: b.dataset.cursor });
    });
  }
  (document.getElementById("ai-close") as HTMLElement).addEventListener("click", () => {
    aiPanel.hidden = true;
  });
  (document.getElementById("ai-copy-url") as HTMLElement).addEventListener("click", () => {
    void copyMcp();
  });
  for (const btn of Array.from(document.querySelectorAll<HTMLElement>("[data-copy]"))) {
    btn.addEventListener("click", () => {
      const snip = snippets(mcpUrl || "http://127.0.0.1:7391/mcp", mcpBridge)[
        btn.dataset.copy ?? ""
      ];
      if (!snip) return;
      void navigator.clipboard
        .writeText(snip)
        .then(() => sidebar.notify(t("已复制配置片段")))
        .catch(() => sidebar.notify(snip));
    });
  }

  // MCP 胶囊：显示内置 server 的端口，点一下把地址复制走（零配置接入用）
  try {
    const info = await invoke<{ url: string; port: number }>("mcp_info");
    mcpUrl = info.url;
    mcpTextEl.textContent = `MCP · ${info.port}`;
    mcpPill.title = t("连接 AI 客户端");
    mcpPill.addEventListener("click", () => void openAiPanel());
  } catch {
    mcpTextEl.textContent = "MCP";
  }

  // Nerd Font 图标字体：到位后整屏重画（canvas 不会自己重画）
  void loadSymbolFont().then((ok) => {
    if (!ok) return;
    for (const pane of panes.values()) {
      pane.view.resetGlyphCache();
      pane.full = true;
      scheduleFrame(pane);
    }
  });

  // 窗口被遮挡时 WKWebView 不跑 rAF，画布会停在旧帧；回到前台先整屏重画
  const repaintAll = () => {
    for (const pane of panes.values()) {
      pane.full = true;
      scheduleFrame(pane);
    }
  };
  document.addEventListener("visibilitychange", () => {
    if (!document.hidden) repaintAll();
  });
  window.addEventListener("focus", repaintAll);

  // 首屏：起一个标签页
  await invoke<string>("create_session", { cols: 120, rows: 34 });
  // 侧边栏拉主机列表（和 MCP 的 host_list 同一份数据）
  await sidebar.refresh();
}

// ───────────────────────── 键盘 ─────────────────────────

function encodeKey(e: KeyboardEvent): string | null {
  if (e.metaKey) return null;

  if (e.ctrlKey && !e.altKey) {
    const k = e.key.toLowerCase();
    if (k.length === 1 && k >= "a" && k <= "z") {
      return String.fromCharCode(k.charCodeAt(0) - 96);
    }
    switch (e.key) {
      case "[":
        return "\x1b";
      case "\\":
        return "\x1c";
      case "]":
        return "\x1d";
      case "^":
        return "\x1e";
      case "_":
        return "\x1f";
      case " ":
        return "\x00";
    }
  }

  const mod = e.altKey ? 3 : 1;
  switch (e.key) {
    case "Enter":
      return "\r";
    case "Backspace":
      return "\x7f";
    case "Tab":
      return e.shiftKey ? "\x1b[Z" : "\t";
    case "Escape":
      return "\x1b";
    case "ArrowUp":
      return e.altKey ? `\x1b[1;${mod}A` : "\x1b[A";
    case "ArrowDown":
      return e.altKey ? `\x1b[1;${mod}B` : "\x1b[B";
    case "ArrowRight":
      return e.altKey ? `\x1b[1;${mod}C` : "\x1b[C";
    case "ArrowLeft":
      return e.altKey ? `\x1b[1;${mod}D` : "\x1b[D";
    case "Home":
      return "\x1b[H";
    case "End":
      return "\x1b[F";
    case "PageUp":
      return "\x1b[5~";
    case "PageDown":
      return "\x1b[6~";
    case "Delete":
      return "\x1b[3~";
    case "Insert":
      return "\x1b[2~";
    case "F1":
      return "\x1bOP";
    case "F2":
      return "\x1bOQ";
    case "F3":
      return "\x1bOR";
    case "F4":
      return "\x1bOS";
    case "F5":
      return "\x1b[15~";
    case "F6":
      return "\x1b[17~";
    case "F7":
      return "\x1b[18~";
    case "F8":
      return "\x1b[19~";
    case "F9":
      return "\x1b[20~";
    case "F10":
      return "\x1b[21~";
    case "F11":
      return "\x1b[23~";
    case "F12":
      return "\x1b[24~";
  }

  if (e.altKey && e.key.length === 1) return "\x1b" + e.key;
  if (e.key.length === 1) return e.key;
  return null;
}

/** 焦点在输入框 / 文本域里 —— 这时候键盘归它，不能转发给终端 */
function inEditable(t: EventTarget | null): boolean {
  const el = t as HTMLElement | null;
  if (!el || !el.tagName) return false;
  const tag = el.tagName.toLowerCase();
  return tag === "input" || tag === "textarea" || tag === "select" || el.isContentEditable;
}

window.addEventListener("keydown", async (e) => {
  // 面板的 input 自己处理过的键（比如 Esc 关面板）就到此为止 ——
  // 否则同一下 Esc 会接着把右侧抽屉也关掉，用户半路填的内容就没了。
  if (e.defaultPrevented) return;

  // ⌘K 开命令面板；在搜索框里也该能开
  if (e.metaKey && !e.shiftKey && e.key.toLowerCase() === "k") {
    e.preventDefault();
    palette.toggle();
    return;
  }
  // 面板开着时键盘归它
  if (palette.isOpen()) return;

  // Esc 收浮层：一次只收最上面那一层
  if (e.key === "Escape" && anyOverlayOpen()) {
    e.preventDefault();
    closeTopOverlay();
    return;
  }

  // 全局快捷键：焦点在哪都该生效（除了输入框里，那儿 ⌘A/⌘C 归输入框）
  if (e.metaKey && !inEditable(e.target)) {
    const k = e.key.toLowerCase();
    // 这几个键帽在 ⌘K 面板里写着，必须真生效
    if (k === "t" && !e.shiftKey) {
      e.preventDefault();
      await invoke("create_session", { cols: 120, rows: 34 }).catch(() => {});
      return;
    }
    if (k === "n" && !e.shiftKey) {
      e.preventDefault();
      sidebar.openNew();
      return;
    }
    if (k === "c" && e.shiftKey) {
      e.preventDefault();
      await copyMcp();
      return;
    }
    if (k === "w" && !e.shiftKey) {
      e.preventDefault();
      await closeFocusedTab();
      return;
    }
    if (k === "d") {
      e.preventDefault();
      await splitFocused(e.shiftKey ? "horizontal" : "vertical");
      return;
    }
  }

  // ↓ 以下是「把键送进终端」。守卫要挡住两种情况：
  //   ① 焦点在某个输入框里（在那儿打字不该同时进 shell）；
  //   ② 任何浮层开着 —— 抽屉不是模态的，点一下面板里的说明文字，
  //      焦点就回到 body，这时按键会直接打进终端，回车还会执行。
  if (inEditable(e.target)) return;
  if (anyOverlayOpen()) return;

  const target = workspace.focused;
  if (!target) return;

  const seq = encodeKey(e);
  if (seq === null) return;
  e.preventDefault();
  await invoke("write_session", { id: target, data: seq }).catch((err) => setStatus(`${err}`));
});

window.addEventListener("paste", async (e) => {
  if (inEditable(e.target)) return;
  if (anyOverlayOpen()) return;
  const target = workspace.focused;
  if (!target) return;
  const text = e.clipboardData?.getData("text");
  if (text) {
    e.preventDefault();
    await invoke("write_session", { id: target, data: text }).catch(() => {});
  }
});

boot().catch((err) => setStatus(t("启动失败: {error}", { error: String(err) })));
