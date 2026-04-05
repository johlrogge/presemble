use crate::dom::{Element, Form, Node};
use crate::error::TemplateError;

// ---------------------------------------------------------------------------
// Tokenizer
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum Token {
    LBracket,
    RBracket,
    LBrace,
    RBrace,
    LParen,
    RParen,
    HashBrace,
    Keyword {
        namespace: Option<String>,
        name: String,
    },
    StringLit(String),
    Symbol(String),
    Integer(i64),
    Nil,
}

/// Characters that can start a symbol (but not a digit, not ':' which starts a keyword).
fn is_symbol_start(c: char) -> bool {
    c.is_alphabetic() || matches!(c, '_' | '-' | '.' | '>' | '<' | '=' | '+' | '*' | '/' | '!' | '?')
}

/// Characters that can continue a symbol.
fn is_symbol_continue(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | '>' | '<' | '=' | '+' | '*' | '/' | '!' | '?')
}

fn tokenize(input: &str) -> Result<Vec<Token>, TemplateError> {
    let mut tokens = Vec::new();
    let chars: Vec<char> = input.chars().collect();
    let len = chars.len();
    let mut i = 0;

    while i < len {
        let ch = chars[i];

        // Skip whitespace and commas (EDN treats both as whitespace)
        if ch.is_ascii_whitespace() || ch == ',' {
            i += 1;
            continue;
        }

        // Line comment: skip from ';' to end of line
        if ch == ';' {
            i += 1;
            while i < len && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }

        match ch {
            '[' => {
                tokens.push(Token::LBracket);
                i += 1;
            }
            ']' => {
                tokens.push(Token::RBracket);
                i += 1;
            }
            '{' => {
                tokens.push(Token::LBrace);
                i += 1;
            }
            '}' => {
                tokens.push(Token::RBrace);
                i += 1;
            }
            '(' => {
                tokens.push(Token::LParen);
                i += 1;
            }
            ')' => {
                tokens.push(Token::RParen);
                i += 1;
            }
            '#' => {
                // '#' must be followed by '{' to form a set literal
                if i + 1 < len && chars[i + 1] == '{' {
                    tokens.push(Token::HashBrace);
                    i += 2;
                } else {
                    return Err(TemplateError::ParseError(format!(
                        "unexpected '#' at position {i}: expected '#{{' for a set literal"
                    )));
                }
            }
            ':' => {
                // Keyword: collect alphanumeric, '-', '_', '/' characters
                i += 1; // skip the leading ':'
                let start = i;
                while i < len {
                    let c = chars[i];
                    if c.is_alphanumeric() || c == '-' || c == '_' || c == '/' {
                        i += 1;
                    } else {
                        break;
                    }
                }
                if i == start {
                    return Err(TemplateError::ParseError(
                        "empty keyword after ':'".into(),
                    ));
                }
                let raw: String = chars[start..i].iter().collect();
                // Split on '/' to handle namespace
                let slash_count = raw.chars().filter(|&c| c == '/').count();
                if slash_count > 1 {
                    return Err(TemplateError::ParseError(format!(
                        "keyword '{raw}' contains more than one '/'"
                    )));
                }
                let token = if let Some(pos) = raw.find('/') {
                    let ns = raw[..pos].to_string();
                    let name = raw[pos + 1..].to_string();
                    Token::Keyword {
                        namespace: Some(ns),
                        name,
                    }
                } else {
                    Token::Keyword {
                        namespace: None,
                        name: raw,
                    }
                };
                tokens.push(token);
            }
            '"' => {
                i += 1; // skip opening quote
                let mut s = String::new();
                let mut closed = false;
                while i < len {
                    let c = chars[i];
                    if c == '\\' {
                        i += 1;
                        if i >= len {
                            return Err(TemplateError::ParseError(
                                "unexpected end of input inside string escape".into(),
                            ));
                        }
                        match chars[i] {
                            '"' => s.push('"'),
                            '\\' => s.push('\\'),
                            'n' => s.push('\n'),
                            'r' => s.push('\r'),
                            't' => s.push('\t'),
                            other => {
                                return Err(TemplateError::ParseError(format!(
                                    "unknown escape sequence '\\{other}'"
                                )))
                            }
                        }
                        i += 1;
                    } else if c == '"' {
                        i += 1;
                        closed = true;
                        break;
                    } else {
                        s.push(c);
                        i += 1;
                    }
                }
                if !closed {
                    return Err(TemplateError::ParseError(
                        "unterminated string literal".into(),
                    ));
                }
                tokens.push(Token::StringLit(s));
            }
            // Integer literal: optional '-' followed by digits, or just digits.
            // Note: '-' followed by a non-digit is the start of a symbol (e.g. "->").
            c if c.is_ascii_digit()
                || (c == '-' && i + 1 < len && chars[i + 1].is_ascii_digit()) =>
            {
                let start = i;
                if c == '-' {
                    i += 1; // consume the '-'
                }
                while i < len && chars[i].is_ascii_digit() {
                    i += 1;
                }
                let raw: String = chars[start..i].iter().collect();
                let n: i64 = raw.parse().map_err(|_| {
                    TemplateError::ParseError(format!("integer literal out of range: {raw}"))
                })?;
                tokens.push(Token::Integer(n));
            }
            // Symbol characters: starts with alpha, '_', '-', '.', '>', '<', '=', '+', '*', '/', '!', '?'
            // but NOT a digit. The special case 'nil' is checked first.
            c if is_symbol_start(c) => {
                let start = i;
                i += 1;
                while i < len && is_symbol_continue(chars[i]) {
                    i += 1;
                }
                let raw: String = chars[start..i].iter().collect();
                if raw == "nil" {
                    tokens.push(Token::Nil);
                } else {
                    tokens.push(Token::Symbol(raw));
                }
            }
            other => {
                return Err(TemplateError::ParseError(format!(
                    "unexpected character '{other}' at position {i}"
                )));
            }
        }
    }

    Ok(tokens)
}

