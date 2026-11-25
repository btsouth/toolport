//! Terminal output: color and Unicode detection, aligned tables, bars and number formats.
//!
//! Everything a command prints goes through [`Style`], so `--no-color`, `NO_COLOR`, a pipe
//! or a dumb terminal all produce plain text with the same layout.

use std::io::IsTerminal as _;

/// How output is drawn. Built once per run from the flags and the environment.
#[derive(Debug, Clone, Copy)]
pub struct Style {
    pub color: bool,
    pub unicode: bool,
    pub width: usize,
}

impl Style {
    /// Color only on a terminal, never with `NO_COLOR` set or `TERM=dumb`. `FORCE_COLOR` or
    /// `CLICOLOR_FORCE` turn it on for pipes (CI logs that render ANSI).
    pub fn detect(no_color: bool) -> Self {
        let var = |key: &str| std::env::var(key).ok().filter(|v| !v.is_empty());
        let tty = std::io::stdout().is_terminal();
        let dumb = var("TERM").is_some_and(|t| t == "dumb");
        let forced = var("FORCE_COLOR").is_some_and(|v| v != "0")
            || var("CLICOLOR_FORCE").is_some_and(|v| v != "0");
        let color = !no_color && var("NO_COLOR").is_none() && (forced || (tty && !dumb));
        Style {
            color,
            unicode: unicode_terminal(),
            width: terminal_width().clamp(60, 120),
        }
    }

    /// Plain output with no escapes: used for tests, JSON side text and the hook line.
    pub fn plain() -> Self {
        Style {
            color: false,
            unicode: true,
            width: 100,
        }
    }

    fn paint(&self, code: &str, text: &str) -> String {
        if self.color && !text.is_empty() {
            format!("\x1b[{code}m{text}\x1b[0m")
        } else {
            text.to_string()
        }
    }

    pub fn bold(&self, text: &str) -> String {
        self.paint("1", text)
    }
    pub fn dim(&self, text: &str) -> String {
        self.paint("2", text)
    }
    pub fn red(&self, text: &str) -> String {
        self.paint("31", text)
    }
    pub fn green(&self, text: &str) -> String {
        self.paint("32", text)
    }
    pub fn yellow(&self, text: &str) -> String {
        self.paint("33", text)
    }
    pub fn cyan(&self, text: &str) -> String {
        self.paint("36", text)
    }
    pub fn magenta(&self, text: &str) -> String {
        self.paint("35", text)
    }

    /// A section heading.
    pub fn heading(&self, title: &str) -> String {
        format!("\n{}\n", self.bold(title))
    }

    /// The colored marker for a severity.
    pub fn mark(&self, severity: Severity) -> String {
        let (u, a) = match severity {
            Severity::Error => ("✗", "x"),
            Severity::Warn => ("!", "!"),
            Severity::Info => ("·", "-"),
            Severity::Ok => ("✓", "+"),
        };
        let symbol = if self.unicode { u } else { a };
        match severity {
            Severity::Error => self.red(symbol),
            Severity::Warn => self.yellow(symbol),
            Severity::Info => self.dim(symbol),
            Severity::Ok => self.green(symbol),
        }
    }

    /// An actionable next step, as a dimmed arrow line.
    pub fn hint(&self, text: &str) -> String {
        let arrow = if self.unicode { "→" } else { "->" };
        format!("{} {}", self.cyan(arrow), text)
    }

    /// A command name inside prose.
    pub fn cmd(&self, text: &str) -> String {
        self.cyan(text)
    }

