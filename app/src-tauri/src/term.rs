//! 终端会话：PTY ↔ alacritty_terminal 仿真。
//!
//! 这是整个应用**唯一的解析点**。前端拿到的永远是「哪些格子变成了什么」，
//! 它不需要、也不允许再解析一遍 ANSI。性能上限就在这里。

use crate::proto::*;
use serde::{Deserialize, Serialize};
use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::{Dimensions, Grid};
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::{Cell, Flags as CellFlags};
use alacritty_terminal::term::{Config as TermConfig, Term, TermDamage};
use alacritty_terminal::vte::ansi::{Color, CursorShape, NamedColor, Processor, StdSyncHandler};
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

/// 滚动历史行数
const SCROLLBACK: usize = 10_000;

/// 默认调色板（Alacritty 默认主题）
const PALETTE: [u32; 16] = [
    0x1d1f21, 0xcc6666, 0xb5bd68, 0xf0c674, 0x81a2be, 0xb294bb, 0x8abeb7, 0xc5c8c6, 0x666666,
    0xd54e53, 0xb9ca4a, 0xe6c547, 0x7aa6da, 0xc397d8, 0x70c0ba, 0xffffff,
];
const DEFAULT_FG: u32 = 0xc5c8c6;
const DEFAULT_BG: u32 = 0x151618;

/// 把 alacritty 的颜色解析成最终 RGB。
/// 注意：这里做掉之后，前端不需要知道任何调色板概念。
fn resolve_color(color: Color, is_fg: bool) -> u32 {
    match color {
        Color::Spec(rgb) => ((rgb.r as u32) << 16) | ((rgb.g as u32) << 8) | rgb.b as u32,
        Color::Indexed(i) => {
            if i < 16 {
                PALETTE[i as usize]
            } else if i < 232 {
                // 6x6x6 色立方
                let i = i - 16;
                let r = (i / 36) % 6;
                let g = (i / 6) % 6;
                let b = i % 6;
                let scale = |v: u8| -> u32 {
                    if v == 0 {
                        0
                    } else {
                        (v as u32) * 40 + 55
                    }
                };
                (scale(r) << 16) | (scale(g) << 8) | scale(b)
            } else {
                // 灰阶
                let v = (i - 232) as u32 * 10 + 8;
                (v << 16) | (v << 8) | v
            }
        }
        Color::Named(name) => match name {
            NamedColor::Foreground | NamedColor::BrightForeground => DEFAULT_FG,
            NamedColor::Background => DEFAULT_BG,
            NamedColor::Cursor => DEFAULT_FG,
            other => {
                let idx = other as usize;
                if idx < 16 {
                    PALETTE[idx]
                } else if is_fg {
                    DEFAULT_FG
                } else {
                    DEFAULT_BG
                }
            }
        },
    }
}

fn map_flags(f: CellFlags) -> u32 {
    let mut out = 0u32;
    if f.contains(CellFlags::BOLD) {
        out |= flags::BOLD;
    }
    if f.contains(CellFlags::ITALIC) {
        out |= flags::ITALIC;
    }
    if f.intersects(
        CellFlags::UNDERLINE
            | CellFlags::DOUBLE_UNDERLINE
            | CellFlags::UNDERCURL
            | CellFlags::DOTTED_UNDERLINE
            | CellFlags::DASHED_UNDERLINE,
    ) {
        out |= flags::UNDERLINE;
    }
    if f.contains(CellFlags::STRIKEOUT) {
        out |= flags::STRIKEOUT;
    }
    if f.contains(CellFlags::INVERSE) {
        out |= flags::INVERSE;
    }
    if f.contains(CellFlags::DIM) {
        out |= flags::DIM;
    }
    if f.contains(CellFlags::HIDDEN) {
        out |= flags::HIDDEN;
    }
    out
}

/// 尺寸描述，喂给 alacritty 的 Term
#[derive(Clone, Copy, Debug)]
pub struct TermSize {
    pub rows: usize,
    pub cols: usize,
}

impl Dimensions for TermSize {
    fn total_lines(&self) -> usize {
        self.rows
    }
    fn screen_lines(&self) -> usize {
        self.rows
    }
    fn columns(&self) -> usize {
        self.cols
    }
}

/// 终端状态（解析结果）。所有访问都在 `Session::state` 这把锁下进行。
pub struct TermState {
    pub term: Term<EventProxy>,
    pub size: TermSize,
    processor: Processor<StdSyncHandler>,
    /// 需要整屏重画（resize 后 / 前端首次挂载）
    force_full: bool,
    /// 上一次的光标位置，用来判断光标是否单独移动了
    last_cursor: CursorState,
    tap: ExecTap,
}

