//! 外观设置：字号、字体、配色。
//!
//! 存在 ~/.daiqiao/appearance.json。改完立刻生效（界面重新量字宽、重算行列、
//! 通知 PTY 新尺寸），不需要重启。
//!
//! 配色用「预设名 + 可选自定义前景/背景」的形式，而不是让人填一堆十六进制：
//! 终端配色真正影响可读性的是前景/背景对比度，16 色调色板改坏了反而更难用。

use serde::{Deserialize, Serialize};

/// 字号的合理区间。再小看不清，再大一屏放不下几个字。
pub const MIN_FONT: u8 = 9;
pub const MAX_FONT: u8 = 28;
pub const DEFAULT_FONT: u8 = 13;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Appearance {
    /// 终端字号（px）
    #[serde(default = "default_font")]
    pub font_size: u8,
    /// 等宽字体族名。空 = 用内置的那串回退链
    #[serde(default)]
    pub font_family: String,
    /// 配色预设名，见 THEMES
    #[serde(default = "default_theme")]
    pub theme: String,
    /// 行高倍数（1.0–2.0），影响行间距松紧
    #[serde(default = "default_line_height")]
    pub line_height: f32,
    /// 光标形状：block / bar / underline
    #[serde(default = "default_cursor")]
    pub cursor_style: String,
    /// 界面语言：zh-CN / en / ja
    #[serde(default = "default_language")]
    pub language: String,
}

fn default_font() -> u8 {
    DEFAULT_FONT
}
fn default_theme() -> String {
    "石墨".to_string()
}
fn default_line_height() -> f32 {
    1.32
}
fn default_cursor() -> String {
    "block".to_string()
}
fn default_language() -> String {
    "zh-CN".to_string()
}

/// 界面支持的语言。名字保持各语言自己的写法，不翻译。
pub const LANGUAGES: &[(&str, &str)] = &[
    ("zh-CN", "简体中文"),
    ("zh-TW", "繁體中文"),
    ("en", "English"),
    ("ja", "日本語"),
    ("ko", "한국어"),
    ("de", "Deutsch"),
    ("fr", "Français"),
    ("es", "Español"),
];

impl Default for Appearance {
    fn default() -> Self {
        Self {
            font_size: DEFAULT_FONT,
            font_family: String::new(),
            theme: default_theme(),
            line_height: default_line_height(),
            cursor_style: default_cursor(),
            language: default_language(),
        }
    }
}

/// 内置配色。(名字, 背景, 前景, 说明)
///
/// 石墨是代敲自己的默认配色（跟界面同色系）；其余是终端界公认的几套，
/// 都按「前景/背景对比度够用」挑的，不搞花哨的低对比主题。
pub const THEMES: &[(&str, &str, &str, &str)] = &[
    ("石墨", "#151618", "#c5c8c6", "代敲默认，暖调深灰"),
    ("午夜", "#0d1117", "#c9d1d9", "偏蓝的深色，和 GitHub 深色一致"),
    ("纸白", "#fbfbfa", "#2e3338", "浅色，白天强光下用"),
    ("Solarized 深", "#002b36", "#93a1a1", "经典低蓝光深色"),
    ("Solarized 浅", "#fdf6e3", "#586e75", "经典暖浅色"),
    ("Dracula", "#282a36", "#f8f8f2", "高对比紫调"),
];

pub fn theme_colors(name: &str) -> (String, String) {
    THEMES
        .iter()
        .find(|(n, _, _, _)| *n == name)
        .map(|(_, bg, fg, _)| (bg.to_string(), fg.to_string()))
        .unwrap_or_else(|| {
            let (_, bg, fg, _) = THEMES[0];
            (bg.to_string(), fg.to_string())
        })
}

fn path() -> std::path::PathBuf {
    crate::keys::daiqiao_dir().join("appearance.json")
}

impl Appearance {
    pub fn load() -> Self {
        std::fs::read_to_string(path())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .map(|a: Appearance| a.clamped())
            .unwrap_or_default()
    }

    pub fn save(&self) -> Result<(), String> {
        let dir = crate::keys::daiqiao_dir();
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let text =
            serde_json::to_string_pretty(&self.clone().clamped()).map_err(|e| e.to_string())?;
        std::fs::write(path(), text).map_err(|e| e.to_string())
    }

    /// 把越界的值夹回合理范围。
    ///
    /// 这不是防御性编程的洁癖：配置文件是人和 AI 都能写的，
    /// 字号写成 200 会让一屏只剩两个字，写成 0 会直接除零。
    pub fn clamped(mut self) -> Self {
        self.font_size = self.font_size.clamp(MIN_FONT, MAX_FONT);
        self.line_height = if self.line_height.is_finite() {
            self.line_height.clamp(1.0, 2.0)
        } else {
            default_line_height()
        };
        if !THEMES.iter().any(|(n, _, _, _)| *n == self.theme) {
            self.theme = default_theme();
        }
        if !matches!(self.cursor_style.as_str(), "block" | "bar" | "underline") {
            self.cursor_style = default_cursor();
        }
        if !LANGUAGES.iter().any(|(code, _)| *code == self.language) {
            self.language = default_language();
        }
        self
    }

