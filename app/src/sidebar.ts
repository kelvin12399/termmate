// 侧边栏：主机管理（参考 Termius 的信息架构）。
//
// 关键点：这里读写的是**和 MCP 工具同一份 hosts.json**。人在界面里加一台主机，
// AI 那边 host_list 立刻就能看到；AI 加的主机，界面会收到广播自动刷新。

import { invoke } from "@tauri-apps/api/core";
import { KeyVault, type KeyInfo } from "./keyvault";
import { GroupCombo } from "./group_combo";
import { t } from "./i18n";

export interface Host {
  id: string;
  name: string;
  hostname: string;
  port: number;
  user: string;
  auth: string;
  key_id?: string | null;
  /** 旧字段，仅兼容方向 A 之前存的主机 */
  key_path?: string | null;
  /** 后端算好的「实际会用哪个私钥文件」，key_id 优先、否则退回 key_path */
  resolved_key_path?: string | null;
  /** 密码认证：密码存没存进钥匙串（内容永远不进前端） */
  password_saved?: boolean;
  tags: string[];
  group?: string | null;
  note?: string | null;
}

interface SshConfigEntry {
  alias: string;
  hostname: string;
  user: string;
  port: number;
  identity: string | null;
}

type TestState =
  | { kind: "save-first" }
  | { kind: "connecting" }
  | { kind: "success"; seconds: number }
  | { kind: "error"; message: string }
  | null;

const UNGROUPED = "未分组";

export type SideView = "hosts" | "sftp";

interface Els {
  body: HTMLElement;
  search: HTMLInputElement;
  newBtn: HTMLElement;
  importBtn: HTMLElement;
  views: HTMLElement;
  drawer: HTMLElement;
  drawerTitle: HTMLElement;
  drawerDelete: HTMLButtonElement;
  drawerSave: HTMLButtonElement;
  drawerClose: HTMLElement;
  fName: HTMLInputElement;
  fHost: HTMLInputElement;
  fPort: HTMLInputElement;
  fUser: HTMLInputElement;
  fAuth: HTMLSelectElement;
  fKeyRow: HTMLElement;
  keyCard: HTMLElement;
  keyName: HTMLElement;
  keyBadge: HTMLElement;
  keyFp: HTMLElement;
  keySwap: HTMLElement;
  keyEmpty: HTMLElement;
  keyPick: HTMLElement;
  keyFile: HTMLElement;
  keyPaste: HTMLElement;
  keyGen: HTMLElement;
  keyAlert: HTMLElement;
  keyAlertText: HTMLElement;
  keyAlertAct: HTMLButtonElement;
  keyUsed: HTMLElement;
  testBtn: HTMLButtonElement;
  testOut: HTMLElement;
  fGroup: HTMLInputElement;
  fNote: HTMLInputElement;
  importPanel: HTMLElement;
  importList: HTMLElement;
  importClose: HTMLElement;
  importDo: HTMLButtonElement;
  ctxMenu: HTMLElement;
  authSeg: HTMLElement;
  passRow: HTMLElement;
  passInput: HTMLInputElement;
  passSave: HTMLButtonElement;
  passClear: HTMLButtonElement;
  passStatus: HTMLElement;
  agentRow: HTMLElement;
  agentStatus: HTMLElement;
}

/** 所有浮层，从上到下。右键菜单浮在面板之上，所以排最前。 */
const OVERLAYS = [
  "ctx-menu",
  "keyvault",
  "drawer",
  "import-panel",
  "ai-panel",
  "look-panel",
  "config-panel",
  "term-menu",
] as const;

/** 同时只让一个浮层露面 —— 否则同 z-index 的两个面板会无声叠在一起 */
export function closeOverlays(except?: string): void {
  for (const id of OVERLAYS) {
    if (id === except) continue;
    const el = document.getElementById(id);
    if (el) el.hidden = true;
  }
}

/** 有没有浮层开着。开着时键盘不能再往终端转发。 */
export function anyOverlayOpen(): boolean {
  return OVERLAYS.some((id) => document.getElementById(id)?.hidden === false);
}

/** Esc 一次只收最上面那一层 */
export function closeTopOverlay(): void {
  for (const id of OVERLAYS) {
    const el = document.getElementById(id);
    if (el && !el.hidden) {
      el.hidden = true;
      return;
    }
  }
}

