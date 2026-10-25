//! Just enough JSON for the link's messages: one object per line.

use std::fmt::Write as _;

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Value {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Array(Vec<Value>),
    /// Keys keep their order, so written messages read the way they were built.
    Object(Vec<(String, Value)>),
}

static NULL: Value = Value::Null;

impl Value {
    /// The value under `key`, or `Null` when there is none (or this is not an object).
    pub(crate) fn get(&self, key: &str) -> &Value {
        match self {
            Value::Object(fields) => fields.iter().find(|(k, _)| k == key).map_or(&NULL, |(_, v)| v),
            _ => &NULL,
        }
    }

    pub(crate) fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(s) => Some(s),
            _ => None,
        }
    }

    pub(crate) fn as_array(&self) -> &[Value] {
        match self {
            Value::Array(items) => items,
            _ => &[],
        }
    }

    pub(crate) fn parse(text: &str) -> Result<Value, String> {
        let mut p = Parser { s: text.as_bytes(), i: 0 };
        p.skip_whitespace();
        let v = p.value()?;
        p.skip_whitespace();
        if p.i != p.s.len() {
            return Err(format!("trailing data at byte {}", p.i));
        }
        Ok(v)
    }

    fn write(&self, out: &mut String) {
        match self {
            Value::Null => out.push_str("null"),
            Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Value::Number(n) if n.is_finite() => {
                let _ = write!(out, "{n}");
            }
            Value::Number(_) => out.push_str("null"),
            Value::String(s) => write_string(s, out),
            Value::Array(items) => {
                out.push('[');
                for (i, v) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    v.write(out);
                }
                out.push(']');
            }
            Value::Object(fields) => {
                out.push('{');
                for (i, (k, v)) in fields.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    write_string(k, out);
                    out.push(':');
                    v.write(out);
                }
                out.push('}');
            }
        }
    }
}

impl std::fmt::Display for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut out = String::new();
        self.write(&mut out);
        f.write_str(&out)
    }
}

impl From<&str> for Value {
    fn from(s: &str) -> Value {
        Value::String(s.to_owned())
    }
}

/// An object from `(key, value)` pairs, in that order.
pub(crate) fn object<const N: usize>(fields: [(&str, Value); N]) -> Value {
    Value::Object(fields.into_iter().map(|(k, v)| (k.to_owned(), v)).collect())
}

fn write_string(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

struct Parser<'s> {
    s: &'s [u8],
    i: usize,
}

