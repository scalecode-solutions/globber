// Minimal JSON reading for `--prefer-from` lines: one flat object of
// scalar values. Not a general JSON parser — nested values are rejected.

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Value {
    Str(String),
    Num(f64),
    Bool(bool),
    Null,
}

impl Value {
    pub(crate) fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }

    pub(crate) fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Num(n) => Some(*n),
            _ => None,
        }
    }
}

/// Parse `{"key": scalar, ...}`. Returns None on anything else.
pub(crate) fn parse_flat_object(s: &str) -> Option<Vec<(String, Value)>> {
    let mut p = Parser { chars: s.trim().chars().peekable() };
    p.expect('{')?;
    let mut out = Vec::new();
    p.ws();
    if p.peek() == Some('}') {
        p.next();
        return p.end().then_some(out);
    }
    loop {
        p.ws();
        let key = p.string()?;
        p.ws();
        p.expect(':')?;
        p.ws();
        let value = p.value()?;
        out.push((key, value));
        p.ws();
        match p.next()? {
            ',' => continue,
            '}' => return p.end().then_some(out),
            _ => return None,
        }
    }
}

struct Parser<'a> {
    chars: std::iter::Peekable<std::str::Chars<'a>>,
}

impl Parser<'_> {
    fn peek(&mut self) -> Option<char> {
        self.chars.peek().copied()
    }

    fn next(&mut self) -> Option<char> {
        self.chars.next()
    }

    fn ws(&mut self) {
        while self.peek().is_some_and(char::is_whitespace) {
            self.next();
        }
    }

    fn end(&mut self) -> bool {
        self.ws();
        self.peek().is_none()
    }

    fn expect(&mut self, c: char) -> Option<()> {
        (self.next()? == c).then_some(())
    }

    fn string(&mut self) -> Option<String> {
        self.expect('"')?;
        let mut out = String::new();
        loop {
            match self.next()? {
                '"' => return Some(out),
                '\\' => match self.next()? {
                    '"' => out.push('"'),
                    '\\' => out.push('\\'),
                    '/' => out.push('/'),
                    'b' => out.push('\u{8}'),
                    'f' => out.push('\u{c}'),
                    'n' => out.push('\n'),
                    'r' => out.push('\r'),
                    't' => out.push('\t'),
                    'u' => {
                        let hex: String = (0..4).map(|_| self.next()).collect::<Option<_>>()?;
                        let code = u32::from_str_radix(&hex, 16).ok()?;
                        out.push(char::from_u32(code).unwrap_or('\u{fffd}'));
                    }
                    _ => return None,
                },
                c => out.push(c),
            }
        }
    }

    fn value(&mut self) -> Option<Value> {
        match self.peek()? {
            '"' => self.string().map(Value::Str),
            't' | 'f' | 'n' => {
                let word: String = std::iter::from_fn(|| {
                    self.peek().filter(char::is_ascii_alphabetic).inspect(|_| {
                        self.next();
                    })
                })
                .collect();
                match word.as_str() {
                    "true" => Some(Value::Bool(true)),
                    "false" => Some(Value::Bool(false)),
                    "null" => Some(Value::Null),
                    _ => None,
                }
            }
            _ => {
                let num: String = std::iter::from_fn(|| {
                    self.peek()
                        .filter(|c| c.is_ascii_digit() || matches!(c, '-' | '+' | '.' | 'e' | 'E'))
                        .inspect(|_| {
                            self.next();
                        })
                })
                .collect();
                num.parse().ok().map(Value::Num)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flat_objects() {
        let v = parse_flat_object(r#"{"path": "src/a b.rs", "score": 12.5, "x": null}"#).unwrap();
        assert_eq!(v[0], ("path".into(), Value::Str("src/a b.rs".into())));
        assert_eq!(v[1], ("score".into(), Value::Num(12.5)));
        let v = parse_flat_object(r#"{"path":"\u00e9\t\"q\"","score":-3e2}"#).unwrap();
        assert_eq!(v[0].1, Value::Str("é\t\"q\"".into()));
        assert_eq!(v[1].1, Value::Num(-300.0));
        assert_eq!(parse_flat_object("{}"), Some(vec![]));
    }

    #[test]
    fn rejects_malformed() {
        for s in [r#"{"a": [1]}"#, r#"{"a": {"b": 1}}"#, r#"{"a" 1}"#, r#"{"a": 1"#, r#"{"a": 1} x"#, "[]", r#"{"a": tru}"#] {
            assert_eq!(parse_flat_object(s), None, "{}", s);
        }
    }
}
