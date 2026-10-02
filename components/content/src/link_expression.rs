//! Link-expression surface syntax (ADR-058).
//!
//! A link expression is written `[text](->> :source op...)` as a whole
//! top-level paragraph. Its destination is a closed grammar, not an
//! s-expression; it is parsed here into [`LinkTarget`] data and never reaches
//! the Lisp reader or evaluator:
//!
//! ```text
//! thread  := '->>' WS ':' keyword (WS op)*
//! op      := '(' 'sort-by' WS ':' keyword [WS (':asc' | ':desc')] ')'
//!          | '(' 'take' WS digits ')'
//!          | '(' 'filter' WS ':' keyword WS string ')'
//!          | '(' 'refs-to' WS ('self' | string) ')'
//! keyword := [A-Za-z0-9_-]+
//! string  := '"' ( [^"\\] | '\\' ["\\] )* '"'
//! WS      := [ \t]+
//! ```
//!
//! Positions (`at`) in errors are byte offsets into the thread text.

use std::fmt;

use crate::document::{Keyword, LinkOp, LinkTarget, RefsToTarget};
use crate::parser::{diagnose_marker_paragraph, is_marker_paragraph};

/// Why a link-expression destination does not match the grammar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkExpressionError {
    /// The destination is empty.
    Empty,
    /// `->` (thread-first) was written; only `->>` is accepted.
    ThreadFirst,
    /// The destination does not start with `->>`.
    ExpectedThread,
    /// Whitespace was required at this position.
    ExpectedWhitespace { at: usize },
    /// A `:keyword` was required at this position.
    ExpectedKeyword { at: usize },
    /// A `:keyword` contains characters outside `[A-Za-z0-9_-]`.
    InvalidKeyword(String),
    /// An operation other than `sort-by`, `take`, `filter` or `refs-to`.
    UnknownOp(String),
    /// `sort-by` direction other than `:asc` or `:desc`.
    ExpectedDirection { at: usize },
    /// `take` without a non-negative whole number.
    ExpectedCount,
    /// A quoted string was required at this position.
    ExpectedString { at: usize },
    /// A string is missing its closing `"`.
    UnterminatedString,
    /// A backslash in a string not followed by `"` or `\`.
    InvalidEscape { at: usize },
    /// A `)` was required at this position.
    ExpectedCloseParen { at: usize },
    /// Text after the end of a well-formed prefix.
    TrailingInput { at: usize },
}

impl fmt::Display for LinkExpressionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(f, "empty link expression"),
            Self::ThreadFirst => {
                write!(f, "`->` is not supported in link expressions; use `->>`")
            }
            Self::ExpectedThread => write!(f, "a link expression starts with `->>`"),
            Self::ExpectedWhitespace { at } => write!(f, "expected whitespace at byte {at}"),
            Self::ExpectedKeyword { at } => write!(f, "expected a `:keyword` at byte {at}"),
            Self::InvalidKeyword(k) => write!(
                f,
                "invalid keyword `:{k}`: use only letters, digits, `_` and `-`"
            ),
            Self::UnknownOp(op) => write!(
                f,
                "unknown operation `{op}`: expected `sort-by`, `take`, `filter` or `refs-to`"
            ),
            Self::ExpectedDirection { at } => {
                write!(f, "expected `:asc` or `:desc` at byte {at}")
            }
            Self::ExpectedCount => write!(f, "`take` needs a non-negative whole number"),
            Self::ExpectedString { at } => write!(f, "expected a quoted string at byte {at}"),
            Self::UnterminatedString => write!(f, "unterminated string: missing closing `\"`"),
            Self::InvalidEscape { at } => write!(
                f,
                "invalid escape at byte {at}: only `\\\"` and `\\\\` are allowed in strings"
            ),
            Self::ExpectedCloseParen { at } => write!(f, "expected `)` at byte {at}"),
            Self::TrailingInput { at } => write!(f, "unexpected input at byte {at}"),
        }
    }
}

impl std::error::Error for LinkExpressionError {}

/// Parse a thread expression `->> :source op...` (no outer parentheses).
pub fn parse_thread(dest: &str) -> Result<LinkTarget, LinkExpressionError> {
    if dest.is_empty() {
        return Err(LinkExpressionError::Empty);
    }
    let mut c = Cursor { src: dest, pos: 0 };
    if !c.eat("->>") {
        return Err(if dest.starts_with("->") {
            LinkExpressionError::ThreadFirst
        } else {
            LinkExpressionError::ExpectedThread
        });
    }
    c.require_ws()?;
    let source = c.keyword()?;
    let mut operations = Vec::new();
    while !c.at_end() {
        let at = c.pos;
        if !c.ws() || c.peek() != Some(b'(') {
            return Err(LinkExpressionError::TrailingInput { at });
        }
        operations.push(c.op()?);
    }
    Ok(LinkTarget::ThreadExpr { source, operations })
}

