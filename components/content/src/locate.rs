//! Finding a diagnostic's characters in a block's source: the block's own
//! slice of the file, quote markers and item indentation included as they are
//! written. A search is exact or gives up: a pattern counts only when it
//! occurs exactly once outside code spans, and the caller then falls back to
//! the block's span. A slot marker is not searched for: it is the block's
//! trailing `{#name}` ([`locate_marker`]).

use std::ops::Range;

/// The byte offset of `pattern` in `block_source`, if it occurs there exactly
/// once outside code spans.
pub(crate) fn find_once(block_source: &str, pattern: &str) -> Option<usize> {
    let code = code_spans(block_source);
    let mut found = occurrences(block_source, pattern).filter(|&at| {
        !code
            .iter()
            .any(|c| at < c.end && at + pattern.len() > c.start)
    });
    let first = found.next()?;
    found.next().is_none().then_some(first)
}

/// The byte range of the slot marker `{#name}` in `block_source`. A marker is
/// not searched for: it is the block's trailing `{#name}`, so this is the last
/// occurrence whose `{` is not escaped (not preceded by an odd number of
/// backslashes). An escaped `\{#name}`, or a literal `{#name}` earlier in the
/// text, is never the marker.
pub(crate) fn locate_marker(block_source: &str, name: &str) -> Option<Range<usize>> {
    let marker = format!("{{#{name}}}");
    let escaped = |at: usize| {
        block_source[..at]
            .bytes()
            .rev()
            .take_while(|&b| b == b'\\')
            .count()
            % 2
            == 1
    };
    occurrences(block_source, &marker)
        .filter(|&at| !escaped(at))
        .last()
        .map(|at| at..at + marker.len())
}

/// The byte range of the link expression written inside text in
/// `block_source` (`[…](->> …)` or `[…]((->> …))`), from its `[` to the
/// matching `)`, if there is exactly one outside code spans.
pub(crate) fn locate_link_expression(block_source: &str) -> Option<Range<usize>> {
    let code = code_spans(block_source);
    let outside_code = |at: usize| !code.iter().any(|c| c.contains(&at));
    let mut found = occurrences(block_source, "](")
        .filter(|&at| {
            let rest = &block_source[at + 2..];
            (rest.starts_with("->>") || rest.starts_with("(->>")) && outside_code(at)
        })
        .filter_map(|at| {
            let start = matching_open(block_source, at)?;
            let end = matching_close(block_source, at + 1)?;
            Some(start..end + 1)
        });
    let first = found.next()?;
    found.next().is_none().then_some(first)
}

/// The `[` that the `]` at byte `close` closes.
fn matching_open(source: &str, close: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (at, c) in source[..close].char_indices().rev() {
        match c {
            ']' => depth += 1,
            '[' if depth == 0 => return Some(at),
            '[' => depth -= 1,
            _ => {}
        }
    }
    None
}

/// The `)` that closes the `(` at byte `open`.
fn matching_close(source: &str, open: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (at, c) in source[open..].char_indices() {
        match c {
            '(' => depth += 1,
            ')' if depth == 1 => return Some(open + at),
            ')' => depth = depth.checked_sub(1)?,
            _ => {}
        }
    }
    None
}

/// Every byte offset where `pattern` starts in `haystack`, overlapping.
fn occurrences<'a>(haystack: &'a str, pattern: &'a str) -> impl Iterator<Item = usize> + 'a {
    let mut from = 0;
    std::iter::from_fn(move || {
        if pattern.is_empty() {
            return None;
        }
        let at = from + haystack.get(from..)?.find(pattern)?;
        from = at + pattern.chars().next().map_or(1, char::len_utf8);
        Some(at)
    })
}

/// The byte ranges of the backtick code spans in `source`: a run of backticks
/// up to the next run of the same length. A run with no closing run is
/// literal and opens nothing.
fn code_spans(source: &str) -> Vec<Range<usize>> {
    let bytes = source.as_bytes();
    let run_at = |i: usize| bytes[i..].iter().take_while(|&&b| b == b'`').count();
    let mut spans = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'`' {
            i += 1;
            continue;
        }
        let open = run_at(i);
        let mut j = i + open;
        let mut close = None;
        while j < bytes.len() {
            if bytes[j] == b'`' {
                let n = run_at(j);
                if n == open {
                    close = Some(j + n);
                    break;
                }
                j += n;
            } else {
                j += 1;
            }
        }
        match close {
            Some(end) => {
                spans.push(i..end);
                i = end;
            }
            None => i += open,
        }
    }
    spans
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_once_skips_code_spans_and_rejects_repeats() {
        assert_eq!(find_once("`{#x}` {#x}", "{#x}"), Some(7));
        assert_eq!(find_once("{#x} {#x}", "{#x}"), None);
        assert_eq!(find_once("nothing", "{#x}"), None);
    }

    #[test]
    fn a_marker_is_the_last_unescaped_occurrence() {
        let source = "# Title {#title}";
        assert_eq!(
            locate_marker(source, "title").map(|r| &source[r]),
            Some("{#title}")
        );
        assert_eq!(locate_marker("# A \\{#t} {#t}", "t"), Some(10..14));
        assert_eq!(locate_marker("a {#t} b {#t}", "t"), Some(9..13));
        // An even run of backslashes escapes itself, not the marker.
        assert_eq!(locate_marker("a \\\\{#t}", "t"), Some(4..8));
        assert_eq!(locate_marker("a {#t} \\{#t}", "t"), Some(2..6));
        assert_eq!(locate_marker("a \\{#t}", "t"), None);
        assert_eq!(locate_marker("nothing", "t"), None);
    }

    #[test]
    fn a_link_expression_in_text_runs_from_its_bracket_to_its_paren() {
        for (source, expected) in [
            ("See [x](->> :post) here.", "[x](->> :post)"),
            ("See [x]((->> :post)) here.", "[x]((->> :post))"),
            (
                "See [a [b] c](->> (f :post)) here.",
                "[a [b] c](->> (f :post))",
            ),
        ] {
            let range = locate_link_expression(source).expect(source);
            assert_eq!(&source[range], expected);
        }
    }

    #[test]
    fn a_link_expression_twice_or_in_code_is_not_located() {
        assert_eq!(locate_link_expression("[x](->> :a) and [y](->> :b)"), None);
        assert_eq!(locate_link_expression("`[x](->> :a)` text"), None);
        assert_eq!(locate_link_expression("[x](->> :a"), None);
    }
}
