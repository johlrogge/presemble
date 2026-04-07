use serde::{Deserialize, Serialize};

/// A Clojure-style data literal / S-expression.
/// This is the AST for the Presemble lisp and the shared type
/// between reader, macros, and evaluator.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Form {
    /// A symbol: `hello`, `sort-by`, `presemble/insert`
    Symbol(String),
    /// A keyword: `:name`, `:presemble/define`
    Keyword {
        namespace: Option<String>,
        name: String,
    },
    /// A string literal: `"hello"`
    Str(String),
    /// An integer literal: `42`
    Integer(i64),
    /// A boolean: `true`, `false`
    Bool(bool),
    /// Nil
    Nil,
    /// A list (function call or macro invocation): `(f x y)`
    List(Vec<Form>),
    /// A vector (data literal): `[a b c]`
    Vector(Vec<Form>),
    /// A map (data literal): `{:a 1 :b 2}`
    Map(Vec<(Form, Form)>),
    /// A set (data literal): `#{a b c}`
    Set(Vec<Form>),
}

impl Form {
    /// Check if this form is a symbol with the given name.
    pub fn is_symbol(&self, name: &str) -> bool {
        matches!(self, Form::Symbol(s) if s == name)
    }

    /// Check if this form is a keyword with the given name (no namespace).
    pub fn is_keyword(&self, name: &str) -> bool {
        matches!(self, Form::Keyword { namespace: None, name: n } if n == name)
    }

    /// Get the symbol name, if this is a symbol.
    pub fn as_symbol(&self) -> Option<&str> {
        match self {
            Form::Symbol(s) => Some(s),
            _ => None,
        }
    }

    /// Get the keyword name, if this is a keyword.
    pub fn as_keyword_name(&self) -> Option<&str> {
        match self {
            Form::Keyword { name, .. } => Some(name),
            _ => None,
        }
    }

    /// Get the string value, if this is a string.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Form::Str(s) => Some(s),
            _ => None,
        }
    }

    /// Get the integer value, if this is an integer.
    pub fn as_integer(&self) -> Option<i64> {
        match self {
            Form::Integer(n) => Some(*n),
            _ => None,
        }
    }

    /// Get the list elements, if this is a list.
    pub fn as_list(&self) -> Option<&[Form]> {
        match self {
            Form::List(items) => Some(items),
            _ => None,
        }
    }
}

