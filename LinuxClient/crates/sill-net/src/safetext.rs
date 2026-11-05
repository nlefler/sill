//! `SafeText.label`: text a peer supplied about itself, as a one-line label. Rust's std has no Unicode
//! general categories, so "format" and "private use" are the explicit ranges below (the Cf block as of
//! Unicode 16, and the three private-use planes); unassigned code points are kept (Swift drops them).
//! Length is counted in `char`s, not grapheme clusters.

fn is_format(c: u32) -> bool {
    matches!(c,
        0x00AD | 0x0600..=0x0605 | 0x061C | 0x06DD | 0x070F | 0x0890..=0x0891 | 0x08E2 | 0x180E
        | 0x200B..=0x200F | 0x202A..=0x202E | 0x2060..=0x2064 | 0x2066..=0x206F | 0xFEFF
        | 0xFFF9..=0xFFFB | 0x110BD | 0x110CD | 0x13430..=0x1343F | 0x1BCA0..=0x1BCA3
        | 0x1D173..=0x1D17A | 0xE0001 | 0xE0020..=0xE007F)
}

fn is_private(c: u32) -> bool {
    matches!(c, 0xE000..=0xF8FF | 0xF0000..=0xFFFFD | 0x100000..=0x10FFFD)
}

pub fn label(text: &str, limit: usize) -> String {
    let mut out = String::new();
    let mut pending = false;
    for c in text.chars() {
        if c.is_whitespace() {
            pending = !out.is_empty();
            continue;
        }
        if c.is_control() || is_format(c as u32) || is_private(c as u32) {
            continue;
        }
        if pending {
            out.push(' ');
            pending = false;
        }
        out.push(c);
    }
    if out.chars().count() > limit {
        out = out.chars().take(limit).collect();
        while out.ends_with(' ') {
            out.pop();
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::label;

    #[test]
    fn cleans() {
        assert_eq!(label("  Tom \t\n  & Jerry ", 64), "Tom & Jerry");
        assert_eq!(label("iPad\u{202E}evil\u{200B}", 64), "iPadevil");
        assert_eq!(label("a\u{0007}b\u{00A0}c\u{3000}d", 64), "ab c d");
        assert_eq!(label("\u{200B}\n", 64), "");
        assert_eq!(label(&"x".repeat(100), 64).len(), 64);
        assert_eq!(label(&format!("{} y", "x".repeat(63)), 64), "x".repeat(63)); // trailing space trimmed
    }
}
