// 前端终端渲染器：**只负责把 Rust 送来的格子画出来**。
//
// 它不解析 ANSI、不维护终端状态机 —— 那是 Rust 侧唯一解析点的职责。
// 每段 run 自带起始列（col），宽字符由 Rust 标 WIDE，前端不再自己猜字宽；
// 每个非 ASCII 字符都落在自己的格子里，所以 emoji / 中文 / 图标不会把后面的字挤歪。

export interface Run {
  col: number;
  text: string;
  fg: number;
  bg: number;
  flags: number;
}

export interface LineUpdate {
  row: number;
  runs: Run[];
}

export interface CursorState {
  row: number;
  col: number;
  visible: boolean;
  shape: number;
}

export interface SelectionPoint {
  row: number;
  col: number;
}

interface Selection {
  anchor: SelectionPoint;
  focus: SelectionPoint;
}

export interface TermUpdate {
  session: string;
  full: boolean;
  cols: number;
  rows: number;
  lines: LineUpdate[];
  cursor: CursorState;
}

const FLAG_BOLD = 1 << 0;
const FLAG_ITALIC = 1 << 1;
const FLAG_UNDERLINE = 1 << 2;
const FLAG_STRIKEOUT = 1 << 3;
const FLAG_DIM = 1 << 5;
const FLAG_HIDDEN = 1 << 6;
const FLAG_WIDE = 1 << 7;

const DEFAULT_BG_NUM = 0x151618;
export const DEFAULT_BG = "#151618";
export const DEFAULT_FG = "#c5c8c6";

// Nerd Font 图标（私有区码位）。只覆盖私有区，不抢普通字符和彩色 emoji。
const SYMBOL_FAMILY = "DQ Symbols";
let symbolFontPromise: Promise<boolean> | null = null;

/** 加载随包带的 Symbols Nerd Font。canvas 不会在字体到位后自动重画，调用方拿到 true 后要整屏重画。 */
export function loadSymbolFont(): Promise<boolean> {
  if (symbolFontPromise) return symbolFontPromise;
  symbolFontPromise = (async () => {
    try {
      const face = new FontFace(SYMBOL_FAMILY, "url(/fonts/SymbolsNerdFontMono-Regular.woff2)", {
        unicodeRange: "U+E000-F8FF, U+F0000-FFFFD",
      });
      await face.load();
      document.fonts.add(face);
      return true;
    } catch (e) {
      console.warn("Nerd Font 图标字体加载失败", e);
      return false;
    }
  })();
  return symbolFontPromise;
}

function rgb(n: number): string {
  return `rgb(${(n >> 16) & 0xff},${(n >> 8) & 0xff},${n & 0xff})`;
}

function isPua(cp: number): boolean {
  return (cp >= 0xe000 && cp <= 0xf8ff) || cp >= 0xf0000;
}

/** 一段 run 占多少格 */
function runCells(run: Run): number {
  let n = 0;
  for (const _ of run.text) n++;
  return run.flags & FLAG_WIDE ? n * 2 : n;
}

function isPlainAscii(s: string): boolean {
  for (let i = 0; i < s.length; i++) {
    const c = s.charCodeAt(i);
    if (c < 0x20 || c > 0x7e) return false;
  }
  return true;
}

// 象限块：bit 0 左上 / 1 右上 / 2 左下 / 3 右下
const QUAD: Record<number, number> = {
  0x2596: 4, 0x2597: 8, 0x2598: 1, 0x2599: 13, 0x259a: 9,
  0x259b: 7, 0x259c: 11, 0x259d: 2, 0x259e: 6, 0x259f: 14,
};

/** 这些字符不走字体，按格子尺寸直接画。
 *  只收两类：方块元素（░▒▓█▌▐ 等，字体画的填不满格子、行间会露缝）和
 *  Powerline 分隔符（三角/半圆，字体里只有字形高度，填不满格高）。
 *  框线字符（┌─┐ 一类）字体本身就是按格子设计的，交给字体更稳。 */
