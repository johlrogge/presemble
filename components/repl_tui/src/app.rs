use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap},
};
use crossterm::{
    event::{self, Event, KeyCode, KeyModifiers},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use std::io;

use crate::backend::{Completion, ReplBackend};

/// A line of output shown in the output panel.
enum OutputLine {
    Input(String),
    Result(String),
    Error(String),
}

/// Full state of the TUI REPL.
struct ReplState {
    input: String,
    cursor_pos: usize,
    output_lines: Vec<OutputLine>,
    history: Vec<String>,
    history_idx: Option<usize>,
    doc_text: String,
    completions: Vec<Completion>,
    completion_visible: bool,
    completion_selected: usize,
}

impl ReplState {
    fn new(mode_label: &str) -> Self {
        Self {
            input: String::new(),
            cursor_pos: 0,
            output_lines: vec![OutputLine::Result(format!(
                "Presemble REPL ({mode_label})  |  Tab: complete  |  Ctrl-Enter: eval  |  Ctrl-D: quit"
            ))],
            history: Vec::new(),
            history_idx: None,
            doc_text: String::new(),
            completions: Vec::new(),
            completion_visible: false,
            completion_selected: 0,
        }
    }
}

/// Run the TUI REPL with the given backend.
pub fn run_repl(mut backend: Box<dyn ReplBackend>) -> io::Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let term_backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(term_backend)?;

    let mut state = ReplState::new(backend.mode_label());
    let mut quit = false;

    while !quit {
        terminal.draw(|f| draw_ui(f, &state))?;

        if let Event::Key(key) = event::read()? {
            match (key.modifiers, key.code) {
                // Quit
                (m, KeyCode::Char('d')) if m.contains(KeyModifiers::CONTROL) => {
                    quit = true;
                }

                // Evaluate — Ctrl-Enter or Alt-Enter
                (m, KeyCode::Enter)
                    if m.contains(KeyModifiers::CONTROL) || m.contains(KeyModifiers::ALT) =>
                {
                    let code = state.input.trim().to_string();
                    if !code.is_empty() {
                        state.output_lines.push(OutputLine::Input(format!("> {code}")));
                        let result = backend.eval(&code);
                        if result.is_error {
                            state.output_lines.push(OutputLine::Error(result.value));
                        } else {
                            state.output_lines.push(OutputLine::Result(result.value));
                        }
                        if !state.history.last().map(|l: &String| l == &code).unwrap_or(false) {
                            state.history.push(code);
                        }
                        state.history_idx = None;
                        state.input.clear();
                        state.cursor_pos = 0;
                        state.completion_visible = false;
                    }
                }

                // Enter — accept completion or insert newline
                (_, KeyCode::Enter) => {
                    if state.completion_visible {
                        let candidate = state
                            .completions
                            .get(state.completion_selected)
                            .map(|c| c.candidate.clone());
                        if let Some(c) = candidate {
                            accept_completion(&mut state, &c);
                        }
                        state.completion_visible = false;
                    } else {
                        state.input.insert(state.cursor_pos, '\n');
                        state.cursor_pos += 1;
                    }
                }

                // Tab — trigger completion
                (_, KeyCode::Tab) => {
                    let prefix = current_word(&state.input, state.cursor_pos);
                    if !prefix.is_empty() {
                        state.completions = backend.completions(&prefix);
                        state.completion_visible = !state.completions.is_empty();
                        state.completion_selected = 0;
                    }
                }

                // Escape — dismiss completion popup
                (_, KeyCode::Esc) => {
                    state.completion_visible = false;
                }

                // Navigate completion popup (Up/Down)
                (_, KeyCode::Up) if state.completion_visible => {
                    if state.completion_selected > 0 {
                        state.completion_selected -= 1;
                    }
                }
                (_, KeyCode::Down) if state.completion_visible => {
                    if state.completion_selected + 1 < state.completions.len() {
                        state.completion_selected += 1;
                    }
                }

                // History navigation (Up/Down when popup not visible)
                (_, KeyCode::Up) => {
                    if !state.history.is_empty() {
                        let idx = match state.history_idx {
                            None => state.history.len() - 1,
                            Some(i) if i > 0 => i - 1,
                            Some(i) => i,
                        };
                        state.history_idx = Some(idx);
                        state.input = state.history[idx].clone();
                        state.cursor_pos = state.input.len();
                    }
                }
                (_, KeyCode::Down) => {
                    if let Some(idx) = state.history_idx {
                        if idx + 1 < state.history.len() {
                            let next = idx + 1;
                            state.history_idx = Some(next);
                            state.input = state.history[next].clone();
                            state.cursor_pos = state.input.len();
                        } else {
                            state.history_idx = None;
                            state.input.clear();
                            state.cursor_pos = 0;
                        }
                    }
                }

                // Cursor movement
                (_, KeyCode::Left) => {
                    if state.cursor_pos > 0 {
                        state.cursor_pos -= 1;
                    }
                }
                (_, KeyCode::Right) => {
                    if state.cursor_pos < state.input.len() {
                        state.cursor_pos += 1;
                    }
                }
                (_, KeyCode::Home) => {
                    state.cursor_pos = 0;
                }
                (_, KeyCode::End) => {
                    state.cursor_pos = state.input.len();
                }

                // Backspace
                (_, KeyCode::Backspace) => {
                    if state.cursor_pos > 0 {
                        state.input.remove(state.cursor_pos - 1);
                        state.cursor_pos -= 1;
                        state.completion_visible = false;
                    }
                }

                // Delete
                (_, KeyCode::Delete) => {
                    if state.cursor_pos < state.input.len() {
                        state.input.remove(state.cursor_pos);
                        state.completion_visible = false;
                    }
                }

                // Ctrl-L — clear output
                (m, KeyCode::Char('l')) if m.contains(KeyModifiers::CONTROL) => {
                    state.output_lines.clear();
                }

                // Regular character input
                (_, KeyCode::Char(c)) => {
                    state.input.insert(state.cursor_pos, c);
                    state.cursor_pos += 1;
                    state.completion_visible = false;

                    // Update doc panel for symbol under cursor
                    let word = current_word(&state.input, state.cursor_pos);
                    if !word.is_empty() {
                        if let Some(doc) = backend.doc_lookup(&word) {
                            state.doc_text = doc;
                        }
                    }
                }

                _ => {}
            }
        }
    }

    // Restore terminal
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}