    /// A horizontal bar `frac` of `width` cells long, with eighth-block precision.
    pub fn bar(&self, frac: f64, width: usize) -> String {
        let frac = if frac.is_finite() {
            frac.clamp(0.0, 1.0)
        } else {
            0.0
        };
        if !self.unicode {
            let filled = (frac * width as f64).round() as usize;
            return format!("{}{}", "#".repeat(filled), " ".repeat(width - filled));
        }
        let eighths = (frac * width as f64 * 8.0).round() as usize;
        let full = eighths / 8;
        let part = eighths % 8;
        let mut out = "█".repeat(full);
        if part > 0 && full < width {
            out.push(['▏', '▎', '▍', '▌', '▋', '▊', '▉'][part - 1]);
        }
        let used = full + usize::from(part > 0 && full < width);
        out.push_str(&" ".repeat(width.saturating_sub(used)));
        out
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warn,
    Info,
    Ok,
}

impl Severity {
    pub fn from_level(level: &str) -> Self {
        match level {
            "error" => Severity::Error,
            "warn" | "warning" => Severity::Warn,
            _ => Severity::Info,
        }
    }
}

fn unicode_terminal() -> bool {
    if cfg!(windows) {
        return true;
    }
    if std::env::var("TERM").is_ok_and(|t| t == "dumb" || t == "linux") {
        return false;
    }
    for key in ["LC_ALL", "LC_CTYPE", "LANG"] {
        if let Ok(value) = std::env::var(key) {
            if !value.is_empty() {
                let upper = value.to_ascii_uppercase();
                return upper.contains("UTF-8") || upper.contains("UTF8");
            }
        }
    }
    true
}

fn terminal_width() -> usize {
    if let Some(cols) = std::env::var("COLUMNS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
    {
        return cols;
    }
    #[cfg(unix)]
    {
        // SAFETY: TIOCGWINSZ fills a plain struct; a failed call leaves it zeroed.
        let mut size: libc::winsize = unsafe { std::mem::zeroed() };
        let ok = unsafe { libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut size) } == 0;
        if ok && size.ws_col > 0 {
            return size.ws_col as usize;
        }
    }
    100
}

/// Display width of text that may carry ANSI color codes. Counts characters, which is right
/// for the box drawing, block and Latin text these commands print.
pub fn visible_width(text: &str) -> usize {
    let mut width = 0;
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            for next in chars.by_ref() {
                if next == 'm' {
                    break;
                }
            }
        } else {
            width += 1;
        }
    }
    width
}

/// Cut text to `max` visible characters with an ellipsis. Expects text without escapes.
pub fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let keep = max.saturating_sub(1);
    let mut out: String = text.chars().take(keep).collect();
    out.push('…');
    out
}

/// Keep the end of a path, which is the part that tells files apart.
pub fn truncate_left(text: &str, max: usize) -> String {
    let count = text.chars().count();
    if count <= max {
        return text.to_string();
    }
    let skip = count - max.saturating_sub(1);
    format!("…{}", text.chars().skip(skip).collect::<String>())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Left,
    Right,
}

