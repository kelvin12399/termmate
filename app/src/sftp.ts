// SFTP 资源管理器面板。
//
// 这里读写的是**和 MCP 的 sftp_* 工具同一套 sftp.rs** ——
// AI 上传的文件，人在这里刷新就看到；人在这里移动的文件，AI 那边 sftp_list 也一致。
//
// 手感按 Finder 的习惯做：
//   · 单击选中、⌘ 点加选、⇧ 点选一段、双击进目录
//   · ⌘C / ⌘X / ⌘V 复制剪切粘贴，⌫ 删除，⏎ 重命名
//   · 把选中项拖到文件夹行上 = 移动进去
//   · 右键「打开方式」用系统应用打开，本地一存就自动回传

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { open as openFileDialog, save as saveFileDialog } from "@tauri-apps/plugin-dialog";
import { t } from "./i18n";

export interface SftpEntry {
  name: string;
  is_dir: boolean;
  size: number;
  size_h: string;
}

interface OpenCandidate {
  name: string;
  app: string;
  default: boolean;
}

/** 剪贴板：记住从哪台主机的哪些路径来的，以及是复制还是剪切 */
interface Clip {
  host: string;
  paths: string[];
  cut: boolean;
}

function icon(name: string, cls = "i"): SVGSVGElement {
  const svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
  svg.setAttribute("class", cls);
  const use = document.createElementNS("http://www.w3.org/2000/svg", "use");
  use.setAttribute("href", `#i-${name}`);
  svg.appendChild(use);
  return svg;
}

/** 拼远端路径，避免出现 `//` 或丢掉开头的 `/` */
function joinPath(dir: string, name: string): string {
  if (dir === "" || dir === ".") return name;
  return dir.endsWith("/") ? `${dir}${name}` : `${dir}/${name}`;
}

function parentPath(dir: string): string {
  if (dir === "/" || dir === "" || dir === ".") return dir;
  const trimmed = dir.replace(/\/+$/, "");
  const i = trimmed.lastIndexOf("/");
  if (i < 0) return ".";
  return i === 0 ? "/" : trimmed.slice(0, i);
}

export class SftpPanel {
  private root: HTMLElement;
  private titleEl: HTMLElement;
  private crumbEl: HTMLElement;
  private listEl: HTMLElement;
  private statusEl: HTMLElement;
  private menuEl: HTMLElement;

  private host = "";
  private cwd = ".";
  private entries: SftpEntry[] = [];
  /** 选中的名字（不是路径 —— 换目录就清空） */
  private sel = new Set<string>();
  private lastClicked: string | null = null;
  private clip: Clip | null = null;
  private busy = false;
  private statusSource = "";
  private statusVars: Record<string, string | number> | undefined;
  private menuContext: { x: number; y: number; entry: SftpEntry | null } | null = null;
  /** 面板 DOM 在不在。不在就所有入口安静返回，别炸别的模块 */
  private ready = false;