/** 取 index.html 里内联的 Lucide 精灵 */
function icon(name: string, cls = "i"): SVGSVGElement {
  const svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
  svg.setAttribute("class", cls === "i" ? "i" : `i ${cls}`);
  const use = document.createElementNS("http://www.w3.org/2000/svg", "use");
  use.setAttribute("href", `#i-${name}`);
  svg.appendChild(use);
  return svg;
}

export class Sidebar {
  private hosts: Host[] = [];
  private query = "";
  private collapsed = new Set<string>();
  private view: SideView = "hosts";
  /** 正在编辑的 key（null = 新建） */
  private editingKey: string | null = null;
  /** 导入面板里勾选中的条目 */
  private picked = new Set<string>();
  /** 当前已连接的主机名（标签页标题），只有这些行才显示绿点 */
  private connected = new Set<string>();
  /** 抽屉里当前选中的密钥（null = 还没选） */
  private pickedKey: KeyInfo | null = null;
  private keyVault: KeyVault;
  private groupCombo: GroupCombo;
  private testState: TestState = null;
  private importCandidates: SshConfigEntry[] = [];

  constructor(
    private els: Els,
    private onConnect: (h: Host) => void,
    private onViewChange: (v: SideView) => void,
    private onSftp: (h: Host) => void
  ) {
    this.keyVault = new KeyVault((msg) => this.toast(msg));
    this.groupCombo = new GroupCombo(this.els.fGroup, () => {
      const names = new Set<string>();
      for (const h of this.hosts) {
        const g = h.group?.trim();
        if (g) names.add(g);
      }
      return [...names].sort((a, b) => a.localeCompare(b, "zh-CN"));
    });

    this.els.search.addEventListener("input", () => {
      this.query = this.els.search.value.trim();
      this.renderHosts();
    });
    this.els.search.addEventListener("keydown", (e) => {
      if (e.key === "Enter") this.quickConnect();
      if (e.key === "Escape") {
        this.els.search.value = "";
        this.query = "";
        this.renderHosts();
      }
    });

    // 下面这些元素少任何一个都不该把整个初始化链炸断（harness 和未来裁剪过的
    // 页面都可能缺某几个），所以统一用可选绑定。
    const bind = (el: HTMLElement | null | undefined, ev: string, fn: () => void) =>
      el?.addEventListener(ev, fn);

    bind(this.els.newBtn, "click", () => this.openDrawer(null));
    bind(this.els.importBtn, "click", () => void this.openImport());
    bind(this.els.importClose, "click", () => {
      this.els.importPanel.hidden = true;
    });
    bind(this.els.importDo, "click", () => void this.doImport());

    bind(this.els.drawerClose, "click", () => this.closeDrawer());
    bind(this.els.drawerSave, "click", () => void this.save());
    bind(this.els.drawerDelete, "click", () => void this.removeEditing());
    this.els.fAuth?.addEventListener("change", () => this.syncAuthRow());

    // 四条入口：从 ~/.ssh 选 / 选择文件 / 粘贴 / 新建一对。
    // 「选择文件」直接走原生选择器导入，其余都进密钥库弹层。
    bind(this.els.keyPick, "click", () => void this.chooseKey("pick"));
    bind(this.els.keySwap, "click", () => void this.chooseKey("pick"));
    bind(this.els.keyPaste, "click", () => void this.chooseKey("paste"));
    bind(this.els.keyGen, "click", () => void this.chooseKey("gen"));
    bind(this.els.keyFile, "click", () => void this.importKeyFile());

    // 把密钥文件拖进空态框
    const empty = this.els.keyEmpty;
    if (empty) {
      empty.addEventListener("dragover", (e) => {
        e.preventDefault();
        empty.classList.add("drag");
      });
      empty.addEventListener("dragleave", () => empty.classList.remove("drag"));
      empty.addEventListener("drop", (e) => {
        e.preventDefault();
        empty.classList.remove("drag");
        void this.dropKeyFile(e);
      });
    }

    bind(this.els.testBtn, "click", () => void this.testConnect());

    // 密码两个按钮都只在已保存的主机上有意义；新建主机的密码随「保存」一起进钥匙串
    bind(this.els.passSave, "click", () => void this.savePasswordNow());
    bind(this.els.passClear, "click", () => void this.clearPasswordNow());

    // 认证方式用分段控件（三种方式一眼全在），隐藏的 select 仍是取值的唯一来源
    for (const b of Array.from(this.els.authSeg?.querySelectorAll<HTMLElement>("button") ?? [])) {
      b.addEventListener("click", () => {
        this.els.fAuth.value = b.dataset.auth ?? "key";
        this.syncAuthSeg();
        this.syncAuthRow();
      });
    }

    for (const btn of Array.from(this.els.views.querySelectorAll<HTMLElement>("button"))) {
      btn.addEventListener("click", () => {
        const v = (btn.dataset.view ?? "hosts") as SideView;
        this.view = v;
        for (const b of Array.from(this.els.views.querySelectorAll<HTMLElement>("button"))) {
          b.classList.toggle("on", b === btn);
        }
        this.onViewChange(v);
        this.renderHosts();
      });
    }

    // 点空白处收起右键菜单
    document.addEventListener("mousedown", (e) => {
      if (!this.els.ctxMenu.hidden && !this.els.ctxMenu.contains(e.target as Node)) {
        this.els.ctxMenu.hidden = true;
      }
    });

    window.addEventListener("dq:locale", () => {
      document.querySelectorAll(".side-toast").forEach((toast) => toast.remove());
      this.renderHosts();
      this.els.ctxMenu.hidden = true;
      if (!this.els.importPanel.hidden) this.renderImport();
      if (this.els.drawer.hidden) return;
      this.els.drawerTitle.textContent = t(this.editingKey ? "编辑主机" : "新建主机");
      this.renderKeyField();
      this.renderPassStatus(this.hosts.find((h) => h.id === this.editingKey) ?? null, false);
      this.renderTestState();
      if (this.els.fAuth.value === "agent") void this.refreshAgentStatus();
    });
  }

