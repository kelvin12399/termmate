// 密钥库弹层：选择 / 粘贴导入 / 新建 三页。
//
// 这里读写的是**和 MCP 的 key_* 工具同一份密钥库**。人在界面里导入一把密钥，
// AI 那边 key_list 立刻就看得到；AI 生成的密钥，界面收到广播自动刷新。
//
// 私钥内容只在「粘贴导入」那一下经过前端，落盘之后界面再也拿不到它 ——
// 列表里只有算法、指纹、有没有口令这些元信息。

import { invoke } from "@tauri-apps/api/core";
import { open as openFileDialog } from "@tauri-apps/plugin-dialog";
import { t } from "./i18n";

export interface KeyInfo {
  id: string;
  name: string;
  path: string;
  /** "ssh_dir" | "vault" */
  source: string;
  algo: string;
  fingerprint: string;
  encrypted: boolean;
  comment: string | null;
  perm_ok: boolean;
  public_key: string | null;
  used_by: string[];
}

type Tab = "pick" | "paste" | "gen";

function icon(name: string, cls = "i"): SVGSVGElement {
  const svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
  svg.setAttribute("class", cls);
  const use = document.createElementNS("http://www.w3.org/2000/svg", "use");
  use.setAttribute("href", `#i-${name}`);
  svg.appendChild(use);
  return svg;
}

export class KeyVault {
  private root: HTMLElement;
  private tabsEl: HTMLElement;
  private pickEl: HTMLElement;
  private pasteEl: HTMLElement;
  private genEl: HTMLElement;
  private noteEl: HTMLElement;
  private okBtn: HTMLButtonElement;
  private fileBtn: HTMLButtonElement;

  private textEl: HTMLTextAreaElement;
  private parsedEl: HTMLElement;
  private pasteNameEl: HTMLInputElement;

  private genForm: HTMLElement;
  private genDone: HTMLElement;
  private genNameEl: HTMLInputElement;
  private genAlgoEl: HTMLElement;
  private genFpEl: HTMLElement;
  private genPubEl: HTMLElement;

  private tab: Tab = "pick";
  private keys: KeyInfo[] = [];
  private selected: string | null = null;
  private generated: KeyInfo | null = null;
  private generating = false;
  private resolve: ((k: KeyInfo | null) => void) | null = null;
  /** 弹层 DOM 在不在。不在就所有入口安静返回 */
  private ready = false;

  constructor(private toast: (msg: string) => void) {
    this.root = document.getElementById("keyvault") as HTMLElement;
    this.tabsEl = document.getElementById("kv-tabs") as HTMLElement;
    this.pickEl = document.getElementById("kv-pick") as HTMLElement;
    this.pasteEl = document.getElementById("kv-paste") as HTMLElement;
    this.genEl = document.getElementById("kv-gen") as HTMLElement;
    this.noteEl = document.getElementById("kv-note") as HTMLElement;
    this.okBtn = document.getElementById("kv-ok") as HTMLButtonElement;
    this.fileBtn = document.getElementById("kv-file") as HTMLButtonElement;

    this.textEl = document.getElementById("kv-text") as HTMLTextAreaElement;
    this.parsedEl = document.getElementById("kv-parsed") as HTMLElement;
    this.pasteNameEl = document.getElementById("kv-paste-name") as HTMLInputElement;

    this.genForm = document.getElementById("kv-gen-form") as HTMLElement;
    this.genDone = document.getElementById("kv-gen-done") as HTMLElement;
    this.genNameEl = document.getElementById("kv-gen-name") as HTMLInputElement;
    this.genAlgoEl = document.getElementById("kv-gen-algo") as HTMLElement;
    this.genFpEl = document.getElementById("kv-gen-fp") as HTMLElement;
    this.genPubEl = document.getElementById("kv-gen-pub") as HTMLElement;

    // 弹层 DOM 不在就安静退场 —— 少个元素不该把整个应用的初始化链炸断
    if (!this.root || !this.tabsEl || !this.genAlgoEl || !this.pickEl) {
      this.ready = false;
      return;
    }
    this.ready = true;

    for (const b of Array.from(this.tabsEl.querySelectorAll<HTMLElement>("button"))) {
      b.addEventListener("click", () => this.switchTab((b.dataset.tab as Tab) ?? "pick"));
    }
    for (const b of Array.from(this.genAlgoEl.querySelectorAll<HTMLElement>("button"))) {
      b.addEventListener("click", () => {
        for (const x of Array.from(this.genAlgoEl.querySelectorAll<HTMLElement>("button"))) {
          x.classList.toggle("on", x === b);
        }
      });
    }

    (document.getElementById("kv-close") as HTMLElement).addEventListener("click", () =>
      this.close(null)
    );
    this.root.addEventListener("mousedown", (e) => {
      if (e.target === this.root) this.close(null);
    });

    // 贴进来就地解析：类型、有没有口令、指纹，确认了才落盘
    this.textEl.addEventListener("input", () => this.previewPasted());

    (document.getElementById("kv-gen-copy") as HTMLElement).addEventListener("click", () => {
      const pub = this.generated?.public_key;
      if (!pub) return;
      void navigator.clipboard
        .writeText(pub)
        .then(() => this.toast(t("公钥已复制")))
        .catch(() => this.toast(pub));
    });

    this.fileBtn.addEventListener("click", () => void this.pickFile());
    this.okBtn.addEventListener("click", () => void this.confirm());

    window.addEventListener("dq:locale", () => {
      this.switchTab(this.tab);
      if (this.tab === "paste") this.previewPasted();
      if (this.tab === "gen" && this.generated) this.renderGenerated(this.generated);
    });
  }