/// Rows of cells laid out in aligned columns with two spaces between them, indented.
pub fn table(rows: &[Vec<String>], aligns: &[Align], indent: usize) -> String {
    let columns = rows.iter().map(Vec::len).max().unwrap_or(0);
    let mut widths = vec![0; columns];
    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            widths[i] = widths[i].max(visible_width(cell));
        }
    }
    let mut out = String::new();
    for row in rows {
        let mut line = " ".repeat(indent);
        for (i, cell) in row.iter().enumerate() {
            let pad = widths[i] - visible_width(cell);
            let align = aligns.get(i).copied().unwrap_or(Align::Left);
            let last = i + 1 == row.len();
            match align {
                Align::Left if last => line.push_str(cell),
                Align::Left => {
                    line.push_str(cell);
                    line.push_str(&" ".repeat(pad));
                }
                Align::Right => {
                    line.push_str(&" ".repeat(pad));
                    line.push_str(cell);
                }
            }
            if !last {
                line.push_str("  ");
            }
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

/// `1.2M`, `220k`, `512`: compact token and count figures.
pub fn compact(n: u64) -> String {
    let n = n as f64;
    if n >= 1e9 {
        format!("{:.1}B", n / 1e9)
    } else if n >= 1e6 {
        format!("{:.1}M", n / 1e6)
    } else if n >= 1e4 {
        format!("{:.0}k", n / 1e3)
    } else if n >= 1e3 {
        format!("{:.1}k", n / 1e3)
    } else {
        format!("{n:.0}")
    }
}

/// `12,345`.
pub fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// `12.3%`, or `<0.1%` for a nonzero sliver.
pub fn percent(frac: f64) -> String {
    if !frac.is_finite() || frac <= 0.0 {
        "0%".into()
    } else if frac < 0.001 {
        "<0.1%".into()
    } else if frac >= 0.995 {
        "100%".into()
    } else {
        format!("{:.1}%", frac * 100.0)
    }
}

/// `$1,234.56`, `$0.42`.
pub fn money(dollars: f64) -> String {
    if !dollars.is_finite() || dollars <= 0.0 {
        return "$0".into();
    }
    if dollars < 0.01 {
        return "<$0.01".into();
    }
    let cents = (dollars * 100.0).round() as u64;
    format!("${}.{:02}", thousands(cents / 100), cents % 100)
}

/// `3.2 MB`.
pub fn bytes(n: u64) -> String {
    let n = n as f64;
    if n >= 1024.0 * 1024.0 * 1024.0 {
        format!("{:.1} GB", n / 1024.0 / 1024.0 / 1024.0)
    } else if n >= 1024.0 * 1024.0 {
        format!("{:.1} MB", n / 1024.0 / 1024.0)
    } else if n >= 1024.0 {
        format!("{:.1} KB", n / 1024.0)
    } else {
        format!("{n:.0} B")
    }
}

/// `1 thing`, `2 things`.
pub fn plural(n: usize, one: &str, many: &str) -> String {
    format!(
        "{} {}",
        thousands(n as u64),
        if n == 1 { one } else { many }
    )
}

/// `~/x` for a path under home.
pub fn home_relative(path: &std::path::Path) -> String {
    if let Some(home) = crate::clients::home() {
        if let Ok(rest) = path.strip_prefix(&home) {
            return if rest.as_os_str().is_empty() {
                "~".into()
            } else {
                format!("~{}{}", std::path::MAIN_SEPARATOR, rest.display())
            };
        }
    }
    path.display().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bar_has_fixed_width_and_eighths() {
        let style = Style::plain();
        for frac in [0.0, 0.01, 0.33, 0.5, 0.999, 1.0, 2.0, f64::NAN] {
            assert_eq!(style.bar(frac, 20).chars().count(), 20, "{frac}");
        }
        assert_eq!(style.bar(0.5, 4), "██  ");
        assert_eq!(style.bar(1.0 / 32.0, 4), "▏   ");
        let ascii = Style {
            unicode: false,
            ..style
        };
        assert_eq!(ascii.bar(0.5, 4), "##  ");
    }

    #[test]
    fn table_aligns_columns_ignoring_color() {
        let style = Style {
            color: true,
            ..Style::plain()
        };
        let rows = vec![
            vec![style.red("a"), "1".into(), "x".into()],
            vec!["bbb".into(), "100".into(), "y".into()],
        ];
        let out = table(&rows, &[Align::Left, Align::Right, Align::Left], 2);
        let plain: Vec<String> = out
            .lines()
            .map(|l| {
                let mut s = String::new();
                let mut skip = false;
                for c in l.chars() {
                    if c == '\x1b' {
                        skip = true;
                    } else if skip && c == 'm' {
                        skip = false;
                    } else if !skip {
                        s.push(c);
                    }
                }
                s
            })
            .collect();
        assert_eq!(plain, vec!["  a      1  x", "  bbb  100  y"]);
    }

    #[test]
    fn number_formats() {
        assert_eq!(compact(512), "512");
        assert_eq!(compact(1_234), "1.2k");
        assert_eq!(compact(220_000), "220k");
        assert_eq!(compact(3_400_000), "3.4M");
        assert_eq!(thousands(1_234_567), "1,234,567");
        assert_eq!(percent(0.1234), "12.3%");
        assert_eq!(percent(0.0001), "<0.1%");
        assert_eq!(money(1234.567), "$1,234.57");
        assert_eq!(money(0.001), "<$0.01");
        assert_eq!(truncate("abcdef", 4), "abc…");
        assert_eq!(truncate_left("abcdef", 4), "…def");
        assert_eq!(plural(1, "file", "files"), "1 file");
    }

    #[test]
    fn plain_style_has_no_escapes() {
        let style = Style::plain();
        assert_eq!(style.red("x"), "x");
        assert_eq!(style.mark(Severity::Error), "✗");
    }
}