  constructor(private toast: (msg: string) => void) {
    this.root = document.getElementById("sftp-panel") as HTMLElement;
    this.titleEl = document.getElementById("sftp-title") as HTMLElement;
    this.crumbEl = document.getElementById("sftp-crumb") as HTMLElement;
    this.listEl = document.getElementById("sftp-list") as HTMLElement;
    this.statusEl = document.getElementById("sftp-status") as HTMLElement;
    this.menuEl = document.getElementById("sftp-menu") as HTMLElement;

    // 面板 DOM 不在就安静退场。少一个元素不该把整个应用的
    // 模块初始化链炸断（keydown 守卫、工作区渲染都在后面排队）。
    if (!this.root || !this.listEl || !this.menuEl) {
      this.ready = false;
      return;
    }
    this.ready = true;

    const on = (id: string, fn: () => void) =>
      document.getElementById(id)?.addEventListener("click", fn);
    on("sftp-close", () => this.close());
    on("sftp-up", () => void this.go(parentPath(this.cwd)));
    on("sftp-refresh", () => void this.reload());
    on("sftp-upload", () => void this.uploadDialog());
    on("sftp-mkdir", () => void this.mkdirPrompt());

    // 点空白处取消选中 / 收起菜单
    this.listEl.addEventListener("mousedown", (e) => {
      if (e.target === this.listEl) {
        this.sel.clear();
        this.renderList();
      }
    });
    document.addEventListener("mousedown", (e) => {
      if (!this.menuEl.hidden && !this.menuEl.contains(e.target as Node)) this.hideMenu();
    });

    // 空白区域右键也给菜单（粘贴/新建）
    this.listEl.addEventListener("contextmenu", (e) => {
      if (e.target !== this.listEl) return;
      e.preventDefault();
      this.showMenu(e.clientX, e.clientY, null);
    });

    // 从 Finder 直接把文件拖进面板 = 上传到当前目录
    this.root.addEventListener("dragover", (e) => {
      if (e.dataTransfer?.types.includes("Files")) {
        e.preventDefault();
        this.root.classList.add("drop");
      }
    });
    this.root.addEventListener("dragleave", (e) => {
      if (e.target === this.root) this.root.classList.remove("drop");
    });
    this.root.addEventListener("drop", (e) => {
      this.root.classList.remove("drop");
      const files = e.dataTransfer?.files;
      if (!files || files.length === 0) return;
      e.preventDefault();
      void this.uploadDropped(files);
    });

    // 后端有改动就刷新（AI 那边动了文件，人这边跟着变）
    void listen<{ host: string }>("sftp://changed", (ev) => {
      if (!this.root.hidden && ev.payload.host === this.host) void this.reload();
    });
    // 「打开方式」改完保存后的回传结果
    void listen<{ remote: string; bytes?: number; ok: boolean; error?: string }>(
      "sftp://synced",
      (ev) => {
        const p = ev.payload;
        const name = p.remote.split("/").pop() ?? p.remote;
        if (p.ok) this.toast(t("已回传「{name}」", { name }));
        else {
          this.toast(t("回传「{name}」失败：{error}", { name, error: p.error ?? "" }));
        }
      }
    );

    window.addEventListener("dq:locale", () => {
      this.renderStatus();
      if (!this.root.hidden) this.renderList();
      const menu = this.menuContext;
      if (menu && !this.menuEl.hidden) this.showMenu(menu.x, menu.y, menu.entry);
    });
  }

  isOpen(): boolean {
    return this.ready && !this.root.hidden;
  }

  async open(host: string): Promise<void> {
    if (!this.ready) return;
    this.host = host;
    this.cwd = ".";
    this.sel.clear();
    this.titleEl.textContent = host;
    this.root.hidden = false;
    await this.reload();
  }

  close(): void {
    this.hideMenu();
    this.root.hidden = true;
  }

  /** 面板自己消化的键盘动作；返回 true 表示已处理，外层别再转发给终端 */
  handleKey(e: KeyboardEvent): boolean {
    if (!this.ready || this.root.hidden) return false;
    const meta = e.metaKey || e.ctrlKey;

    if (e.key === "Escape") {
      if (!this.menuEl.hidden) this.hideMenu();
      else this.close();
      return true;
    }
    if (meta && e.key.toLowerCase() === "c") {
      this.copySel(false);
      return true;
    }
    if (meta && e.key.toLowerCase() === "x") {
      this.copySel(true);
      return true;
    }
    if (meta && e.key.toLowerCase() === "v") {
      void this.paste();
      return true;
    }
    if (meta && e.key.toLowerCase() === "a") {
      this.sel = new Set(this.entries.map((x) => x.name));
      this.renderList();
      return true;
    }
    if (e.key === "Backspace" || e.key === "Delete") {
      void this.removeSel();
      return true;
    }
    if (e.key === "Enter") {
      const one = this.onlySelected();
      if (one) void this.renamePrompt(one);
      return true;
    }
    if (e.key === "ArrowUp" || e.key === "ArrowDown") {
      this.moveCursor(e.key === "ArrowDown" ? 1 : -1, e.shiftKey);
      return true;
    }
    return false;
  }

  // ─────────── 读目录 / 渲染 ───────────

  private async reload(): Promise<void> {
    await this.go(this.cwd);
  }

  private async go(path: string): Promise<void> {
    if (this.busy) return;
    this.busy = true;
    this.setStatus("读取中…");
    try {
      const list = await invoke<SftpEntry[]>("sftp_ls", { host: this.host, path });
      this.cwd = path;
      this.entries = list;
      this.sel.clear();
      this.lastClicked = null;
      this.renderCrumb();
      this.renderList();
      const dirs = list.filter((x) => x.is_dir).length;
      this.setStatus("{dirs} 个文件夹，{files} 个文件", {
        dirs,
        files: list.length - dirs,
      });
    } catch (e) {
      this.setStatus(`${e}`);
      this.listEl.replaceChildren();
      const err = document.createElement("div");
      err.className = "sftp-empty";
      err.textContent = `${e}`;
      this.listEl.appendChild(err);
    } finally {
      this.busy = false;
    }
  }