// ---------------------------------------------------------------------------
// Keyword → tag/attr name helpers
// ---------------------------------------------------------------------------

/// Convert a keyword to an element tag name.
///
/// - `:presemble/insert` → `"presemble:insert"`
/// - `:div` → `"div"`
/// - Any other namespace → error
fn keyword_to_tag_name(namespace: &Option<String>, name: &str) -> Result<String, TemplateError> {
    match namespace.as_deref() {
        None => Ok(name.to_string()),
        Some("presemble") => Ok(format!("presemble:{name}")),
        Some(other) => Err(TemplateError::ParseError(format!(
            "unknown namespace '{other}' in element keyword"
        ))),
    }
}

/// Convert a keyword to an attribute name: strip leading ':' (already done by
/// tokenizer), keep the raw name. Namespaces use ':' as separator to match
/// `keyword_to_tag_name` and the transformer's expectations.
fn keyword_to_attr_name(namespace: &Option<String>, name: &str) -> String {
    match namespace.as_deref() {
        None => name.to_string(),
        Some(ns) => format!("{ns}:{name}"),
    }
}

// ---------------------------------------------------------------------------
// Parser
// ---------------------------------------------------------------------------

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

impl Parser {
    fn new(tokens: Vec<Token>) -> Self {
        Parser { tokens, pos: 0 }
    }

    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    fn next(&mut self) -> Option<Token> {
        if self.pos < self.tokens.len() {
            let tok = self.tokens[self.pos].clone();
            self.pos += 1;
            Some(tok)
        } else {
            None
        }
    }