fn draw_ui(f: &mut ratatui::Frame, state: &ReplState) {
    let area = f.area();

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(5),
            Constraint::Length(5),
            Constraint::Length(5),
        ])
        .split(area);

    let output_area = chunks[0];
    let doc_area = chunks[1];
    let input_area = chunks[2];

    // Output panel — show most-recent lines at the bottom
    let output_items: Vec<ListItem> = state
        .output_lines
        .iter()
        .map(|line| match line {
            OutputLine::Input(s) => {
                ListItem::new(s.as_str()).style(Style::default().fg(Color::Cyan))
            }
            OutputLine::Result(s) => {
                ListItem::new(s.as_str()).style(Style::default().fg(Color::White))
            }
            OutputLine::Error(s) => {
                ListItem::new(s.as_str()).style(Style::default().fg(Color::Red))
            }
        })
        .collect();

    let output_widget =
        List::new(output_items).block(Block::default().borders(Borders::ALL).title(" Output "));
    f.render_widget(output_widget, output_area);

    // Doc panel
    let doc_widget = Paragraph::new(state.doc_text.as_str())
        .block(Block::default().borders(Borders::ALL).title(" Doc "))
        .wrap(Wrap { trim: false })
        .style(Style::default().fg(Color::Yellow));
    f.render_widget(doc_widget, doc_area);

    // Input panel with EDN syntax highlighting
    let highlighted = highlight_edn(&state.input);
    let input_widget = Paragraph::new(highlighted)
        .block(Block::default().borders(Borders::ALL).title(" Input (Ctrl-Enter to eval) "));
    f.render_widget(input_widget, input_area);

    // Cursor position inside input box
    let cursor_x = (state.cursor_pos as u16).min(input_area.width.saturating_sub(2))
        + input_area.x
        + 1;
    let cursor_y = input_area.y + 1;
    f.set_cursor_position((cursor_x, cursor_y));

    // Completion popup
    if state.completion_visible && !state.completions.is_empty() {
        let items: Vec<ListItem> = state
            .completions
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let style = if i == state.completion_selected {
                    Style::default()
                        .bg(Color::Blue)
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };
                let doc_hint = c.doc.as_deref().unwrap_or("");
                let text = format!("{:<20} {}", c.candidate, first_line(doc_hint));
                ListItem::new(text).style(style)
            })
            .collect();

        let popup_height = (state.completions.len() as u16 + 2).min(10);
        let popup_y = input_area.y.saturating_sub(popup_height);
        let popup_area = ratatui::layout::Rect {
            x: input_area.x + 1,
            y: popup_y,
            width: input_area.width.min(60),
            height: popup_height,
        };

        let popup_widget = List::new(items)
            .block(Block::default().borders(Borders::ALL).title(" Completions "));
        f.render_widget(Clear, popup_area);
        f.render_widget(popup_widget, popup_area);
    }
}

