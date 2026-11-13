//! Small re-implementations of Python standard-library behaviour the output files depend on:
//! `textwrap.wrap`, `difflib.get_close_matches`, `json.dumps`, and Python's text-file newlines.

use std::sync::LazyLock;

use fancy_regex::Regex;
use serde_json::Value;

// ------------------------------------------------------------------ textwrap.wrap

static WORDSEP: LazyLock<Regex> = LazyLock::new(|| {
    let ws = r"[\t\n\x0b\x0c\r ]";
    let wp = r#"[\w!"'&.,?]"#;
    let lt = r"[^\d\W]";
    let nws = r"[^\t\n\x0b\x0c\r ]";
    Regex::new(&format!(
        r"({ws}+|(?<={wp})-{{2,}}(?=\w)|{nws}+?(?:-(?:(?<={lt}{lt}-)|(?<={lt}-{lt}-))(?={lt}-?{lt})|(?={ws}|\z)|(?<={wp})(?=-{{2,}}\w)))"
    ))
    .unwrap()
});

fn clen(s: &str) -> usize {
    s.chars().count()
}

fn split_at_char(s: &str, n: usize) -> (&str, &str) {
    let b = s.char_indices().nth(n).map(|(i, _)| i).unwrap_or(s.len());
    s.split_at(b)
}

/// Python's `textwrap.wrap(text, width)` with default options.
pub fn wrap(text: &str, width: usize) -> Vec<String> {
    // expand_tabs + replace_whitespace
    let text: String = text.replace('\t', "        ").chars().map(|c| if "\n\x0b\x0c\r".contains(c) { ' ' } else { c }).collect();
    let mut chunks: Vec<String> = vec![];
    let mut last = 0;
    for m in WORDSEP.find_iter(&text).flatten() {
        if m.start() > last {
            chunks.push(text[last..m.start()].to_string());
        }
        chunks.push(m.as_str().to_string());
        last = m.end();
    }
    if last < text.len() {
        chunks.push(text[last..].to_string());
    }
    chunks.retain(|c| !c.is_empty());
    chunks.reverse();
    let mut lines: Vec<String> = vec![];
    while !chunks.is_empty() {
        let mut cur: Vec<String> = vec![];
        let mut cur_len = 0;
        if chunks.last().unwrap().trim().is_empty() && !lines.is_empty() {
            chunks.pop();
        }
        while let Some(c) = chunks.last() {
            let l = clen(c);
            if cur_len + l <= width {
                cur_len += l;
                cur.push(chunks.pop().unwrap());
            } else {
                break;
            }
        }
        if let Some(c) = chunks.last() {
            if clen(c) > width {
                // _handle_long_word
                let space_left = if width < 1 { 1 } else { width - cur_len };
                let chunk = c.clone();
                let mut end = space_left;
                if clen(&chunk) > space_left {
                    let head: String = chunk.chars().take(space_left).collect();
                    if let Some(h) = head.rfind('-') {
                        let hc = clen(&head[..h]);
                        if hc > 0 && head[..h].chars().any(|c| c != '-') {
                            end = hc + 1;
                        }
                    }
                }
                let (a, b) = split_at_char(&chunk, end);
                cur.push(a.to_string());
                *chunks.last_mut().unwrap() = b.to_string();
            }
        }
        if cur.last().is_some_and(|c| c.trim().is_empty()) {
            cur.pop();
        }
        if !cur.is_empty() {
            lines.push(cur.concat());
        }
    }
    lines
}

// ------------------------------------------------------------------ difflib