    /// 发给前端的完整外观（含预设解析出来的颜色）
    pub fn to_view(&self) -> serde_json::Value {
        let (bg, fg) = theme_colors(&self.theme);
        serde_json::json!({
            "font_size": self.font_size,
            "font_family": self.font_family,
            "theme": self.theme,
            "line_height": self.line_height,
            "cursor_style": self.cursor_style,
            "language": self.language,
            "languages": LANGUAGES.iter().map(|(code, name)| serde_json::json!({
                "code": code, "name": name
            })).collect::<Vec<_>>(),
            "bg": bg,
            "fg": fg,
            "themes": THEMES.iter().map(|(n,b,f,d)| serde_json::json!({
                "name": n, "bg": b, "fg": f, "desc": d
            })).collect::<Vec<_>>(),
            "min_font": MIN_FONT,
            "max_font": MAX_FONT,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 默认值合理() {
        let a = Appearance::default();
        assert_eq!(a.font_size, 13);
        assert_eq!(a.theme, "石墨");
        assert_eq!(a.cursor_style, "block");
        assert_eq!(a.language, "zh-CN");
        assert!((a.line_height - 1.32).abs() < 0.001);
    }

    #[test]
    fn 字号越界夹回区间() {
        let big = Appearance {
            font_size: 200,
            ..Default::default()
        }
        .clamped();
        assert_eq!(big.font_size, MAX_FONT, "超大字号要夹到上限");

        let zero = Appearance {
            font_size: 0,
            ..Default::default()
        }
        .clamped();
        assert_eq!(zero.font_size, MIN_FONT, "0 会导致除零，必须夹到下限");
    }

    #[test]
    fn 行高越界和非法值都处理() {
        let a = Appearance {
            line_height: 99.0,
            ..Default::default()
        }
        .clamped();
        assert!(a.line_height <= 2.0);

        let nan = Appearance {
            line_height: f32::NAN,
            ..Default::default()
        }
        .clamped();
        assert!(nan.line_height.is_finite(), "NaN 行高会让布局全崩");

        let inf = Appearance {
            line_height: f32::INFINITY,
            ..Default::default()
        }
        .clamped();
        assert!(inf.line_height.is_finite());
    }

    #[test]
    fn 未知主题退回默认() {
        let a = Appearance {
            theme: "不存在的主题".into(),
            ..Default::default()
        }
        .clamped();
        assert_eq!(a.theme, "石墨");
    }

    #[test]
    fn 未知光标形状退回默认() {
        let a = Appearance {
            cursor_style: "乱填".into(),
            ..Default::default()
        }
        .clamped();
        assert_eq!(a.cursor_style, "block");
    }

    #[test]
    fn 所有预设都有有效颜色() {
        for (name, bg, fg, _) in THEMES {
            assert!(bg.starts_with('#') && bg.len() == 7, "{name} 背景色格式错: {bg}");
            assert!(fg.starts_with('#') && fg.len() == 7, "{name} 前景色格式错: {fg}");
            let (b, f) = theme_colors(name);
            assert_eq!(&b, bg);
            assert_eq!(&f, fg);
        }
    }

    #[test]
    fn 未知主题取色退回第一个() {
        let (bg, fg) = theme_colors("没有这个");
        assert_eq!(bg, THEMES[0].1);
        assert_eq!(fg, THEMES[0].2);
    }

    #[test]
    fn 缺字段的旧配置能读() {
        // 老版本写的文件只有 font_size —— 不能因为缺字段就整个读不出来
        let a: Appearance = serde_json::from_str(r#"{"font_size":16}"#).unwrap();
        assert_eq!(a.font_size, 16);
        assert_eq!(a.theme, "石墨", "缺的字段要用默认值填上");
        assert_eq!(a.cursor_style, "block");
    }

    #[test]
    fn to_view带出颜色和预设表() {
        let v = Appearance::default().to_view();
        assert_eq!(v["bg"], "#151618");
        assert_eq!(v["fg"], "#c5c8c6");
        assert_eq!(v["themes"].as_array().unwrap().len(), THEMES.len());
        assert_eq!(v["min_font"], MIN_FONT);
    }

    #[test]
    fn 未知语言退回默认() {
        let a = Appearance {
            language: "xx-no-such".into(),
            ..Default::default()
        }
        .clamped();
        assert_eq!(a.language, "zh-CN");
    }

    #[test]
    fn 语言列表进入视图且旧配置默认中文() {
        let old: Appearance = serde_json::from_str(r#"{"font_size":16}"#).unwrap();
        assert_eq!(old.language, "zh-CN");
        let v = old.to_view();
        let langs = v["languages"].as_array().unwrap();
        assert_eq!(langs.len(), 8);
        assert!(langs.iter().any(|l| l["code"] == "en"));
        assert!(langs.iter().any(|l| l["code"] == "ja"));
    }
}