    /// Parse a single node from the current position. Returns `None` if the
    /// next token closes a parent context (RBracket / RBrace) or there is no
    /// more input.
    fn parse_node(&mut self) -> Result<Option<Node>, TemplateError> {
        match self.peek() {
            None | Some(Token::RBracket) | Some(Token::RBrace) => Ok(None),

            Some(Token::Nil) => {
                self.next(); // consume
                Ok(None)     // nil → skip
            }

            Some(Token::StringLit(_)) => {
                if let Some(Token::StringLit(s)) = self.next() {
                    Ok(Some(Node::Text(s)))
                } else {
                    unreachable!()
                }
            }

            Some(Token::LBracket) => {
                self.next(); // consume '['

                // First token must be a Keyword (tag name)
                let (namespace, name) = match self.next() {
                    Some(Token::Keyword { namespace, name }) => (namespace, name),
                    Some(other) => {
                        return Err(TemplateError::ParseError(format!(
                            "expected keyword as first element of vector, got {other:?}"
                        )))
                    }
                    None => {
                        return Err(TemplateError::ParseError(
                            "unexpected end of input after '['".into(),
                        ))
                    }
                };

                let tag_name = keyword_to_tag_name(&namespace, &name)?;

                // Optional attribute map
                let attrs = if self.peek() == Some(&Token::LBrace) {
                    self.next(); // consume '{'
                    self.parse_attr_map()?
                } else {
                    Vec::new()
                };

                // Children until ']'
                let mut children = Vec::new();
                loop {
                    match self.peek() {
                        None => {
                            return Err(TemplateError::ParseError(
                                "unexpected end of input inside element, expected ']'".into(),
                            ))
                        }
                        Some(Token::RBracket) => {
                            self.next(); // consume ']'
                            break;
                        }
                        _ => {
                            if let Some(child) = self.parse_node()? {
                                children.push(child);
                            }
                        }
                    }
                }

                Ok(Some(Node::Element(Element {
                    name: tag_name,
                    attrs,
                    children,
                })))
            }

            Some(Token::LParen) => {
                self.next(); // consume '('

                // The next token must be a Symbol naming the composition form.
                let form_name = match self.next() {
                    Some(Token::Symbol(s)) => s,
                    Some(other) => {
                        return Err(TemplateError::ParseError(format!(
                            "expected symbol after '(' in composition form, got {other:?}"
                        )))
                    }
                    None => {
                        return Err(TemplateError::ParseError(
                            "unexpected end of input after '(' in composition form".into(),
                        ))
                    }
                };

                match form_name.as_str() {
                    "juxt" => {
                        // (juxt arg1 arg2 ...) — collect arguments until ')'
                        let mut children: Vec<Node> = Vec::new();
                        loop {
                            match self.peek() {
                                None => {
                                    return Err(TemplateError::ParseError(
                                        "unexpected end of input inside (juxt ...), expected ')'".into(),
                                    ))
                                }
                                Some(Token::RParen) => {
                                    self.next(); // consume ')'
                                    break;
                                }
                                Some(Token::Symbol(_)) => {
                                    let sym = match self.next() {
                                        Some(Token::Symbol(s)) => s,
                                        _ => unreachable!(),
                                    };
                                    let child = juxt_symbol_to_node(&sym);
                                    children.push(child);
                                }
                                Some(Token::LParen) => {
                                    // Nested composition form — parse as a child node
                                    if let Some(nested) = self.parse_node()? {
                                        children.push(nested);
                                    }
                                }
                                Some(other) => {
                                    let other = other.clone();
                                    return Err(TemplateError::ParseError(format!(
                                        "unexpected token in (juxt ...) argument position: {other:?}"
                                    )));
                                }
                            }
                        }
                        Ok(Some(Node::Element(Element {
                            name: "presemble:juxt".to_string(),
                            attrs: Vec::new(),
                            children,
                        })))
                    }
                    "apply" => {
                        // (apply template-name) — single argument
                        let name = match self.next() {
                            Some(Token::Symbol(s)) => s,
                            Some(other) => {
                                return Err(TemplateError::ParseError(format!(
                                    "expected symbol as argument to (apply ...), got {other:?}"
                                )))
                            }
                            None => {
                                return Err(TemplateError::ParseError(
                                    "unexpected end of input in (apply ...) form".into(),
                                ))
                            }
                        };
                        // Consume ')'
                        match self.next() {
                            Some(Token::RParen) => {}
                            Some(other) => {
                                return Err(TemplateError::ParseError(format!(
                                    "expected ')' after (apply {name}), got {other:?}"
                                )))
                            }
                            None => {
                                return Err(TemplateError::ParseError(
                                    "unexpected end of input after (apply ...) argument".into(),
                                ))
                            }
                        }
                        let node = apply_symbol_to_node(&name);
                        Ok(Some(node))
                    }
                    other => Err(TemplateError::ParseError(format!(
                        "unknown composition form '({other} ...)'"
                    ))),
                }
            }

            Some(other) => Err(TemplateError::ParseError(format!(
                "unexpected token in node position: {other:?}"
            ))),
        }
    }

    /// Parse the interior of an attribute map (after `{` has been consumed).
    /// Expects pairs of `Keyword <form>` until `}`.
    fn parse_attr_map(&mut self) -> Result<Vec<(String, Form)>, TemplateError> {
        let mut attrs = Vec::new();

        loop {
            match self.peek() {
                None => {
                    return Err(TemplateError::ParseError(
                        "unexpected end of input inside attribute map".into(),
                    ))
                }
                Some(Token::RBrace) => {
                    self.next(); // consume '}'
                    break;
                }
                Some(Token::Keyword { .. }) => {
                    // Consume the keyword key
                    let (ns, name) = match self.next() {
                        Some(Token::Keyword { namespace, name }) => (namespace, name),
                        _ => unreachable!(),
                    };
                    let attr_name = keyword_to_attr_name(&ns, &name);

                    // Value is any EDN form
                    let value = self.parse_form().map_err(|e| {
                        TemplateError::ParseError(format!(
                            "invalid value for attribute '{attr_name}': {e}"
                        ))
                    })?;
                    attrs.push((attr_name, value));
                }
                Some(other) => {
                    let other = other.clone();
                    return Err(TemplateError::ParseError(format!(
                        "expected keyword or '}}' inside attribute map, got {other:?}"
                    )));
                }
            }
        }

        Ok(attrs)
    }