/// `difflib.SequenceMatcher(None, a, b).ratio()` (no junk; inputs here are far below the 200-item autojunk size).
pub fn seq_ratio(a: &str, b: &str) -> f64 {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let total = a.len() + b.len();
    if total == 0 {
        return 1.0;
    }
    let mut b2j: std::collections::HashMap<char, Vec<usize>> = Default::default();
    for (j, &c) in b.iter().enumerate() {
        b2j.entry(c).or_default().push(j);
    }
    let longest = |alo: usize, ahi: usize, blo: usize, bhi: usize| -> (usize, usize, usize) {
        let (mut bi, mut bj, mut bs) = (alo, blo, 0usize);
        let mut j2len: std::collections::HashMap<usize, usize> = Default::default();
        for i in alo..ahi {
            let mut new: std::collections::HashMap<usize, usize> = Default::default();
            if let Some(js) = b2j.get(&a[i]) {
                for &j in js {
                    if j < blo {
                        continue;
                    }
                    if j >= bhi {
                        break;
                    }
                    let k = j2len.get(&(j.wrapping_sub(1))).copied().unwrap_or(0) + 1;
                    new.insert(j, k);
                    if k > bs {
                        bi = i + 1 - k;
                        bj = j + 1 - k;
                        bs = k;
                    }
                }
            }
            j2len = new;
        }
        while bi > alo && bj > blo && a[bi - 1] == b[bj - 1] {
            bi -= 1;
            bj -= 1;
            bs += 1;
        }
        while bi + bs < ahi && bj + bs < bhi && a[bi + bs] == b[bj + bs] {
            bs += 1;
        }
        (bi, bj, bs)
    };
    let mut queue = vec![(0, a.len(), 0, b.len())];
    let mut matched = 0;
    while let Some((alo, ahi, blo, bhi)) = queue.pop() {
        let (i, j, k) = longest(alo, ahi, blo, bhi);
        if k > 0 {
            matched += k;
            if alo < i && blo < j {
                queue.push((alo, i, blo, j));
            }
            if i + k < ahi && j + k < bhi {
                queue.push((i + k, ahi, j + k, bhi));
            }
        }
    }
    2.0 * matched as f64 / total as f64
}

/// `difflib.get_close_matches(word, possibilities, n=1, cutoff)`: best ratio, ties go to the larger string.
pub fn close_match<'a>(word: &str, possibilities: impl IntoIterator<Item = &'a String>, cutoff: f64) -> Option<&'a String> {
    let mut best: Option<(f64, &String)> = None;
    for x in possibilities {
        let r = seq_ratio(x, word);
        if r >= cutoff && best.is_none_or(|(br, bx)| r > br || (r == br && x > bx)) {
            best = Some((r, x));
        }
    }
    best.map(|b| b.1)
}

// ------------------------------------------------------------------ json.dumps

/// Python's `repr(float)`.
pub fn float_repr(x: f64) -> String {
    if x.is_nan() {
        return "NaN".into();
    }
    if x.is_infinite() {
        return if x > 0.0 { "Infinity" } else { "-Infinity" }.into();
    }
    if x == 0.0 {
        return if x.is_sign_negative() { "-0.0" } else { "0.0" }.into();
    }
    let e = format!("{:e}", x); // shortest round-trip digits, e.g. "-1.234e5"
    let (mant, exp) = e.split_once('e').unwrap();
    let exp: i32 = exp.parse().unwrap();
    let neg = mant.starts_with('-');
    let digits: String = mant.chars().filter(|c| c.is_ascii_digit()).collect();
    let sign = if neg { "-" } else { "" };
    if (-4..16).contains(&exp) {
        let n = digits.len() as i32;
        let s = if exp >= n - 1 {
            format!("{}{}.0", digits, "0".repeat((exp - n + 1) as usize))
        } else if exp >= 0 {
            format!("{}.{}", &digits[..(exp + 1) as usize], &digits[(exp + 1) as usize..])
        } else {
            format!("0.{}{}", "0".repeat((-exp - 1) as usize), digits)
        };
        format!("{sign}{s}")
    } else {
        let m = if digits.len() > 1 { format!("{}.{}", &digits[..1], &digits[1..]) } else { digits };
        format!("{sign}{m}e{}{:02}", if exp < 0 { '-' } else { '+' }, exp.abs())
    }
}