/// Simple EDN syntax highlighting — returns a single line of styled spans.
fn highlight_edn(input: &str) -> Vec<Line<'_>> {
    let mut spans: Vec<Span> = Vec::new();
    let mut chars = input.char_indices().peekable();
    let mut current_start = 0usize;

    while let Some(&(i, c)) = chars.peek() {
        match c {
            // Keyword (:foo)
            ':' => {
                if current_start < i {
                    spans.push(Span::raw(&input[current_start..i]));
                }
                let start = i;
                chars.next();
                while let Some(&(_, nc)) = chars.peek() {
                    if nc.is_alphanumeric()
                        || matches!(nc, '-' | '_' | '?' | '!' | '/')
                    {
                        chars.next();
                    } else {
                        break;
                    }
                }
                let end = chars.peek().map(|&(j, _)| j).unwrap_or(input.len());
                spans.push(Span::styled(
                    &input[start..end],
                    Style::default().fg(Color::Green),
                ));
                current_start = end;
            }

            // String literal
            '"' => {
                if current_start < i {
                    spans.push(Span::raw(&input[current_start..i]));
                }
                let start = i;
                chars.next(); // skip opening quote
                let mut escaped = false;
                loop {
                    match chars.peek() {
                        Some(&(_, '\\')) if !escaped => {
                            escaped = true;
                            chars.next();
                        }
                        Some(&(_, '"')) if !escaped => {
                            chars.next();
                            break;
                        }
                        Some(_) => {
                            escaped = false;
                            chars.next();
                        }
                        None => break,
                    }
                }
                let end = chars.peek().map(|&(j, _)| j).unwrap_or(input.len());
                spans.push(Span::styled(
                    &input[start..end],
                    Style::default().fg(Color::Yellow),
                ));
                current_start = end;
            }

            // Number (positive or negative)
            c if c.is_ascii_digit()
                || (c == '-'
                    && chars
                        .clone()
                        .nth(1)
                        .is_some_and(|(_, nc)| nc.is_ascii_digit())) =>
            {
                if current_start < i {
                    spans.push(Span::raw(&input[current_start..i]));
                }
                let start = i;
                chars.next();
                while let Some(&(_, nc)) = chars.peek() {
                    if nc.is_ascii_digit() || nc == '.' {
                        chars.next();
                    } else {
                        break;
                    }
                }
                let end = chars.peek().map(|&(j, _)| j).unwrap_or(input.len());
                spans.push(Span::styled(
                    &input[start..end],
                    Style::default().fg(Color::Magenta),
                ));
                current_start = end;
            }

            // Brackets and braces
            '(' | ')' | '[' | ']' | '{' | '}' => {
                if current_start < i {
                    spans.push(Span::raw(&input[current_start..i]));
                }
                spans.push(Span::styled(
                    &input[i..i + 1],
                    Style::default().fg(Color::DarkGray),
                ));
                chars.next();
                current_start = i + 1;
            }

            // Line comment
            ';' => {
                if current_start < i {
                    spans.push(Span::raw(&input[current_start..i]));
                }
                let start = i;
                while let Some(&(_, nc)) = chars.peek() {
                    if nc == '\n' {
                        break;
                    }
                    chars.next();
                }
                let end = chars.peek().map(|&(j, _)| j).unwrap_or(input.len());
                spans.push(Span::styled(
                    &input[start..end],
                    Style::default().fg(Color::DarkGray),
                ));
                current_start = end;
            }

            _ => {
                chars.next();
            }
        }
    }

    // Flush trailing text
    if current_start < input.len() {
        spans.push(Span::raw(&input[current_start..]));
    }

    vec![Line::from(spans)]
}

/// Extract the symbol (word) immediately before the cursor.
fn current_word(input: &str, cursor: usize) -> String {
    let before = &input[..cursor];
    let word_start = before
        .rfind(|c: char| c.is_whitespace() || "()[]{}".contains(c))
        .map(|i| i + 1)
        .unwrap_or(0);
    before[word_start..].to_string()
}

/// Replace the current word with `candidate`.
fn accept_completion(state: &mut ReplState, candidate: &str) {
    let word = current_word(&state.input, state.cursor_pos);
    let word_start = state.cursor_pos - word.len();
    state.input.replace_range(word_start..state.cursor_pos, candidate);
    state.cursor_pos = word_start + candidate.len();
}

/// Return the first line of a string.
fn first_line(s: &str) -> &str {
    s.lines().next().unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_word_at_end() {
        assert_eq!(current_word("(map ", 5), "");
        assert_eq!(current_word("(map", 4), "map");
        assert_eq!(current_word("(filter eve", 11), "eve");
    }

    #[test]
    fn current_word_mid_input() {
        // cursor is after 'map'
        assert_eq!(current_word("(map inc [1 2])", 4), "map");
    }

    #[test]
    fn first_line_works() {
        assert_eq!(first_line("one\ntwo"), "one");
        assert_eq!(first_line("only"), "only");
        assert_eq!(first_line(""), "");
    }

    #[test]
    fn highlight_edn_returns_line() {
        let lines = highlight_edn("(+ 1 2)");
        assert_eq!(lines.len(), 1);
    }

    #[test]
    fn highlight_edn_empty_input() {
        let lines = highlight_edn("");
        // empty input — may produce 0 or 1 empty line, just must not panic
        let _ = lines;
    }
}