  private renderCrumb(): void {
    this.crumbEl.replaceChildren();
    const parts = this.cwd === "." ? ["~"] : this.cwd.split("/").filter((x) => x !== "");
    const isAbs = this.cwd.startsWith("/");

    const mk = (label: string, target: string, last: boolean) => {
      const b = document.createElement("button");
      b.className = "sftp-crumb-item" + (last ? " on" : "");
      b.textContent = label;
      if (!last) b.addEventListener("click", () => void this.go(target));
      this.crumbEl.appendChild(b);
      if (!last) {
        const sep = document.createElement("span");
        sep.className = "sftp-crumb-sep";
        sep.textContent = "/";
        this.crumbEl.appendChild(sep);
      }
    };

    if (this.cwd === ".") {
      mk("~", ".", true);
      return;
    }
    if (isAbs) mk("/", "/", parts.length === 0);
    let acc = isAbs ? "" : ".";
    parts.forEach((p, i) => {
      acc = isAbs ? `${acc}/${p}` : joinPath(acc, p);
      mk(p, acc, i === parts.length - 1);
    });
  }

  private renderList(): void {
    this.listEl.replaceChildren();
    if (this.entries.length === 0) {
      const empty = document.createElement("div");
      empty.className = "sftp-empty";
      empty.textContent = t("这个目录是空的。可以把文件从 Finder 拖进来。");
      this.listEl.appendChild(empty);
      return;
    }
    for (const e of this.entries) this.listEl.appendChild(this.row(e));
  }

  private row(e: SftpEntry): HTMLElement {
    const row = document.createElement("div");
    row.className = "sftp-row" + (this.sel.has(e.name) ? " on" : "");
    row.draggable = true;
    row.dataset.name = e.name;

    const ic = document.createElement("span");
    ic.className = "sftp-ico";
    ic.appendChild(icon(e.is_dir ? "folder" : "hard-drive"));
    row.appendChild(ic);

    const nm = document.createElement("span");
    nm.className = "sftp-name";
    nm.textContent = e.name;
    row.appendChild(nm);

    const sz = document.createElement("span");
    sz.className = "sftp-size";
    sz.textContent = e.size_h;
    row.appendChild(sz);

    row.addEventListener("click", (ev) => {
      ev.stopPropagation();
      this.clickSelect(e.name, ev);
    });
    row.addEventListener("dblclick", (ev) => {
      ev.stopPropagation();
      if (e.is_dir) void this.go(joinPath(this.cwd, e.name));
      else void this.openWith(e.name, "");
    });
    row.addEventListener("contextmenu", (ev) => {
      ev.preventDefault();
      ev.stopPropagation();
      // 右键没选中的行时，先把它选上（Finder 的行为）
      if (!this.sel.has(e.name)) {
        this.sel = new Set([e.name]);
        this.renderList();
      }
      this.showMenu(ev.clientX, ev.clientY, e);
    });

    // 拖拽：把选中项拖到文件夹行上 = 移动进去
    row.addEventListener("dragstart", (ev) => {
      if (!this.sel.has(e.name)) {
        this.sel = new Set([e.name]);
        this.renderList();
      }
      ev.dataTransfer?.setData(
        "application/x-daiqiao-sftp",
        JSON.stringify({ host: this.host, names: [...this.sel] })
      );
      if (ev.dataTransfer) ev.dataTransfer.effectAllowed = "move";
    });
    if (e.is_dir) {
      row.addEventListener("dragover", (ev) => {
        if (!ev.dataTransfer?.types.includes("application/x-daiqiao-sftp")) return;
        ev.preventDefault();
        ev.stopPropagation();
        row.classList.add("drop");
      });
      row.addEventListener("dragleave", () => row.classList.remove("drop"));
      row.addEventListener("drop", (ev) => {
        row.classList.remove("drop");
        const raw = ev.dataTransfer?.getData("application/x-daiqiao-sftp");
        if (!raw) return;
        ev.preventDefault();
        ev.stopPropagation();
        try {
          const { host, names } = JSON.parse(raw) as { host: string; names: string[] };
          if (host !== this.host) {
            this.toast(t("跨主机拖拽还没做"));
            return;
          }
          void this.moveInto(names, joinPath(this.cwd, e.name));
        } catch {
          /* 拖进来的不是我们的数据，忽略 */
        }
      });
    }

    return row;
  }

