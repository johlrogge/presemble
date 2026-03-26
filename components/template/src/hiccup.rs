use crate::dom::{Element, Node};
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
    Keyword {
        namespace: Option<String>,
        name: String,
    },
    StringLit(String),
    Nil,
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
            // nil keyword (bare word)
            'n' if chars[i..].starts_with(&['n', 'i', 'l']) => {
                // Make sure it's not followed by an identifier character
                let end = i + 3;
                let next_is_ident = end < len
                    && (chars[end].is_alphanumeric() || chars[end] == '-' || chars[end] == '_');
                if next_is_ident {
                    return Err(TemplateError::ParseError(format!(
                        "unexpected token at position {i}"
                    )));
                }
                tokens.push(Token::Nil);
                i += 3;
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
/// tokenizer), keep the raw name. Namespaces are not translated for attributes —
/// just concatenate with '/'.
fn keyword_to_attr_name(namespace: &Option<String>, name: &str) -> String {
    match namespace.as_deref() {
        None => name.to_string(),
        Some(ns) => format!("{ns}/{name}"),
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

    fn expect(&mut self, expected: &Token) -> Result<(), TemplateError> {
        match self.next() {
            Some(ref tok) if tok == expected => Ok(()),
            Some(other) => Err(TemplateError::ParseError(format!(
                "expected {expected:?}, got {other:?}"
            ))),
            None => Err(TemplateError::ParseError(format!(
                "expected {expected:?}, got end of input"
            ))),
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

            Some(other) => Err(TemplateError::ParseError(format!(
                "unexpected token in node position: {other:?}"
            ))),
        }
    }

    /// Parse the interior of an attribute map (after `{` has been consumed).
    /// Expects pairs of `Keyword StringLit` until `}`.
    fn parse_attr_map(&mut self) -> Result<Vec<(String, String)>, TemplateError> {
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
                    // Consume the keyword
                    let (ns, name) = match self.next() {
                        Some(Token::Keyword { namespace, name }) => (namespace, name),
                        _ => unreachable!(),
                    };
                    let attr_name = keyword_to_attr_name(&ns, &name);

                    // Next must be a StringLit value
                    match self.next() {
                        Some(Token::StringLit(value)) => {
                            attrs.push((attr_name, value));
                        }
                        Some(other) => {
                            return Err(TemplateError::ParseError(format!(
                                "expected string value for attribute '{attr_name}', got {other:?}"
                            )))
                        }
                        None => {
                            return Err(TemplateError::ParseError(format!(
                                "unexpected end of input after attribute key '{attr_name}'"
                            )))
                        }
                    }
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
        let nodes = parse("[:template {:data-each \"site.features\"} [:li \"item\"]]");
        if let Node::Element(el) = &nodes[0] {
            assert_eq!(el.name, "template");
            assert_eq!(el.attr("data-each"), Some("site.features"));
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
}