impl TermState {
    fn new(size: TermSize, proxy: EventProxy) -> Self {
        let config = TermConfig {
            scrolling_history: SCROLLBACK,
            ..TermConfig::default()
        };
        let term = Term::new(config, &size, proxy);
        Self {
            term,
            size,
            processor: Processor::new(),
            force_full: true,
            last_cursor: CursorState::default(),
            tap: ExecTap::default(),
        }
    }

    /// 把 PTY 原始字节喂进仿真器（唯一解析发生地）
    pub fn feed(&mut self, bytes: &[u8]) {
        // exec 采集：原始字节原样留一份，结束后按不可见标记切出输出
        if let Some(cap) = self.tap.capture.as_mut() {
            if cap.len() < CAPTURE_MAX {
                cap.extend_from_slice(bytes);
            }
        }
        // 静默窗口：注入命令包装器期间远端的回显不进屏幕
        let outcome = if let Some(m) = self.tap.mute.as_mut() {
            m.buf.extend_from_slice(bytes);
            if let Some(i) = find_bytes(&m.buf, &m.marker) {
                Some((true, m.buf.split_off(i + m.marker.len())))
            } else if Instant::now() >= m.deadline {
                Some((false, std::mem::take(&mut m.buf)))
            } else {
                return;
            }
        } else {
            None
        };
        match outcome {
            None => self.processor.advance(&mut self.term, bytes),
            Some((ok, rest)) => self.end_mute(ok, rest),
        }
    }

    fn end_mute(&mut self, ok: bool, rest: Vec<u8>) {
        self.tap.mute = None;
        self.tap.state = if ok { EXEC_READY } else { EXEC_FAILED };
        // 成功：被吞掉的是「回显 + 回车」，补一个换行让新提示符落在下一行
        let mut out = if ok { b"\r\n".to_vec() } else { Vec::new() };
        out.extend_from_slice(&rest);
        self.processor.advance(&mut self.term, &out);
    }

    /// 静默窗口超时但远端再没来字节时，由轮询方调用
    fn poll_mute(&mut self) -> bool {
        let expired = matches!(&self.tap.mute, Some(m) if Instant::now() >= m.deadline);
        if expired {
            let buf = self.tap.mute.as_mut().map(|m| std::mem::take(&mut m.buf)).unwrap_or_default();
            self.end_mute(false, buf);
        }
        expired
    }

    /// 取出自上次调用以来的变化。返回 None 表示这一批没有任何视觉变化。
    pub fn take_update(&mut self, session: &str) -> Option<TermUpdate> {
        let forced = std::mem::replace(&mut self.force_full, false);

        let (damage_full, mut damaged_rows) = {
            let damage = self.term.damage();
            match damage {
                TermDamage::Full => (true, Vec::new()),
                TermDamage::Partial(iter) => {
                    let mut rows = Vec::new();
                    for bounds in iter {
                        // 这里的 line 已经是可见区域的行号（usize）
                        if bounds.line < self.size.rows {
                            rows.push(bounds.line as u16);
                        }
                    }
                    (false, rows)
                }
            }
        };
        self.term.reset_damage();

        let cursor = self.cursor_state();
        let cursor_moved = cursor.row != self.last_cursor.row
            || cursor.col != self.last_cursor.col
            || cursor.visible != self.last_cursor.visible
            || cursor.shape != self.last_cursor.shape;

        let full = forced || damage_full;
        if !full && damaged_rows.is_empty() && !cursor_moved {
            return None;
        }

        // 光标在空行上移动时没有 damage，但屏幕必须刷新 —— 把新旧两行都带上
        if !full && cursor_moved {
            damaged_rows.push(self.last_cursor.row);
            damaged_rows.push(cursor.row);
            damaged_rows.retain(|r| (*r as usize) < self.size.rows);
        }
        damaged_rows.sort_unstable();
        damaged_rows.dedup();
        self.last_cursor = cursor;

        let mut lines = Vec::new();
        if full {
            for row in 0..self.size.rows {
                lines.push(self.build_line(row as u16));
            }
        } else {
            for row in damaged_rows {
                lines.push(self.build_line(row));
            }
        }

        Some(TermUpdate {
            session: session.to_string(),
            full,
            cols: self.size.cols as u16,
            rows: self.size.rows as u16,
            lines,
            cursor,
            title: None,
        })
    }