/// Parse a link-expression destination into a [`LinkTarget`]:
///
/// - `:path` → `PathRef(path)`
/// - `->> …` → [`parse_thread`]
/// - `(->> …)` → the outer parentheses stripped, then [`parse_thread`]
/// - `->` or `(->` → [`LinkExpressionError::ThreadFirst`]
pub fn parse_destination(dest: &str) -> Result<LinkTarget, LinkExpressionError> {
    if let Some(path) = dest.strip_prefix(':') {
        return Ok(LinkTarget::PathRef(path.to_string()));
    }
    if dest.is_empty() {
        return Err(LinkExpressionError::Empty);
    }
    if dest.starts_with("->>") {
        return parse_thread(dest);
    }
    if let Some(after_open) = dest.strip_prefix("(") {
        if after_open.starts_with("->>") {
            let inner = after_open
                .strip_suffix(')')
                .ok_or(LinkExpressionError::ExpectedCloseParen { at: dest.len() })?;
            return parse_thread(inner);
        }
        if after_open.starts_with("->") {
            return Err(LinkExpressionError::ThreadFirst);
        }
    }
    if dest.starts_with("->") {
        return Err(LinkExpressionError::ThreadFirst);
    }
    Err(LinkExpressionError::ExpectedThread)
}

/// A paragraph that has the shape of a link expression: a single line
/// `[text](dest)`, optionally followed by a slot marker `{#slot}`, with
/// nothing else before or after it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Candidate<'a> {
    pub text: &'a str,
    pub dest: &'a str,
    /// The slot marker's name (a valid [`Keyword`]), if a marker is present.
    pub slot: Option<&'a str>,
}

/// Recognise a paragraph whose (trimmed) source is a single line
/// `[text](dest)`, optionally followed by one or more spaces or tabs and a
/// slot marker `{#slot}` (ADR-058, decision 9), where:
///
/// - `text` contains no `[`, `]` or newline;
/// - `dest` starts with `->>`, `->`, `(->>` or `(->`;
/// - the parentheses in `dest` never close more than they opened (parentheses
///   inside quoted strings do not count), so the final `)` is the outer one;
/// - the marker's name, if present, is a valid [`Keyword`].
///
/// Whether `dest` parses is not checked here.
pub fn recognise_paragraph(source: &str) -> Option<Candidate<'_>> {
    let line = source.trim();
    if line.contains('\n') {
        return None;
    }
    match split_marker(line) {
        Some((expression, name)) => {
            let candidate = recognise_expression(expression)?;
            Keyword::is_valid(name).then_some(Candidate { slot: Some(name), ..candidate })
        }
        None => recognise_expression(line),
    }
}

/// Split a trailing slot marker off `line`: `(expression, name)` when `line`
/// ends with `}` and its last `{#` that follows a space or tab starts the
/// marker, and the text before the marker (whitespace trimmed) ends with `)`.
/// The name is not validated.
pub(crate) fn split_marker(line: &str) -> Option<(&str, &str)> {
    let body = line.strip_suffix('}')?;
    let at = body
        .rmatch_indices("{#")
        .map(|(at, _)| at)
        .find(|&at| body[..at].ends_with([' ', '\t']))?;
    let expression = body[..at].trim_end_matches([' ', '\t']);
    expression.ends_with(')').then_some((expression, &body[at + 2..]))
}

/// Recognise `[text](dest)` (no marker) on a single trimmed line.
fn recognise_expression(line: &str) -> Option<Candidate<'_>> {
    let after_open = line.strip_prefix('[')?;
    let close = after_open.find(']')?;
    let text = &after_open[..close];
    if text.contains('[') {
        return None;
    }
    let dest = after_open[close + 1..].strip_prefix('(')?.strip_suffix(')')?;
    if !(dest.starts_with("->") || dest.starts_with("(->")) {
        return None;
    }
    parens_never_negative(dest).then_some(Candidate { text, dest, slot: None })
}

