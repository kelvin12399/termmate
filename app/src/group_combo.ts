import { t } from "./i18n";

export type GroupOption = {
  value: string;
  label: string;
  create?: boolean;
  clear?: boolean;
};

/**
 * 可输入、也可选择的分组框。
 *
 * 分组没有独立台账：候选永远来自当前主机列表，最后一台主机移出后，
 * 这个组就不会再出现。输入已存在的名字（忽略大小写）会归并到原组，
 * 避免「服务器」「服务器 」「Server」被当成几个组。
 */
export class GroupCombo {
  private wrap: HTMLDivElement;
  private menu: HTMLDivElement;
  private toggle: HTMLButtonElement;
  private options: GroupOption[] = [];
  private active = 0;

  constructor(
    private input: HTMLInputElement,
    private groups: () => string[]
  ) {
    this.wrap = document.createElement("div");
    this.wrap.className = "group-combo";
    input.parentElement?.insertBefore(this.wrap, input);
    this.wrap.appendChild(input);

    this.toggle = document.createElement("button");
    this.toggle.type = "button";
    this.toggle.className = "group-combo-toggle";
    this.toggle.tabIndex = -1;
    this.toggle.title = t("选择已有分组");
    this.toggle.setAttribute("aria-label", t("选择已有分组"));
    this.toggle.innerHTML = '<svg class="i"><use href="#i-chevron-down" /></svg>';

    this.menu = document.createElement("div");
    this.menu.className = "group-menu";
    this.menu.id = "f-group-options";
    this.menu.setAttribute("role", "listbox");
    this.menu.hidden = true;

    this.wrap.append(this.toggle, this.menu);
    input.setAttribute("role", "combobox");
    input.setAttribute("aria-autocomplete", "list");
    input.setAttribute("aria-controls", this.menu.id);
    input.setAttribute("aria-expanded", "false");
    input.setAttribute("autocomplete", "off");
    input.placeholder = t("选择已有分组，或输入新分组");

    input.addEventListener("focus", () => this.open());
    input.addEventListener("input", () => {
      this.active = 0;
      this.open();
    });
    input.addEventListener("keydown", (e) => this.onKeydown(e));
    this.toggle.addEventListener("mousedown", (e) => e.preventDefault());
    this.toggle.addEventListener("click", () => {
      // 先记住点击前的状态。focus 会触发 open()，如果先 focus 再判断，
      // 永远会把刚打开的菜单立刻关掉（点箭头等于无效）。
      const wasOpen = !this.menu.hidden;
      if (wasOpen) {
        this.close();
        return;
      }
      input.focus({ preventScroll: true });
      if (this.menu.hidden) this.open();
    });
    document.addEventListener("mousedown", (e) => {
      if (!this.wrap.contains(e.target as Node)) this.close();
    });
    window.addEventListener("dq:locale", () => this.syncLocale());
  }

  private syncLocale(): void {
    const label = t("选择已有分组");
    this.toggle.title = label;
    this.toggle.setAttribute("aria-label", label);
    this.input.placeholder = t("选择已有分组，或输入新分组");
    if (!this.menu.hidden) this.render();
  }

  /** 保存时取规范名：已有组忽略大小写归并，新组保留用户输入。 */
  value(): string {
    const raw = this.input.value.trim();
    if (!raw) return "";
    return (
      this.groups().find((g) => g.localeCompare(raw, undefined, { sensitivity: "accent" }) === 0) ??
      raw
    );
  }

  close(): void {
    this.menu.hidden = true;
    this.input.setAttribute("aria-expanded", "false");
  }

  private open(): void {
    this.render();
    this.menu.hidden = false;
    this.input.setAttribute("aria-expanded", "true");
  }

  private onKeydown(e: KeyboardEvent): void {
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      if (this.menu.hidden) this.open();
      if (this.options.length === 0) return;
      const delta = e.key === "ArrowDown" ? 1 : -1;
      this.active = (this.active + delta + this.options.length) % this.options.length;
      this.paintActive();
      return;
    }
    if (e.key === "Enter" && !this.menu.hidden && this.options[this.active]) {
      e.preventDefault();
      this.choose(this.options[this.active]);
      return;
    }
    if (e.key === "Escape" && !this.menu.hidden) {
      // 只收候选框，不把整个主机抽屉一起关掉
      e.preventDefault();
      e.stopPropagation();
      this.close();
      return;
    }
    if (e.key === "Tab") this.close();
  }

  private render(): void {
    const query = this.input.value.trim();
    const q = query.toLocaleLowerCase();
    const all = this.groups();
    const matched = all.filter((g) => !q || g.toLocaleLowerCase().includes(q));

    this.options = [];
    if (!query) this.options.push({ value: "", label: t("不分组"), clear: true });
    for (const g of matched) this.options.push({ value: g, label: g });
    const exact = all.some((g) => g.toLocaleLowerCase() === q);
    if (query && !exact) {
      this.options.push({ value: query, label: t("新建分组「{name}」", { name: query }), create: true });
    }

    this.active = Math.max(0, Math.min(this.active, this.options.length - 1));
    this.menu.replaceChildren();

    for (const [index, option] of this.options.entries()) {
      const row = document.createElement("div");
      row.className =
        "group-option" +
        (option.create ? " create" : "") +
        (option.clear ? " clear" : "") +
        (index === this.active ? " active" : "");
      row.id = `f-group-option-${index}`;
      row.setAttribute("role", "option");
      row.setAttribute("aria-selected", index === this.active ? "true" : "false");
      if (option.create) {
        row.innerHTML = '<svg class="i"><use href="#i-plus" /></svg>';
      } else if (option.clear) {
        row.innerHTML = '<svg class="i"><use href="#i-x" /></svg>';
      } else {
        row.innerHTML = '<svg class="i"><use href="#i-folder" /></svg>';
      }
      const text = document.createElement("span");
      text.textContent = option.label;
      row.appendChild(text);
      // 不让 mousedown 把焦点从输入框抢走，否则连续键盘输入会断
      row.addEventListener("mousedown", (e) => e.preventDefault());
      row.addEventListener("mouseenter", () => {
        this.active = index;
        this.paintActive();
      });
      row.addEventListener("click", () => this.choose(option));
      this.menu.appendChild(row);
    }

    if (this.options.length === 0) {
      const empty = document.createElement("div");
      empty.className = "group-menu-hint";
      empty.textContent = t("继续输入即可创建新分组");
      this.menu.appendChild(empty);
    }
    this.paintActive();
  }

  private paintActive(): void {
    const rows = Array.from(this.menu.querySelectorAll<HTMLElement>(".group-option"));
    for (const [index, row] of rows.entries()) {
      row.classList.toggle("active", index === this.active);
      row.setAttribute("aria-selected", index === this.active ? "true" : "false");
    }
    if (this.options[this.active]) {
      this.input.setAttribute("aria-activedescendant", `f-group-option-${this.active}`);
      rows[this.active]?.scrollIntoView({ block: "nearest" });
    } else {
      this.input.removeAttribute("aria-activedescendant");
    }
  }

  private choose(option: GroupOption): void {
    this.input.value = option.value;
    this.close();
    this.input.focus({ preventScroll: true });
  }
}