  isOpen(): boolean {
    return this.ready && !this.root.hidden;
  }

  /** 打开密钥库，等用户选/导入/生成一把密钥。关掉或取消返回 null。 */
  async choose(tab: Tab = "pick"): Promise<KeyInfo | null> {
    if (!this.ready) return null;
    this.selected = null;
    this.generated = null;
    this.textEl.value = "";
    this.pasteNameEl.value = "";
    this.genNameEl.value = "";
    this.parsedEl.hidden = true;
    this.genDone.hidden = true;
    this.genForm.hidden = false;
    this.root.hidden = false;
    this.switchTab(tab);
    await this.reload();
    return new Promise((resolve) => {
      this.resolve = resolve;
    });
  }

  close(result: KeyInfo | null): void {
    this.root.hidden = true;
    const r = this.resolve;
    this.resolve = null;
    if (r) r(result);
  }

  async reload(): Promise<void> {
    try {
      this.keys = await invoke<KeyInfo[]>("list_keys_cmd");
    } catch (e) {
      this.keys = [];
      this.toast(`${e}`);
    }
    if (this.tab === "pick") this.renderPick();
  }

  private switchTab(tab: Tab): void {
    this.tab = tab;
    for (const b of Array.from(this.tabsEl.querySelectorAll<HTMLElement>("button"))) {
      b.classList.toggle("on", b.dataset.tab === tab);
    }
    this.pickEl.hidden = tab !== "pick";
    this.pasteEl.hidden = tab !== "paste";
    this.genEl.hidden = tab !== "gen";
    this.fileBtn.hidden = tab !== "pick";

    if (tab === "pick") {
      this.okBtn.textContent = t("用这把");
      this.noteEl.textContent = t("AI 能列密钥、能指定用哪把，拿不到私钥内容。");
      this.renderPick();
    } else if (tab === "paste") {
      this.okBtn.textContent = t("导入并使用");
      this.noteEl.textContent = t("私钥文本只在导入这一下经过界面，落盘后就只剩元信息。");
    } else {
      this.okBtn.textContent = t(this.generated ? "用这把" : "生成");
      this.noteEl.textContent = t(
        this.generated ? "私钥已存进代敲库，权限 600。" : "走系统 ssh-keygen 生成。"
      );
    }
    this.syncOk();
  }

  private syncOk(): void {
    if (this.tab === "pick") this.okBtn.disabled = this.selected === null;
    else if (this.tab === "paste") this.okBtn.disabled = this.textEl.value.trim() === "";
    else this.okBtn.disabled = this.generating;
  }

  private renderPick(): void {
    this.pickEl.replaceChildren();
    if (this.keys.length === 0) {
      const empty = document.createElement("div");
      empty.className = "kv-empty";
      empty.textContent = t(
        "~/.ssh 里没扫到私钥，代敲库里也还没有。\n用上面的「新建」生成一对，或「粘贴导入」贴一把进来。"
      );
      empty.style.whiteSpace = "pre-line";
      this.pickEl.appendChild(empty);
      this.syncOk();
      return;
    }

    const groups: [string, string, string, KeyInfo[]][] = [
      [
        "ssh_dir",
        "search",
        t("只记位置，不复制"),
        this.keys.filter((k) => k.source === "ssh_dir"),
      ],
      ["vault", "key", "", this.keys.filter((k) => k.source === "vault")],
    ];

    for (const [source, ic, tail, items] of groups) {
      if (items.length === 0) continue;
      const sec = document.createElement("div");
      sec.className = "kv-sec";
      sec.appendChild(icon(ic));
      const label = document.createElement("span");
      label.textContent =
        source === "ssh_dir"
          ? t(items.length === 1 ? "在 ~/.ssh 里扫到 {count} 把" : "在 ~/.ssh 里共扫到 {count} 把", {
              count: items.length,
            })
          : t("代敲库（~/.daiqiao/keys，目录 700）");
      sec.appendChild(label);
      const ln = document.createElement("span");
      ln.className = "ln";
      sec.appendChild(ln);
      const tailEl = document.createElement("span");
      tailEl.textContent =
        tail || t(items.length === 1 ? "{count} 把" : "共 {count} 把", { count: items.length });
      sec.appendChild(tailEl);
      this.pickEl.appendChild(sec);

      for (const k of items) this.pickEl.appendChild(this.row(k));
    }
    this.syncOk();
  }