/// Whether the depth of parentheses in `dest` (outside quoted strings) never
/// drops below zero.
fn parens_never_negative(dest: &str) -> bool {
    let mut depth: i32 = 0;
    let mut in_string = false;
    let mut escaped = false;
    for b in dest.bytes() {
        if in_string {
            match (escaped, b) {
                (true, _) => escaped = false,
                (false, b'\\') => escaped = true,
                (false, b'"') => in_string = false,
                _ => {}
            }
            continue;
        }
        match b {
            b'"' => in_string = true,
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth < 0 {
                    return false;
                }
            }
            _ => {}
        }
    }
    true
}

/// What is wrong with a paragraph that looks like a link expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkExpressionDiagnostic {
    /// The paragraph has link-expression shape but does not parse.
    Malformed(LinkExpressionError),
    /// A link expression appears inside running text instead of standing
    /// alone as a whole paragraph.
    MustStandAlone,
    /// A link expression is followed by a slot marker whose name is not a
    /// valid keyword (`{#a b}`); the paragraph is not a link expression.
    InvalidSlotMarker(String),
    /// A link expression's slot marker `{#name}` is on the line after it, in
    /// the same paragraph.
    MarkerOnOwnLine(String),
    /// A `{#name}` that marks nothing: a paragraph that is only the marker,
    /// or a bare marker line after something that is not a link expression.
    StrayMarker(String),
    /// A slot marker after a link expression that spans several lines.
    MarkedExpressionSpansLines(String),
    /// A slot marker after something that is not a link expression (an
    /// image, a plain link, text).
    MarkerNotOnLinkExpression(String),
}

impl fmt::Display for LinkExpressionDiagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Malformed(LinkExpressionError::ThreadFirst) => {
                write!(f, "thread-first `->` is not supported in link expressions; use `->>`")
            }
            Self::Malformed(e) => write!(f, "malformed link expression: {e}"),
            Self::MustStandAlone => write!(f, "link expressions must stand alone (ADR-058)"),
            Self::InvalidSlotMarker(name) => write!(
                f,
                "invalid slot marker `{{#{name}}}` on a link expression: \
                 use only letters, digits, `_` and `-`"
            ),
            Self::MarkerOnOwnLine(name) => write!(
                f,
                "`{{#{name}}}`: put the slot marker on the same line as the link expression"
            ),
            Self::StrayMarker(name) => write!(
                f,
                "`{{#{name}}}` on its own is not allowed; put it at the end of a link \
                 expression's line, or remove it"
            ),
            Self::MarkedExpressionSpansLines(_) => write!(
                f,
                "a slot marker must be on the same line as a one-line link expression"
            ),
            Self::MarkerNotOnLinkExpression(name) => write!(
                f,
                "`{{#{name}}}`: slot markers apply to link expressions only"
            ),
        }
    }
}

/// Diagnose a paragraph source that is, or contains, a link expression which
/// will not take effect.
///
/// A paragraph that starts with `\[` and is otherwise a candidate is the
/// serializer's escaped form of a literal paragraph and is not diagnosed.
///
/// A marker-shaped paragraph (`is_marker_paragraph`) is diagnosed by
/// `diagnose_marker_paragraph`.
pub fn diagnose_paragraph(source: &str) -> Option<LinkExpressionDiagnostic> {
    let trimmed = source.trim();
    if is_marker_paragraph(trimmed) {
        return Some(diagnose_marker_paragraph(trimmed));
    }
    if let Some(candidate) = recognise_paragraph(trimmed) {
        return parse_destination(candidate.dest)
            .err()
            .map(LinkExpressionDiagnostic::Malformed);
    }
    if trimmed
        .strip_prefix('\\')
        .is_some_and(|rest| recognise_paragraph(rest).is_some())
    {
        return None;
    }
    let prose = without_code_spans(trimmed);
    (prose.contains("](->>") || prose.contains("]((->>"))
        .then_some(LinkExpressionDiagnostic::MustStandAlone)
}

/// The name in a line that is only a slot marker `{#name}`.
pub(crate) fn bare_marker(line: &str) -> Option<&str> {
    let name = line.trim().strip_prefix("{#")?.strip_suffix('}')?;
    (!name.contains('}')).then_some(name)
}

