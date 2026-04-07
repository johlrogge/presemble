use forms::Form;

#[derive(Debug)]
pub struct ReadError(pub String);

impl std::fmt::Display for ReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for ReadError {}

/// Read a single form from the input string.
pub fn read(input: &str) -> Result<Form, ReadError> {
    let mut reader = Reader::new(input);
    let form = reader.read_form()?;
    Ok(form)
}

/// Read all forms from the input string.
pub fn read_all(input: &str) -> Result<Vec<Form>, ReadError> {
    let mut reader = Reader::new(input);
    let mut forms = Vec::new();
    while reader.skip_whitespace_and_comments() {
        forms.push(reader.read_form()?);
    }
    Ok(forms)
}

struct Reader<'a> {
    input: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(input: &'a str) -> Self {
        Self {
            input: input.as_bytes(),
            pos: 0,
        }
    }

    fn peek(&self) -> Option<u8> {
        self.input.get(self.pos).copied()
    }

    fn advance(&mut self) -> Option<u8> {
        let ch = self.input.get(self.pos).copied()?;
        self.pos += 1;
        Some(ch)
    }

    /// Skip whitespace (including commas, which are whitespace in EDN) and
    /// line comments (`;` to end of line). Returns `true` if there is more
    /// input to read, `false` if the end has been reached.
    fn skip_whitespace_and_comments(&mut self) -> bool {
        loop {
            match self.peek() {
                None => return false,
                Some(b' ' | b'\t' | b'\n' | b'\r' | b',') => {
                    self.pos += 1;
                }
                Some(b';') => {
                    // Skip to end of line
                    while let Some(ch) = self.peek() {
                        self.pos += 1;
                        if ch == b'\n' {
                            break;
                        }
                    }
                }
                _ => return true,
            }
        }
    }

    fn read_form(&mut self) -> Result<Form, ReadError> {
        self.skip_whitespace_and_comments();
        match self.peek() {
            None => Err(ReadError("unexpected end of input".into())),
            Some(b'(') => self.read_list(),
            Some(b'[') => self.read_vector(),
            Some(b'{') => self.read_map(),
            Some(b'#') => {
                self.advance(); // consume '#'
                match self.peek() {
                    Some(b'{') => self.read_set(),
                    _ => Err(ReadError("expected '{' after '#'".into())),
                }
            }
            Some(b'"') => self.read_string(),
            Some(b':') => self.read_keyword(),
            Some(b'-') => {
                // Could be negative number or symbol starting with -
                if self.pos + 1 < self.input.len() && self.input[self.pos + 1].is_ascii_digit() {
                    self.read_number()
                } else {
                    self.read_symbol()
                }
            }
            Some(ch) if ch.is_ascii_digit() => self.read_number(),
            _ => self.read_symbol(),
        }
    }

    fn read_list(&mut self) -> Result<Form, ReadError> {
        self.advance(); // consume '('
        let mut items = Vec::new();
        loop {
            self.skip_whitespace_and_comments();
            match self.peek() {
                None => return Err(ReadError("unterminated list".into())),
                Some(b')') => {
                    self.advance();
                    return Ok(Form::List(items));
                }
                _ => items.push(self.read_form()?),
            }
        }
    }

    fn read_vector(&mut self) -> Result<Form, ReadError> {
        self.advance(); // consume '['
        let mut items = Vec::new();
        loop {
            self.skip_whitespace_and_comments();
            match self.peek() {
                None => return Err(ReadError("unterminated vector".into())),
                Some(b']') => {
                    self.advance();
                    return Ok(Form::Vector(items));
                }
                _ => items.push(self.read_form()?),
            }
        }
    }

    fn read_map(&mut self) -> Result<Form, ReadError> {
        self.advance(); // consume '{'
        let mut pairs = Vec::new();
        loop {
            self.skip_whitespace_and_comments();
            match self.peek() {
                None => return Err(ReadError("unterminated map".into())),
                Some(b'}') => {
                    self.advance();
                    return Ok(Form::Map(pairs));
                }
                _ => {
                    let key = self.read_form()?;
                    let val = self.read_form()?;
                    pairs.push((key, val));
                }
            }
        }
    }

    fn read_set(&mut self) -> Result<Form, ReadError> {
        self.advance(); // consume '{'
        let mut items = Vec::new();
        loop {
            self.skip_whitespace_and_comments();
            match self.peek() {
                None => return Err(ReadError("unterminated set".into())),
                Some(b'}') => {
                    self.advance();
                    return Ok(Form::Set(items));
                }
                _ => items.push(self.read_form()?),
            }
        }
    }

    fn read_string(&mut self) -> Result<Form, ReadError> {
        self.advance(); // consume opening '"'
        let mut s = String::new();
        loop {
            match self.advance() {
                None => return Err(ReadError("unterminated string".into())),
                Some(b'"') => return Ok(Form::Str(s)),
                Some(b'\\') => match self.advance() {
                    Some(b'n') => s.push('\n'),
                    Some(b't') => s.push('\t'),
                    Some(b'r') => s.push('\r'),
                    Some(b'"') => s.push('"'),
                    Some(b'\\') => s.push('\\'),
                    Some(ch) => {
                        s.push('\\');
                        s.push(ch as char);
                    }
                    None => return Err(ReadError("unterminated string escape".into())),
                },
                Some(ch) => s.push(ch as char),
            }
        }
    }

    fn read_keyword(&mut self) -> Result<Form, ReadError> {
        self.advance(); // consume ':'
        let name = self.read_symbol_string();
        if name.is_empty() {
            return Err(ReadError("empty keyword".into()));
        }
        if let Some((ns, n)) = name.split_once('/') {
            Ok(Form::Keyword {
                namespace: Some(ns.to_string()),
                name: n.to_string(),
            })
        } else {
            Ok(Form::Keyword {
                namespace: None,
                name,
            })
        }
    }

    fn read_number(&mut self) -> Result<Form, ReadError> {
        let start = self.pos;
        if self.peek() == Some(b'-') {
            self.advance();
        }
        while let Some(ch) = self.peek() {
            if ch.is_ascii_digit() {
                self.advance();
            } else {
                break;
            }
        }
        let s = std::str::from_utf8(&self.input[start..self.pos])
            .map_err(|_| ReadError("invalid number".into()))?;
        let n: i64 = s
            .parse()
            .map_err(|_| ReadError(format!("invalid number: {s}")))?;
        Ok(Form::Integer(n))
    }

    fn read_symbol(&mut self) -> Result<Form, ReadError> {
        let name = self.read_symbol_string();
        match name.as_str() {
            "nil" => Ok(Form::Nil),
            "true" => Ok(Form::Bool(true)),
            "false" => Ok(Form::Bool(false)),
            "" => Err(ReadError("unexpected character".into())),
            _ => Ok(Form::Symbol(name)),
        }
    }

    fn read_symbol_string(&mut self) -> String {
        let start = self.pos;
        while let Some(ch) = self.peek() {
            if is_symbol_char(ch) {
                self.advance();
            } else {
                break;
            }
        }
        String::from_utf8_lossy(&self.input[start..self.pos]).to_string()
    }
}