  private row(k: KeyInfo): HTMLElement {
    const row = document.createElement("div");
    row.className = "kv-row" + (k.id === this.selected ? " on" : "");

    const check = document.createElement("span");
    check.className = "kv-check";
    check.appendChild(icon("circle-check"));
    row.appendChild(check);

    const box = document.createElement("div");
    box.className = "key-ico";
    box.appendChild(icon(k.encrypted ? "lock" : k.perm_ok ? "key" : "circle-alert"));
    row.appendChild(box);

    const info = document.createElement("div");
    info.className = "kv-info";
    const name = document.createElement("div");
    name.className = "kv-name";
    const nm = document.createElement("span");
    nm.textContent = k.name;
    name.appendChild(nm);
    const badge = document.createElement("span");
    badge.className = "key-badge" + (k.source === "vault" ? " vault" : "");
    badge.textContent = k.source === "vault" ? t("代敲库") : "~/.ssh";
    name.appendChild(badge);
    if (k.encrypted) {
      const b = document.createElement("span");
      b.className = "key-badge warn";
      b.textContent = t("有口令");
      name.appendChild(b);
    }
    if (!k.perm_ok) {
      const b = document.createElement("span");
      b.className = "key-badge warn";
      b.textContent = t("权限太松");
      name.appendChild(b);
    }
    if (k.used_by.length > 0) {
      const b = document.createElement("span");
      b.className = "key-badge";
      b.textContent = t(k.used_by.length === 1 ? "{count} 台在用" : "共 {count} 台在用", {
        count: k.used_by.length,
      });
      b.title = k.used_by.join(t("、"));
      name.appendChild(b);
    }
    info.appendChild(name);

    const sub = document.createElement("div");
    sub.className = "kv-sub";
    const a = document.createElement("span");
    a.textContent = k.algo;
    const f = document.createElement("span");
    f.textContent = k.fingerprint;
    sub.append(a, f);
    info.appendChild(sub);
    row.appendChild(info);

    const acts = document.createElement("div");
    acts.className = "kv-acts";

    if (!k.perm_ok) {
      acts.appendChild(
        this.act("shield", t("改成 600"), async () => {
          try {
            await invoke("fix_key_permissions", { id: k.id });
            this.toast(t("已把「{name}」权限改成 600", { name: k.name }));
            await this.reload();
          } catch (e) {
            this.toast(`${e}`);
          }
        })
      );
    }
    acts.appendChild(
      this.act("copy", t("复制公钥"), async () => {
        if (!k.public_key) {
          this.toast(t("这把密钥读不出公钥"));
          return;
        }
        try {
          await navigator.clipboard.writeText(k.public_key);
          this.toast(t("公钥已复制"));
        } catch {
          this.toast(k.public_key);
        }
      })
    );
    acts.appendChild(
      this.act("pencil", t("改名"), async () => {
        const next = window.prompt(t("密钥名称"), k.name);
        if (next === null) return;
        const name = next.trim();
        if (!name || name === k.name) return;
        try {
          await invoke("rename_key_cmd", { id: k.id, name });
          await this.reload();
        } catch (e) {
          this.toast(`${e}`);
        }
      })
    );
    if (k.source === "vault") {
      const del = this.act("trash", t("从代敲库删除"), async () => {
        const used =
          k.used_by.length > 0
            ? t(
                k.used_by.length === 1
                  ? "\n还有 {count} 台主机在用：{names}"
                  : "\n共有 {count} 台主机在用：{names}",
                { count: k.used_by.length, names: k.used_by.join(t("、")) }
              )
            : "";
        if (
          !window.confirm(t("删掉密钥「{name}」？私钥文件会一起删掉。{used}", { name: k.name, used }))
        )
          return;
        try {
          await invoke("remove_key_cmd", { id: k.id });
          if (this.selected === k.id) this.selected = null;
          await this.reload();
        } catch (e) {
          this.toast(`${e}`);
        }
      });
      del.classList.add("danger");
      acts.appendChild(del);
    }
    row.appendChild(acts);

    row.addEventListener("click", () => {
      this.selected = k.id;
      for (const el of Array.from(this.pickEl.querySelectorAll(".kv-row"))) {
        el.classList.toggle("on", el === row);
      }
      this.syncOk();
    });
    row.addEventListener("dblclick", () => {
      this.selected = k.id;
      void this.confirm();
    });

    return row;
  }