/// `s` with inline code spans removed, so syntax quoted in documentation
/// (`` `[x](->> :post)` ``) is not diagnosed. A backtick run opens a span that
/// closes at the next run of the same length; an unclosed run is literal.
fn without_code_spans(s: &str) -> String {
    let bytes = s.as_bytes();
    let run_at = |i: usize| bytes[i..].iter().take_while(|&&b| b == b'`').count();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    let mut copied_to = 0;
    while i < bytes.len() {
        if bytes[i] != b'`' {
            i += 1;
            continue;
        }
        let n = run_at(i);
        let mut j = i + n;
        let close = loop {
            match bytes[j..].iter().position(|&b| b == b'`') {
                None => break None,
                Some(k) => {
                    let m = run_at(j + k);
                    if m == n {
                        break Some(j + k + m);
                    }
                    j += k + m;
                }
            }
        };
        match close {
            Some(end) => {
                out.push_str(&s[copied_to..i]);
                copied_to = end;
                i = end;
            }
            None => i += n,
        }
    }
    out.push_str(&s[copied_to..]);
    out
}

// ── Tokenizer ────────────────────────────────────────────────────────────────

struct Cursor<'a> {
    src: &'a str,
    pos: usize,
}

impl Cursor<'_> {
    fn rest(&self) -> &str {
        &self.src[self.pos..]
    }

    fn at_end(&self) -> bool {
        self.pos >= self.src.len()
    }

    fn peek(&self) -> Option<u8> {
        self.src.as_bytes().get(self.pos).copied()
    }

    fn eat(&mut self, s: &str) -> bool {
        if self.rest().starts_with(s) {
            self.pos += s.len();
            true
        } else {
            false
        }
    }

    /// Consume `[ \t]*`; return whether anything was consumed.
    fn ws(&mut self) -> bool {
        let start = self.pos;
        while matches!(self.peek(), Some(b' ' | b'\t')) {
            self.pos += 1;
        }
        self.pos > start
    }

    fn require_ws(&mut self) -> Result<(), LinkExpressionError> {
        let at = self.pos;
        if self.ws() {
            Ok(())
        } else {
            Err(LinkExpressionError::ExpectedWhitespace { at })
        }
    }

    fn require_close(&mut self) -> Result<(), LinkExpressionError> {
        let at = self.pos;
        if self.eat(")") {
            Ok(())
        } else {
            Err(LinkExpressionError::ExpectedCloseParen { at })
        }
    }

    /// A word: everything up to whitespace, a parenthesis or the end.
    fn word(&mut self) -> &str {
        let start = self.pos;
        while let Some(b) = self.peek() {
            if matches!(b, b' ' | b'\t' | b'(' | b')') {
                break;
            }
            // Advance by whole characters so slices stay on char boundaries.
            self.pos += self.rest().chars().next().map_or(1, char::len_utf8);
        }
        &self.src[start..self.pos]
    }

    /// `':' keyword`
    fn keyword(&mut self) -> Result<Keyword, LinkExpressionError> {
        let at = self.pos;
        if !self.eat(":") {
            return Err(LinkExpressionError::ExpectedKeyword { at });
        }
        let word = self.word();
        if word.is_empty() {
            return Err(LinkExpressionError::ExpectedKeyword { at });
        }
        Keyword::new(word).map_err(|e| LinkExpressionError::InvalidKeyword(e.0))
    }

    /// `'"' ( [^"\\] | '\\' ["\\] )* '"'`, unescaped.
    fn string(&mut self) -> Result<String, LinkExpressionError> {
        let at = self.pos;
        if !self.eat("\"") {
            return Err(LinkExpressionError::ExpectedString { at });
        }
        let mut out = String::new();
        let mut chars = self.rest().char_indices();
        while let Some((i, ch)) = chars.next() {
            match ch {
                '"' => {
                    self.pos += i + 1;
                    return Ok(out);
                }
                '\\' => match chars.next() {
                    Some((_, e @ ('"' | '\\'))) => out.push(e),
                    Some(_) => {
                        return Err(LinkExpressionError::InvalidEscape { at: self.pos + i });
                    }
                    None => return Err(LinkExpressionError::UnterminatedString),
                },
                other => out.push(other),
            }
        }
        Err(LinkExpressionError::UnterminatedString)
    }

    /// `'(' name args ')'`
    fn op(&mut self) -> Result<LinkOp, LinkExpressionError> {
        let at = self.pos;
        if !self.eat("(") {
            return Err(LinkExpressionError::TrailingInput { at });
        }
        let name = self.word().to_string();
        let op = match name.as_str() {
            "sort-by" => {
                self.require_ws()?;
                let field = self.keyword()?;
                let mut descending = false;
                if self.ws() {
                    let at = self.pos;
                    descending = match self.word() {
                        ":asc" => false,
                        ":desc" => true,
                        _ => return Err(LinkExpressionError::ExpectedDirection { at }),
                    };
                }
                LinkOp::SortBy { field, descending }
            }
            "take" => {
                self.require_ws()?;
                let digits = self.word();
                if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
                    return Err(LinkExpressionError::ExpectedCount);
                }
                LinkOp::Take(digits.parse().map_err(|_| LinkExpressionError::ExpectedCount)?)
            }
            "filter" => {
                self.require_ws()?;
                let field = self.keyword()?;
                self.require_ws()?;
                let value = self.string()?;
                LinkOp::Filter { field, value }
            }
            "refs-to" => {
                self.require_ws()?;
                if self.peek() == Some(b'"') {
                    LinkOp::RefsTo(RefsToTarget::Url(self.string()?))
                } else {
                    let at = self.pos;
                    if self.word() != "self" {
                        return Err(LinkExpressionError::ExpectedString { at });
                    }
                    LinkOp::RefsTo(RefsToTarget::SelfRef)
                }
            }
            _ => return Err(LinkExpressionError::UnknownOp(name)),
        };
        self.require_close()?;
        Ok(op)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use LinkExpressionError as E;

    fn kw(s: &str) -> Keyword {
        Keyword::new(s).unwrap()
    }

    fn thread(source: &str, operations: Vec<LinkOp>) -> LinkTarget {
        LinkTarget::ThreadExpr { source: kw(source), operations }
    }

    fn ops(dest: &str) -> Vec<LinkOp> {
        match parse_thread(dest) {
            Ok(LinkTarget::ThreadExpr { operations, .. }) => operations,
            other => panic!("{dest:?}: {other:?}"),
        }
    }

    // ── Grammar: each op ────────────────────────────────────────────────────

    #[test]
    fn bare_source() {
        assert_eq!(parse_thread("->> :post"), Ok(thread("post", vec![])));
        assert_eq!(parse_thread("->>\t:a-b_C9"), Ok(thread("a-b_C9", vec![])));
    }

    #[test]
    fn sort_by_defaults_to_ascending() {
        assert_eq!(
            ops("->> :post (sort-by :title)"),
            vec![LinkOp::SortBy { field: kw("title"), descending: false }]
        );
        assert_eq!(
            ops("->> :post (sort-by :title :asc)"),
            vec![LinkOp::SortBy { field: kw("title"), descending: false }]
        );
        assert_eq!(
            ops("->> :post (sort-by :title :desc)"),
            vec![LinkOp::SortBy { field: kw("title"), descending: true }]
        );
    }

    #[test]
    fn take() {
        assert_eq!(ops("->> :post (take 4)"), vec![LinkOp::Take(4)]);
        assert_eq!(ops("->> :post (take 0)"), vec![LinkOp::Take(0)]);
    }

    #[test]
    fn filter() {
        assert_eq!(
            ops(r#"->> :post (filter :status "pinned")"#),
            vec![LinkOp::Filter { field: kw("status"), value: "pinned".to_string() }]
        );
    }

    #[test]
    fn refs_to_self_and_quoted_self() {
        assert_eq!(ops("->> :post (refs-to self)"), vec![LinkOp::RefsTo(RefsToTarget::SelfRef)]);
        assert_eq!(
            ops(r#"->> :post (refs-to "self")"#),
            vec![LinkOp::RefsTo(RefsToTarget::Url("self".to_string()))]
        );
        assert_eq!(
            ops(r#"->> :post (refs-to "/author/jo")"#),
            vec![LinkOp::RefsTo(RefsToTarget::Url("/author/jo".to_string()))]
        );
    }

    #[test]
    fn ops_in_order_with_tabs_and_runs_of_spaces() {
        assert_eq!(
            ops("->>  :post\t(take 2)   (refs-to self)"),
            vec![LinkOp::Take(2), LinkOp::RefsTo(RefsToTarget::SelfRef)]
        );
    }

    #[test]
    fn quoted_values_with_spaces_quotes_backslashes_and_parens() {
        let value = |dest: &str| match ops(dest).as_slice() {
            [LinkOp::Filter { value, .. }] => value.clone(),
            other => panic!("{other:?}"),
        };
        assert_eq!(value(r#"->> :p (filter :f "a b")"#), "a b");
        assert_eq!(value(r#"->> :p (filter :f "say \"hi\"")"#), "say \"hi\"");
        assert_eq!(value(r#"->> :p (filter :f "a\\b")"#), "a\\b");
        assert_eq!(value(r#"->> :p (filter :f "(x)")"#), "(x)");
        assert_eq!(value(r#"->> :p (filter :f ")")"#), ")");
        assert_eq!(value(r#"->> :p (filter :f "a%20b")"#), "a%20b");
        assert_eq!(value(r#"->> :p (filter :f "")"#), "");
    }

    // ── Grammar: one test per error variant ─────────────────────────────────

    #[test]
    fn error_empty() {
        assert_eq!(parse_thread(""), Err(E::Empty));
        assert_eq!(parse_destination(""), Err(E::Empty));
    }

    #[test]
    fn error_thread_first() {
        assert_eq!(parse_thread("-> :post"), Err(E::ThreadFirst));
        assert_eq!(parse_destination("-> :post"), Err(E::ThreadFirst));
        assert_eq!(parse_destination("(-> :post)"), Err(E::ThreadFirst));
        assert_eq!(parse_destination("->"), Err(E::ThreadFirst));
    }

    #[test]
    fn error_expected_thread() {
        assert_eq!(parse_thread(":post"), Err(E::ExpectedThread));
        assert_eq!(parse_destination("/a/b"), Err(E::ExpectedThread));
    }

    #[test]
    fn error_expected_whitespace() {
        assert_eq!(parse_thread("->>:post"), Err(E::ExpectedWhitespace { at: 3 }));
        assert_eq!(parse_thread("->>"), Err(E::ExpectedWhitespace { at: 3 }));
        assert_eq!(parse_thread("->> :p (take)"), Err(E::ExpectedWhitespace { at: 12 }));
    }

    #[test]
    fn error_expected_keyword() {
        assert_eq!(parse_thread("->> post"), Err(E::ExpectedKeyword { at: 4 }));
        assert_eq!(parse_thread("->> :"), Err(E::ExpectedKeyword { at: 4 }));
        assert_eq!(parse_thread("->> :p (sort-by title)"), Err(E::ExpectedKeyword { at: 16 }));
    }

    #[test]
    fn error_invalid_keyword() {
        assert_eq!(parse_thread("->> :po$t"), Err(E::InvalidKeyword("po$t".to_string())));
        assert_eq!(parse_thread("->> ::post"), Err(E::InvalidKeyword(":post".to_string())));
    }

    #[test]
    fn error_unknown_op() {
        assert_eq!(parse_thread("->> :post (map :title)"), Err(E::UnknownOp("map".to_string())));
        assert_eq!(parse_thread("->> :post ()"), Err(E::UnknownOp(String::new())));
    }

    #[test]
    fn error_expected_direction() {
        assert_eq!(
            parse_thread("->> :p (sort-by :t :up)"),
            Err(E::ExpectedDirection { at: 19 })
        );
    }

    #[test]
    fn error_expected_count() {
        assert_eq!(parse_thread("->> :p (take x)"), Err(E::ExpectedCount));
        assert_eq!(parse_thread("->> :p (take -1)"), Err(E::ExpectedCount));
        assert_eq!(
            parse_thread("->> :p (take 99999999999999999999999999)"),
            Err(E::ExpectedCount)
        );
    }

    #[test]
    fn error_expected_string() {
        assert_eq!(parse_thread("->> :p (filter :f news)"), Err(E::ExpectedString { at: 18 }));
        assert_eq!(parse_thread("->> :p (refs-to /a/b)"), Err(E::ExpectedString { at: 16 }));
    }

    #[test]
    fn error_unterminated_string() {
        assert_eq!(parse_thread(r#"->> :p (filter :f "abc)"#), Err(E::UnterminatedString));
        assert_eq!(parse_thread(r#"->> :p (filter :f "abc\"#), Err(E::UnterminatedString));
    }

    #[test]
    fn error_invalid_escape() {
        assert_eq!(
            parse_thread(r#"->> :p (filter :f "a\nb")"#),
            Err(E::InvalidEscape { at: 20 })
        );
    }

    #[test]
    fn error_expected_close_paren() {
        assert_eq!(parse_thread("->> :p (take 4"), Err(E::ExpectedCloseParen { at: 14 }));
        assert_eq!(parse_thread("->> :p (take 4 5)"), Err(E::ExpectedCloseParen { at: 14 }));
        assert_eq!(parse_destination("(->> :p"), Err(E::ExpectedCloseParen { at: 7 }));
    }

    #[test]
    fn error_trailing_input() {
        assert_eq!(parse_thread("->> :post extra"), Err(E::TrailingInput { at: 9 }));
        assert_eq!(parse_thread("->> :post "), Err(E::TrailingInput { at: 9 }));
        assert_eq!(parse_thread("->> :post(take 1)"), Err(E::TrailingInput { at: 9 }));
    }

    #[test]
    fn every_error_has_a_message() {
        let all = [
            E::Empty,
            E::ThreadFirst,
            E::ExpectedThread,
            E::ExpectedWhitespace { at: 1 },
            E::ExpectedKeyword { at: 1 },
            E::InvalidKeyword("x".to_string()),
            E::UnknownOp("map".to_string()),
            E::ExpectedDirection { at: 1 },
            E::ExpectedCount,
            E::ExpectedString { at: 1 },
            E::UnterminatedString,
            E::InvalidEscape { at: 1 },
            E::ExpectedCloseParen { at: 1 },
            E::TrailingInput { at: 1 },
        ];
        for e in all {
            assert!(!e.to_string().is_empty(), "{e:?}");
        }
        assert!(E::ThreadFirst.to_string().contains("->>"));
    }

    // ── Destinations ────────────────────────────────────────────────────────

    #[test]
    fn destination_dispatch() {
        assert_eq!(parse_destination(":a/b"), Ok(LinkTarget::PathRef("a/b".to_string())));
        assert_eq!(parse_destination("->> :post (take 4)"), Ok(thread("post", vec![LinkOp::Take(4)])));
        assert_eq!(parse_destination("(->> :post (take 4))"), Ok(thread("post", vec![LinkOp::Take(4)])));
        // `%20` is not decoded here; only the legacy pulldown path does that.
        assert_eq!(parse_destination("->>%20:post"), Err(E::ExpectedWhitespace { at: 3 }));
    }

    // ── Paragraph recognition ───────────────────────────────────────────────

    #[test]
    fn recognises_whole_paragraph_candidates() {
        let c = |text, dest| Some(Candidate { text, dest, slot: None });
        assert_eq!(recognise_paragraph("[x](->> :post (take 4))"), c("x", "->> :post (take 4)"));
        assert_eq!(recognise_paragraph("  [](->> :post)  "), c("", "->> :post"));
        assert_eq!(recognise_paragraph("[x]((->> :post))"), c("x", "(->> :post)"));
        assert_eq!(recognise_paragraph("[x](-> :post)"), c("x", "-> :post"));
        assert_eq!(recognise_paragraph("[x](->>)"), c("x", "->>"));
        // Unbalanced at the end is still a candidate (the parse reports it).
        assert_eq!(recognise_paragraph("[x](->> :p (take 4)"), c("x", "->> :p (take 4"));
        // Parentheses in strings do not count.
        assert_eq!(
            recognise_paragraph(r#"[x](->> :p (filter :f ")"))"#),
            c("x", r#"->> :p (filter :f ")")"#)
        );
    }

    #[test]
    fn rejects_non_candidates() {
        for s in [
            "a [x](->> :post)",
            "[x](->> :post) b",
            "[x](->> :a) [y](->> :b)",
            "[x](:path)",
            "[x](/a/b)",
            "[a[b]](->> :p)",
            "[x](->> :p)\n[y](->> :q)",
            "\\[x](->> :p)",
            "[x] (->> :p)",
        ] {
            assert_eq!(recognise_paragraph(s), None, "{s:?}");
        }
    }

    #[test]
    fn diagnoses_malformed_and_inline_expressions() {
        assert_eq!(diagnose_paragraph("[x](->> :post)"), None);
        assert_eq!(diagnose_paragraph("plain text"), None);
        assert_eq!(
            diagnose_paragraph("[x](->> :post (map :title))"),
            Some(LinkExpressionDiagnostic::Malformed(E::UnknownOp("map".to_string())))
        );
        assert_eq!(
            diagnose_paragraph("[x](-> :post)"),
            Some(LinkExpressionDiagnostic::Malformed(E::ThreadFirst))
        );
        assert_eq!(
            diagnose_paragraph("see [x](->> :post) here"),
            Some(LinkExpressionDiagnostic::MustStandAlone)
        );
        assert_eq!(
            diagnose_paragraph("see [x]((->> :post)) here"),
            Some(LinkExpressionDiagnostic::MustStandAlone)
        );
        // The serializer's escaped form of a literal paragraph is fine.
        assert_eq!(diagnose_paragraph("\\[x](->> :post)"), None);
        // Syntax quoted in a code span is documentation, not an expression.
        assert_eq!(diagnose_paragraph("write `[x](->> :post)` alone"), None);
        assert_eq!(diagnose_paragraph("write ``[x](->> :post)`` alone"), None);
        assert_eq!(
            diagnose_paragraph("`a` then [x](->> :post) `b`"),
            Some(LinkExpressionDiagnostic::MustStandAlone)
        );
        assert_eq!(
            diagnose_paragraph("unclosed ` then [x](->> :post)"),
            Some(LinkExpressionDiagnostic::MustStandAlone)
        );
    }

    // ── Slot markers (ADR-058, decision 9) ──────────────────────────────────

    #[test]
    fn recognises_a_slot_marker_after_spaces_or_tabs() {
        let marked = |text, dest, slot| Some(Candidate { text, dest, slot: Some(slot) });
        assert_eq!(
            recognise_paragraph("[Latest posts](->> :post (take 4)) {#latest}"),
            marked("Latest posts", "->> :post (take 4)", "latest")
        );
        assert_eq!(
            recognise_paragraph("[x](->> :post)   {#news}"),
            marked("x", "->> :post", "news")
        );
        assert_eq!(
            recognise_paragraph("[x](->> :post)\t{#a-b_1}"),
            marked("x", "->> :post", "a-b_1")
        );
        assert_eq!(
            recognise_paragraph("  [x](->> :post) \t {#news}  "),
            marked("x", "->> :post", "news")
        );
        // A marker on a malformed expression: still a candidate, so the
        // destination is diagnosed.
        assert_eq!(
            recognise_paragraph("[x](->> :post (map :t)) {#news}"),
            marked("x", "->> :post (map :t)", "news")
        );
        // `{#` inside a quoted value is not the marker.
        assert_eq!(
            recognise_paragraph(r#"[x](->> :p (filter :f "a {#b}")) {#news}"#),
            marked("x", r#"->> :p (filter :f "a {#b}")"#, "news")
        );
    }

    #[test]
    fn rejects_markers_that_do_not_follow_the_rules() {
        for s in [
            // No whitespace before the marker.
            "[x](->> :post){#news}",
            // Invalid marker names.
            "[x](->> :post) {#a b}",
            "[x](->> :post) {#}",
            "[x](->> :post) {#a}b}",
            // Text after the marker.
            "[x](->> :post) {#news} more",
            // Marker on its own line.
            "[x](->> :post)\n{#news}",
            // Not a link expression before the marker.
            "text {#news}",
            "[x](/a/b) {#news}",
            // The raw recogniser does not take legacy or PathRef spellings;
            // the parser reads those through pulldown.
            "[x](<(-\\>\\> :post)>) {#news}",
        ] {
            assert_eq!(recognise_paragraph(s), None, "{s:?}");
        }
    }

    #[test]
    fn diagnoses_invalid_and_misplaced_markers() {
        assert_eq!(diagnose_paragraph("[x](->> :post) {#news}"), None);
        assert_eq!(
            diagnose_paragraph("[x](->> :post) {#a b}"),
            Some(LinkExpressionDiagnostic::InvalidSlotMarker("a b".to_string()))
        );
        assert_eq!(
            diagnose_paragraph("[x](->> :post (map :t)) {#news}"),
            Some(LinkExpressionDiagnostic::Malformed(E::UnknownOp("map".to_string())))
        );
        let own_line = diagnose_paragraph("[x](->> :post)\n{#news}");
        assert_eq!(
            own_line,
            Some(LinkExpressionDiagnostic::MarkerOnOwnLine("news".to_string()))
        );
        assert!(
            own_line
                .unwrap()
                .to_string()
                .contains("put the slot marker on the same line as the link expression")
        );
        // An escaped (literal) paragraph ending in a marker is marker-shaped
        // (ADR-060 P1): the marker is not on a link expression.
        assert_eq!(
            diagnose_paragraph("\\[x](->> :post) {#news}"),
            Some(LinkExpressionDiagnostic::MarkerNotOnLinkExpression("news".to_string()))
        );
        let all = [
            LinkExpressionDiagnostic::InvalidSlotMarker("a b".to_string()),
            LinkExpressionDiagnostic::MarkerOnOwnLine("news".to_string()),
            LinkExpressionDiagnostic::StrayMarker("x".to_string()),
            LinkExpressionDiagnostic::MarkedExpressionSpansLines("x".to_string()),
            LinkExpressionDiagnostic::MarkerNotOnLinkExpression("x".to_string()),
        ];
        for d in all {
            assert!(!d.to_string().is_empty(), "{d:?}");
        }
    }
}