  async refresh(): Promise<void> {
    try {
      this.hosts = await invoke<Host[]>("list_hosts");
    } catch {
      this.hosts = [];
    }
    this.renderHosts();
  }

  // ─────────── 搜索框里直接输 user@host 就能连 ───────────

  private quickConnect(): void {
    const q = this.query;
    if (!q || !/^[\w.-]+@[\w.-]+/.test(q)) return;
    const [user, rest] = q.split("@");
    const [hostname, portStr] = rest.split(":");
    this.els.search.value = "";
    this.query = "";
    this.renderHosts();
    void this.saveQuick(user, hostname, Number(portStr) || 22);
  }

  private async saveQuick(user: string, hostname: string, port: number): Promise<void> {
    try {
      const key = await invoke<string>("save_host", {
        key: null,
        name: hostname,
        hostname,
        user,
        port,
        auth: "key",
        keyPath: null,
        group: null,
        note: "由快速连接创建",
      });
      await this.refresh();
      const h = this.hosts.find((x) => x.id === key);
      if (h) this.onConnect(h);
    } catch (e) {
      this.toast(String(e));
    }
  }

  // ─────────── 渲染 ───────────

  private visibleHosts(): Host[] {
    const q = this.query.toLowerCase();
    if (!q) return this.hosts;
    return this.hosts.filter(
      (h) =>
        h.name.toLowerCase().includes(q) ||
        h.hostname.toLowerCase().includes(q) ||
        h.user.toLowerCase().includes(q) ||
        (h.group ?? "").toLowerCase().includes(q)
    );
  }

  private renderHosts(): void {
    const body = this.els.body;
    body.replaceChildren();

    if (this.view === "sftp") {
      this.renderSftpHostList(body);
      return;
    }

    const list = this.visibleHosts();
    if (list.length === 0) {
      const empty = document.createElement("div");
      empty.className = "side-empty";
      empty.textContent = t(
        this.hosts.length === 0
          ? "还没有主机。点右上角 ＋ 新建，或从下面导入 ~/.ssh/config。"
          : "没找到匹配的主机。"
      );
      body.appendChild(empty);
      return;
    }

    const groups = new Map<string, Host[]>();
    for (const h of list) {
      const g = h.group && h.group.trim() ? h.group.trim() : UNGROUPED;
      if (!groups.has(g)) groups.set(g, []);
      groups.get(g)!.push(h);
    }

    const ordered = [...groups.entries()].sort((a, b) =>
      a[0] === UNGROUPED ? 1 : b[0] === UNGROUPED ? -1 : a[0].localeCompare(b[0])
    );

    for (const [g, items] of ordered) {
      const isCollapsed = this.collapsed.has(g);
      const head = document.createElement("div");
      head.className = "side-group";
      const caret = icon(isCollapsed ? "chevron-right" : "chevron-down", "caret");
      const gname = document.createElement("span");
      gname.className = "gname";
      gname.textContent = g === UNGROUPED ? t("未分组") : g;
      const gcount = document.createElement("span");
      gcount.className = "gcount";
      gcount.textContent = String(items.length);
      head.append(caret, gname, gcount);
      head.addEventListener("click", () => {
        if (isCollapsed) this.collapsed.delete(g);
        else this.collapsed.add(g);
        this.renderHosts();
      });
      body.appendChild(head);

      if (isCollapsed) continue;
      for (const h of items) body.appendChild(this.hostRow(h));
    }
  }