impl Parser<'_> {
    fn skip_whitespace(&mut self) {
        while self.s.get(self.i).is_some_and(u8::is_ascii_whitespace) {
            self.i += 1;
        }
    }

    fn expect(&mut self, b: u8) -> Result<(), String> {
        if self.s.get(self.i) == Some(&b) {
            self.i += 1;
            Ok(())
        } else {
            Err(format!("expected '{}' at byte {}", b as char, self.i))
        }
    }

    fn literal(&mut self, word: &str, v: Value) -> Result<Value, String> {
        if self.s[self.i..].starts_with(word.as_bytes()) {
            self.i += word.len();
            Ok(v)
        } else {
            Err(format!("unexpected token at byte {}", self.i))
        }
    }

    fn value(&mut self) -> Result<Value, String> {
        match self.s.get(self.i) {
            Some(b'{') => self.object(),
            Some(b'[') => self.array(),
            Some(b'"') => self.string().map(Value::String),
            Some(b't') => self.literal("true", Value::Bool(true)),
            Some(b'f') => self.literal("false", Value::Bool(false)),
            Some(b'n') => self.literal("null", Value::Null),
            Some(_) => self.number(),
            None => Err("unexpected end of input".into()),
        }
    }

    fn object(&mut self) -> Result<Value, String> {
        self.expect(b'{')?;
        let mut fields = Vec::new();
        self.skip_whitespace();
        if self.s.get(self.i) == Some(&b'}') {
            self.i += 1;
            return Ok(Value::Object(fields));
        }
        loop {
            self.skip_whitespace();
            let key = self.string()?;
            self.skip_whitespace();
            self.expect(b':')?;
            self.skip_whitespace();
            fields.push((key, self.value()?));
            self.skip_whitespace();
            match self.s.get(self.i) {
                Some(b',') => self.i += 1,
                Some(b'}') => {
                    self.i += 1;
                    return Ok(Value::Object(fields));
                }
                _ => return Err(format!("expected ',' or '}}' at byte {}", self.i)),
            }
        }
    }

    fn array(&mut self) -> Result<Value, String> {
        self.expect(b'[')?;
        let mut items = Vec::new();
        self.skip_whitespace();
        if self.s.get(self.i) == Some(&b']') {
            self.i += 1;
            return Ok(Value::Array(items));
        }
        loop {
            self.skip_whitespace();
            items.push(self.value()?);
            self.skip_whitespace();
            match self.s.get(self.i) {
                Some(b',') => self.i += 1,
                Some(b']') => {
                    self.i += 1;
                    return Ok(Value::Array(items));
                }
                _ => return Err(format!("expected ',' or ']' at byte {}", self.i)),
            }
        }
    }

    fn number(&mut self) -> Result<Value, String> {
        let start = self.i;
        while self.s.get(self.i).is_some_and(|b| b"+-.eE0123456789".contains(b)) {
            self.i += 1;
        }
        std::str::from_utf8(&self.s[start..self.i])
            .ok()
            .and_then(|t| t.parse().ok())
            .map(Value::Number)
            .ok_or_else(|| format!("bad number at byte {start}"))
    }

    fn hex4(&mut self) -> Result<u32, String> {
        let digits = self.s.get(self.i..self.i + 4).ok_or("short \\u escape")?;
        let n = std::str::from_utf8(digits)
            .ok()
            .and_then(|t| u32::from_str_radix(t, 16).ok())
            .ok_or_else(|| format!("bad \\u escape at byte {}", self.i))?;
        self.i += 4;
        Ok(n)
    }

    fn string(&mut self) -> Result<String, String> {
        self.expect(b'"')?;
        let mut out = String::new();
        loop {
            let start = self.i;
            while self.s.get(self.i).is_some_and(|b| *b != b'"' && *b != b'\\') {
                self.i += 1;
            }
            out.push_str(std::str::from_utf8(&self.s[start..self.i]).map_err(|_| "invalid UTF-8 in string")?);
            match self.s.get(self.i) {
                Some(b'"') => {
                    self.i += 1;
                    return Ok(out);
                }
                Some(b'\\') => {
                    self.i += 1;
                    let escape = *self.s.get(self.i).ok_or("unterminated escape")?;
                    self.i += 1;
                    match escape {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let mut code = self.hex4()?;
                            if (0xD800..0xDC00).contains(&code) && self.s[self.i..].starts_with(b"\\u") {
                                self.i += 2;
                                let low = self.hex4()?;
                                code = 0x10000 + ((code - 0xD800) << 10) + (low.wrapping_sub(0xDC00) & 0x3FF);
                            }
                            out.push(char::from_u32(code).unwrap_or('\u{FFFD}'));
                        }
                        _ => return Err(format!("bad escape at byte {}", self.i - 1)),
                    }
                }
                _ => return Err("unterminated string".into()),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_a_message() {
        let v = object([
            ("type", "confirmation".into()),
            ("text", "line one\n\"quoted\" \u{1}".into()),
            ("choices", Value::Array(vec![object([("id", "ok".into())])])),
        ]);
        let text = v.to_string();
        assert_eq!(Value::parse(&text), Ok(v), "{text}");
    }

    #[test]
    fn reads_escapes_and_nesting() {
        let v = Value::parse(r#" {"a": [1, 2.5, true, null], "b": "é😀", "c": {}} "#).unwrap();
        assert_eq!(v.get("a").as_array().len(), 4);
        assert_eq!(v.get("b").as_str(), Some("é😀"));
        assert_eq!(v.get("missing"), &Value::Null);
        assert!(Value::parse("{\"a\": 1} x").is_err());
        assert!(Value::parse("{\"a\": }").is_err());
    }
}