function isBuiltin(cp: number): boolean {
  return (cp >= 0x2580 && cp <= 0x259f) || (cp >= 0xe0b0 && cp <= 0xe0be);
}

export interface Metrics {
  cols: number;
  rows: number;
}

export class TerminalView {
  private canvas: HTMLCanvasElement;
  private ctx: CanvasRenderingContext2D;
  private lines = new Map<number, Run[]>();
  private cursor: CursorState = { row: 0, col: 0, visible: false, shape: 0 };
  private selection: Selection | null = null;
  private glyphW = new Map<string, number>();

  private fontSize = 13;
  /** 外观设置可改的部分。默认值和 Rust 侧 appearance.rs 的默认保持一致。 */
  private fontFamily = "";
  private lineHeight = 1.32;
  private bg = DEFAULT_BG;
  private fg = DEFAULT_FG;
  private bgNum = DEFAULT_BG_NUM;
  private cursorStyle: "block" | "bar" | "underline" = "block";
  private cellW = 8;
  private cellH = 17;
  private baseline = 13;
  private dpr = 1;

  cols = 80;
  rows = 24;

  constructor(canvas: HTMLCanvasElement) {
    this.canvas = canvas;
    const ctx = canvas.getContext("2d", { alpha: false });
    if (!ctx) throw new Error("无法获取 2D 上下文");
    this.ctx = ctx;
    this.measureFont();
  }

  /** 字体度量：等宽字体下所有 ASCII 同宽，用 M 量一次即可 */
  measureFont(): void {
    const ctx = this.ctx;
    ctx.font = this.fontString();
    const m = ctx.measureText("M");
    this.cellW = Math.max(6, m.width);
    this.cellH = Math.round(this.fontSize * this.lineHeight);
    this.baseline = Math.round(this.cellH * 0.78);
    this.glyphW.clear();
  }

  /**
   * 套用外观设置。改完要重新量字宽、重算行列 ——
   * 调用方拿返回值去通知 PTY 新尺寸，否则远端程序还按老行列画。
   */
  applyAppearance(a: {
    font_size?: number;
    font_family?: string;
    line_height?: number;
    bg?: string;
    fg?: string;
    cursor_style?: string;
  }): void {
    if (typeof a.font_size === "number") this.fontSize = a.font_size;
    if (typeof a.font_family === "string") this.fontFamily = a.font_family;
    if (typeof a.line_height === "number") this.lineHeight = a.line_height;
    if (a.bg) {
      this.bg = a.bg;
      // 底色要同时更新数值形式：drawRow 靠它判断「这段是默认背景、可以跳过填充」
      const n = Number.parseInt(a.bg.replace("#", ""), 16);
      if (Number.isFinite(n)) this.bgNum = n;
    }
    if (a.fg) this.fg = a.fg;
    if (a.cursor_style === "block" || a.cursor_style === "bar" || a.cursor_style === "underline") {
      this.cursorStyle = a.cursor_style;
    }
    this.measureFont();
  }

  /** 当前底色，给外层容器同步用 */
  background(): string {
    return this.bg;
  }

  /** 字体（如图标字体）刚加载完：清掉字宽缓存，下一次 render 会重新量 */
  resetGlyphCache(): void {
    this.glyphW.clear();
  }

  private fontString(bold = false, italic = false): string {
    const style = italic ? "italic " : "";
    const weight = bold ? "600 " : "";
    // 自定义字体排在最前，内置那串永远留在后面兜底 ——
    // 人填了个系统里没有的字体名时，不该让整个终端变成衬线体。
    const custom = this.fontFamily.trim() ? `"${this.fontFamily.trim()}", ` : "";
    return `${style}${weight}${this.fontSize}px ${custom}"SF Mono", ui-monospace, Menlo, "${SYMBOL_FAMILY}", "PingFang SC", monospace`;
  }