  private clickSelect(name: string, ev: MouseEvent): void {
    if (ev.metaKey || ev.ctrlKey) {
      if (this.sel.has(name)) this.sel.delete(name);
      else this.sel.add(name);
    } else if (ev.shiftKey && this.lastClicked) {
      const names = this.entries.map((x) => x.name);
      const a = names.indexOf(this.lastClicked);
      const b = names.indexOf(name);
      if (a >= 0 && b >= 0) {
        const [lo, hi] = a < b ? [a, b] : [b, a];
        for (let i = lo; i <= hi; i++) this.sel.add(names[i]);
      }
    } else {
      this.sel = new Set([name]);
    }
    this.lastClicked = name;
    this.renderList();
  }

  private moveCursor(delta: number, extend: boolean): void {
    if (this.entries.length === 0) return;
    const names = this.entries.map((x) => x.name);
    const cur = this.lastClicked ? names.indexOf(this.lastClicked) : -1;
    const next = Math.max(0, Math.min(names.length - 1, cur + delta));
    const name = names[next];
    if (extend) this.sel.add(name);
    else this.sel = new Set([name]);
    this.lastClicked = name;
    this.renderList();
    this.listEl
      .querySelector<HTMLElement>(`.sftp-row[data-name="${CSS.escape(name)}"]`)
      ?.scrollIntoView({ block: "nearest" });
  }

  private onlySelected(): SftpEntry | null {
    if (this.sel.size !== 1) return null;
    const name = [...this.sel][0];
    return this.entries.find((x) => x.name === name) ?? null;
  }

  private setStatus(text: string, vars?: Record<string, string | number>): void {
    this.statusSource = text;
    this.statusVars = vars;
    this.renderStatus();
  }

  private renderStatus(): void {
    this.statusEl.textContent = t(this.statusSource, this.statusVars);
  }

  // ─────────── 右键菜单 ───────────

  private showMenu(x: number, y: number, e: SftpEntry | null): void {
    this.menuContext = { x, y, entry: e };
    this.menuEl.replaceChildren();

    const item = (label: string, ic: string, fn: () => void, danger = false) => {
      const b = document.createElement("button");
      if (danger) b.className = "danger";
      b.appendChild(icon(ic));
      const s = document.createElement("span");
      s.textContent = label;
      b.appendChild(s);
      b.addEventListener("click", () => {
        this.hideMenu();
        fn();
      });
      this.menuEl.appendChild(b);
    };
    const sep = () => {
      const d = document.createElement("div");
      d.className = "ctx-sep";
      this.menuEl.appendChild(d);
    };

    if (e) {
      if (e.is_dir) {
        item(t("打开"), "folder", () => void this.go(joinPath(this.cwd, e.name)));
      } else {
        item(t("打开方式…"), "pencil", () => void this.openWithPicker(e.name));
        item(t("下载到…"), "download", () => void this.downloadOne(e.name));
      }
      sep();
      item(t("复制"), "copy", () => this.copySel(false));
      item(t("剪切"), "columns", () => this.copySel(true));
      item(t("重命名"), "pencil", () => void this.renamePrompt(e));
      sep();
      item(this.sel.size > 1 ? t("删除 {count} 项", { count: this.sel.size }) : t("删除"), "trash", () =>
        void this.removeSel(), true
      );
    } else {
      if (this.clip && this.clip.paths.length > 0) {
        const count = this.clip.paths.length;
        item(t(count === 1 ? "粘贴 {count} 项" : "粘贴这 {count} 项", { count }), "clipboard", () =>
          void this.paste()
        );
        sep();
      }
      item(t("上传文件…"), "plus", () => void this.uploadDialog());
      item(t("新建文件夹"), "folder", () => void this.mkdirPrompt());
      item(t("刷新"), "search", () => void this.reload());
    }

    this.menuEl.hidden = false;
    // 贴边时翻转，别跑到窗口外面去
    const r = this.menuEl.getBoundingClientRect();
    const left = Math.min(x, window.innerWidth - r.width - 8);
    const top = Math.min(y, window.innerHeight - r.height - 8);
    this.menuEl.style.left = `${Math.max(8, left)}px`;
    this.menuEl.style.top = `${Math.max(8, top)}px`;
  }