    /// Parse a single EDN form from the current position.
    fn parse_form(&mut self) -> Result<Form, TemplateError> {
        match self.next() {
            Some(Token::StringLit(s)) => Ok(Form::Str(s)),
            Some(Token::Symbol(s)) => Ok(Form::Symbol(s)),
            Some(Token::Integer(n)) => Ok(Form::Integer(n)),
            Some(Token::Nil) => Ok(Form::Nil),
            Some(Token::Keyword { namespace, name }) => Ok(Form::Keyword { namespace, name }),
            Some(Token::LParen) => self.parse_list(),
            Some(Token::HashBrace) => self.parse_set(),
            Some(Token::LBracket) => self.parse_vector_form(),
            Some(Token::LBrace) => self.parse_map_form(),
            Some(other) => Err(TemplateError::ParseError(format!(
                "unexpected token in form position: {other:?}"
            ))),
            None => Err(TemplateError::ParseError(
                "unexpected end of input in form position".into(),
            )),
        }
    }

    /// Parse a list form `(item1 item2 ...)` — LParen already consumed.
    fn parse_list(&mut self) -> Result<Form, TemplateError> {
        let mut items = Vec::new();
        loop {
            match self.peek() {
                None => {
                    return Err(TemplateError::ParseError(
                        "unexpected end of input inside list, expected ')'".into(),
                    ))
                }
                Some(Token::RParen) => {
                    self.next(); // consume ')'
                    break;
                }
                _ => items.push(self.parse_form()?),
            }
        }
        Ok(Form::List(items))
    }

    /// Parse a set form `#{item1 item2 ...}` — HashBrace already consumed.
    fn parse_set(&mut self) -> Result<Form, TemplateError> {
        let mut items = Vec::new();
        loop {
            match self.peek() {
                None => {
                    return Err(TemplateError::ParseError(
                        "unexpected end of input inside set, expected '}'".into(),
                    ))
                }
                Some(Token::RBrace) => {
                    self.next(); // consume '}'
                    break;
                }
                _ => items.push(self.parse_form()?),
            }
        }
        Ok(Form::Set(items))
    }

    /// Parse a vector form `[item1 item2 ...]` as a `Form::Vector` — LBracket already consumed.
    fn parse_vector_form(&mut self) -> Result<Form, TemplateError> {
        let mut items = Vec::new();
        loop {
            match self.peek() {
                None => {
                    return Err(TemplateError::ParseError(
                        "unexpected end of input inside vector form, expected ']'".into(),
                    ))
                }
                Some(Token::RBracket) => {
                    self.next(); // consume ']'
                    break;
                }
                _ => items.push(self.parse_form()?),
            }
        }
        Ok(Form::Vector(items))
    }

    /// Parse a map form `{k1 v1 k2 v2 ...}` as a `Form::Map` — LBrace already consumed.
    fn parse_map_form(&mut self) -> Result<Form, TemplateError> {
        let mut pairs = Vec::new();
        loop {
            match self.peek() {
                None => {
                    return Err(TemplateError::ParseError(
                        "unexpected end of input inside map form, expected '}'".into(),
                    ))
                }
                Some(Token::RBrace) => {
                    self.next(); // consume '}'
                    break;
                }
                _ => {
                    let key = self.parse_form()?;
                    let val = self.parse_form()?;
                    pairs.push((key, val));
                }
            }
        }
        Ok(Form::Map(pairs))
    }
}

// ---------------------------------------------------------------------------
// Composition form helpers
// ---------------------------------------------------------------------------

/// Convert a symbol from a `(juxt ...)` argument to a Node.
///
/// - `self/name` → `presemble:apply` with template=name, data=input
/// - bare `name` → `presemble:include` with src=name
fn juxt_symbol_to_node(sym: &str) -> Node {
    if sym.starts_with("self/") {
        // self/name variant — delegate to apply_symbol_to_node
        apply_symbol_to_node(sym)
    } else {
        Node::Element(Element {
            name: "presemble:include".to_string(),
            attrs: vec![("src".to_string(), crate::dom::Form::Str(sym.to_string()))],
            children: Vec::new(),
        })
    }
}

