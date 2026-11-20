// ⌘K 命令面板：所有入口收口的地方。
//
// 直接打主机名就连，打命令就执行。行结构是「图标 + 名称 + 灰色说明 + 右侧键帽」，
// 面板里出现的键帽都是真生效的快捷键 —— 不写做不到的事。

import { t } from "./i18n";

export interface Cmd {
  id: string;
  /** 分区小标题 */
  section: string;
  icon: string;
  label: string;
  desc?: string;
  /** 右侧键帽，没有就不显示 */
  keys?: string;
  /** 额外的搜索关键字（比如主机的 user@host） */
  hint?: string;
  run: () => void;
}

function icon(name: string): SVGSVGElement {
  const svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
  svg.setAttribute("class", "i");
  const use = document.createElementNS("http://www.w3.org/2000/svg", "use");
  use.setAttribute("href", `#i-${name}`);
  svg.appendChild(use);
  return svg;
}

export class Palette {
  private root: HTMLDivElement;
  private input: HTMLInputElement;
  private crumb: HTMLSpanElement;
  private foot: HTMLDivElement;
  private list: HTMLDivElement;
  private items: Cmd[] = [];
  private shown: Cmd[] = [];
  private cursor = 0;
  private open = false;

  constructor(private provide: () => Cmd[]) {
    this.root = document.createElement("div");
    this.root.id = "palette";
    this.root.hidden = true;

    const box = document.createElement("div");
    box.className = "pal-box";

    const head = document.createElement("div");
    head.className = "pal-head";
    head.appendChild(icon("search"));
    this.crumb = document.createElement("span");
    this.crumb.className = "pal-crumb";
    head.appendChild(this.crumb);
    this.input = document.createElement("input");
    this.input.placeholder = t("输入主机名或命令…");
    this.input.spellcheck = false;
    head.appendChild(this.input);
    box.appendChild(head);

    this.list = document.createElement("div");
    this.list.className = "pal-list";
    box.appendChild(this.list);

    this.foot = document.createElement("div");
    this.foot.className = "pal-foot";
    box.appendChild(this.foot);

    this.root.appendChild(box);
    document.body.appendChild(this.root);
    this.syncChrome();

    this.root.addEventListener("mousedown", (e) => {
      if (e.target === this.root) this.close();
    });
    this.input.addEventListener("input", () => {
      this.cursor = 0;
      this.renderList();
    });
    this.input.addEventListener("keydown", (e) => this.onKey(e));
  }

  isOpen(): boolean {
    return this.open;
  }

  toggle(): void {
    if (this.open) this.close();
    else this.show();
  }

  show(): void {
    this.items = this.provide();
    this.input.value = "";
    this.cursor = 0;
    this.open = true;
    this.root.hidden = false;
    this.renderList();
    this.input.focus();
  }

  close(): void {
    this.open = false;
    this.root.hidden = true;
  }

  refresh(): void {
    this.syncChrome();
    this.items = this.provide();
    this.renderList();
  }

  private syncChrome(): void {
    this.crumb.textContent = t("代敲");
    this.input.placeholder = t("输入主机名或命令…");
    this.foot.replaceChildren();
    for (const [key, label] of [
      ["↑↓", t("选择")],
      ["↵", t("执行")],
      ["esc", t("关闭")],
    ]) {
      const item = document.createElement("span");
      const cap = document.createElement("kbd");
      cap.className = "kbd";
      cap.textContent = key;
      item.append(cap, document.createTextNode(` ${label}`));
      this.foot.appendChild(item);
    }
  }

  private onKey(e: KeyboardEvent): void {
    if (e.key === "Escape") {
      e.preventDefault();
      this.close();
      return;
    }
    if (e.key === "ArrowDown" || (e.key === "n" && e.ctrlKey)) {
      e.preventDefault();
      this.move(1);
      return;
    }
    if (e.key === "ArrowUp" || (e.key === "p" && e.ctrlKey)) {
      e.preventDefault();
      this.move(-1);
      return;
    }
    if (e.key === "Enter") {
      e.preventDefault();
      const c = this.shown[this.cursor];
      if (c) {
        this.close();
        c.run();
      }
    }
  }

  private move(d: number): void {
    if (this.shown.length === 0) return;
    this.cursor = (this.cursor + d + this.shown.length) % this.shown.length;
    this.renderList();
    this.list.querySelector<HTMLElement>(".pal-row.on")?.scrollIntoView({ block: "nearest" });
  }

  private renderList(): void {
    const q = this.input.value.trim().toLowerCase();
    this.shown = q
      ? this.items.filter((c) =>
          `${c.label} ${c.desc ?? ""} ${c.hint ?? ""} ${c.section}`.toLowerCase().includes(q)
        )
      : this.items;

    this.list.replaceChildren();
    if (this.shown.length === 0) {
      const empty = document.createElement("div");
      empty.className = "pal-empty";
      empty.textContent = t("没有匹配的主机或命令");
      this.list.appendChild(empty);
      return;
    }

    let section = "";
    this.shown.forEach((c, i) => {
      if (c.section !== section) {
        section = c.section;
        const h = document.createElement("div");
        h.className = "pal-sec";
        h.textContent = section;
        this.list.appendChild(h);
      }

      const row = document.createElement("div");
      row.className = "pal-row" + (i === this.cursor ? " on" : "");
      row.appendChild(icon(c.icon));

      const label = document.createElement("span");
      label.className = "pal-label";
      label.textContent = c.label;
      row.appendChild(label);

      if (c.desc) {
        const d = document.createElement("span");
        d.className = "pal-desc";
        d.textContent = c.desc;
        row.appendChild(d);
      }

      const spacer = document.createElement("div");
      spacer.style.flex = "1";
      row.appendChild(spacer);

      const cap = document.createElement("kbd");
      cap.className = "kbd";
      cap.textContent = c.keys ?? "↵";
      row.appendChild(cap);

      row.addEventListener("mouseenter", () => {
        this.cursor = i;
        for (const el of Array.from(this.list.querySelectorAll(".pal-row"))) {
          el.classList.toggle("on", el === row);
        }
      });
      row.addEventListener("click", () => {
        this.close();
        c.run();
      });

      this.list.appendChild(row);
    });
  }
}