  /** SFTP 页：列出主机，点一台就开资源管理器面板 */
  private renderSftpHostList(body: HTMLElement): void {
    const list = this.visibleHosts();
    if (list.length === 0) {
      const empty = document.createElement("div");
      empty.className = "side-empty";
      empty.textContent = t(
        this.hosts.length === 0 ? "还没有主机。先去「主机」页加一台。" : "没找到匹配的主机。"
      );
      body.appendChild(empty);
      return;
    }

    for (const h of list) {
      const row = document.createElement("div");
      row.className = "host-row";
      const box = document.createElement("div");
      box.className = "host-ico";
      box.appendChild(icon("folder"));
      row.appendChild(box);

      const meta = document.createElement("div");
      meta.className = "host-meta";
      const nm = document.createElement("div");
      nm.className = "host-name";
      nm.textContent = h.name;
      const sub = document.createElement("div");
      sub.className = "host-sub";
      sub.textContent = `${h.user}@${h.hostname}:${h.port}`;
      meta.append(nm, sub);
      row.appendChild(meta);

      row.addEventListener("click", () => this.onSftp(h));
      body.appendChild(row);
    }
  }

  private hostRow(h: Host): HTMLElement {
    const row = document.createElement("div");
    row.className = "host-row";
    row.dataset.hostId = h.id;
    if (this.connected.has(h.name)) row.classList.add("on");

    const box = document.createElement("div");
    box.className = "host-ico";
    box.appendChild(icon("server"));
    row.appendChild(box);

    const meta = document.createElement("div");
    meta.className = "host-meta";
    const name = document.createElement("div");
    name.className = "host-name";
    name.textContent = h.name;
    const sub = document.createElement("div");
    sub.className = "host-sub";
    sub.textContent = `${h.user}@${h.hostname}:${h.port}`;
    meta.append(name, sub);
    row.appendChild(meta);

    // 正常状态什么都不挂：已连接才有绿点，缺私钥才有琥珀钥匙
    if (this.connected.has(h.name)) {
      const dot = document.createElement("span");
      dot.className = "dot";
      dot.title = t("已连接");
      row.appendChild(dot);
    } else if (h.auth === "key" && !h.resolved_key_path) {
      const w = document.createElement("span");
      w.className = "warn";
      w.title = t("没有填私钥路径");
      w.appendChild(icon("key"));
      row.appendChild(w);
    }

    const more = document.createElement("button");
    more.className = "host-more";
    more.title = t("更多");
    more.appendChild(icon("ellipsis"));
    more.addEventListener("click", (e) => {
      e.stopPropagation();
      const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
      this.showCtxMenu(r.left, r.bottom + 4, h);
    });
    row.appendChild(more);

    row.addEventListener("click", () => this.onConnect(h));
    row.addEventListener("contextmenu", (e) => {
      e.preventDefault();
      this.showCtxMenu(e.clientX, e.clientY, h);
    });

    return row;
  }

  /** 会话列表变化时更新连接状态（绿点是条件渲染的，所以变了才重画） */
  markConnected(names: Set<string>): void {
    const same =
      names.size === this.connected.size && [...names].every((n) => this.connected.has(n));
    if (same) return;
    this.connected = new Set(names);
    this.renderHosts();
  }

  // ─────────── 右键菜单 ───────────

  private showCtxMenu(x: number, y: number, h: Host): void {
    const menu = this.els.ctxMenu;
    closeOverlays("ctx-menu");
    menu.replaceChildren();
    menu.hidden = false;

    const item = (label: string, ic: string, danger: boolean, fn: () => void) => {
      const b = document.createElement("button");
      if (danger) b.classList.add("danger");
      b.appendChild(icon(ic));
      const t = document.createElement("span");
      t.textContent = label;
      b.appendChild(t);
      b.addEventListener("click", () => {
        menu.hidden = true;
        fn();
      });
      menu.appendChild(b);
    };
    const sep = () => {
      const d = document.createElement("div");
      d.className = "sep";
      menu.appendChild(d);
    };

    item(t("连接"), "plug", false, () => this.onConnect(h));
    item(t("编辑"), "pencil", false, () => this.openDrawer(h));
    item(t("复制一份"), "copy", false, () => void this.duplicate(h));
    sep();
    item(t("删除"), "trash", true, () => void this.removeHost(h));

    const w = menu.offsetWidth || 140;
    const hh = menu.offsetHeight || 120;
    menu.style.left = `${Math.min(x, window.innerWidth - w - 8)}px`;
    menu.style.top = `${Math.min(y, window.innerHeight - hh - 8)}px`;
  }

