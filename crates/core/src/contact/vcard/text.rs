/// Unescapes a vCard 3.0 TEXT value (RFC 2426 §5): `\n`/`\N` become a
/// newline; `\\`, `\,` and `\;` their literal character. Any other escaped
/// character is kept without the backslash.
pub(super) fn unescape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n' | 'N') => out.push('\n'),
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    out
}

/// Splits a structured value (`N`, `ORG`, `ADR`) on unescaped `;`. The
/// components stay escaped.
pub(super) fn split_components(value: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0;
    let mut escaped = false;
    for (i, c) in value.char_indices() {
        if escaped {
            escaped = false;
        } else if c == '\\' {
            escaped = true;
        } else if c == ';' {
            parts.push(&value[start..i]);
            start = i + 1;
        }
    }
    parts.push(&value[start..]);
    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unescape_handles_rfc_2426_escapes() {
        assert_eq!(unescape("a\\,b\\;c\\\\d\\ne\\Nf"), "a,b;c\\d\ne\nf");
        assert_eq!(unescape("trailing\\"), "trailing\\");
    }

    #[test]
    fn split_components_ignores_escaped_semicolons() {
        assert_eq!(split_components("Smith\\; Sons;Sales;"), ["Smith\\; Sons", "Sales", ""]);
        assert_eq!(split_components("single"), ["single"]);
    }
}