/// Convert a symbol from an `(apply ...)` form to a Node.
///
/// - `self/name` → `presemble:apply` with template=name, data=input
/// - bare `name` → also `presemble:apply` with template=name, data=input
fn apply_symbol_to_node(sym: &str) -> Node {
    let template_name = sym.strip_prefix("self/").unwrap_or(sym);
    Node::Element(Element {
        name: "presemble:apply".to_string(),
        attrs: vec![
            ("template".to_string(), crate::dom::Form::Str(template_name.to_string())),
            ("data".to_string(), crate::dom::Form::Str("input".to_string())),
        ],
        children: Vec::new(),
    })
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Parse a sequence of top-level Hiccup/EDN nodes into a `Vec<Node>`.
pub fn parse_template_hiccup(src: &str) -> Result<Vec<Node>, TemplateError> {
    let tokens = tokenize(src)?;
    let mut parser = Parser::new(tokens);
    let mut nodes = Vec::new();

    loop {
        match parser.peek() {
            None => break,
            _ => {
                if let Some(node) = parser.parse_node()? {
                    nodes.push(node);
                }
            }
        }
    }

    Ok(nodes)
}

/// Parse a string as a single EDN form.
/// Used by the transformer to re-parse HTML attribute strings as structured forms.
pub fn parse_edn_form(src: &str) -> Result<Form, TemplateError> {
    let tokens = tokenize(src)?;
    let mut parser = Parser::new(tokens);
    parser.parse_form()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dom::Node;

    fn parse(src: &str) -> Vec<Node> {
        parse_template_hiccup(src).expect("parse should succeed")
    }

    fn parse_err(src: &str) -> String {
        parse_template_hiccup(src).expect_err("should fail").to_string()
    }

    #[test]
    fn simple_element() {
        let nodes = parse("[:div [:p \"Hello\"]]");
        assert_eq!(nodes.len(), 1);
        if let Node::Element(el) = &nodes[0] {
            assert_eq!(el.name, "div");
            assert_eq!(el.children.len(), 1);
            if let Node::Element(p) = &el.children[0] {
                assert_eq!(p.name, "p");
                assert_eq!(p.children.len(), 1);
                assert!(matches!(&p.children[0], Node::Text(t) if t == "Hello"));
            }
        } else {
            panic!("expected Element");
        }
    }

    #[test]
    fn element_with_attrs() {
        let nodes = parse("[:meta {:charset \"utf-8\"}]");
        if let Node::Element(el) = &nodes[0] {
            assert_eq!(el.name, "meta");
            assert_eq!(el.attr("charset"), Some("utf-8"));
            assert!(el.children.is_empty());
        }
    }

    #[test]
    fn presemble_namespace() {
        let nodes = parse("[:presemble/insert {:data \"article.title\" :as \"h1\"}]");
        if let Node::Element(el) = &nodes[0] {
            assert_eq!(el.name, "presemble:insert");
            assert_eq!(el.attr("data"), Some("article.title"));
            assert_eq!(el.attr("as"), Some("h1"));
            assert!(el.is_presemble());
        }
    }

    #[test]
    fn data_each_attr() {
        let nodes = parse("[:template {:data-each \"features\"} [:li \"item\"]]");
        if let Node::Element(el) = &nodes[0] {
            assert_eq!(el.name, "template");
            assert_eq!(el.attr("data-each"), Some("features"));
            assert_eq!(el.children.len(), 1);
        }
    }

    #[test]
    fn multi_root() {
        let nodes = parse("[:h1 \"Title\"] [:p \"Body\"]");
        assert_eq!(nodes.len(), 2);
    }

    #[test]
    fn text_node_at_top_level() {
        let nodes = parse("\"Just text\"");
        assert_eq!(nodes.len(), 1);
        assert!(matches!(&nodes[0], Node::Text(t) if t == "Just text"));
    }

    #[test]
    fn nil_children_are_skipped() {
        let nodes = parse("[:div nil \"hello\" nil]");
        if let Node::Element(el) = &nodes[0] {
            assert_eq!(el.children.len(), 1);
            assert!(matches!(&el.children[0], Node::Text(t) if t == "hello"));
        }
    }

    #[test]
    fn self_closing_equivalent() {
        // An element with only attrs and no children
        let nodes = parse("[:link {:rel \"stylesheet\" :href \"/assets/style.css\"}]");
        if let Node::Element(el) = &nodes[0] {
            assert_eq!(el.name, "link");
            assert_eq!(el.attr("rel"), Some("stylesheet"));
            assert_eq!(el.attr("href"), Some("/assets/style.css"));
            assert!(el.children.is_empty());
        }
    }

    #[test]
    fn error_non_keyword_tag() {
        let err = parse_err("[\"div\"]");
        assert!(err.contains("keyword") || err.contains("expected"), "{err}");
    }

    #[test]
    fn error_unclosed_bracket() {
        let err = parse_err("[:div ");
        assert!(!err.is_empty());
    }

    #[test]
    fn presemble_namespaced_attr_uses_colon() {
        let nodes = parse("[:div {:presemble/class \"article.title\"} \"text\"]");
        if let Node::Element(el) = &nodes[0] {
            assert!(
                el.attrs.iter().any(|(k, _)| k == "presemble:class"),
                "expected presemble:class attr, got {:?}",
                el.attrs
            );
        } else {
            panic!("expected element");
        }
    }

    #[test]
    fn attr_values_are_form_str() {
        use crate::dom::Form;
        let nodes = parse("[:div {:class \"hero\"}]");
        if let Node::Element(el) = &nodes[0] {
            assert_eq!(
                el.attr_form("class"),
                Some(&Form::Str("hero".to_string())),
                "attr value should be Form::Str"
            );
        } else {
            panic!("expected element");
        }
    }

    #[test]
    fn line_comment_before_form() {
        let nodes = parse("; comment\n[:div \"text\"]");
        assert_eq!(nodes.len(), 1);
        if let Node::Element(el) = &nodes[0] {
            assert_eq!(el.name, "div");
        } else {
            panic!("expected element");
        }
    }

    #[test]
    fn line_comment_inside_element() {
        let nodes = parse("[:div\n  ; child comment\n  \"text\"]");
        assert_eq!(nodes.len(), 1);
        if let Node::Element(el) = &nodes[0] {
            assert_eq!(el.children.len(), 1);
            assert!(matches!(&el.children[0], Node::Text(t) if t == "text"));
        } else {
            panic!("expected element");
        }
    }

    #[test]
    fn line_comment_trailing() {
        let nodes = parse("[:div \"text\"] ; trailing");
        assert_eq!(nodes.len(), 1);
        if let Node::Element(el) = &nodes[0] {
            assert_eq!(el.name, "div");
        } else {
            panic!("expected element");
        }
    }

    // -----------------------------------------------------------------------
    // Phase 2: Symbol, Integer, Keyword, List, Set attribute values
    // -----------------------------------------------------------------------

    #[test]
    fn parse_symbol_attr_value() {
        let nodes = parse("[:div {:apply text}]");
        if let Node::Element(el) = &nodes[0] {
            assert_eq!(
                el.attr_form("apply"),
                Some(&Form::Symbol("text".to_string())),
                "expected Form::Symbol(\"text\")"
            );
        } else {
            panic!("expected element");
        }
    }

    #[test]
    fn parse_integer_attr_value() {
        let nodes = parse("[:div {:count 42}]");
        if let Node::Element(el) = &nodes[0] {
            assert_eq!(
                el.attr_form("count"),
                Some(&Form::Integer(42)),
                "expected Form::Integer(42)"
            );
        } else {
            panic!("expected element");
        }
    }

    #[test]
    fn parse_negative_integer_attr_value() {
        let nodes = parse("[:div {:offset -1}]");
        if let Node::Element(el) = &nodes[0] {
            assert_eq!(
                el.attr_form("offset"),
                Some(&Form::Integer(-1)),
                "expected Form::Integer(-1)"
            );
        } else {
            panic!("expected element");
        }
    }

    #[test]
    fn parse_keyword_attr_value() {
        let nodes = parse("[:div {:as :h3}]");
        if let Node::Element(el) = &nodes[0] {
            assert_eq!(
                el.attr_form("as"),
                Some(&Form::Keyword { namespace: None, name: "h3".to_string() }),
                "expected Form::Keyword"
            );
        } else {
            panic!("expected element");
        }
    }

    #[test]
    fn parse_namespaced_keyword_attr_value() {
        let nodes = parse("[:div {:type :presemble/heading}]");
        if let Node::Element(el) = &nodes[0] {
            assert_eq!(
                el.attr_form("type"),
                Some(&Form::Keyword {
                    namespace: Some("presemble".to_string()),
                    name: "heading".to_string()
                }),
                "expected namespaced Form::Keyword"
            );
        } else {
            panic!("expected element");
        }
    }

    #[test]
    fn parse_list_attr_value() {
        let nodes = parse("[:div {:apply (-> text to_lower)}]");
        if let Node::Element(el) = &nodes[0] {
            let form = el.attr_form("apply").expect("apply attr");
            assert!(
                matches!(form, Form::List(_)),
                "expected Form::List, got {form:?}"
            );
            if let Form::List(items) = form {
                assert_eq!(items.len(), 3);
                assert_eq!(items[0], Form::Symbol("->".to_string()));
                assert_eq!(items[1], Form::Symbol("text".to_string()));
                assert_eq!(items[2], Form::Symbol("to_lower".to_string()));
            }
        } else {
            panic!("expected element");
        }
    }

    #[test]
    fn parse_set_attr_value() {
        let nodes = parse("[:div {:tags #{a b c}}]");
        if let Node::Element(el) = &nodes[0] {
            let form = el.attr_form("tags").expect("tags attr");
            assert!(
                matches!(form, Form::Set(_)),
                "expected Form::Set, got {form:?}"
            );
            if let Form::Set(items) = form {
                assert_eq!(items.len(), 3);
                assert!(items.contains(&Form::Symbol("a".to_string())));
                assert!(items.contains(&Form::Symbol("b".to_string())));
                assert!(items.contains(&Form::Symbol("c".to_string())));
            }
        } else {
            panic!("expected element");
        }
    }

    #[test]
    fn parse_nested_list() {
        // (-> text (format "yyyy")) — nested list inside list
        let nodes = parse("[:div {:apply (-> text (format \"yyyy\"))}]");
        if let Node::Element(el) = &nodes[0] {
            let form = el.attr_form("apply").expect("apply attr");
            if let Form::List(items) = form {
                assert_eq!(items.len(), 3);
                assert_eq!(items[0], Form::Symbol("->".to_string()));
                assert_eq!(items[1], Form::Symbol("text".to_string()));
                // Third item is nested list (format "yyyy")
                if let Form::List(inner) = &items[2] {
                    assert_eq!(inner[0], Form::Symbol("format".to_string()));
                    assert_eq!(inner[1], Form::Str("yyyy".to_string()));
                } else {
                    panic!("expected nested Form::List, got {:?}", items[2]);
                }
            } else {
                panic!("expected Form::List, got {form:?}");
            }
        } else {
            panic!("expected element");
        }
    }

    #[test]
    fn nil_still_works() {
        // nil in child position is skipped
        let nodes = parse("[:div nil \"text\"]");
        if let Node::Element(el) = &nodes[0] {
            assert_eq!(el.children.len(), 1);
            assert!(matches!(&el.children[0], Node::Text(t) if t == "text"));
        } else {
            panic!("expected element");
        }
    }

    #[test]
    fn nil_as_attr_value_gives_nil_form() {
        let nodes = parse("[:div {:data nil}]");
        if let Node::Element(el) = &nodes[0] {
            assert_eq!(
                el.attr_form("data"),
                Some(&Form::Nil),
                "nil attr value should be Form::Nil"
            );
        } else {
            panic!("expected element");
        }
    }

    #[test]
    fn symbol_with_arrow_is_valid() {
        // The threading macro -> is a valid symbol
        let nodes = parse("[:div {:fn ->}]");
        if let Node::Element(el) = &nodes[0] {
            assert_eq!(
                el.attr_form("fn"),
                Some(&Form::Symbol("->".to_string()))
            );
        } else {
            panic!("expected element");
        }
    }

    #[test]
    fn dash_followed_by_alpha_is_symbol() {
        // -foo should be parsed as a symbol, not an integer
        let nodes = parse("[:div {:fn -foo}]");
        if let Node::Element(el) = &nodes[0] {
            assert_eq!(
                el.attr_form("fn"),
                Some(&Form::Symbol("-foo".to_string()))
            );
        } else {
            panic!("expected element");
        }
    }

    // -----------------------------------------------------------------------
    // Phase 3a: (juxt ...) and (apply ...) composition forms
    // -----------------------------------------------------------------------

    #[test]
    fn parse_juxt_form() {
        // (juxt header footer) should produce presemble:juxt with two presemble:include children
        let nodes = parse("(juxt header footer)");
        assert_eq!(nodes.len(), 1);
        if let Node::Element(juxt) = &nodes[0] {
            assert_eq!(juxt.name, "presemble:juxt");
            assert_eq!(juxt.children.len(), 2);
            if let Node::Element(c0) = &juxt.children[0] {
                assert_eq!(c0.name, "presemble:include");
                assert_eq!(c0.attr("src"), Some("header"));
            } else {
                panic!("expected first child to be element");
            }
            if let Node::Element(c1) = &juxt.children[1] {
                assert_eq!(c1.name, "presemble:include");
                assert_eq!(c1.attr("src"), Some("footer"));
            } else {
                panic!("expected second child to be element");
            }
        } else {
            panic!("expected presemble:juxt element");
        }
    }

    #[test]
    fn parse_juxt_with_self_apply() {
        // (juxt header (apply self/body) footer) should produce:
        //   presemble:juxt with [presemble:include, presemble:apply, presemble:include]
        let nodes = parse("(juxt header (apply self/body) footer)");
        assert_eq!(nodes.len(), 1);
        if let Node::Element(juxt) = &nodes[0] {
            assert_eq!(juxt.name, "presemble:juxt");
            assert_eq!(juxt.children.len(), 3, "expected 3 children, got {:?}", juxt.children.len());

            if let Node::Element(c0) = &juxt.children[0] {
                assert_eq!(c0.name, "presemble:include");
                assert_eq!(c0.attr("src"), Some("header"));
            } else {
                panic!("first child should be presemble:include");
            }

            if let Node::Element(c1) = &juxt.children[1] {
                assert_eq!(c1.name, "presemble:apply");
                assert_eq!(c1.attr("template"), Some("body"));
                assert_eq!(c1.attr("data"), Some("input"));
            } else {
                panic!("second child should be presemble:apply");
            }

            if let Node::Element(c2) = &juxt.children[2] {
                assert_eq!(c2.name, "presemble:include");
                assert_eq!(c2.attr("src"), Some("footer"));
            } else {
                panic!("third child should be presemble:include");
            }
        } else {
            panic!("expected presemble:juxt element");
        }
    }

    #[test]
    fn parse_juxt_with_bare_self_symbol() {
        // (juxt header self/nav footer) — self/nav becomes presemble:apply
        let nodes = parse("(juxt header self/nav footer)");
        assert_eq!(nodes.len(), 1);
        if let Node::Element(juxt) = &nodes[0] {
            assert_eq!(juxt.children.len(), 3);
            if let Node::Element(c1) = &juxt.children[1] {
                assert_eq!(c1.name, "presemble:apply");
                assert_eq!(c1.attr("template"), Some("nav"));
                assert_eq!(c1.attr("data"), Some("input"));
            } else {
                panic!("middle child should be presemble:apply");
            }
        }
    }

    #[test]
    fn parse_standalone_apply_form() {
        // (apply self/body) at top level should produce a presemble:apply node
        let nodes = parse("(apply self/body)");
        assert_eq!(nodes.len(), 1);
        if let Node::Element(el) = &nodes[0] {
            assert_eq!(el.name, "presemble:apply");
            assert_eq!(el.attr("template"), Some("body"));
            assert_eq!(el.attr("data"), Some("input"));
        } else {
            panic!("expected presemble:apply element");
        }
    }

    #[test]
    fn parse_juxt_inside_element() {
        // [:body (juxt header self/content footer)] — juxt inside an element
        let nodes = parse("[:body (juxt header self/content footer)]");
        assert_eq!(nodes.len(), 1);
        if let Node::Element(body) = &nodes[0] {
            assert_eq!(body.name, "body");
            assert_eq!(body.children.len(), 1);
            if let Node::Element(juxt) = &body.children[0] {
                assert_eq!(juxt.name, "presemble:juxt");
                assert_eq!(juxt.children.len(), 3);
            } else {
                panic!("expected presemble:juxt child inside body");
            }
        } else {
            panic!("expected body element");
        }
    }

    // -----------------------------------------------------------------------
    // Round-trip tests
    // -----------------------------------------------------------------------

    #[test]
    fn roundtrip_symbol_attr() {
        use crate::hiccup_serializer::serialize_to_hiccup;
        let src = "[:div {:apply text}]";
        let nodes1 = parse_template_hiccup(src).expect("first parse");
        let serialized = serialize_to_hiccup(&nodes1);
        let nodes2 = parse_template_hiccup(&serialized).expect("second parse");
        if let (Node::Element(el1), Node::Element(el2)) = (&nodes1[0], &nodes2[0]) {
            assert_eq!(el1.attrs, el2.attrs, "symbol attr roundtrip mismatch");
        } else {
            panic!("expected elements");
        }
    }

    #[test]
    fn roundtrip_integer_attr() {
        use crate::hiccup_serializer::serialize_to_hiccup;
        let src = "[:div {:count 42}]";
        let nodes1 = parse_template_hiccup(src).expect("first parse");
        let serialized = serialize_to_hiccup(&nodes1);
        let nodes2 = parse_template_hiccup(&serialized).expect("second parse");
        if let (Node::Element(el1), Node::Element(el2)) = (&nodes1[0], &nodes2[0]) {
            assert_eq!(el1.attrs, el2.attrs, "integer attr roundtrip mismatch");
        } else {
            panic!("expected elements");
        }
    }

    #[test]
    fn roundtrip_keyword_attr() {
        use crate::hiccup_serializer::serialize_to_hiccup;
        let src = "[:div {:as :h3}]";
        let nodes1 = parse_template_hiccup(src).expect("first parse");
        let serialized = serialize_to_hiccup(&nodes1);
        let nodes2 = parse_template_hiccup(&serialized).expect("second parse");
        if let (Node::Element(el1), Node::Element(el2)) = (&nodes1[0], &nodes2[0]) {
            assert_eq!(el1.attrs, el2.attrs, "keyword attr roundtrip mismatch");
        } else {
            panic!("expected elements");
        }
    }

    #[test]
    fn roundtrip_list_attr() {
        use crate::hiccup_serializer::serialize_to_hiccup;
        let src = "[:div {:apply (-> text to_lower)}]";
        let nodes1 = parse_template_hiccup(src).expect("first parse");
        let serialized = serialize_to_hiccup(&nodes1);
        let nodes2 = parse_template_hiccup(&serialized).expect("second parse");
        if let (Node::Element(el1), Node::Element(el2)) = (&nodes1[0], &nodes2[0]) {
            assert_eq!(el1.attrs, el2.attrs, "list attr roundtrip mismatch");
        } else {
            panic!("expected elements");
        }
    }

    #[test]
    fn roundtrip_set_attr() {
        use crate::hiccup_serializer::serialize_to_hiccup;
        let src = "[:div {:tags #{a b c}}]";
        let nodes1 = parse_template_hiccup(src).expect("first parse");
        let serialized = serialize_to_hiccup(&nodes1);
        let nodes2 = parse_template_hiccup(&serialized).expect("second parse");
        if let (Node::Element(el1), Node::Element(el2)) = (&nodes1[0], &nodes2[0]) {
            assert_eq!(el1.attrs, el2.attrs, "set attr roundtrip mismatch");
        } else {
            panic!("expected elements");
        }
    }
}
