use std::ops::Range;

use super::{Param, Property, VCardError};

/// A leading UTF-8 BOM, skipped for parsing only: `as_bytes` still returns
/// it, and the first property's span still starts after it.
const BOM: &[u8] = &[0xEF, 0xBB, 0xBF];

/// Parses and structurally validates one card.
pub(super) fn parse(raw: &[u8]) -> Result<Vec<Property>, VCardError> {
    let properties = unfold(raw).into_iter().map(parse_line).collect::<Result<Vec<_>, _>>()?;
    check_structure(&properties)?;
    Ok(properties)
}

struct LogicalLine {
    span: Range<usize>,
    line: usize,
    bytes: Vec<u8>,
}

/// Splits `raw` into unfolded lines. Works on bytes, not `str`, because
/// servers fold at 75 octets and may split a multi-byte UTF-8 character
/// across the fold. Accepts CRLF and bare LF; skips blank lines.
fn unfold(raw: &[u8]) -> Vec<LogicalLine> {
    let mut lines: Vec<LogicalLine> = Vec::new();
    let mut start = if raw.starts_with(BOM) { BOM.len() } else { 0 };
    let mut line = 0;

    while start < raw.len() {
        line += 1;
        let end = raw[start..].iter().position(|&b| b == b'\n').map_or(raw.len(), |i| start + i + 1);
        let physical = &raw[start..end];
        let content = physical.strip_suffix(b"\n").unwrap_or(physical);
        let content = content.strip_suffix(b"\r").unwrap_or(content);

        match (content.split_first(), lines.last_mut()) {
            (Some((b' ' | b'\t', rest)), Some(previous)) => {
                previous.bytes.extend_from_slice(rest);
                previous.span.end = end;
            }
            (None, _) => {}
            _ => lines.push(LogicalLine {
                span: start..end,
                line,
                bytes: content.to_vec(),
            }),
        }
        start = end;
    }

    lines
}

fn parse_line(logical: LogicalLine) -> Result<Property, VCardError> {
    let LogicalLine { span, line, bytes } = logical;
    let text = String::from_utf8(bytes).map_err(|_| VCardError::InvalidUtf8 { line })?;
    let colon = find_unquoted(&text, b':').ok_or(VCardError::MalformedLine { line })?;
    let (head, value) = (&text[..colon], &text[colon + 1..]);

    let mut segments = split_unquoted(head, b';').into_iter();
    let qualified = segments.next().unwrap_or_default();
    let (group, name) = match qualified.split_once('.') {
        Some((group, name)) => (Some(group), name),
        None => (None, qualified),
    };
    if !is_token(name) || group.is_some_and(|g| !is_token(g)) {
        return Err(VCardError::MalformedLine { line });
    }
    let params = segments
        .filter(|segment| !segment.is_empty())
        .map(|segment| parse_param(segment).ok_or(VCardError::MalformedLine { line }))
        .collect::<Result<Vec<_>, _>>()?;

    Ok(Property {
        span,
        line,
        group: group.map(str::to_owned),
        name: name.to_owned(),
        params,
        value: value.to_owned(),
    })
}

fn parse_param(segment: &str) -> Option<Param> {
    if segment.is_empty() {
        return None;
    }
    let (name, values) = segment.split_once('=').unwrap_or(("TYPE", segment));
    if !is_token(name) {
        return None;
    }

    Some(Param {
        name: name.to_owned(),
        values: split_unquoted(values, b',').into_iter().map(unquote).map(str::to_owned).collect(),
    })
}

/// Names and groups: letters, digits, `-` (RFC 2426), plus `_` for leniency
/// towards vendor extensions.
fn is_token(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn unquote(value: &str) -> &str {
    value.strip_prefix('"').and_then(|v| v.strip_suffix('"')).unwrap_or(value)
}

/// Byte index of the first `target` outside double quotes.
fn find_unquoted(s: &str, target: u8) -> Option<usize> {
    let mut quoted = false;
    s.bytes().position(|b| {
        if b == b'"' {
            quoted = !quoted;
        }
        !quoted && b == target
    })
}

/// Splits on `delimiter` outside double quotes.
fn split_unquoted(s: &str, delimiter: u8) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0;
    let mut quoted = false;
    for (i, b) in s.bytes().enumerate() {
        if b == b'"' {
            quoted = !quoted;
        } else if b == delimiter && !quoted {
            parts.push(&s[start..i]);
            start = i + 1;
        }
    }
    parts.push(&s[start..]);
    parts
}

fn check_structure(properties: &[Property]) -> Result<(), VCardError> {
    let is_marker = |p: &Property, name: &str| p.group.is_none() && p.is(name) && p.value.trim().eq_ignore_ascii_case("VCARD");

    match properties.first() {
        Some(first) if is_marker(first, "BEGIN") => {}
        _ => return Err(VCardError::MissingBegin),
    }
    if properties[1..].iter().any(|p| is_marker(p, "BEGIN")) {
        return Err(VCardError::MultipleCards);
    }
    let end = properties.iter().position(|p| is_marker(p, "END")).ok_or(VCardError::MissingEnd)?;
    if let Some(extra) = properties.get(end + 1) {
        return Err(VCardError::TrailingContent { line: extra.line });
    }

    let version = properties.iter().find(|p| p.is("VERSION")).ok_or(VCardError::MissingVersion)?;
    match version.value.trim() {
        "3.0" => Ok(()),
        other => Err(VCardError::UnsupportedVersion {
            version: sanitize_version(other),
        }),
    }
}

/// The version comes from untrusted input; keep only what a version number
/// can contain so the error can never carry card content.
fn sanitize_version(version: &str) -> String {
    version.chars().filter(|c| c.is_ascii_digit() || *c == '.').take(8).collect()
}