    fn build_line(&self, row: u16) -> LineUpdate {
        let grid: &Grid<Cell> = self.term.grid();
        let line = &grid[Line(row as i32)];
        let mut runs: Vec<Run> = Vec::new();
        let mut cur: Option<Run> = None;

        for col in 0..self.size.cols {
            let cell = &line[Column(col)];
            // 宽字符的占位格：位置已由宽字符那一格的 2 列宽度覆盖
            if cell
                .flags
                .intersects(CellFlags::WIDE_CHAR_SPACER | CellFlags::LEADING_WIDE_CHAR_SPACER)
            {
                continue;
            }
            let ch = if cell.c == '\0' || cell.c == '\t' { ' ' } else { cell.c };

            let mut fg = resolve_color(cell.fg, true);
            let mut bg = resolve_color(cell.bg, false);
            let mut fl = map_flags(cell.flags);
            if fl & flags::BOLD != 0 && cell.fg == Color::Named(NamedColor::Foreground) {
                fg = PALETTE[15];
            }
            if fl & flags::INVERSE != 0 {
                std::mem::swap(&mut fg, &mut bg);
            }
            if cell.flags.contains(CellFlags::WIDE_CHAR) {
                fl |= flags::WIDE;
            }

            // 默认底色的空格：只并入同样默认底色、无下划线/删除线的窄 run，
            // 否则直接跳过（每段 run 自带起始列，空格不画也不会错位）
            let blank = ch == ' ' && bg == DEFAULT_BG && fl & (flags::UNDERLINE | flags::STRIKEOUT | flags::WIDE) == 0;
            if blank {
                if let Some(r) = cur.as_mut() {
                    if r.bg == DEFAULT_BG && r.flags & (flags::UNDERLINE | flags::STRIKEOUT | flags::WIDE) == 0 {
                        r.text.push(' ');
                        continue;
                    }
                }
                if let Some(r) = cur.take() {
                    runs.push(r);
                }
                continue;
            }

            if let Some(r) = cur.as_mut() {
                if r.fg == fg && r.bg == bg && r.flags == fl {
                    r.text.push(ch);
                    continue;
                }
            }
            if let Some(r) = cur.take() {
                runs.push(r);
            }
            cur = Some(Run { col: col as u16, text: ch.to_string(), fg, bg, flags: fl });
        }
        if let Some(r) = cur.take() {
            runs.push(r);
        }
        for r in runs.iter_mut() {
            if r.bg == DEFAULT_BG && r.flags & (flags::UNDERLINE | flags::STRIKEOUT) == 0 {
                let t = r.text.trim_end_matches(' ').len();
                r.text.truncate(t);
            }
        }
        runs.retain(|r| !r.text.is_empty());

        LineUpdate { row, runs }
    }

    fn cursor_state(&self) -> CursorState {
        let grid = self.term.grid();
        let point = grid.cursor.point;
        let (visible, shape) = match self.term.cursor_style().shape {
            CursorShape::Hidden => (false, 0),
            CursorShape::Block => (true, 0),
            CursorShape::Underline => (true, 1),
            CursorShape::Beam => (true, 2),
            CursorShape::HollowBlock => (true, 3),
        };
        CursorState {
            row: point.line.0.max(0) as u16,
            col: point.column.0 as u16,
            visible,
            shape,
        }
    }

    /// 给 MCP 用的纯文本快照：可见区域 + scrollback。
    pub fn snapshot(&self) -> ScreenSnapshot {
        let grid = self.term.grid();
        let mut visible = Vec::with_capacity(self.size.rows);
        for row in 0..self.size.rows {
            visible.push(line_text(grid, row as i32, self.size.cols));
        }
        let history = grid.history_size();
        let mut scrollback = Vec::with_capacity(history);
        for i in 0..history {
            let line = -(history as i32) + i as i32;
            scrollback.push(line_text(grid, line, self.size.cols));
        }
        ScreenSnapshot {
            cols: self.size.cols as u16,
            rows: self.size.rows as u16,
            visible,
            scrollback,
            cursor: self.cursor_state(),
        }
    }
}