  private async duplicate(h: Host): Promise<void> {
    try {
      await invoke<string>("save_host", {
        key: null,
        name: t("{name} 副本", { name: h.name }),
        hostname: h.hostname,
        user: h.user,
        port: h.port,
        auth: h.auth,
        keyId: h.key_id ?? null,
        group: h.group ?? null,
        note: h.note ?? null,
      });
      await this.refresh();
    } catch (e) {
      this.toast(String(e));
    }
  }

  private async removeHost(h: Host): Promise<void> {
    const ok = window.confirm(
      t("删除主机「{name}」？\n\n{target}\n\n这个操作不可撤销。", {
        name: h.name,
        target: `${h.user}@${h.hostname}:${h.port}`,
      })
    );
    if (!ok) return;
    try {
      await invoke("delete_host", { key: h.id });
      await this.refresh();
    } catch (e) {
      this.toast(String(e));
    }
  }

  // ─────────── 编辑抽屉 ───────────

  private openDrawer(h: Host | null): void {
    this.editingKey = h ? h.id : null;
    this.els.drawerTitle.textContent = t(h ? "编辑主机" : "新建主机");
    this.els.drawerDelete.hidden = !h;

    this.els.fName.value = h?.name ?? "";
    this.els.fHost.value = h?.hostname ?? "";
    this.els.fPort.value = String(h?.port ?? 22);
    this.els.fUser.value = h?.user ?? "";
    this.els.fAuth.value = h?.auth ?? "key";
    this.els.fGroup.value = h?.group ?? "";
    this.els.fNote.value = h?.note ?? "";

    this.els.testOut.textContent = "";
    this.els.testOut.className = "test-out";
    this.testState = null;
    // 编辑已有主机：把它当前用的那把密钥找出来显示；新建就是空态
    this.pickedKey = null;
    this.renderPassStatus(h);
    void this.loadDrawerKey(h);

    this.syncAuthSeg();
    this.syncAuthRow();
    closeOverlays("drawer");
    this.els.drawer.hidden = false;
    this.els.fName.focus();
  }

  /** 编辑已有主机时，把它当前用的密钥查出来显示在卡上 */
  private async loadDrawerKey(h: Host | null): Promise<void> {
    this.pickedKey = null;
    if (h?.key_id) {
      try {
        const all = await invoke<KeyInfo[]>("list_keys_cmd");
        this.pickedKey = all.find((k) => k.id === h.key_id) ?? null;
        if (!this.pickedKey) {
          this.toast(
            t("这台主机引用的密钥已经不在了（{keyId}），请重新选一把", { keyId: h.key_id! })
          );
        }
      } catch (e) {
        this.toast(`${e}`);
      }
    } else if (h?.key_path) {
      // 老主机：直接存的路径，还没进密钥库。扫一遍看能不能对上同一个文件
      try {
        const all = await invoke<KeyInfo[]>("list_keys_cmd");
        this.pickedKey = all.find((k) => k.path === h.key_path) ?? null;
      } catch {
        this.pickedKey = null;
      }
    }
    this.renderKeyField();
  }

  /** 按 pickedKey 画密钥区：卡 / 空态 / 提示条 / 被几台主机用 */
  private renderKeyField(): void {
    const k = this.pickedKey;
    this.els.keyCard.hidden = k === null;
    this.els.keyEmpty.hidden = k !== null;
    this.els.keyAlert.hidden = true;
    this.els.keyUsed.hidden = true;
    if (!k) return;

    this.els.keyName.textContent = k.name;
    this.els.keyBadge.textContent = k.source === "vault" ? t("代敲库") : "~/.ssh";
    this.els.keyBadge.className = "key-badge" + (k.source === "vault" ? " vault" : "");
    this.els.keyFp.textContent = `${k.algo} · ${k.fingerprint}`;

    // 权限太松比有口令更要紧（SSH 会直接拒），所以优先报它
    if (!k.perm_ok) {
      this.els.keyAlert.hidden = false;
      this.els.keyAlertText.textContent = t("这个私钥文件权限太松，SSH 会拒绝用它");
      this.els.keyAlertAct.hidden = false;
      this.els.keyAlertAct.textContent = t("改成 600");
      this.els.keyAlertAct.onclick = () => void this.fixKeyPerm(k);
    } else if (k.encrypted) {
      this.els.keyAlert.hidden = false;
      this.els.keyAlertText.textContent = t("这把密钥有口令，连接时需要解锁（钥匙串还没做）");
      this.els.keyAlertAct.hidden = true;
      this.els.keyAlertAct.onclick = null;
    }

    const others = k.used_by.filter((n) => n !== this.els.fName.value.trim());
    if (others.length > 0) {
      this.els.keyUsed.hidden = false;
      const source =
        others.length === 1
          ? "另外 {count} 台主机也在用这把密钥：{names}"
          : "另外共有 {count} 台主机也在用这把密钥：{names}";
      this.els.keyUsed.textContent = t(source, {
        count: others.length,
        names: others.join(t("、")),
      });
    }
  }