fn json_str(s: &str, ascii: bool, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\x08' => out.push_str("\\b"),
            '\x0c' => out.push_str("\\f"),
            c if (c as u32) < 0x20 || (ascii && (c as u32) > 0x7e) => {
                let mut buf = [0u16; 2];
                for u in c.encode_utf16(&mut buf) {
                    out.push_str(&format!("\\u{:04x}", u));
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

fn dump(v: &Value, indent: Option<usize>, ascii: bool, level: usize, out: &mut String) {
    let nl = |out: &mut String, lvl: usize| {
        if let Some(n) = indent {
            out.push('\n');
            out.push_str(&" ".repeat(n * lvl));
        }
    };
    let sep = if indent.is_some() { "," } else { ", " };
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => {
            if n.is_f64() {
                out.push_str(&float_repr(n.as_f64().unwrap()))
            } else {
                out.push_str(&n.to_string())
            }
        }
        Value::String(s) => json_str(s, ascii, out),
        Value::Array(a) => {
            if a.is_empty() {
                out.push_str("[]");
                return;
            }
            out.push('[');
            for (i, x) in a.iter().enumerate() {
                if i > 0 {
                    out.push_str(sep);
                }
                nl(out, level + 1);
                dump(x, indent, ascii, level + 1, out);
            }
            nl(out, level);
            out.push(']');
        }
        Value::Object(m) => {
            if m.is_empty() {
                out.push_str("{}");
                return;
            }
            out.push('{');
            for (i, (k, x)) in m.iter().enumerate() {
                if i > 0 {
                    out.push_str(sep);
                }
                nl(out, level + 1);
                json_str(k, ascii, out);
                out.push_str(": ");
                dump(x, indent, ascii, level + 1, out);
            }
            nl(out, level);
            out.push('}');
        }
    }
}

/// `json.dumps(v, indent=indent, ensure_ascii=ascii)`
pub fn json_dumps(v: &Value, indent: Option<usize>, ascii: bool) -> String {
    let mut s = String::new();
    dump(v, indent, ascii, 0, &mut s);
    s
}

/// Like Python's `Path.write_text` on Windows: "\n" becomes "\r\n".
pub fn write_text(path: &std::path::Path, text: &str) -> std::io::Result<()> {
    std::fs::write(path, text.replace('\n', "\r\n"))
}

/// A JSON float that stays a float in the output (3600 -> 3600.0, as Python writes it).
pub fn f(x: f64) -> Value {
    Value::Number(serde_json::Number::from_f64(x).unwrap_or_else(|| serde_json::Number::from(0)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn floats_like_python() {
        for (x, s) in [(3600.0, "3600.0"), (0.99951171875, "0.99951171875"), (1794420214.1296, "1794420214.1296"), (1e-5, "1e-05"), (1e16, "1e+16"), (123456789012345.6, "123456789012345.6"), (0.0001, "0.0001"), (-2.5, "-2.5")] {
            assert_eq!(float_repr(x), s);
        }
    }

    #[test]
    fn wrap_like_python() {
        assert_eq!(wrap("They're fighting 5v4. Heraldo is at 6,000 HP.", 25), vec!["They're fighting 5v4.", "Heraldo is at 6,000 HP."]);
        assert_eq!(wrap("a well-known all-in play", 10), vec!["a well-", "known all-", "in play"]);
        assert_eq!(wrap("abcdefghijklmnop", 5), vec!["abcde", "fghij", "klmno", "p"]);
    }

    #[test]
    fn ratio_like_difflib() {
        assert!((seq_ratio("nafiri", "naafiri") - 12.0 / 13.0).abs() < 1e-12);
        assert_eq!(seq_ratio("abc", "xyz"), 0.0);
    }

    #[test]
    fn json_like_python() {
        let v: Value = serde_json::from_str(r#"{"a": [1, 2.5], "b": "ñ", "c": {}}"#).unwrap();
        assert_eq!(json_dumps(&v, None, false), r#"{"a": [1, 2.5], "b": "ñ", "c": {}}"#);
        assert_eq!(json_dumps(&v, Some(2), true), "{\n  \"a\": [\n    1,\n    2.5\n  ],\n  \"b\": \"\\u00f1\",\n  \"c\": {}\n}");
    }
}