  private hideMenu(): void {
    this.menuEl.hidden = true;
    this.menuContext = null;
  }

  // ─────────── 文件操作 ───────────

  private copySel(cut: boolean): void {
    if (this.sel.size === 0) return;
    this.clip = {
      host: this.host,
      paths: [...this.sel].map((n) => joinPath(this.cwd, n)),
      cut,
    };
    const count = this.clip.paths.length;
    if (cut) {
      this.toast(t(count === 1 ? "已剪切 {count} 项" : "已剪切这 {count} 项", { count }));
    } else {
      this.toast(t(count === 1 ? "已复制 {count} 项" : "已复制这 {count} 项", { count }));
    }
  }

  private async paste(): Promise<void> {
    const clip = this.clip;
    if (!clip || clip.paths.length === 0) return;
    if (clip.host !== this.host) {
      this.toast(t("跨主机复制还没做"));
      return;
    }
    this.setStatus(clip.cut ? "移动中…" : "复制中…");
    let done = 0;
    for (const from of clip.paths) {
      const name = from.split("/").pop() ?? from;
      let to = joinPath(this.cwd, name);
      if (to === from) {
        // 同目录粘贴：给个副本名，别覆盖原文件
        if (clip.cut) continue;
        to = joinPath(this.cwd, this.copyName(name));
      }
      try {
        if (clip.cut) await invoke("sftp_mv", { host: this.host, from, to });
        else await invoke("sftp_copy", { host: this.host, from, to });
        done++;
      } catch (e) {
        this.toast(t("{name}：{error}", { name, error: `${e}` }));
      }
    }
    if (clip.cut) this.clip = null;
    if (clip.cut) {
      this.toast(t(done === 1 ? "移动了 {count} 项" : "共移动了 {count} 项", { count: done }));
    } else {
      this.toast(t(done === 1 ? "复制了 {count} 项" : "共复制了 {count} 项", { count: done }));
    }
    await this.reload();
  }

  /** a.txt → a 副本.txt（已存在就继续加） */
  private copyName(name: string): string {
    const dot = name.lastIndexOf(".");
    const [base, ext] = dot > 0 ? [name.slice(0, dot), name.slice(dot)] : [name, ""];
    let candidate = `${base} 副本${ext}`;
    let n = 2;
    const taken = new Set(this.entries.map((x) => x.name));
    while (taken.has(candidate)) candidate = `${base} 副本 ${n++}${ext}`;
    return candidate;
  }

  private async moveInto(names: string[], destDir: string): Promise<void> {
    this.setStatus("移动中…");
    let done = 0;
    for (const n of names) {
      const from = joinPath(this.cwd, n);
      if (from === destDir) continue; // 别把文件夹拖进自己
      try {
        await invoke("sftp_mv", { host: this.host, from, to: joinPath(destDir, n) });
        done++;
      } catch (e) {
        this.toast(`${n}：${e}`);
      }
    }
    this.toast(t(done === 1 ? "移动了 {count} 项" : "共移动了 {count} 项", { count: done }));
    await this.reload();
  }

  private async removeSel(): Promise<void> {
    if (this.sel.size === 0) return;
    const names = [...this.sel];
    if (
      !window.confirm(
        t(
          names.length === 1
            ? "删除「{name}」？远端删除没有回收站，删了就没了。"
            : "删除这 {count} 项？远端删除没有回收站，删了就没了。",
          { name: names[0], count: names.length }
        )
      )
    )
      return;
    this.setStatus("删除中…");
    let done = 0;
    for (const n of names) {
      try {
        await invoke("sftp_rm", { host: this.host, path: joinPath(this.cwd, n) });
        done++;
      } catch (e) {
        this.toast(`${n}：${e}`);
      }
    }
    this.toast(t(done === 1 ? "删除了 {count} 项" : "共删除了 {count} 项", { count: done }));
    await this.reload();
  }

  private async renamePrompt(e: SftpEntry): Promise<void> {
    const next = window.prompt(t("新名字"), e.name);
    if (next === null) return;
    const name = next.trim();
    if (!name || name === e.name) return;
    try {
      await invoke("sftp_mv", {
        host: this.host,
        from: joinPath(this.cwd, e.name),
        to: joinPath(this.cwd, name),
      });
      await this.reload();
    } catch (err) {
      this.toast(`${err}`);
    }
  }