  private async chooseKey(tab: "pick" | "paste" | "gen"): Promise<void> {
    const k = await this.keyVault.choose(tab);
    if (k) {
      this.pickedKey = k;
      this.renderKeyField();
    }
    // 弹层关掉之后抽屉还得在
    this.els.drawer.hidden = false;
  }

  private async importKeyFile(): Promise<void> {
    const { open: openFileDialog } = await import("@tauri-apps/plugin-dialog");
    let picked: string | null = null;
    try {
      const r = await openFileDialog({ multiple: false, directory: false, title: t("选择私钥文件") });
      picked = typeof r === "string" ? r : null;
    } catch (e) {
      this.toast(t("打不开文件选择器：{error}", { error: String(e) }));
      return;
    }
    if (!picked) return;
    await this.importKeyPath(picked);
  }

  private async dropKeyFile(e: DragEvent): Promise<void> {
    const f = e.dataTransfer?.files?.[0];
    if (!f) return;
    // WKWebView 里拖进来的 File 没有可用的本地路径，只能读内容走文本导入
    try {
      const text = await f.text();
      const info = await invoke<KeyInfo>("import_key_text", {
        text,
        name: f.name || null,
      });
      this.pickedKey = info;
      this.renderKeyField();
      this.toast(t("已导入「{name}」", { name: info.name }));
    } catch (err) {
      this.toast(`${err}`);
    }
  }

  private async importKeyPath(path: string): Promise<void> {
    try {
      const info = await invoke<KeyInfo>("import_key_file", { path });
      this.pickedKey = info;
      this.renderKeyField();
      this.toast(t("已导入「{name}」", { name: info.name }));
    } catch (e) {
      this.toast(`${e}`);
    }
  }

  private async fixKeyPerm(k: KeyInfo): Promise<void> {
    try {
      await invoke("fix_key_permissions", { id: k.id });
      const all = await invoke<KeyInfo[]>("list_keys_cmd");
      this.pickedKey = all.find((x) => x.id === k.id) ?? this.pickedKey;
      this.renderKeyField();
      this.toast(t("权限已改成 600"));
    } catch (e) {
      this.toast(`${e}`);
    }
  }

  /** 「存」按钮：把密码直接写进钥匙串（主机得先保存过，要有 id） */
  private async savePasswordNow(): Promise<void> {
    const pw = this.els.passInput.value;
    if (!this.editingKey) {
      this.toast(t("先把主机保存一下（要拿到它的 id 才能存密码）"));
      return;
    }
    if (!pw) {
      this.toast(t("密码是空的"));
      return;
    }
    try {
      await invoke("set_host_password", { hostId: this.editingKey, password: pw });
      this.els.passInput.value = "";
      this.els.passStatus.textContent = t("已存进钥匙串");
      this.els.passStatus.className = "pass-status ok";
      this.els.passClear.hidden = false;
      this.toast(t("密码已存进钥匙串"));
      await this.refresh();
    } catch (e) {
      this.toast(`${e}`);
    }
  }

  private async clearPasswordNow(): Promise<void> {
    if (!this.editingKey) return;
    if (!window.confirm(t("清掉这台主机存在钥匙串里的密码？"))) return;
    try {
      await invoke("clear_host_password", { hostId: this.editingKey });
      this.els.passStatus.textContent = t("还没存密码，在这里填上");
      this.els.passStatus.className = "pass-status";
      this.els.passClear.hidden = true;
      this.toast(t("已清除"));
      await this.refresh();
    } catch (e) {
      this.toast(`${e}`);
    }
  }