  /** 容器像素尺寸 → 终端行列数 */
  fit(pxWidth: number, pxHeight: number): Metrics {
    this.dpr = window.devicePixelRatio || 1;
    this.cols = Math.max(2, Math.floor(pxWidth / this.cellW));
    this.rows = Math.max(2, Math.floor(pxHeight / this.cellH));

    const cssW = Math.ceil(this.cols * this.cellW);
    const cssH = this.rows * this.cellH;
    this.canvas.width = Math.ceil(cssW * this.dpr);
    this.canvas.height = Math.ceil(cssH * this.dpr);
    this.canvas.style.width = `${cssW}px`;
    this.canvas.style.height = `${cssH}px`;
    return { cols: this.cols, rows: this.rows };
  }

  applyUpdate(u: TermUpdate): void {
    if (u.full) {
      this.lines.clear();
      this.cols = u.cols;
      this.rows = u.rows;
    }
    for (const line of u.lines) {
      if (line.runs.length === 0) {
        this.lines.delete(line.row);
      } else {
        this.lines.set(line.row, line.runs);
      }
    }
    this.cursor = u.cursor;
  }

  /** 整屏重画（resize 后或首次挂载时用） */
  render(): void {
    const ctx = this.ctx;
    ctx.setTransform(this.dpr, 0, 0, this.dpr, 0, 0);
    ctx.fillStyle = this.bg;
    ctx.fillRect(0, 0, this.canvas.width / this.dpr, this.canvas.height / this.dpr);
    for (let r = 0; r < this.rows; r++) {
      this.drawRow(r);
    }
    this.drawSelection();
    this.drawCursor();
  }

  /** 只重画变化过的行 */
  renderRows(rows: number[]): void {
    const ctx = this.ctx;
    ctx.setTransform(this.dpr, 0, 0, this.dpr, 0, 0);
    for (const r of rows) {
      ctx.fillStyle = this.bg;
      ctx.fillRect(0, r * this.cellH, this.canvas.width / this.dpr, this.cellH);
      this.drawRow(r);
    }
    this.drawSelection();
    this.drawCursor();
  }

  /** 列 → 像素 x，对齐到物理像素，避免相邻色块之间出现发丝缝 */
  private colX(col: number): number {
    return Math.round(col * this.cellW * this.dpr) / this.dpr;
  }

  beginSelection(point: SelectionPoint): void {
    this.selection = { anchor: point, focus: point };
  }

  updateSelection(point: SelectionPoint): void {
    if (this.selection) this.selection.focus = point;
  }

  clearSelection(): void {
    this.selection = null;
  }

  selectAll(): void {
    if (this.rows <= 0 || this.cols <= 0) return;
    this.selection = {
      anchor: { row: 0, col: 0 },
      focus: { row: this.rows - 1, col: this.cols - 1 },
    };
  }

  hasSelection(): boolean {
    if (!this.selection) return false;
    const { anchor, focus } = this.selection;
    return anchor.row !== focus.row || anchor.col !== focus.col;
  }

  private orderedSelection(): [SelectionPoint, SelectionPoint] | null {
    if (!this.hasSelection() || !this.selection) return null;
    const { anchor: a, focus: b } = this.selection;
    return a.row < b.row || (a.row === b.row && a.col <= b.col) ? [a, b] : [b, a];
  }

  /** 把一行的 runs 展开成真正的终端格子，宽字符占两格。 */
  private rowCells(row: number): string[] {
    const cells = Array<string>(this.cols).fill(" ");
    const runs = this.lines.get(row) ?? [];
    for (const run of runs) {
      if (run.flags & FLAG_HIDDEN) continue;
      const wide = (run.flags & FLAG_WIDE) !== 0;
      let i = 0;
      for (const ch of [...run.text]) {
        const col = run.col + i * (wide ? 2 : 1);
        if (col >= 0 && col < this.cols) cells[col] = ch;
        if (wide && col + 1 >= 0 && col + 1 < this.cols) cells[col + 1] = "";
        i++;
      }
    }
    return cells;
  }

