//! Workspace：标签页 + 分屏布局。
//!
//! 这是人和 AI **共用**的同一份布局状态 —— AI 通过 MCP 改它，人立刻在同一块屏上
//! 看到结果。布局的真相在内核里，前端只是它的一个渲染器。

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SplitDir {
    /// 上下分：新 pane 在下方
    Horizontal,
    /// 左右分：新 pane 在右侧
    Vertical,
}

impl SplitDir {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "horizontal" | "h" | "down" | "row" | "上下" | "下" => Some(SplitDir::Horizontal),
            "vertical" | "v" | "right" | "col" | "左右" | "右" => Some(SplitDir::Vertical),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            SplitDir::Horizontal => "horizontal",
            SplitDir::Vertical => "vertical",
        }
    }
}

/// 分屏布局树（二叉树，和 tmux 一个模型）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Layout {
    Leaf {
        session: String,
    },
    Split {
        dir: SplitDir,
        ratio: f64,
        a: Box<Layout>,
        b: Box<Layout>,
    },
}

impl Layout {
    pub fn leaf(session: &str) -> Self {
        Layout::Leaf {
            session: session.to_string(),
        }
    }

    pub fn sessions(&self, out: &mut Vec<String>) {
        match self {
            Layout::Leaf { session } => out.push(session.clone()),
            Layout::Split { a, b, .. } => {
                a.sessions(out);
                b.sessions(out);
            }
        }
    }

    pub fn contains(&self, target: &str) -> bool {
        match self {
            Layout::Leaf { session } => session == target,
            Layout::Split { a, b, .. } => a.contains(target) || b.contains(target),
        }
    }

    /// 把包含 target 的叶子换成一次分屏，新 pane 挂 `new_session`
    pub fn split_at(&mut self, target: &str, new_session: &str, dir: SplitDir) -> bool {
        match self {
            Layout::Leaf { session } if session == target => {
                let old = self.clone();
                *self = Layout::Split {
                    dir,
                    ratio: 0.5,
                    a: Box::new(old),
                    b: Box::new(Layout::leaf(new_session)),
                };
                true
            }
            Layout::Leaf { .. } => false,
            Layout::Split { a, b, .. } => {
                a.split_at(target, new_session, dir) || b.split_at(target, new_session, dir)
            }
        }
    }

    /// 摘掉一个叶子。父节点被另一个子节点顶替（保住树的形状）。
    pub fn remove(&mut self, target: &str) -> bool {
        let (a_is, b_is) = match self {
            Layout::Split { a, b, .. } => (
                matches!(a.as_ref(), Layout::Leaf { session } if session == target),
                matches!(b.as_ref(), Layout::Leaf { session } if session == target),
            ),
            Layout::Leaf { .. } => return false,
        };

        if a_is || b_is {
            let keep = match self {
                Layout::Split { a, b, .. } => {
                    if a_is {
                        (**b).clone()
                    } else {
                        (**a).clone()
                    }
                }
                _ => unreachable!(),
            };
            *self = keep;
            return true;
        }

        match self {
            Layout::Split { a, b, .. } => a.remove(target) || b.remove(target),
            Layout::Leaf { .. } => false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tab {
    pub id: String,
    pub title: String,
    pub layout: Layout,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Workspace {
    pub tabs: Vec<Tab>,
    pub active_tab: Option<String>,
    /// 键盘焦点所在的 pane（也是 exec 不指定 session 时的默认目标）
    pub focused: Option<String>,
}

impl Workspace {
    pub fn new_tab(&mut self, tab_id: String, session: String, title: String) {
        self.tabs.push(Tab {
            id: tab_id.clone(),
            title,
            layout: Layout::leaf(&session),
        });
        self.active_tab = Some(tab_id);
        self.focused = Some(session);
    }

    pub fn split(&mut self, at: &str, new_session: &str, dir: SplitDir) -> bool {
        for t in self.tabs.iter_mut() {
            if t.layout.contains(at) && t.layout.split_at(at, new_session, dir) {
                self.active_tab = Some(t.id.clone());
                self.focused = Some(new_session.to_string());
                return true;
            }
        }
        false
    }

    pub fn activate(&mut self, session: &str) -> bool {
        if let Some(t) = self.tabs.iter().find(|t| t.layout.contains(session)) {
            self.active_tab = Some(t.id.clone());
            self.focused = Some(session.to_string());
            return true;
        }
        false
    }

    pub fn set_title(&mut self, session: &str, title: &str) -> bool {
        for t in self.tabs.iter_mut() {
            if t.layout.contains(session) {
                t.title = title.to_string();
                return true;
            }
        }
        false
    }

    pub fn first_session_of(&self, tab_id: &str) -> Option<String> {
        self.tabs
            .iter()
            .find(|t| t.id == tab_id)
            .map(|t| {
                let mut v = Vec::new();
                t.layout.sessions(&mut v);
                v
            })
            .and_then(|v| v.first().cloned())
    }

    /// 移除一个会话；返回它的 tab 是否被整个关掉
    pub fn remove_session(&mut self, session: &str) -> bool {
        let mut closed_tab = false;
        let mut i = 0;
        while i < self.tabs.len() {
            let is_root = matches!(&self.tabs[i].layout, Layout::Leaf { session: s } if s == session);
            if is_root {
                self.tabs.remove(i);
                closed_tab = true;
                continue;
            }
            self.tabs[i].layout.remove(session);
            i += 1;
        }

        if closed_tab {
            let active_still_exists = self
                .active_tab
                .as_ref()
                .map(|id| self.tabs.iter().any(|t| &t.id == id))
                .unwrap_or(false);
            if !active_still_exists {
                self.active_tab = self.tabs.last().map(|t| t.id.clone());
                self.focused = self
                    .active_tab
                    .clone()
                    .and_then(|id| self.first_session_of(&id));
            }
        }

        // 焦点所在的 pane 被关掉了 → 落到当前 tab 的第一个 pane
        if self.focused.as_deref() == Some(session) {
            self.focused = self
                .active_tab
                .clone()
                .and_then(|id| self.first_session_of(&id));
        }
        closed_tab
    }

    pub fn all_sessions(&self) -> Vec<String> {
        let mut v = Vec::new();
        for t in &self.tabs {
            t.layout.sessions(&mut v);
        }
        v
    }

    /// 某个会话所属的 tab id
    pub fn tab_id_of(&self, session: &str) -> Option<String> {
        self.tabs
            .iter()
            .find(|t| t.layout.contains(session))
            .map(|t| t.id.clone())
    }
}
