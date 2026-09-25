//! Source text → tokens, each with its line.

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Tok {
    Num(f32),
    Ident(String),
    Sym(&'static str),
}

const SYMBOLS: [&str; 24] = [
    "+=", "-=", "*=", "/=", "==", "!=", "<=", ">=", "&&", "||", "+", "-", "*", "/", "%", "(", ")", ",", ";", "=", "<", ">", "!", "^",
];

/// `first_line` is the number of the source's first line (for a part of a larger file).
pub(crate) fn lex(source: &str, first_line: usize) -> Result<Vec<(Tok, usize)>, String> {
    let mut out = Vec::new();
    let bytes = source.as_bytes();
    let (mut i, mut line) = (0, first_line);
    while i < bytes.len() {
        let c = bytes[i] as char;
        if c == '\n' {
            line += 1;
            i += 1;
        } else if c.is_whitespace() {
            i += 1;
        } else if c == '#' || source[i..].starts_with("//") {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
        } else if c.is_ascii_digit() || (c == '.' && bytes.get(i + 1).is_some_and(u8::is_ascii_digit)) {
            let start = i;
            while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'.') {
                i += 1;
            }
            if i < bytes.len() && (bytes[i] == b'e' || bytes[i] == b'E') {
                i += 1;
                if i < bytes.len() && (bytes[i] == b'-' || bytes[i] == b'+') {
                    i += 1;
                }
                while i < bytes.len() && bytes[i].is_ascii_digit() {
                    i += 1;
                }
            }
            let text = &source[start..i];
            let n = text.parse::<f32>().map_err(|_| format!("line {line}: \"{text}\" isn't a number"))?;
            out.push((Tok::Num(n), line));
        } else if c.is_ascii_alphabetic() || c == '_' {
            let start = i;
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            out.push((Tok::Ident(source[start..i].to_string()), line));
        } else if let Some(s) = SYMBOLS.iter().find(|s| source[i..].starts_with(**s)) {
            out.push((Tok::Sym(s), line));
            i += s.len();
        } else {
            let ch = source[i..].chars().next().unwrap_or('?');
            return Err(format!("line {line}: unexpected \"{ch}\""));
        }
    }
    Ok(out)
}

pub(crate) fn is_ident(s: &str) -> bool {
    let mut chars = s.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_') && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}