  /** 当前选区对应的纯文本，按行去掉右侧空白。 */
  selectionText(): string {
    const ordered = this.orderedSelection();
    if (!ordered) return "";
    const [start, end] = ordered;
    const out: string[] = [];
    for (let row = start.row; row <= end.row; row++) {
      const cells = this.rowCells(row);
      const from = row === start.row ? Math.max(0, start.col) : 0;
      const to = row === end.row ? Math.min(this.cols - 1, end.col) : this.cols - 1;
      if (to >= from) out.push(cells.slice(from, to + 1).join("").replace(/\s+$/, ""));
    }
    return out.join("\n").replace(/(?:\n\s*)+$/, "");
  }

  private drawSelection(): void {
    const ordered = this.orderedSelection();
    if (!ordered) return;
    const [start, end] = ordered;
    const ctx = this.ctx;
    ctx.fillStyle = "rgba(10, 132, 255, 0.30)";
    for (let row = start.row; row <= end.row; row++) {
      if (row < 0 || row >= this.rows) continue;
      const from = row === start.row ? Math.max(0, start.col) : 0;
      const to = row === end.row ? Math.min(this.cols - 1, end.col) : this.cols - 1;
      if (to < from) continue;
      const x = this.colX(from);
      const right = this.colX(to + 1);
      ctx.fillRect(x, row * this.cellH, right - x, this.cellH);
    }
  }

  /**
   * 光标现在在哪（CSS 像素，相对 canvas 左上角）。
   * 输入法的候选词窗口要贴着光标弹，不然会跑到屏幕角上。
   */
  caretBox(): { x: number; y: number; h: number } {
    return {
      x: this.colX(this.cursor.col),
      y: this.cursor.row * this.cellH,
      h: this.cellH,
    };
  }

  private drawRow(row: number): void {
    const runs = this.lines.get(row);
    if (!runs || runs.length === 0) return;
    const ctx = this.ctx;
    const y = row * this.cellH;

    // 第一遍：底色。先全画完再写字，图标溢出到右邻空格时不会被后面的底色盖掉
    for (const run of runs) {
      if (run.bg === this.bgNum) continue;
      const x0 = this.colX(run.col);
      const x1 = this.colX(run.col + runCells(run));
      ctx.fillStyle = rgb(run.bg);
      ctx.fillRect(x0, y, x1 - x0, this.cellH);
    }

    // 第二遍：文字 / 内置图形 / 下划线
    for (const run of runs) {
      if (run.flags & FLAG_HIDDEN) continue;
      this.drawRunText(run, y);
    }
  }

  private drawRunText(run: Run, y: number): void {
    const ctx = this.ctx;
    const flags = run.flags;
    const wide = (flags & FLAG_WIDE) !== 0;
    let fg = run.fg;
    if (flags & FLAG_DIM) fg = dim(fg);
    ctx.fillStyle = rgb(fg);
    ctx.strokeStyle = rgb(fg);
    ctx.font = this.fontString(!!(flags & FLAG_BOLD), !!(flags & FLAG_ITALIC));
    ctx.textBaseline = "alphabetic";

    if (!wide && isPlainAscii(run.text)) {
      // 快路径：等宽 ASCII 一次画完，前进量就是格宽
      ctx.fillText(run.text, this.colX(run.col), y + this.baseline);
    } else {
      const chars = [...run.text];
      const step = wide ? 2 : 1;
      for (let i = 0; i < chars.length; i++) {
        const ch = chars[i];
        const cp = ch.codePointAt(0)!;
        if (cp === 0x20) continue;
        const col = run.col + i * step;
        const x = this.colX(col);
        const slot = this.colX(col + step) - x;
        if (!wide && isBuiltin(cp)) {
          this.drawBuiltin(cp, x, y, slot, this.cellH);
          continue;
        }
        // 图标后面紧跟空格时允许占两格，不然 13px 的图标塞进 8px 会缩得看不清
        let room = slot;
        if (!wide && isPua(cp) && chars[i + 1] === " ") room = this.colX(col + 2) - x;
        this.drawGlyph(ch, x, y, room, wide || isPua(cp));
      }
    }

    const cells = runCells(run);
    const x0 = this.colX(run.col);
    const w = this.colX(run.col + cells) - x0;
    const px = 1 / this.dpr;
    if (flags & FLAG_UNDERLINE) {
      ctx.fillRect(x0, y + this.baseline + 2, w, Math.max(px, 1));
    }
    if (flags & FLAG_STRIKEOUT) {
      ctx.fillRect(x0, y + Math.round(this.cellH * 0.55), w, Math.max(px, 1));
    }
  }