/// Display a Form as EDN.
impl std::fmt::Display for Form {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Form::Symbol(s) => write!(f, "{s}"),
            Form::Keyword {
                namespace: Some(ns),
                name,
            } => write!(f, ":{ns}/{name}"),
            Form::Keyword {
                namespace: None,
                name,
            } => write!(f, ":{name}"),
            Form::Str(s) => write!(
                f,
                "\"{}\"",
                s.replace('\\', "\\\\").replace('"', "\\\"")
            ),
            Form::Integer(n) => write!(f, "{n}"),
            Form::Bool(b) => write!(f, "{b}"),
            Form::Nil => write!(f, "nil"),
            Form::List(items) => {
                write!(f, "(")?;
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        write!(f, " ")?;
                    }
                    write!(f, "{item}")?;
                }
                write!(f, ")")
            }
            Form::Vector(items) => {
                write!(f, "[")?;
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        write!(f, " ")?;
                    }
                    write!(f, "{item}")?;
                }
                write!(f, "]")
            }
            Form::Map(pairs) => {
                write!(f, "{{")?;
                for (i, (k, v)) in pairs.iter().enumerate() {
                    if i > 0 {
                        write!(f, " ")?;
                    }
                    write!(f, "{k} {v}")?;
                }
                write!(f, "}}")
            }
            Form::Set(items) => {
                write!(f, "#{{")?;
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        write!(f, " ")?;
                    }
                    write!(f, "{item}")?;
                }
                write!(f, "}}")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_symbol() {
        assert_eq!(Form::Symbol("hello".into()).to_string(), "hello");
        assert_eq!(Form::Symbol("sort-by".into()).to_string(), "sort-by");
    }

    #[test]
    fn display_keyword_no_namespace() {
        let k = Form::Keyword {
            namespace: None,
            name: "name".into(),
        };
        assert_eq!(k.to_string(), ":name");
    }

    #[test]
    fn display_keyword_with_namespace() {
        let k = Form::Keyword {
            namespace: Some("presemble".into()),
            name: "define".into(),
        };
        assert_eq!(k.to_string(), ":presemble/define");
    }

    #[test]
    fn display_string_escaping() {
        let s = Form::Str("hello \"world\"".into());
        assert_eq!(s.to_string(), r#""hello \"world\"""#);

        let s2 = Form::Str("back\\slash".into());
        assert_eq!(s2.to_string(), r#""back\\slash""#);
    }

    #[test]
    fn display_integer() {
        assert_eq!(Form::Integer(42).to_string(), "42");
        assert_eq!(Form::Integer(-1).to_string(), "-1");
    }

    #[test]
    fn display_bool() {
        assert_eq!(Form::Bool(true).to_string(), "true");
        assert_eq!(Form::Bool(false).to_string(), "false");
    }

    #[test]
    fn display_nil() {
        assert_eq!(Form::Nil.to_string(), "nil");
    }

    #[test]
    fn display_list() {
        let l = Form::List(vec![
            Form::Symbol("+".into()),
            Form::Integer(1),
            Form::Integer(2),
        ]);
        assert_eq!(l.to_string(), "(+ 1 2)");
    }

    #[test]
    fn display_vector() {
        let v = Form::Vector(vec![
            Form::Integer(1),
            Form::Integer(2),
            Form::Integer(3),
        ]);
        assert_eq!(v.to_string(), "[1 2 3]");
    }

    #[test]
    fn display_map() {
        let m = Form::Map(vec![(
            Form::Keyword {
                namespace: None,
                name: "a".into(),
            },
            Form::Integer(1),
        )]);
        assert_eq!(m.to_string(), "{:a 1}");
    }

    #[test]
    fn display_set() {
        let s = Form::Set(vec![Form::Integer(1), Form::Integer(2)]);
        assert_eq!(s.to_string(), "#{1 2}");
    }

    #[test]
    fn is_symbol_helper() {
        let sym = Form::Symbol("foo".into());
        assert!(sym.is_symbol("foo"));
        assert!(!sym.is_symbol("bar"));
        assert!(!Form::Integer(1).is_symbol("foo"));
    }

    #[test]
    fn is_keyword_helper() {
        let kw = Form::Keyword {
            namespace: None,
            name: "foo".into(),
        };
        assert!(kw.is_keyword("foo"));
        assert!(!kw.is_keyword("bar"));

        // Namespaced keyword does NOT match is_keyword (requires namespace: None)
        let nsk = Form::Keyword {
            namespace: Some("ns".into()),
            name: "foo".into(),
        };
        assert!(!nsk.is_keyword("foo"));
    }

    #[test]
    fn accessor_methods() {
        assert_eq!(Form::Symbol("sym".into()).as_symbol(), Some("sym"));
        assert_eq!(Form::Integer(5).as_symbol(), None);

        let kw = Form::Keyword {
            namespace: Some("ns".into()),
            name: "kw".into(),
        };
        assert_eq!(kw.as_keyword_name(), Some("kw"));
        assert_eq!(Form::Nil.as_keyword_name(), None);

        assert_eq!(Form::Str("hello".into()).as_str(), Some("hello"));
        assert_eq!(Form::Nil.as_str(), None);

        assert_eq!(Form::Integer(99).as_integer(), Some(99));
        assert_eq!(Form::Nil.as_integer(), None);

        let list = Form::List(vec![Form::Integer(1)]);
        assert_eq!(list.as_list(), Some([Form::Integer(1)].as_ref()));
        assert_eq!(Form::Nil.as_list(), None);
    }

    #[test]
    fn display_roundtrip_nested() {
        // Nested structure display check
        let form = Form::List(vec![
            Form::Symbol("->>".into()),
            Form::Keyword {
                namespace: None,
                name: "post".into(),
            },
            Form::List(vec![
                Form::Symbol("sort-by".into()),
                Form::Keyword {
                    namespace: None,
                    name: "published".into(),
                },
                Form::Keyword {
                    namespace: None,
                    name: "desc".into(),
                },
            ]),
            Form::List(vec![
                Form::Symbol("take".into()),
                Form::Integer(3),
            ]),
        ]);
        assert_eq!(
            form.to_string(),
            "(->> :post (sort-by :published :desc) (take 3))"
        );
    }
}