  private async mkdirPrompt(): Promise<void> {
    const name = window.prompt(t("文件夹名字"), t("新建文件夹"));
    if (name === null) return;
    const n = name.trim();
    if (!n) return;
    try {
      await invoke("sftp_mkdir_cmd", { host: this.host, path: joinPath(this.cwd, n) });
      await this.reload();
    } catch (e) {
      this.toast(`${e}`);
    }
  }

  private async uploadDialog(): Promise<void> {
    let picked: string[] = [];
    try {
      const r = await openFileDialog({
        multiple: true,
        directory: false,
        title: t("选择要上传的文件"),
      });
      picked = Array.isArray(r) ? r : typeof r === "string" ? [r] : [];
    } catch (e) {
      this.toast(`${e}`);
      return;
    }
    if (picked.length === 0) return;
    await this.uploadPaths(picked);
  }

  private async uploadPaths(paths: string[]): Promise<void> {
    this.setStatus("上传中…");
    let done = 0;
    for (const local of paths) {
      const name = local.split("/").pop() ?? local;
      try {
        await invoke("sftp_put", {
          host: this.host,
          local,
          remote: joinPath(this.cwd, name),
        });
        done++;
      } catch (e) {
        this.toast(t("{name}：{error}", { name, error: `${e}` }));
      }
    }
    this.toast(t(done === 1 ? "上传了 {count} 个文件" : "共上传了 {count} 个文件", { count: done }));
    await this.reload();
  }

  /** 从 Finder 拖进来的文件。WKWebView 给不到本地路径，只能读内容转存再上传。 */
  private async uploadDropped(files: FileList): Promise<void> {
    this.setStatus("上传中…");
    let done = 0;
    for (const f of Array.from(files)) {
      try {
        const buf = new Uint8Array(await f.arrayBuffer());
        await invoke("sftp_put_bytes", {
          host: this.host,
          remote: joinPath(this.cwd, f.name),
          bytes: Array.from(buf),
        });
        done++;
      } catch (e) {
        this.toast(`${f.name}：${e}`);
      }
    }
    this.toast(t(done === 1 ? "上传了 {count} 个文件" : "共上传了 {count} 个文件", { count: done }));
    await this.reload();
  }

  private async downloadOne(name: string): Promise<void> {
    let dest: string | null = null;
    try {
      dest = await saveFileDialog({ defaultPath: name, title: t("下载到") });
    } catch (e) {
      this.toast(`${e}`);
      return;
    }
    if (!dest) return;
    this.setStatus("下载中…");
    try {
      const n = await invoke<number>("sftp_get", {
        host: this.host,
        remote: joinPath(this.cwd, name),
        local: dest,
      });
      this.toast(t("已下载 {name}（{bytes} 字节）", { name, bytes: n }));
      this.setStatus("");
    } catch (e) {
      this.toast(`${e}`);
    }
  }

  /** 「打开方式」：先问后端这个文件能用哪些 App 打开，再让用户挑 */
  private async openWithPicker(name: string): Promise<void> {
    let apps: OpenCandidate[] = [];
    try {
      apps = await invoke<OpenCandidate[]>("sftp_open_candidates", { name });
    } catch {
      apps = [{ name: t("默认应用"), app: "", default: true }];
    }
    // 候选只有「默认应用」时不弹选择，直接开
    if (apps.length <= 1) {
      await this.openWith(name, "");
      return;
    }
    const lines = apps.map((a, i) => `${i + 1}. ${a.name}`).join("\n");
    const pick = window.prompt(
      t("用哪个应用打开「{name}」？\n{apps}\n\n输入序号：", { name, apps: lines }),
      "1"
    );
    if (pick === null) return;
    const idx = parseInt(pick.trim(), 10) - 1;
    const chosen = apps[idx] ?? apps[0];
    await this.openWith(name, chosen.app);
  }

  private async openWith(name: string, app: string): Promise<void> {
    this.setStatus("下载并打开…");
    try {
      await invoke<string>("sftp_open_with", {
        host: this.host,
        remote: joinPath(this.cwd, name),
        application: app || null,
      });
      this.toast(t("已打开 {name}，在本地保存后会自动回传", { name }));
      this.setStatus("");
    } catch (e) {
      this.toast(`${e}`);
    }
  }
}