fn is_symbol_char(ch: u8) -> bool {
    ch.is_ascii_alphanumeric()
        || matches!(
            ch,
            b'_' | b'-' | b'.' | b'>' | b'<' | b'=' | b'+' | b'*' | b'/' | b'!' | b'?' | b'&'
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use forms::Form;

    #[test]
    fn read_integer() {
        assert_eq!(read("42").unwrap(), Form::Integer(42));
    }

    #[test]
    fn read_negative_integer() {
        assert_eq!(read("-1").unwrap(), Form::Integer(-1));
    }

    #[test]
    fn read_zero() {
        assert_eq!(read("0").unwrap(), Form::Integer(0));
    }

    #[test]
    fn read_keyword_simple() {
        assert_eq!(
            read(":keyword").unwrap(),
            Form::Keyword {
                namespace: None,
                name: "keyword".into()
            }
        );
    }

    #[test]
    fn read_keyword_namespaced() {
        assert_eq!(
            read(":presemble/define").unwrap(),
            Form::Keyword {
                namespace: Some("presemble".into()),
                name: "define".into()
            }
        );
    }

    #[test]
    fn read_list_simple() {
        let form = read("(+ 1 2)").unwrap();
        match &form {
            Form::List(items) => {
                assert_eq!(items.len(), 3);
                assert_eq!(items[0], Form::Symbol("+".into()));
                assert_eq!(items[1], Form::Integer(1));
                assert_eq!(items[2], Form::Integer(2));
            }
            _ => panic!("expected list, got {form:?}"),
        }
    }

    #[test]
    fn read_nested_list() {
        let form = read("(->> :post (sort-by :published :desc) (take 3))").unwrap();
        match &form {
            Form::List(items) => {
                assert_eq!(items.len(), 4);
                assert_eq!(items[0], Form::Symbol("->>".into()));
                assert_eq!(
                    items[1],
                    Form::Keyword {
                        namespace: None,
                        name: "post".into()
                    }
                );
                // Third item is (sort-by :published :desc)
                assert!(matches!(&items[2], Form::List(inner) if inner.len() == 3));
                // Fourth item is (take 3)
                assert!(matches!(&items[3], Form::List(inner) if inner.len() == 2));
            }
            _ => panic!("expected list, got {form:?}"),
        }
    }

    #[test]
    fn read_map() {
        let form = read("{:a 1 :b 2}").unwrap();
        match &form {
            Form::Map(pairs) => {
                assert_eq!(pairs.len(), 2);
                assert_eq!(
                    pairs[0].0,
                    Form::Keyword {
                        namespace: None,
                        name: "a".into()
                    }
                );
                assert_eq!(pairs[0].1, Form::Integer(1));
                assert_eq!(
                    pairs[1].0,
                    Form::Keyword {
                        namespace: None,
                        name: "b".into()
                    }
                );
                assert_eq!(pairs[1].1, Form::Integer(2));
            }
            _ => panic!("expected map, got {form:?}"),
        }
    }

    #[test]
    fn read_vector() {
        assert_eq!(
            read("[1 2 3]").unwrap(),
            Form::Vector(vec![Form::Integer(1), Form::Integer(2), Form::Integer(3)])
        );
    }

    #[test]
    fn read_set() {
        let form = read("#{1 2 3}").unwrap();
        assert!(matches!(form, Form::Set(items) if items.len() == 3));
    }

    #[test]
    fn read_bool_true() {
        assert_eq!(read("true").unwrap(), Form::Bool(true));
    }

    #[test]
    fn read_bool_false() {
        assert_eq!(read("false").unwrap(), Form::Bool(false));
    }

    #[test]
    fn read_nil() {
        assert_eq!(read("nil").unwrap(), Form::Nil);
    }

    #[test]
    fn read_string_simple() {
        assert_eq!(
            read(r#""hello world""#).unwrap(),
            Form::Str("hello world".into())
        );
    }

    #[test]
    fn read_string_with_escape_newline() {
        assert_eq!(
            read(r#""line1\nline2""#).unwrap(),
            Form::Str("line1\nline2".into())
        );
    }

    #[test]
    fn read_string_with_escaped_quote() {
        assert_eq!(
            read(r#""say \"hi\"""#).unwrap(),
            Form::Str("say \"hi\"".into())
        );
    }

    #[test]
    fn read_string_with_escaped_backslash() {
        assert_eq!(
            read(r#""back\\slash""#).unwrap(),
            Form::Str("back\\slash".into())
        );
    }

    #[test]
    fn read_all_two_forms() {
        let forms = read_all("(+ 1 2) (- 3 4)").unwrap();
        assert_eq!(forms.len(), 2);
        assert!(matches!(&forms[0], Form::List(items) if items.len() == 3));
        assert!(matches!(&forms[1], Form::List(items) if items.len() == 3));
    }

    #[test]
    fn read_all_empty_input() {
        let forms = read_all("").unwrap();
        assert!(forms.is_empty());
    }

    #[test]
    fn read_all_skips_comments() {
        let input = r#"
;; This is a comment
42
;; Another comment
:keyword
"#;
        let forms = read_all(input).unwrap();
        assert_eq!(forms.len(), 2);
        assert_eq!(forms[0], Form::Integer(42));
        assert_eq!(
            forms[1],
            Form::Keyword {
                namespace: None,
                name: "keyword".into()
            }
        );
    }

    #[test]
    fn read_commas_as_whitespace() {
        let form = read("[1, 2, 3]").unwrap();
        assert_eq!(
            form,
            Form::Vector(vec![Form::Integer(1), Form::Integer(2), Form::Integer(3)])
        );
    }

    #[test]
    fn read_symbol() {
        assert_eq!(
            read("sort-by").unwrap(),
            Form::Symbol("sort-by".into())
        );
    }

    #[test]
    fn read_namespaced_symbol() {
        assert_eq!(
            read("presemble/insert").unwrap(),
            Form::Symbol("presemble/insert".into())
        );
    }

    #[test]
    fn read_empty_list() {
        assert_eq!(read("()").unwrap(), Form::List(vec![]));
    }

    #[test]
    fn read_empty_vector() {
        assert_eq!(read("[]").unwrap(), Form::Vector(vec![]));
    }

    #[test]
    fn read_empty_map() {
        assert_eq!(read("{}").unwrap(), Form::Map(vec![]));
    }

    #[test]
    fn read_error_on_unterminated_list() {
        assert!(read("(+ 1").is_err());
    }

    #[test]
    fn read_error_on_unterminated_string() {
        assert!(read(r#""hello"#).is_err());
    }

    #[test]
    fn read_error_on_empty_keyword() {
        assert!(read(": ").is_err());
    }
}