  private act(ic: string, title: string, fn: () => void | Promise<void>): HTMLButtonElement {
    const b = document.createElement("button");
    b.title = title;
    b.appendChild(icon(ic));
    b.addEventListener("click", (e) => {
      e.stopPropagation();
      void fn();
    });
    return b;
  }

  /** 贴进来的文本就地看一眼：是不是私钥、什么算法、有没有口令 */
  private previewPasted(): void {
    const text = this.textEl.value.trim();
    this.syncOk();
    if (!text) {
      this.parsedEl.hidden = true;
      return;
    }
    this.parsedEl.hidden = false;
    this.parsedEl.replaceChildren();

    // 真正的解析在 Rust 侧（导入时会再校验一次）。这里只做格式判断，
    // 不把私钥内容送去任何别的地方。
    const looksKey = text.startsWith("-----BEGIN") && text.includes("PRIVATE KEY");
    const encrypted = /Proc-Type:.*ENCRYPTED/i.test(text) || text.includes("bcrypt");
    this.parsedEl.classList.toggle("bad", !looksKey);
    this.parsedEl.appendChild(icon(looksKey ? "circle-check" : "circle-alert"));
    const msg = document.createElement("span");
    if (looksKey) {
      const kind = /OPENSSH PRIVATE KEY/.test(text)
        ? t("OpenSSH 私钥")
        : /RSA PRIVATE KEY/.test(text)
          ? t("PEM 格式 RSA 私钥")
          : t("私钥");
      msg.textContent = t(
        encrypted
          ? "看着像{kind}（带口令）。点「导入并使用」后由后端解析出算法和指纹。"
          : "看着像{kind}。点「导入并使用」后由后端解析出算法和指纹。",
        { kind }
      );
    } else {
      msg.textContent = t("这段文本不像私钥（应该以 -----BEGIN 开头）。");
    }
    this.parsedEl.appendChild(msg);
  }

  /** 原生文件选择器：挑一个私钥文件导入代敲库 */
  private async pickFile(): Promise<void> {
    let picked: string | null = null;
    try {
      const r = await openFileDialog({
        multiple: false,
        directory: false,
        title: t("选择私钥文件"),
      });
      picked = typeof r === "string" ? r : null;
    } catch (e) {
      this.toast(t("打不开文件选择器：{error}", { error: `${e}` }));
      return;
    }
    if (!picked) return;

    try {
      const info = await invoke<KeyInfo>("import_key_file", { path: picked });
      this.toast(t("已导入「{name}」", { name: info.name }));
      this.close(info);
    } catch (e) {
      this.toast(`${e}`);
    }
  }

  private async confirm(): Promise<void> {
    if (this.tab === "pick") {
      const k = this.keys.find((x) => x.id === this.selected);
      if (k) this.close(k);
      return;
    }

    if (this.tab === "paste") {
      const text = this.textEl.value.trim();
      if (!text) return;
      const name = this.pasteNameEl.value.trim();
      try {
        const info = await invoke<KeyInfo>("import_key_text", {
          text,
          name: name || null,
        });
        this.textEl.value = "";
        this.toast(t("已导入「{name}」 {algo}", { name: info.name, algo: info.algo }));
        this.close(info);
      } catch (e) {
        this.toast(`${e}`);
      }
      return;
    }

    // 新建：第一下生成，生成完按钮变成「用这把」
    if (this.generated) {
      this.close(this.generated);
      return;
    }
    const algo =
      this.genAlgoEl.querySelector<HTMLElement>("button.on")?.dataset.algo ?? "ed25519";
    const name = this.genNameEl.value.trim();
    this.generating = true;
    this.syncOk();
    try {
      const info = await invoke<KeyInfo>("generate_key_cmd", {
        name: name || null,
        algo,
        comment: "daiqiao",
      });
      this.generated = info;
      this.renderGenerated(info);
      this.genDone.hidden = false;
      this.genForm.hidden = true;
      this.okBtn.textContent = t("用这把");
      this.noteEl.textContent = t("私钥已存进代敲库，权限 600。");
    } catch (e) {
      this.toast(`${e}`);
    } finally {
      this.generating = false;
      this.syncOk();
    }
  }

  private renderGenerated(info: KeyInfo): void {
    this.genFpEl.textContent = t("已生成 {algo} · {fingerprint}", {
      algo: info.algo,
      fingerprint: info.fingerprint,
    });
    this.genPubEl.textContent = info.public_key ?? "";
  }
}
