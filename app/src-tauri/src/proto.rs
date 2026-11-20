//! 前后端共享的数据结构。
//!
//! 设计要点：Rust 侧是唯一的解析方，前端只负责把收到的格子画出来。
//! 传输时按「样式相同的连续格子」合并成 run，避免逐格 JSON 开销。

use serde::{Deserialize, Serialize};

/// 样式位标志
pub mod flags {
    pub const BOLD: u32 = 1 << 0;
    pub const ITALIC: u32 = 1 << 1;
    pub const UNDERLINE: u32 = 1 << 2;
    pub const STRIKEOUT: u32 = 1 << 3;
    pub const INVERSE: u32 = 1 << 4;
    pub const DIM: u32 = 1 << 5;
    pub const HIDDEN: u32 = 1 << 6;
    /// 这段 run 里每个字符都占 2 格（中文 / emoji）
    pub const WIDE: u32 = 1 << 7;
}

/// 一段样式相同的连续格子
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Run {
    /// 起始列。前端按列定位，不再自己累加字宽
    pub col: u16,
    pub text: String,
    /// 前景色 0xRRGGBB（已由 Rust 侧解析成最终值）
    pub fg: u32,
    /// 背景色 0xRRGGBB
    pub bg: u32,
    pub flags: u32,
}

/// 一行的内容
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LineUpdate {
    pub row: u16,
    pub runs: Vec<Run>,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct CursorState {
    pub row: u16,
    pub col: u16,
    pub visible: bool,
    /// 0=方块 1=下划线 2=竖线 3=空心方块
    pub shape: u8,
}

/// 一次界面更新。`full=true` 时前端整屏重画，否则只画 `lines` 里的行。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TermUpdate {
    pub session: String,
    pub full: bool,
    pub cols: u16,
    pub rows: u16,
    pub lines: Vec<LineUpdate>,
    pub cursor: CursorState,
    /// 终端标题（供标签栏显示），仅在变化时有值
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

/// 给 MCP 用的纯文本快照
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScreenSnapshot {
    pub cols: u16,
    pub rows: u16,
    /// 可见区域，每行一个字符串（已去尾部空白）
    pub visible: Vec<String>,
    /// scrollback 里的历史行（不含可见区域）
    pub scrollback: Vec<String>,
    pub cursor: CursorState,
}
