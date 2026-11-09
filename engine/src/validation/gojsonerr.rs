//! The syntax errors Go's `encoding/json` reports (`scanner.go`), for validation messages that
//! quote them (a Secret's `.dockerconfigjson` that does not parse).

/// `quoteChar`.
fn quote_char(c: u8) -> String {
    match c {
        b'\'' => "'\\''".into(),
        b'"' => "'\"'".into(),
        c => {
            let q = super::field::go_quote(&(c as char).to_string());
            format!("'{}'", &q[1..q.len() - 1])
        }
    }
}

struct Scanner<'a> {
    data: &'a [u8],
    pos: usize,
}

type Res = Result<(), String>;

impl Scanner<'_> {
    fn peek(&self) -> Option<u8> {
        self.data.get(self.pos).copied()
    }

    fn skip_space(&mut self) {
        while let Some(b' ' | b'\t' | b'\n' | b'\r') = self.peek() {
            self.pos += 1;
        }
    }

    fn eof() -> String {
        "unexpected end of JSON input".into()
    }

    fn bad(c: u8, context: &str) -> String {
        format!("invalid character {} {context}", quote_char(c))
    }

    fn value(&mut self) -> Res {
        self.skip_space();
        let Some(c) = self.peek() else { return Err(Self::eof()) };
        match c {
            b'{' => self.object(),
            b'[' => self.array(),
            b'"' => self.string(),
            b'-' | b'0'..=b'9' => self.number(),
            b't' => self.literal(b"true"),
            b'f' => self.literal(b"false"),
            b'n' => self.literal(b"null"),
            c => Err(Self::bad(c, "looking for beginning of value")),
        }
    }

    fn literal(&mut self, word: &[u8]) -> Res {
        let name = std::str::from_utf8(word).expect("ascii");
        for (i, &want) in word.iter().enumerate() {
            match self.peek() {
                None => return Err(Self::eof()),
                Some(c) if c == want => self.pos += 1,
                Some(c) if i > 0 => {
                    return Err(format!(
                        "invalid character {} in literal {name} (expecting {})",
                        quote_char(c),
                        quote_char(want)
                    ))
                }
                Some(c) => return Err(Self::bad(c, "looking for beginning of value")),
            }
        }
        Ok(())
    }

    fn string(&mut self) -> Res {
        self.pos += 1;
        loop {
            match self.peek() {
                None => return Err(Self::eof()),
                Some(b'"') => {
                    self.pos += 1;
                    return Ok(());
                }
                Some(b'\\') => {
                    self.pos += 1;
                    match self.peek() {
                        None => return Err(Self::eof()),
                        Some(b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't') => self.pos += 1,
                        Some(b'u') => {
                            self.pos += 1;
                            for _ in 0..4 {
                                match self.peek() {
                                    None => return Err(Self::eof()),
                                    Some(c) if c.is_ascii_hexdigit() => self.pos += 1,
                                    Some(c) => return Err(Self::bad(c, "in \\u hexadecimal character escape")),
                                }
                            }
                        }
                        Some(c) => return Err(Self::bad(c, "in string escape code")),
                    }
                }
                Some(c) if c < 0x20 => return Err(Self::bad(c, "in string literal")),
                Some(_) => self.pos += 1,
            }
        }
    }

    fn digits(&mut self) -> usize {
        let start = self.pos;
        while self.peek().is_some_and(|c| c.is_ascii_digit()) {
            self.pos += 1;
        }
        self.pos - start
    }

    fn number(&mut self) -> Res {
        if self.peek() == Some(b'-') {
            self.pos += 1;
            match self.peek() {
                None => return Err(Self::eof()),
                Some(b'0'..=b'9') => {}
                Some(c) => return Err(Self::bad(c, "in numeric literal")),
            }
        }
        if self.peek() == Some(b'0') {
            self.pos += 1;
        } else {
            self.digits();
        }
        if self.peek() == Some(b'.') {
            self.pos += 1;
            match self.peek() {
                None => return Err(Self::eof()),
                Some(c) if !c.is_ascii_digit() => return Err(Self::bad(c, "after decimal point in numeric literal")),
                _ => {}
            }
            self.digits();
        }
        if let Some(b'e' | b'E') = self.peek() {
            self.pos += 1;
            if let Some(b'+' | b'-') = self.peek() {
                self.pos += 1;
            }
            match self.peek() {
                None => return Err(Self::eof()),
                Some(c) if !c.is_ascii_digit() => return Err(Self::bad(c, "in exponent of numeric literal")),
                _ => {}
            }
            self.digits();
        }
        Ok(())
    }

    fn array(&mut self) -> Res {
        self.pos += 1;
        self.skip_space();
        if self.peek() == Some(b']') {
            self.pos += 1;
            return Ok(());
        }
        loop {
            self.value()?;
            self.skip_space();
            match self.peek() {
                None => return Err(Self::eof()),
                Some(b',') => self.pos += 1,
                Some(b']') => {
                    self.pos += 1;
                    return Ok(());
                }
                Some(c) => return Err(Self::bad(c, "after array element")),
            }
        }
    }

    fn object(&mut self) -> Res {
        self.pos += 1;
        self.skip_space();
        if self.peek() == Some(b'}') {
            self.pos += 1;
            return Ok(());
        }
        loop {
            self.skip_space();
            match self.peek() {
                None => return Err(Self::eof()),
                Some(b'"') => self.string()?,
                Some(c) => return Err(Self::bad(c, "looking for beginning of object key string")),
            }
            self.skip_space();
            match self.peek() {
                None => return Err(Self::eof()),
                Some(b':') => self.pos += 1,
                Some(c) => return Err(Self::bad(c, "after object key")),
            }
            self.value()?;
            self.skip_space();
            match self.peek() {
                None => return Err(Self::eof()),
                Some(b',') => self.pos += 1,
                Some(b'}') => {
                    self.pos += 1;
                    return Ok(());
                }
                Some(c) => return Err(Self::bad(c, "after object key:value pair")),
            }
        }
    }
}

/// Go's syntax check of a JSON document: its error text, if the document does not parse.
pub fn check(data: &[u8]) -> Result<(), String> {
    let mut s = Scanner { data, pos: 0 };
    s.value()?;
    s.skip_space();
    match s.peek() {
        None => Ok(()),
        Some(c) => Err(Scanner::bad(c, "after top-level value")),
    }
}

#[cfg(test)]
mod tests {
    use super::check;

    #[test]
    fn messages_match_go() {
        assert_eq!(
            check(b"not json").unwrap_err(),
            "invalid character 'o' in literal null (expecting 'u')"
        );
        assert_eq!(
            check(b"{\"a\" 1}").unwrap_err(),
            "invalid character '1' after object key"
        );
        assert_eq!(
            check(b"[1,]").unwrap_err(),
            "invalid character ']' looking for beginning of value"
        );
        assert_eq!(check(b"{\"a\":1").unwrap_err(), "unexpected end of JSON input");
        assert!(check(b"{\"auths\":{}}").is_ok());
    }
}