  /** 普通字形放进 room 宽的格子：放得下就（按需）居中，放不下就等比缩小 */
  private drawGlyph(ch: string, x: number, y: number, room: number, center: boolean): void {
    const ctx = this.ctx;
    const key = ctx.font + "\u0000" + ch;
    let w = this.glyphW.get(key);
    if (w === undefined) {
      w = ctx.measureText(ch).width;
      this.glyphW.set(key, w);
    }
    if (w <= room + 0.5) {
      const dx = center ? (room - w) / 2 : 0;
      ctx.fillText(ch, x + dx, y + this.baseline);
      return;
    }
    const s = room / w;
    const mid = y + this.cellH / 2;
    ctx.save();
    ctx.translate(x, mid);
    ctx.scale(s, s);
    ctx.fillText(ch, 0, this.baseline - this.cellH / 2);
    ctx.restore();
  }

  /** 方块元素 / Powerline 分隔符 / 框线：按格子画，保证跨行跨列无缝 */
  private drawBuiltin(cp: number, x: number, y: number, w: number, h: number): void {
    const ctx = this.ctx;
    const d = this.dpr;
    const snap = (v: number) => Math.round(v * d) / d;
    const rect = (x0: number, y0: number, x1: number, y1: number) => {
      const a = snap(x0), b = snap(y0);
      ctx.fillRect(a, b, snap(x1) - a, snap(y1) - b);
    };

    // ── 方块元素 U+2580–259F
    if (cp >= 0x2580 && cp <= 0x259f) {
      if (cp === 0x2580) return rect(x, y, x + w, y + h / 2);
      if (cp >= 0x2581 && cp <= 0x2588) return rect(x, y + h * (1 - (cp - 0x2580) / 8), x + w, y + h);
      if (cp >= 0x2589 && cp <= 0x258f) return rect(x, y, x + w * ((0x2590 - cp) / 8), y + h);
      if (cp === 0x2590) return rect(x + w / 2, y, x + w, y + h);
      if (cp >= 0x2591 && cp <= 0x2593) {
        ctx.globalAlpha = (cp - 0x2590) * 0.25;
        rect(x, y, x + w, y + h);
        ctx.globalAlpha = 1;
        return;
      }
      if (cp === 0x2594) return rect(x, y, x + w, y + h / 8);
      if (cp === 0x2595) return rect(x + w * 7 / 8, y, x + w, y + h);
      const q = QUAD[cp] ?? 0;
      if (q & 1) rect(x, y, x + w / 2, y + h / 2);
      if (q & 2) rect(x + w / 2, y, x + w, y + h / 2);
      if (q & 4) rect(x, y + h / 2, x + w / 2, y + h);
      if (q & 8) rect(x + w / 2, y + h / 2, x + w, y + h);
      return;
    }

    // ── Powerline
    if (cp >= 0xe0b0 && cp <= 0xe0be) {
      const cy = y + h / 2;
      ctx.lineWidth = Math.max(1 / d, 1);
      ctx.beginPath();
      switch (cp) {
        case 0xe0b0: ctx.moveTo(x, y); ctx.lineTo(x + w, cy); ctx.lineTo(x, y + h); ctx.closePath(); ctx.fill(); return;
        case 0xe0b2: ctx.moveTo(x + w, y); ctx.lineTo(x, cy); ctx.lineTo(x + w, y + h); ctx.closePath(); ctx.fill(); return;
        case 0xe0b1: ctx.moveTo(x, y); ctx.lineTo(x + w, cy); ctx.lineTo(x, y + h); ctx.stroke(); return;
        case 0xe0b3: ctx.moveTo(x + w, y); ctx.lineTo(x, cy); ctx.lineTo(x + w, y + h); ctx.stroke(); return;
        case 0xe0b4: ctx.ellipse(x, cy, w, h / 2, 0, -Math.PI / 2, Math.PI / 2); ctx.closePath(); ctx.fill(); return;
        case 0xe0b6: ctx.ellipse(x + w, cy, w, h / 2, 0, Math.PI / 2, Math.PI * 1.5); ctx.closePath(); ctx.fill(); return;
        case 0xe0b5: ctx.ellipse(x, cy, w - 0.5, h / 2 - 0.5, 0, -Math.PI / 2, Math.PI / 2); ctx.stroke(); return;
        case 0xe0b7: ctx.ellipse(x + w, cy, w - 0.5, h / 2 - 0.5, 0, Math.PI / 2, Math.PI * 1.5); ctx.stroke(); return;
        case 0xe0b8: ctx.moveTo(x, y); ctx.lineTo(x + w, y + h); ctx.lineTo(x, y + h); ctx.closePath(); ctx.fill(); return;
        case 0xe0ba: ctx.moveTo(x + w, y); ctx.lineTo(x + w, y + h); ctx.lineTo(x, y + h); ctx.closePath(); ctx.fill(); return;
        case 0xe0bc: ctx.moveTo(x, y); ctx.lineTo(x + w, y); ctx.lineTo(x, y + h); ctx.closePath(); ctx.fill(); return;
        case 0xe0be: ctx.moveTo(x, y); ctx.lineTo(x + w, y); ctx.lineTo(x + w, y + h); ctx.closePath(); ctx.fill(); return;
      }
      return;
    }

  }