fn line_text(grid: &Grid<Cell>, line: i32, cols: usize) -> String {
    let row = &grid[Line(line)];
    let mut s = String::with_capacity(cols);
    for col in 0..cols {
        let cell = &row[Column(col)];
        if cell
            .flags
            .intersects(CellFlags::WIDE_CHAR_SPACER | CellFlags::LEADING_WIDE_CHAR_SPACER)
        {
            continue;
        }
        s.push(if cell.c == '\0' { ' ' } else { cell.c });
    }
    s.trim_end().to_string()
}

/// 事件代理：负责终端回写（DA 响应等）、标题变化、以及 exec 的完成信号。
#[derive(Clone)]
pub struct EventProxy {
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    pub title: Arc<Mutex<String>>,
    /// exec 完成信号：token → 退出码。
    /// 用 OSC 设置标题来送信号，所以**不会出现在屏幕上**。
    pub done: Arc<Mutex<HashMap<String, String>>>,
}

impl EventListener for EventProxy {
    fn send_event(&self, event: Event) {
        match event {
            // 关键：TUI 程序（vim/htop/claude）会查询终端能力，必须回写
            Event::PtyWrite(text) => {
                if let Ok(mut w) = self.writer.lock() {
                    let _ = w.write_all(text.as_bytes());
                    let _ = w.flush();
                }
            }
            Event::Title(t) => {
                // 以 __DQD_ 开头的是我们自己的完成信号，不是真标题 ——
                // 不能让它污染会话标题（标签栏和 list_sessions 都在用）
                if t.starts_with("__DQ") {
                    if let Some(rest) = t.strip_prefix("__DQD_") {
                        if let Some((token, code)) = rest.split_once('_') {
                            if let Ok(mut d) = self.done.lock() {
                                d.insert(token.to_string(), code.to_string());
                            }
                        }
                    }
                    return;
                }
                if let Ok(mut cur) = self.title.lock() {
                    *cur = t;
                }
            }
            _ => {}
        }
    }
}

/// exec 包装器在远端 shell 里的状态
pub const EXEC_NONE: u8 = 0;
pub const EXEC_INJECTING: u8 = 1;
pub const EXEC_READY: u8 = 2;
pub const EXEC_FAILED: u8 = 3;
const CAPTURE_MAX: usize = 8 * 1024 * 1024;

struct Mute {
    marker: Vec<u8>,
    buf: Vec<u8>,
    deadline: Instant,
}

/// SSH exec 用的旁路：注入时的静默窗口 + 命令输出的原始字节采集
#[derive(Default)]
struct ExecTap {
    state: u8,
    mute: Option<Mute>,
    capture: Option<Vec<u8>>,
}

pub fn find_bytes(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    hay.windows(needle.len()).position(|w| w == needle)
}

/// 会话的底层通道：本地 PTY 和 SSH 各自实现它
pub trait SessionIo: Send + Sync {
    fn write(&self, data: &[u8]) -> anyhow::Result<()>;
    fn resize(&self, cols: u16, rows: u16) -> anyhow::Result<()>;
    fn is_alive(&self) -> bool;
    fn kill(&self);
    /// 本地会话的 shell 进程号。有了它就能直接查系统要 cwd / 环境变量，
    /// 不用往屏幕上打命令（前台跑着程序时也安全）。SSH 会话没有本地进程，返回 None。
    fn pid(&self) -> Option<u32> {
        None
    }
}

/// 本地 PTY 通道
struct LocalPtyIo {
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    master: Arc<Mutex<Box<dyn MasterPty + Send>>>,
    child: Arc<Mutex<Box<dyn Child + Send + Sync>>>,
}

impl SessionIo for LocalPtyIo {
    fn write(&self, data: &[u8]) -> anyhow::Result<()> {
        let mut w = self
            .writer
            .lock()
            .map_err(|_| anyhow::anyhow!("writer 锁中毒"))?;
        w.write_all(data)?;
        w.flush()?;
        Ok(())
    }

    fn resize(&self, cols: u16, rows: u16) -> anyhow::Result<()> {
        let m = self
            .master
            .lock()
            .map_err(|_| anyhow::anyhow!("master 锁中毒"))?;
        m.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;
        Ok(())
    }

    fn is_alive(&self) -> bool {
        self.child
            .lock()
            .map(|mut c| c.try_wait().ok().flatten().is_none())
            .unwrap_or(false)
    }

    fn kill(&self) {
        if let Ok(mut c) = self.child.lock() {
            let _ = c.kill();
        }
    }

    fn pid(&self) -> Option<u32> {
        self.child.lock().ok().and_then(|c| c.process_id())
    }
}