  /** 试连一下。主机必须已经保存过 —— 后端是按 id/名字从 hosts.json 读的。 */
  private async testConnect(): Promise<void> {
    if (!this.editingKey) {
      this.testState = { kind: "save-first" };
      this.renderTestState();
      return;
    }
    this.els.testBtn.disabled = true;
    this.testState = { kind: "connecting" };
    this.renderTestState();
    try {
      const secs = await invoke<number>("test_host_connect", { key: this.editingKey });
      this.testState = { kind: "success", seconds: secs };
    } catch (e) {
      this.testState = { kind: "error", message: String(e) };
    } finally {
      this.renderTestState();
      this.els.testBtn.disabled = false;
    }
  }

  private renderTestState(): void {
    const out = this.els.testOut;
    out.className = "test-out";
    out.replaceChildren();
    if (!this.testState) return;
    if (this.testState.kind === "save-first") {
      out.textContent = t("先保存这台主机，再测试连接");
    } else if (this.testState.kind === "connecting") {
      out.textContent = t("连接中…");
    } else if (this.testState.kind === "success") {
      out.className = "test-out ok";
      out.appendChild(icon("circle-check"));
      const text = document.createElement("span");
      text.textContent = ` ${t("认证通过 · {seconds}s", { seconds: this.testState.seconds.toFixed(2) })}`;
      out.appendChild(text);
    } else {
      out.className = "test-out bad";
      out.textContent = this.testState.message;
    }
  }

  private closeDrawer(): void {
    this.els.drawer.hidden = true;
    this.editingKey = null;
  }

  private syncAuthSeg(): void {
    for (const b of Array.from(this.els.authSeg.querySelectorAll<HTMLElement>("button"))) {
      b.classList.toggle("on", b.dataset.auth === this.els.fAuth.value);
    }
  }

  private syncAuthRow(): void {
    const auth = this.els.fAuth.value;
    this.els.fKeyRow.style.display = auth === "key" ? "" : "none";
    this.els.passRow.hidden = auth !== "password";
    this.els.agentRow.hidden = auth !== "agent";
    if (auth === "agent") void this.refreshAgentStatus();
  }

  /** agent 行的实况：ssh-add -l 问一次 agent 里现在有几把钥匙 */
  private async refreshAgentStatus(): Promise<void> {
    this.els.agentStatus.textContent = t("正在看 agent…");
    try {
      const st = await invoke<{ socket: string; count: number }>("agent_status", {
        host: this.els.fHost.value.trim() || null,
      });
      this.els.agentStatus.replaceChildren();
      const kind = st.socket.toLowerCase().includes("1password") ? "1Password Agent" : "ssh-agent";
      if (st.count > 0) {
        this.els.agentStatus.className = "agent-status ok";
        this.els.agentStatus.textContent = t(
          st.count === 1
            ? "{kind} 里有 {count} 把密钥，连接时逐把试"
            : "{kind} 里共有 {count} 把密钥，连接时逐把试",
          { kind, count: st.count }
        );
      } else {
        this.els.agentStatus.className = "agent-status warn";
        this.els.agentStatus.textContent = t("{kind} 在跑但没加载密钥，先加载再连接", { kind });
      }
    } catch (e) {
      this.els.agentStatus.className = "agent-status warn";
      this.els.agentStatus.textContent = `${e}`;
    }
  }

  /** 密码行状态文案：存没存、存在哪 */
  private renderPassStatus(h: Host | null, resetInput = true): void {
    if (resetInput) this.els.passInput.value = "";
    if (h?.password_saved) {
      this.els.passStatus.textContent = t("已存进钥匙串（不回显；要换就输新的再存）");
      this.els.passStatus.className = "pass-status ok";
      this.els.passClear.hidden = false;
    } else {
      this.els.passStatus.textContent = t("还没存密码，保存前在这里填上");
      this.els.passStatus.className = "pass-status";
      this.els.passClear.hidden = true;
    }
  }

  private async save(): Promise<void> {
    const name = this.els.fName.value.trim();
    const hostname = this.els.fHost.value.trim();
    const user = this.els.fUser.value.trim();
    if (!name || !hostname || !user) {
      this.toast(t("名称、地址、用户名都要填"));
      return;
    }
    try {
      await invoke<string>("save_host", {
        key: this.editingKey,
        name,
        hostname,
        user,
        port: Number(this.els.fPort.value) || 22,
        auth: this.els.fAuth.value,
        keyId: this.pickedKey?.id ?? null,
        password: this.els.fAuth.value === "password" ? this.els.passInput.value || null : null,
        group: this.groupCombo.value() || null,
        note: this.els.fNote.value.trim() || null,
      });
      this.closeDrawer();
      await this.refresh();
      this.toast(t("已保存"));
    } catch (e) {
      this.toast(String(e));
    }
  }