  private drawCursor(): void {
    const c = this.cursor;
    if (!c.visible) return;
    if (c.row >= this.rows || c.col >= this.cols) return;

    const ctx = this.ctx;
    const x = this.colX(c.col);
    const w = this.colX(c.col + 1) - x;
    const y = c.row * this.cellH;

    ctx.fillStyle = this.fg;

    // 程序主动设过光标形状（DECSCUSR）就听程序的 —— vim 的插入模式
    // 靠这个把方块变竖线，外观设置不该把它盖掉。
    // shape 0 是「没指定」，这时候才用外观设置里的偏好。
    const shape =
      c.shape !== 0
        ? c.shape
        : this.cursorStyle === "bar"
          ? 2
          : this.cursorStyle === "underline"
            ? 1
            : 0;

    if (shape === 0) {
      ctx.globalAlpha = 0.55;
      ctx.fillRect(x, y, w, this.cellH);
      ctx.globalAlpha = 1;
    } else if (shape === 1) {
      ctx.fillRect(x, y + this.cellH - 2, w, 2);
    } else if (shape === 2) {
      ctx.fillRect(x, y, 2, this.cellH);
    } else {
      ctx.strokeStyle = this.fg;
      ctx.lineWidth = 1;
      ctx.strokeRect(x + 0.5, y + 0.5, w - 1, this.cellH - 1);
    }
  }

  /** 鼠标位置 → 格子坐标（供选区使用；拖出画面时夹到边缘） */
  cellAt(offsetX: number, offsetY: number): { col: number; row: number } {
    return {
      col: Math.max(0, Math.min(this.cols - 1, Math.floor(offsetX / this.cellW))),
      row: Math.max(0, Math.min(this.rows - 1, Math.floor(offsetY / this.cellH))),
    };
  }
}

function dim(color: number): number {
  const r = Math.round(((color >> 16) & 0xff) * 0.6);
  const g = Math.round(((color >> 8) & 0xff) * 0.6);
  const b = Math.round((color & 0xff) * 0.6);
  return (r << 16) | (g << 8) | b;
}