/// 延迟绑定的写入端。
/// SSH 场景下需要先建好 TermState 才能发起连接（连接回调要把数据喂给 state），
/// 而 EventProxy 又需要写入端 —— 用它打破这个循环。
struct LateWriter(Arc<Mutex<Option<Arc<dyn SessionIo>>>>);

impl Write for LateWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if let Ok(g) = self.0.lock() {
            if let Some(io) = g.as_ref() {
                io.write(buf)
                    .map_err(|e| std::io::Error::other(e.to_string()))?;
            }
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// 会话类型（前端标签栏和审计都用得到）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum SessionKind {
    Local,
    Ssh { host: String, label: String },
}

/// 一个终端会话
pub struct Session {
    pub id: String,
    pub title: Arc<Mutex<String>>,
    pub state: Arc<Mutex<TermState>>,
    pub kind: SessionKind,
    io: Arc<dyn SessionIo>,
    done: Arc<Mutex<HashMap<String, String>>>,
    /// 把变化推给前端（静默窗口超时时由轮询方调用）
    emit: Arc<dyn Fn(TermUpdate) + Send + Sync>,
}

/// 注进 zsh 的命令包装器。
/// 让 AI 的命令在屏幕上是 `__dq <token> '命令'`，而不是一长串重定向；
/// 完成信号走 OSC 标题，不在屏幕上留任何痕迹。
const ZSHRC_INJECT: &str = r#"# 代敲注入：AI 命令包装器
if [ -f "$HOME/.zshrc" ]; then source "$HOME/.zshrc"; fi
__dq() {
  local t="$1" c="$2"
  { eval "$c" ; } > "/tmp/daiqiao.$t" 2>&1
  local r=$?
  cat "/tmp/daiqiao.$t" 2>/dev/null
  printf '\033]0;__DQD_%s_%d\007' "$t" "$r"
}
"#;

const ZPROFILE_INJECT: &str = r#"if [ -f "$HOME/.zprofile" ]; then source "$HOME/.zprofile"; fi
"#;

impl Session {
    /// 起一个本地 shell 会话
    pub fn spawn_local<F>(
        id: String,
        cols: u16,
        rows: u16,
        on_update: F,
        on_exit: impl FnOnce() + Send + 'static,
    ) -> anyhow::Result<Self>
    where
        F: Fn(TermUpdate) + Send + Sync + 'static,
    {
        let emit: Arc<dyn Fn(TermUpdate) + Send + Sync> = Arc::new(on_update);
        let pty_system = native_pty_system();
        let pair = pty_system.openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;

        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string());
        let mut cmd = CommandBuilder::new(&shell);
        cmd.arg("-l");
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");

        // zsh：把 rc 放到临时 ZDOTDIR，命令包装器的注入就不会在屏幕上留下痕迹
        if shell.ends_with("zsh") {
            if let Ok(home) = std::env::var("HOME") {
                let dir = std::path::Path::new(&home).join(".daiqiao").join("shell");
                if std::fs::create_dir_all(&dir).is_ok() {
                    let _ = std::fs::write(dir.join(".zshrc"), ZSHRC_INJECT);
                    let _ = std::fs::write(dir.join(".zprofile"), ZPROFILE_INJECT);
                    cmd.env("ZDOTDIR", dir.to_string_lossy().to_string());
                }
            }
        }

        if let Ok(home) = std::env::var("HOME") {
            cmd.cwd(home);
        }

        let child = pair.slave.spawn_command(cmd)?;
        drop(pair.slave);

        let reader = pair.master.try_clone_reader()?;
        let writer = pair.master.take_writer()?;

        let writer = Arc::new(Mutex::new(writer));
        let title = Arc::new(Mutex::new(String::from("本地终端")));
        let done: Arc<Mutex<HashMap<String, String>>> = Arc::new(Mutex::new(HashMap::new()));

        let proxy = EventProxy {
            writer: writer.clone(),
            title: title.clone(),
            done: done.clone(),
        };

        let size = TermSize {
            rows: rows as usize,
            cols: cols as usize,
        };
        let state = Arc::new(Mutex::new(TermState::new(size, proxy)));