  private async removeEditing(): Promise<void> {
    if (!this.editingKey) return;
    const h = this.hosts.find((x) => x.id === this.editingKey);
    const ok = window.confirm(
      t("删除主机「{name}」？这个操作不可撤销。", { name: h?.name ?? this.editingKey! })
    );
    if (!ok) return;
    try {
      await invoke("delete_host", { key: this.editingKey });
      this.closeDrawer();
      await this.refresh();
    } catch (e) {
      this.toast(String(e));
    }
  }

  // ─────────── 从 ~/.ssh/config 导入 ───────────

  private async openImport(): Promise<void> {
    let candidates: SshConfigEntry[] = [];
    try {
      candidates = await invoke<SshConfigEntry[]>("scan_ssh_config");
    } catch (e) {
      this.toast(String(e));
      return;
    }

    this.importCandidates = candidates;
    this.picked = new Set(candidates.map((c) => c.alias));
    this.renderImport();
    closeOverlays("import-panel");
    this.els.importPanel.hidden = false;
  }

  private renderImport(): void {
    const candidates = this.importCandidates;
    const list = this.els.importList;
    list.replaceChildren();

    if (candidates.length === 0) {
      const empty = document.createElement("div");
      empty.className = "side-empty";
      empty.textContent = t(
        "~/.ssh/config 里没有可导入的主机（通配符条目和已经导入过的会自动跳过）。"
      );
      list.appendChild(empty);
      this.els.importDo.disabled = true;
      this.els.importDo.textContent = t("没有可导入的");
    } else {
      this.els.importDo.disabled = false;
      for (const c of candidates) {
        const rowEl = document.createElement("label");
        rowEl.className = "import-row";

        const cb = document.createElement("input");
        cb.type = "checkbox";
        cb.checked = this.picked.has(c.alias);
        cb.addEventListener("change", () => {
          if (cb.checked) this.picked.add(c.alias);
          else this.picked.delete(c.alias);
          this.updateImportButton();
        });

        const box = document.createElement("div");
        box.className = "host-meta";
        const nm = document.createElement("div");
        nm.className = "host-name";
        nm.textContent = c.alias;
        const sub = document.createElement("div");
        sub.className = "host-sub";
        sub.textContent = `${c.user}@${c.hostname}:${c.port}`;
        box.append(nm, sub);

        rowEl.append(cb, box);
        if (c.identity) {
          const b = document.createElement("span");
          b.className = "warn";
          b.title = t("config 里有 IdentityFile");
          b.style.color = "var(--t3)";
          b.appendChild(icon("key"));
          rowEl.appendChild(b);
        }
        list.appendChild(rowEl);
      }
      this.updateImportButton();
    }
  }

  private updateImportButton(): void {
    const count = this.picked.size;
    this.els.importDo.textContent = t(
      count === 1 ? "导入选中的 {count} 台" : "导入选中的 {count} 台主机",
      { count }
    );
  }

  private async doImport(): Promise<void> {
    if (this.picked.size === 0) return;
    try {
      const n = await invoke<number>("import_ssh_config", {
        aliases: Array.from(this.picked),
      });
      this.els.importPanel.hidden = true;
      await this.refresh();
      this.toast(
        t(n === 1 ? "已导入 {count} 台主机" : "已成功导入 {count} 台主机", { count: n })
      );
    } catch (e) {
      this.toast(String(e));
    }
  }

  /** 给 ⌘K 面板用：当前主机列表 */
  allHosts(): Host[] {
    return this.hosts;
  }

  /** 给 ⌘K 面板 / ⌘N 用：打开新建主机 */
  openNew(): void {
    this.openDrawer(null);
  }

  /** 给 ⌘K 面板用：打开导入面板 */
  async openImportPanel(): Promise<void> {
    await this.openImport();
  }

  /** 标签页标题是不是一台已知主机（决定标签上用服务器图标还是终端图标） */
  isHostName(name: string): boolean {
    return this.hosts.some((h) => h.name === name);
  }

  /** 给外面用的 toast */
  notify(msg: string): void {
    this.toast(msg);
  }

  private toast(msg: string): void {
    const t = document.createElement("div");
    t.className = "side-toast";
    t.textContent = msg;
    document.body.appendChild(t);
    window.setTimeout(() => t.remove(), 2600);
  }
}
