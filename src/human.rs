//! Human formatting helpers: sizes, durations, alignment and ANSI colour.

/// Bytes as `512 B`, `12.3 MiB` or `1.42 GiB`.
pub fn bytes(value: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = KIB * 1024.0;
    const GIB: f64 = MIB * 1024.0;
    let v = value as f64;
    if v >= GIB {
        format!("{:.2} GiB", v / GIB)
    } else if v >= MIB {
        format!("{:.1} MiB", v / MIB)
    } else if v >= KIB {
        format!("{:.0} KiB", v / KIB)
    } else {
        format!("{value} B")
    }
}

/// Duration as `42s`, `5m 12s`, `3h 04m` or `2d 11h`.
pub fn duration(secs: u64) -> String {
    if secs >= 86_400 {
        format!("{}d {:02}h", secs / 86_400, (secs % 86_400) / 3600)
    } else if secs >= 3600 {
        format!("{}h {:02}m", secs / 3600, (secs % 3600) / 60)
    } else if secs >= 60 {
        format!("{}m {:02}s", secs / 60, secs % 60)
    } else {
        format!("{secs}s")
    }
}

/// CPU percentage with one decimal; `-` when the sample has no history yet.
pub fn cpu(pct: f32) -> String {
    if pct <= 0.0 {
        "0.0%".to_string()
    } else if pct >= 100.0 {
        format!("{pct:.0}%")
    } else {
        format!("{pct:.1}%")
    }
}

/// Up to three ports, then `+N`.
pub fn ports(list: &[u16]) -> String {
    if list.is_empty() {
        return "-".to_string();
    }
    let shown: Vec<String> = list.iter().take(3).map(u16::to_string).collect();
    let mut text = shown.join(", ");
    if list.len() > 3 {
        text.push_str(&format!(" +{}", list.len() - 3));
    }
    text
}

/// Right-pad to a display width measured in characters.
pub fn pad(s: &str, width: usize) -> String {
    let len = s.chars().count();
    if len >= width {
        s.to_string()
    } else {
        format!("{s}{}", " ".repeat(width - len))
    }
}

/// Left-pad to a display width measured in characters.
pub fn pad_left(s: &str, width: usize) -> String {
    let len = s.chars().count();
    if len >= width {
        s.to_string()
    } else {
        format!("{}{s}", " ".repeat(width - len))
    }
}

/// ANSI styling, enabled only on a TTY with `NO_COLOR` unset.
#[derive(Clone, Copy, Debug, Default)]
pub struct Style {
    pub color: bool,
}

impl Style {
    fn wrap(self, code: &str, text: &str) -> String {
        if self.color {
            format!("\x1b[{code}m{text}\x1b[0m")
        } else {
            text.to_string()
        }
    }

    pub fn bold(self, text: &str) -> String {
        self.wrap("1", text)
    }

    pub fn dim(self, text: &str) -> String {
        self.wrap("2", text)
    }

    pub fn cyan(self, text: &str) -> String {
        self.wrap("36", text)
    }

    pub fn yellow(self, text: &str) -> String {
        self.wrap("33", text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(bytes(0), "0 B");
        assert_eq!(bytes(1024), "1 KiB");
        assert_eq!(bytes(1024 * 1024), "1.0 MiB");
        assert_eq!(bytes(3 * 1024 * 1024 * 1024 / 2), "1.50 GiB");
    }

    #[test]
    fn durations() {
        assert_eq!(duration(9), "9s");
        assert_eq!(duration(312), "5m 12s");
        assert_eq!(duration(3 * 3600 + 4 * 60), "3h 04m");
        assert_eq!(duration(2 * 86_400 + 11 * 3600), "2d 11h");
    }

    #[test]
    fn cpu_percent() {
        assert_eq!(cpu(0.0), "0.0%");
        assert_eq!(cpu(12.34), "12.3%");
        assert_eq!(cpu(250.0), "250%");
    }

    #[test]
    fn port_lists_collapse() {
        assert_eq!(ports(&[]), "-");
        assert_eq!(ports(&[5173]), "5173");
        assert_eq!(ports(&[1, 2, 3, 4, 5]), "1, 2, 3 +2");
    }

    #[test]
    fn padding_uses_char_count() {
        assert_eq!(pad("ab", 4), "ab  ");
        assert_eq!(pad_left("ab", 4), "  ab");
        assert_eq!(pad("abcd", 4), "abcd");
    }

    #[test]
    fn style_is_plain_without_color() {
        let style = Style { color: false };
        assert_eq!(style.bold("x"), "x");
        assert_eq!(Style { color: true }.dim("x"), "\x1b[2mx\x1b[0m");
    }
}