        // 读取线程：PTY 字节 → 唯一解析 → 变化量 → 前端
        {
            let state = state.clone();
            let sid = id.clone();
            let emit = emit.clone();
            thread::spawn(move || {
                let mut reader = reader;
                let mut buf = [0u8; 65536];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => {
                            let update = {
                                let mut st = match state.lock() {
                                    Ok(g) => g,
                                    Err(_) => break,
                                };
                                st.feed(&buf[..n]);
                                st.take_update(&sid)
                            };
                            if let Some(mut u) = update {
                                u.title = None;
                                emit(u);
                            }
                        }
                        Err(_) => break,
                    }
                }
                on_exit();
            });
        }

        let io: Arc<dyn SessionIo> = Arc::new(LocalPtyIo {
            writer,
            master: Arc::new(Mutex::new(pair.master)),
            child: Arc::new(Mutex::new(child)),
        });

        Ok(Self {
            id,
            title,
            state,
            kind: SessionKind::Local,
            io,
            done,
            emit,
        })
    }

    /// 起一个 SSH 会话：远端机器在本地看起来就是个普通会话
    pub fn spawn_ssh<F>(
        id: String,
        cols: u16,
        rows: u16,
        host: crate::hosts::Host,
        on_update: F,
        on_exit: impl Fn() + Send + 'static,
    ) -> anyhow::Result<Self>
    where
        F: Fn(TermUpdate) + Send + Sync + 'static,
    {
        let emit: Arc<dyn Fn(TermUpdate) + Send + Sync> = Arc::new(on_update);
        let title = Arc::new(Mutex::new(host.name.clone()));
        let done: Arc<Mutex<HashMap<String, String>>> = Arc::new(Mutex::new(HashMap::new()));
        let io_slot: Arc<Mutex<Option<Arc<dyn SessionIo>>>> = Arc::new(Mutex::new(None));

        let proxy = EventProxy {
            writer: Arc::new(Mutex::new(Box::new(LateWriter(io_slot.clone())))),
            title: title.clone(),
            done: done.clone(),
        };

        let size = TermSize {
            rows: rows as usize,
            cols: cols as usize,
        };
        let state = Arc::new(Mutex::new(TermState::new(size, proxy)));

        // SSH 通道的字节 → 同一个解析器 → 前端
        let state_cb = state.clone();
        let sid = id.clone();
        let emit_cb = emit.clone();
        let on_data = move |bytes: &[u8]| {
            let update = {
                let mut st = match state_cb.lock() {
                    Ok(g) => g,
                    Err(_) => return,
                };
                st.feed(bytes);
                st.take_update(&sid)
            };
            if let Some(u) = update {
                emit_cb(u);
            }
        };

        let io = crate::ssh::spawn_ssh_worker(host.clone(), cols, rows, on_data, on_exit)?;
        *io_slot
            .lock()
            .map_err(|_| anyhow::anyhow!("io 槽锁中毒"))? = Some(io.clone() as Arc<dyn SessionIo>);

        Ok(Self {
            id,
            title,
            state,
            kind: SessionKind::Ssh {
                host: format!("{}@{}:{}", host.user, host.hostname, host.port),
                label: host.name.clone(),
            },
            io,
            done,
            emit,
        })
    }

    /// exec 包装器在这个会话（远端 shell）里的状态
    pub fn exec_state(&self) -> u8 {
        self.state.lock().map(|s| s.tap.state).unwrap_or(EXEC_FAILED)
    }

    pub fn reset_exec_state(&self) {
        if let Ok(mut s) = self.state.lock() {
            s.tap.state = EXEC_NONE;
            s.tap.mute = None;
        }
    }

    /// 静默地往远端 shell 里送一行：在 marker 出现前，远端回来的字节都不上屏
    pub fn begin_inject(&self, token: &str, line: &[u8], timeout: Duration) -> anyhow::Result<()> {
        {
            let mut s = self.state.lock().map_err(|_| anyhow::anyhow!("state 锁中毒"))?;
            s.tap.state = EXEC_INJECTING;
            s.tap.mute = Some(Mute {
                marker: format!("\x1b]0;__DQI_{token}\x07").into_bytes(),
                buf: Vec::new(),
                deadline: Instant::now() + timeout,
            });
        }
        self.io.write(line)
    }

    /// 静默窗口超时检查（远端一直没有新字节时由 exec 轮询驱动）
    pub fn poll_tap(&self) {
        let update = match self.state.lock() {
            Ok(mut s) => {
                if s.poll_mute() {
                    s.take_update(&self.id)
                } else {
                    None
                }
            }
            Err(_) => None,
        };
        if let Some(u) = update {
            (self.emit)(u);
        }
    }

    pub fn start_capture(&self) {
        if let Ok(mut s) = self.state.lock() {
            s.tap.capture = Some(Vec::new());
        }
    }

    pub fn take_capture(&self) -> Option<Vec<u8>> {
        self.state.lock().ok()?.tap.capture.take()
    }

    /// 取出并清掉某个 exec token 的完成信号
    pub fn take_done(&self, token: &str) -> Option<String> {
        self.done.lock().ok()?.remove(token)
    }

    /// 键盘/程序输入
    pub fn write(&self, data: &[u8]) -> anyhow::Result<()> {
        self.io.write(data)
    }

    /// 改变窗口大小时同步给 PTY / SSH 和仿真器
    pub fn resize(&self, cols: u16, rows: u16) -> anyhow::Result<()> {
        self.io.resize(cols, rows)?;
        let mut st = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("state 锁中毒"))?;
        let size = TermSize {
            rows: rows as usize,
            cols: cols as usize,
        };
        st.size = size;
        st.term.resize(size);
        st.force_full = true;
        Ok(())
    }

    /// 强制一次整屏重绘（前端重连/首次挂载时用）
    pub fn force_full(&self, session_id: &str) -> Option<TermUpdate> {
        let mut st = self.state.lock().ok()?;
        st.force_full = true;
        st.take_update(session_id)
    }

    /// 给 MCP 用的纯文本快照
    /// 本地会话的 shell 进程号（SSH 会话没有，返回 None）
    pub fn pid(&self) -> Option<u32> {
        self.io.pid()
    }

    pub fn snapshot(&self) -> Option<ScreenSnapshot> {
        self.state.lock().ok().map(|st| st.snapshot())
    }

    /// 当前终端尺寸（分屏/新建标签页时给新会话用）
    pub fn size(&self) -> (u16, u16) {
        self.state
            .lock()
            .map(|s| (s.size.cols as u16, s.size.rows as u16))
            .unwrap_or((80, 24))
    }

    pub fn is_alive(&self) -> bool {
        self.io.is_alive()
    }

    pub fn kill(&self) {
        self.io.kill();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(cols: usize, rows: usize) -> TermState {
        let proxy = EventProxy {
            writer: Arc::new(Mutex::new(Box::new(std::io::sink()))),
            title: Arc::new(Mutex::new(String::new())),
            done: Arc::new(Mutex::new(HashMap::new())),
        };
        TermState::new(TermSize { cols, rows }, proxy)
    }

    /// 每段 run 的起始列必须等于前一段的列 + 前一段占的格数
    fn assert_columns(line: &LineUpdate) {
        let mut expect = 0u16;
        for r in &line.runs {
            assert!(
                r.col >= expect,
                "row {} run {:?} 起始列 {} 小于上一段结束列 {}",
                line.row, r.text, r.col, expect
            );
            let n = r.text.chars().count() as u16;
            expect = r.col + if r.flags & flags::WIDE != 0 { n * 2 } else { n };
        }
    }

    #[test]
    fn 宽字符占两格且列号连续() {
        let mut st = state(40, 4);
        st.feed(b"ab\xe4\xb8\xad\xe6\x96\x87cd|");
        let line = st.build_line(0);
        assert_columns(&line);
        let joined: String = line.runs.iter().map(|r| r.text.as_str()).collect();
        assert_eq!(joined, "ab中文cd|");
        let wide = line.runs.iter().find(|r| r.text == "中文").expect("中文 应单独成段");
        assert_ne!(wide.flags & flags::WIDE, 0, "中文那段必须带 WIDE");
        assert_eq!(wide.col, 2);
        // "cd|" 紧跟在 2 个宽字符之后 → 第 6 列
        assert_eq!(line.runs.iter().find(|r| r.text == "cd|").unwrap().col, 6);
    }

    #[test]
    fn 带底色的段不会把底色串到后面的空格() {
        let mut st = state(20, 2);
        // 蓝底 AB，恢复默认，两个空格，再 CD
        st.feed(b"\x1b[44mAB\x1b[0m  CD");
        let line = st.build_line(0);
        assert_columns(&line);
        let colored = &line.runs[0];
        assert_eq!(colored.text, "AB", "带底色的段只能包含 AB");
        assert_ne!(colored.bg, DEFAULT_BG);
        let cd = line.runs.iter().find(|r| r.text.contains("CD")).unwrap();
        assert_eq!(cd.bg, DEFAULT_BG);
        assert_eq!(cd.col, 4, "CD 必须落在第 4 列");
    }

    #[test]
    fn 行尾默认空白被裁掉() {
        let mut st = state(20, 2);
        st.feed(b"hi        ");
        let line = st.build_line(0);
        assert_eq!(line.runs.len(), 1);
        assert_eq!(line.runs[0].text, "hi");
    }

    #[test]
    fn 静默窗口吞掉注入回显并在标记后恢复() {
        let mut st = state(40, 4);
        st.tap.mute = Some(Mute {
            marker: b"\x1b]0;__DQI_t1\x07".to_vec(),
            buf: Vec::new(),
            deadline: Instant::now() + Duration::from_secs(5),
        });
        // 远端回显一长串定义，然后打标记，再打真正的输出
        st.feed(b"__dq(){ eval \"$2\"; }\r\n");
        assert_eq!(st.build_line(0).runs.len(), 0, "静默期间屏幕必须是空的");
        st.feed(b"\x1b]0;__DQI_t1\x07PROMPT$ ");
        assert_eq!(st.tap.state, EXEC_READY);
        let text: String = st.build_line(1).runs.iter().map(|r| r.text.as_str()).collect();
        assert_eq!(text, "PROMPT$", "标记之后的字节要正常上屏");
    }

    #[test]
    fn 静默窗口超时后把攒下的字节放出来() {
        let mut st = state(40, 4);
        st.tap.mute = Some(Mute {
            marker: b"\x1b]0;__DQI_zz\x07".to_vec(),
            buf: Vec::new(),
            deadline: Instant::now() - Duration::from_millis(1),
        });
        st.feed(b"fish: Unknown command");
        assert_eq!(st.tap.state, EXEC_FAILED);
        let text: String = st.build_line(0).runs.iter().map(|r| r.text.as_str()).collect();
        assert!(text.contains("Unknown command"), "超时要把吞掉的内容放出来，实际 {text:?}");
    }

    #[test]
    fn 完成信号不污染标题() {
        let proxy = EventProxy {
            writer: Arc::new(Mutex::new(Box::new(std::io::sink()))),
            title: Arc::new(Mutex::new(String::new())),
            done: Arc::new(Mutex::new(HashMap::new())),
        };
        let title = proxy.title.clone();
        let done = proxy.done.clone();
        let mut st = TermState::new(TermSize { cols: 20, rows: 2 }, proxy);
        st.feed(b"\x1b]0;\xe7\x9c\x9f\xe6\xa0\x87\xe9\xa2\x98\x07");
        assert_eq!(&*title.lock().unwrap(), "真标题");
        st.feed(b"\x1b]0;__DQD_abc_7\x07");
        assert_eq!(&*title.lock().unwrap(), "真标题", "完成信号不能覆盖标题");
        assert_eq!(done.lock().unwrap().get("abc").map(|s| s.as_str()), Some("7"));
    }

    /// 主人真实 starship 提示符：图标、emoji、色块、真彩色都在里面
    #[test]
    fn 真实提示符的列号连续且色块不串格() {
        let Ok(bytes) = std::fs::read("/tmp/dq-premium/prompt.ansi") else { return };
        let mut st = state(120, 10);
        st.feed(&bytes);
        let mut checked = 0;
        for row in 0..6u16 {
            let line = st.build_line(row);
            assert_columns(&line);
            if line.runs.is_empty() {
                continue;
            }
            checked += 1;
            eprintln!("--- 第 {row} 行 {} 段", line.runs.len());
            for r in &line.runs {
                eprintln!("col={:3} w={} fg={:06x} bg={:06x} fl={:02x} {:?}",
                    r.col, if r.flags & flags::WIDE != 0 { 2 } else { 1 }, r.fg, r.bg, r.flags, r.text);
            }
            // 默认底色的格子不许被卷进带底色的段（串色的根因）
            for r in &line.runs {
                if r.bg != DEFAULT_BG {
                    continue;
                }
                assert_eq!(r.flags & flags::WIDE == 0 && r.text.trim().is_empty(), false,
                    "默认底色的纯空白段不该单独存在：{:?}", r.text);
            }
        }
        assert!(checked >= 1, "提示符一行都没解析出来，测试无效");
        // 🚀 必须被识别成宽字符
        let all: Vec<Run> = (0..6u16).flat_map(|r| st.build_line(r).runs).collect();
        let rocket = all.iter().find(|r| r.text.contains('🚀'));
        if let Some(r) = rocket {
            assert_ne!(r.flags & flags::WIDE, 0, "🚀 必须带 WIDE（否则后面整行错位）");
        }
    }
}
