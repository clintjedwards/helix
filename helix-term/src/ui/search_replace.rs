use crate::compositor::{Component, Context, Event, EventResult};
use crate::{alt, ctrl, key, shift};
use helix_core::{
    regex,
    unicode::segmentation::GraphemeCursor,
    unicode::width::{UnicodeWidthChar, UnicodeWidthStr},
    Position, Rope, Tendril, Transaction,
};
use helix_stdx::rope::RopeSliceExt;
use helix_view::{
    editor::Action,
    graphics::{CursorKind, Margin, Modifier, Rect},
    input::KeyEvent,
    keyboard::KeyCode,
    Editor,
};
use tui::{
    buffer::Buffer as Surface,
    widgets::{Block, Borders, Widget},
};

use grep_matcher::Matcher;
use grep_regex::RegexMatcherBuilder;
use grep_searcher::{sinks, BinaryDetection, SearcherBuilder};
use ignore::{DirEntry, WalkBuilder, WalkState};

use std::{
    path::PathBuf,
    sync::mpsc::{self, Receiver, SyncSender},
    time::{Duration, Instant},
};

// ──────────────────────────────────────────────────────────────────────────────
// Data types
// ──────────────────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SearchScope {
    Buffer,
    Workspace,
}

#[derive(Clone, Default, Debug)]
pub struct SearchOptions {
    pub match_case: bool,
    pub regex_mode: bool,
    pub whole_word: bool,
}

/// Where the actual match offsets come from.
#[derive(Clone, Debug)]
pub enum MatchLocation {
    /// Buffer-scope: char offsets within the rope.
    BufferChars { char_start: usize, char_end: usize },
    /// Workspace-scope: line-relative byte offsets (converted at replacement time).
    FileBytes {
        line_num: usize,
        line_byte_start: usize,
        line_byte_end: usize,
    },
}

#[derive(Clone, Debug)]
pub struct SearchResult {
    pub path: PathBuf,
    /// 0-indexed line number.
    pub line_num: usize,
    /// Full line text, used for display.
    pub line_content: String,
    /// Byte offsets *within* `line_content` for highlighting.
    pub match_start_in_line: usize,
    pub match_end_in_line: usize,
    pub location: MatchLocation,
    pub selected: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum FocusedField {
    Search,
    Replace,
    Results,
}

// ──────────────────────────────────────────────────────────────────────────────
// Movement enum (local, mirrors Prompt's)
// ──────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy)]
enum Movement {
    BackwardChar(usize),
    BackwardWord(usize),
    ForwardChar(usize),
    ForwardWord(usize),
}

// ──────────────────────────────────────────────────────────────────────────────
// Main struct
// ──────────────────────────────────────────────────────────────────────────────

pub struct SearchReplace {
    scope: SearchScope,
    options: SearchOptions,

    search_input: String,
    replace_input: String,
    search_cursor: usize,  // byte offset into search_input
    replace_cursor: usize, // byte offset into replace_input

    focused: FocusedField,
    results: Vec<SearchResult>,
    result_cursor: usize,
    scroll_offset: usize,
    status: Option<String>,

    // Background workspace search channels (always initialized)
    query_tx: SyncSender<Option<(String, SearchOptions)>>,
    results_rx: Receiver<Vec<SearchResult>>,
}

// ──────────────────────────────────────────────────────────────────────────────
// Construction
// ──────────────────────────────────────────────────────────────────────────────

impl SearchReplace {
    pub fn new(scope: SearchScope) -> Self {
        let (qtx, qrx) = mpsc::sync_channel::<Option<(String, SearchOptions)>>(1);
        let (rtx, rrx) = mpsc::channel::<Vec<SearchResult>>();
        let search_root = helix_stdx::env::current_working_dir();

        std::thread::spawn(move || {
            loop {
                let first = match qrx.recv() {
                    Ok(msg) => msg,
                    Err(_) => return,
                };
                let Some((query, opts)) = first else { return };

                // Debounce: collect any newer queries within 300ms
                let deadline = Instant::now() + Duration::from_millis(300);
                let mut latest_query = query;
                let mut latest_opts = opts;
                loop {
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    if remaining.is_zero() {
                        break;
                    }
                    match qrx.recv_timeout(remaining) {
                        Ok(Some((q, o))) => {
                            latest_query = q;
                            latest_opts = o;
                        }
                        Ok(None) => return, // shutdown
                        Err(_) => break,
                    }
                }

                if latest_query.is_empty() {
                    let _ = rtx.send(Vec::new());
                    continue;
                }

                let results =
                    run_workspace_search(&search_root, &latest_query, &latest_opts);
                let _ = rtx.send(results);
            }
        });

        Self {
            scope,
            options: SearchOptions::default(),
            search_input: String::new(),
            replace_input: String::new(),
            search_cursor: 0,
            replace_cursor: 0,
            focused: FocusedField::Search,
            results: Vec::new(),
            result_cursor: 0,
            scroll_offset: 0,
            status: None,
            query_tx: qtx,
            results_rx: rrx,
        }
    }

    // ── Text input helpers ─────────────────────────────────────────────────

    fn active_input(&self) -> &str {
        match self.focused {
            FocusedField::Search | FocusedField::Results => &self.search_input,
            FocusedField::Replace => &self.replace_input,
        }
    }

    fn active_cursor(&self) -> usize {
        match self.focused {
            FocusedField::Search | FocusedField::Results => self.search_cursor,
            FocusedField::Replace => self.replace_cursor,
        }
    }

    fn active_input_and_cursor_mut(&mut self) -> (&mut String, &mut usize) {
        match self.focused {
            FocusedField::Search | FocusedField::Results => {
                (&mut self.search_input, &mut self.search_cursor)
            }
            FocusedField::Replace => (&mut self.replace_input, &mut self.replace_cursor),
        }
    }

    fn insert_char_at_cursor(&mut self, c: char) {
        let (line, cursor) = self.active_input_and_cursor_mut();
        line.insert(*cursor, c);
        let mut gc = GraphemeCursor::new(*cursor, line.len(), false);
        if let Ok(Some(pos)) = gc.next_boundary(line, 0) {
            *cursor = pos;
        }
    }

    fn delete_char_backwards(&mut self) {
        let pos = self.eval_movement(Movement::BackwardChar(1));
        let (line, cursor) = self.active_input_and_cursor_mut();
        line.replace_range(pos..*cursor, "");
        *cursor = pos;
    }

    fn delete_char_forwards(&mut self) {
        let pos = self.eval_movement(Movement::ForwardChar(1));
        let cursor = self.active_cursor();
        let (line, _) = self.active_input_and_cursor_mut();
        line.replace_range(cursor..pos, "");
    }

    fn delete_word_backwards(&mut self) {
        let pos = self.eval_movement(Movement::BackwardWord(1));
        let (line, cursor) = self.active_input_and_cursor_mut();
        line.replace_range(pos..*cursor, "");
        *cursor = pos;
    }

    fn kill_to_end(&mut self) {
        let cursor = self.active_cursor();
        let (line, _) = self.active_input_and_cursor_mut();
        line.truncate(cursor);
    }

    fn kill_to_start(&mut self) {
        let cursor = self.active_cursor();
        let (line, cur) = self.active_input_and_cursor_mut();
        let rest = line[cursor..].to_string();
        *line = rest;
        *cur = 0;
    }

    fn move_cursor_to(&mut self, pos: usize) {
        match self.focused {
            FocusedField::Search | FocusedField::Results => self.search_cursor = pos,
            FocusedField::Replace => self.replace_cursor = pos,
        }
    }

    fn eval_movement(&self, movement: Movement) -> usize {
        let line = self.active_input();
        let cursor = self.active_cursor();

        match movement {
            Movement::BackwardChar(rep) => {
                let mut position = cursor;
                for _ in 0..rep {
                    let mut gc = GraphemeCursor::new(position, line.len(), false);
                    if let Ok(Some(pos)) = gc.prev_boundary(line, 0) {
                        position = pos;
                    } else {
                        break;
                    }
                }
                position
            }
            Movement::ForwardChar(rep) => {
                let mut position = cursor;
                for _ in 0..rep {
                    let mut gc = GraphemeCursor::new(position, line.len(), false);
                    if let Ok(Some(pos)) = gc.next_boundary(line, 0) {
                        position = pos;
                    } else {
                        break;
                    }
                }
                position
            }
            Movement::BackwardWord(rep) => {
                let char_indices: Vec<(usize, char)> = line.char_indices().collect();
                if char_indices.is_empty() {
                    return cursor;
                }
                let mut char_pos = char_indices
                    .iter()
                    .position(|(idx, _)| *idx == cursor)
                    .unwrap_or(char_indices.len().saturating_sub(1));

                for _ in 0..rep {
                    if char_pos == 0 {
                        break;
                    }
                    let mut found = None;
                    for prev in (0..char_pos.saturating_sub(1)).rev() {
                        if char_indices[prev].1.is_whitespace() {
                            found = Some(prev + 1);
                            break;
                        }
                    }
                    char_pos = found.unwrap_or(0);
                }
                char_indices.get(char_pos).map(|(i, _)| *i).unwrap_or(0)
            }
            Movement::ForwardWord(rep) => {
                let char_indices: Vec<(usize, char)> = line.char_indices().collect();
                if char_indices.is_empty() {
                    return cursor;
                }
                let mut char_pos = char_indices
                    .iter()
                    .position(|(idx, _)| *idx == cursor)
                    .unwrap_or(char_indices.len());

                for _ in 0..rep {
                    while char_pos < char_indices.len()
                        && !char_indices[char_pos].1.is_whitespace()
                    {
                        char_pos += 1;
                    }
                    while char_pos < char_indices.len()
                        && char_indices[char_pos].1.is_whitespace()
                    {
                        char_pos += 1;
                    }
                }
                char_indices
                    .get(char_pos)
                    .map(|(i, _)| *i)
                    .unwrap_or(line.len())
            }
        }
    }

    // ── Search ─────────────────────────────────────────────────────────────

    fn on_search_input_changed(&mut self, cx: &mut Context) {
        self.result_cursor = 0;
        self.scroll_offset = 0;

        if self.scope == SearchScope::Buffer {
            let (_, doc) = current_ref!(cx.editor);
            let doc_path = doc.path().cloned().unwrap_or_default();
            let text = doc.text().clone();
            self.run_search_buffer(&text, doc_path);
        } else {
            let pattern = build_pattern(&self.search_input, &self.options);
            let _ = self.query_tx.try_send(Some((pattern, self.options.clone())));
            self.status = Some("Searching…".to_string());
        }
    }

    fn run_search_buffer(&mut self, text: &Rope, path: PathBuf) {
        self.results.clear();
        if self.search_input.is_empty() {
            self.status = None;
            return;
        }

        let pattern = build_pattern(&self.search_input, &self.options);
        let regex = match helix_stdx::rope::RegexBuilder::new()
            .syntax(
                helix_stdx::rope::Config::new()
                    .case_insensitive(!self.options.match_case)
                    .multi_line(true),
            )
            .build(&pattern)
        {
            Ok(r) => r,
            Err(e) => {
                self.status = Some(format!("Invalid pattern: {e}"));
                return;
            }
        };

        let slice = text.slice(..);
        for mat in regex.find_iter(slice.regex_input()) {
            let char_start = text.byte_to_char(mat.start());
            let char_end = text.byte_to_char(mat.end());
            let line_num = text.char_to_line(char_start);
            let line_char_start = text.line_to_char(line_num);
            let line_content = text.line(line_num).to_string();

            let wl_char_start = char_start.saturating_sub(line_char_start);
            let wl_char_end = char_end.saturating_sub(line_char_start);
            let match_start_in_line = line_content
                .char_indices()
                .nth(wl_char_start)
                .map(|(i, _)| i)
                .unwrap_or(0);
            let match_end_in_line = line_content
                .char_indices()
                .nth(wl_char_end)
                .map(|(i, _)| i)
                .unwrap_or(line_content.len());

            self.results.push(SearchResult {
                path: path.clone(),
                line_num,
                line_content,
                match_start_in_line,
                match_end_in_line,
                location: MatchLocation::BufferChars { char_start, char_end },
                selected: true,
            });
        }

        self.update_status();
    }

    fn update_status(&mut self) {
        let total = self.results.len();
        self.status = if total == 0 {
            if !self.search_input.is_empty() {
                Some("No matches found".to_string())
            } else {
                None
            }
        } else {
            Some(format!(
                "{total} match{}",
                if total == 1 { "" } else { "es" }
            ))
        };
        if self.result_cursor >= total && total > 0 {
            self.result_cursor = total - 1;
        }
    }

    // ── Replacement ────────────────────────────────────────────────────────

    fn compute_replacement_for(&self, matched_text: &str) -> String {
        if self.options.regex_mode && self.replace_input.contains('$') {
            if let Ok(re) = regex::Regex::new(&self.search_input) {
                if let Some(caps) = re.captures(matched_text) {
                    let mut dest = String::new();
                    caps.expand(&self.replace_input, &mut dest);
                    return dest;
                }
            }
        }
        self.replace_input.clone()
    }

    /// Apply the replacement for exactly the result at `index`, then remove it
    /// from the list.  Returns true if the panel should close (list now empty).
    fn apply_single_replacement(&mut self, cx: &mut Context, index: usize) -> bool {
        let Some(result) = self.results.get(index).cloned() else {
            return self.results.is_empty();
        };

        let view_id = view!(cx.editor).id;
        let mut applied = false;

        if let Ok(doc_id) = cx.editor.open(&result.path, Action::Load) {
            let doc = doc_mut!(cx.editor, &doc_id);
            let text = doc.text().clone();

            let (char_start, char_end) = match &result.location {
                MatchLocation::BufferChars { char_start, char_end } => (*char_start, *char_end),
                MatchLocation::FileBytes { line_num, line_byte_start, line_byte_end } => {
                    if *line_num < text.len_lines() {
                        let lcs = text.line_to_char(*line_num);
                        let ls = text.line(*line_num).to_string();
                        let cs = ls[..*line_byte_start].chars().count();
                        let ce = ls[..*line_byte_end].chars().count();
                        (lcs + cs, lcs + ce)
                    } else {
                        self.status = Some("Match location out of bounds".to_string());
                        return false;
                    }
                }
            };

            if char_end <= text.len_chars() {
                let matched_text = text.slice(char_start..char_end).to_string();
                let replacement = self.compute_replacement_for(&matched_text);
                let transaction = Transaction::change(
                    doc.text(),
                    std::iter::once((char_start, char_end, Some(Tendril::from(replacement.as_str())))),
                );
                doc.apply(&transaction, view_id);
                applied = true;
            }
        }

        // Remove this result from the list regardless of success so the user
        // can keep moving through remaining matches.
        self.results.remove(index);
        if self.result_cursor >= self.results.len() && !self.results.is_empty() {
            self.result_cursor = self.results.len() - 1;
        }

        if applied {
            self.status = Some(format!(
                "Replaced 1 — {} remaining",
                self.results.len()
            ));
        }

        self.results.is_empty()
    }

    fn apply_replacements(&mut self, cx: &mut Context) {
        let selected: Vec<SearchResult> =
            self.results.iter().filter(|r| r.selected).cloned().collect();

        if selected.is_empty() {
            self.status = Some("Nothing selected — use [space] to select or [a] to select all".to_string());
            return;
        }

        // Group by path
        let mut by_path: std::collections::HashMap<PathBuf, Vec<SearchResult>> =
            std::collections::HashMap::new();
        for r in selected {
            by_path.entry(r.path.clone()).or_default().push(r);
        }

        let view_id = view!(cx.editor).id;
        let mut applied = 0usize;
        let mut errors = 0usize;

        for (file_path, mut matches) in by_path {
            // Sort ascending — Transaction::change requires changes in ascending order
            matches.sort_unstable_by(|a, b| {
                let a_key = match &a.location {
                    MatchLocation::BufferChars { char_start, .. } => *char_start,
                    MatchLocation::FileBytes { line_num, line_byte_start, .. } => {
                        line_num * 1_000_000 + line_byte_start
                    }
                };
                let b_key = match &b.location {
                    MatchLocation::BufferChars { char_start, .. } => *char_start,
                    MatchLocation::FileBytes { line_num, line_byte_start, .. } => {
                        line_num * 1_000_000 + line_byte_start
                    }
                };
                a_key.cmp(&b_key)
            });

            let doc_id = match cx.editor.open(&file_path, Action::Load) {
                Ok(id) => id,
                Err(e) => {
                    log::error!(
                        "search_replace: failed to open {}: {e}",
                        file_path.display()
                    );
                    errors += 1;
                    continue;
                }
            };

            let doc = doc_mut!(cx.editor, &doc_id);
            let text = doc.text().clone();

            let mut changes: Vec<(usize, usize, Option<Tendril>)> = Vec::new();

            for result in &matches {
                let (char_start, char_end) = match &result.location {
                    MatchLocation::BufferChars { char_start, char_end } => {
                        (*char_start, *char_end)
                    }
                    MatchLocation::FileBytes {
                        line_num,
                        line_byte_start,
                        line_byte_end,
                    } => {
                        if *line_num >= text.len_lines() {
                            errors += 1;
                            continue;
                        }
                        let line_char_start = text.line_to_char(*line_num);
                        let line_str = text.line(*line_num).to_string();
                        let cs = line_str[..*line_byte_start].chars().count();
                        let ce = line_str[..*line_byte_end].chars().count();
                        (line_char_start + cs, line_char_start + ce)
                    }
                };

                if char_end > text.len_chars() {
                    errors += 1;
                    continue;
                }

                let matched_text = text.slice(char_start..char_end).to_string();
                let replacement = self.compute_replacement_for(&matched_text);
                changes.push((
                    char_start,
                    char_end,
                    Some(Tendril::from(replacement.as_str())),
                ));
                applied += 1;
            }

            if !changes.is_empty() {
                let transaction = Transaction::change(doc.text(), changes.into_iter());
                doc.apply(&transaction, view_id);
            }
        }

        self.results.clear();
        self.status = if errors == 0 {
            Some(format!(
                "Replaced {applied} occurrence{}",
                if applied == 1 { "" } else { "s" }
            ))
        } else {
            Some(format!("Replaced {applied}, skipped {errors}"))
        };
    }

    // ── Scroll helpers ─────────────────────────────────────────────────────

    fn scroll_to_cursor(&mut self, list_height: usize) {
        if list_height == 0 {
            return;
        }
        if self.result_cursor < self.scroll_offset {
            self.scroll_offset = self.result_cursor;
        } else if self.result_cursor >= self.scroll_offset + list_height {
            self.scroll_offset = self.result_cursor + 1 - list_height;
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Workspace search (runs on background thread)
// ──────────────────────────────────────────────────────────────────────────────

fn build_pattern(input: &str, opts: &SearchOptions) -> String {
    let mut pat = if opts.regex_mode {
        input.to_string()
    } else {
        regex::escape(input)
    };
    if opts.whole_word {
        pat = format!(r"\b{}\b", pat);
    }
    pat
}

fn run_workspace_search(
    search_root: &std::path::Path,
    query: &str,
    opts: &SearchOptions,
) -> Vec<SearchResult> {
    if query.is_empty() {
        return Vec::new();
    }

    let matcher = match RegexMatcherBuilder::new()
        .case_insensitive(!opts.match_case)
        .build(query)
    {
        Ok(m) => m,
        Err(e) => {
            log::info!("search_replace: invalid workspace pattern: {e}");
            return Vec::new();
        }
    };

    let (tx, rx) = mpsc::channel::<SearchResult>();

    let searcher_proto = SearcherBuilder::new()
        .binary_detection(BinaryDetection::quit(b'\x00'))
        .build();

    WalkBuilder::new(search_root)
        .hidden(true)
        .git_ignore(true)
        .add_custom_ignore_filename(helix_loader::config_dir().join("ignore"))
        .add_custom_ignore_filename(".helix/ignore")
        .build_parallel()
        .run(|| {
            let mut searcher = searcher_proto.clone();
            let matcher = matcher.clone();
            let tx = tx.clone();
            Box::new(move |entry: Result<DirEntry, ignore::Error>| -> WalkState {
                let entry = match entry {
                    Ok(e) => e,
                    Err(_) => return WalkState::Continue,
                };
                if !entry.path().is_file() {
                    return WalkState::Continue;
                }

                let path = entry.path().to_path_buf();
                let mut stop = false;

                let sink = sinks::UTF8(|line_num_1indexed, line_content| {
                    let line_num = line_num_1indexed as usize - 1;
                    let _ = matcher.find_iter(line_content.as_bytes(), |m| {
                        let line_byte_start = m.start();
                        let line_byte_end = m.end();

                        let line_content_str = line_content
                            .trim_end_matches('\n')
                            .trim_end_matches('\r')
                            .to_string();

                        // For highlight: byte offsets into the trimmed line
                        let display_end = line_content_str.len();
                        let ms = line_byte_start.min(display_end);
                        let me = line_byte_end.min(display_end);

                        let result = SearchResult {
                            path: path.clone(),
                            line_num,
                            line_content: line_content_str,
                            match_start_in_line: ms,
                            match_end_in_line: me,
                            location: MatchLocation::FileBytes {
                                line_num,
                                line_byte_start,
                                line_byte_end,
                            },
                            selected: true,
                        };

                        if tx.send(result).is_err() {
                            stop = true;
                        }
                        !stop
                    });
                    Ok(!stop)
                });

                let _ = searcher.search_path(&matcher, entry.path(), sink);

                if stop {
                    WalkState::Quit
                } else {
                    WalkState::Continue
                }
            })
        });

    // Drop original tx so channel closes when all senders (clones) finish
    drop(tx);
    rx.into_iter().collect()
}

// ──────────────────────────────────────────────────────────────────────────────
// Component impl
// ──────────────────────────────────────────────────────────────────────────────

impl Component for SearchReplace {
    fn id(&self) -> Option<&'static str> {
        Some("search-replace")
    }

    fn required_size(&mut self, viewport: (u16, u16)) -> Option<(u16, u16)> {
        Some((viewport.0.min(120), (viewport.1 * 3 / 4).max(15)))
    }

    fn handle_event(&mut self, event: &Event, cx: &mut Context) -> EventResult {
        // Poll workspace results on any event (including IdleTimeout)
        if self.scope == SearchScope::Workspace {
            if let Ok(results) = self.results_rx.try_recv() {
                self.results = results;
                self.update_status();
            }
        }

        let close_fn = EventResult::Consumed(Some(Box::new(
            |compositor: &mut crate::compositor::Compositor, _cx: &mut Context| {
                compositor.remove("search-replace");
            },
        )));

        let event = match event {
            Event::Key(k) => *k,
            Event::Paste(s) => {
                if matches!(self.focused, FocusedField::Search | FocusedField::Replace) {
                    let s = s.clone();
                    {
                        let (line, cursor) = self.active_input_and_cursor_mut();
                        line.insert_str(*cursor, &s);
                        *cursor += s.len();
                    }
                    self.on_search_input_changed(cx);
                }
                return EventResult::Consumed(None);
            }
            Event::IdleTimeout | Event::Resize(..) => return EventResult::Consumed(None),
            _ => return EventResult::Ignored(None),
        };

        // ── Global keys ────────────────────────────────────────────────────
        match event {
            ctrl!('c') | key!(Esc) => return close_fn,
            key!(Tab) => {
                self.focused = match self.focused {
                    FocusedField::Search => FocusedField::Replace,
                    FocusedField::Replace => {
                        if !self.results.is_empty() {
                            FocusedField::Results
                        } else {
                            FocusedField::Search
                        }
                    }
                    FocusedField::Results => FocusedField::Search,
                };
                return EventResult::Consumed(None);
            }
            shift!(Tab) => {
                self.focused = match self.focused {
                    FocusedField::Search => {
                        if !self.results.is_empty() {
                            FocusedField::Results
                        } else {
                            FocusedField::Replace
                        }
                    }
                    FocusedField::Replace => FocusedField::Search,
                    FocusedField::Results => FocusedField::Replace,
                };
                return EventResult::Consumed(None);
            }
            alt!('c') => {
                self.options.match_case = !self.options.match_case;
                self.on_search_input_changed(cx);
                return EventResult::Consumed(None);
            }
            alt!('r') => {
                self.options.regex_mode = !self.options.regex_mode;
                self.on_search_input_changed(cx);
                return EventResult::Consumed(None);
            }
            alt!('w') => {
                self.options.whole_word = !self.options.whole_word;
                self.on_search_input_changed(cx);
                return EventResult::Consumed(None);
            }
            _ => {}
        }

        // ── Field-specific keys ────────────────────────────────────────────
        match self.focused {
            FocusedField::Search | FocusedField::Replace => {
                match event {
                    key!(Enter) => {
                        if self.scope == SearchScope::Buffer {
                            let (_, doc) = current_ref!(cx.editor);
                            let doc_path = doc.path().cloned().unwrap_or_default();
                            let text = doc.text().clone();
                            self.run_search_buffer(&text, doc_path);
                        }
                        if !self.results.is_empty() {
                            self.focused = FocusedField::Results;
                        }
                    }
                    ctrl!('s') => {
                        self.scope = match self.scope {
                            SearchScope::Buffer => SearchScope::Workspace,
                            SearchScope::Workspace => SearchScope::Buffer,
                        };
                        self.results.clear();
                        self.status = None;
                        self.on_search_input_changed(cx);
                    }
                    key!(Left) | ctrl!('b') => {
                        let pos = self.eval_movement(Movement::BackwardChar(1));
                        self.move_cursor_to(pos);
                    }
                    key!(Right) | ctrl!('f') => {
                        let pos = self.eval_movement(Movement::ForwardChar(1));
                        self.move_cursor_to(pos);
                    }
                    key!(Home) | ctrl!('a') => self.move_cursor_to(0),
                    key!(End) | ctrl!('e') => {
                        let end = self.active_input().len();
                        self.move_cursor_to(end);
                    }
                    ctrl!(Left) | alt!('b') => {
                        let pos = self.eval_movement(Movement::BackwardWord(1));
                        self.move_cursor_to(pos);
                    }
                    ctrl!(Right) | alt!('f') => {
                        let pos = self.eval_movement(Movement::ForwardWord(1));
                        self.move_cursor_to(pos);
                    }
                    key!(Backspace) | ctrl!('h') => {
                        self.delete_char_backwards();
                        self.on_search_input_changed(cx);
                    }
                    key!(Delete) | ctrl!('d') => {
                        self.delete_char_forwards();
                        self.on_search_input_changed(cx);
                    }
                    ctrl!('w') => {
                        self.delete_word_backwards();
                        self.on_search_input_changed(cx);
                    }
                    ctrl!('k') => {
                        self.kill_to_end();
                        self.on_search_input_changed(cx);
                    }
                    ctrl!('u') => {
                        self.kill_to_start();
                        self.on_search_input_changed(cx);
                    }
                    KeyEvent {
                        code: KeyCode::Char(c),
                        modifiers: _,
                    } => {
                        self.insert_char_at_cursor(c);
                        self.on_search_input_changed(cx);
                    }
                    _ => {}
                }
            }
            FocusedField::Results => {
                match event {
                    key!('j') | key!(Down) => {
                        if self.result_cursor + 1 < self.results.len() {
                            self.result_cursor += 1;
                        }
                    }
                    key!('k') | key!(Up) => {
                        self.result_cursor = self.result_cursor.saturating_sub(1);
                    }
                    key!(' ') => {
                        if let Some(r) = self.results.get_mut(self.result_cursor) {
                            r.selected = !r.selected;
                        }
                    }
                    key!('a') => {
                        for r in &mut self.results {
                            r.selected = true;
                        }
                        self.update_status();
                    }
                    key!('n') => {
                        for r in &mut self.results {
                            r.selected = false;
                        }
                        self.update_status();
                    }
                    // <enter>: replace only the hovered result, then advance
                    key!(Enter) => {
                        let idx = self.result_cursor;
                        if self.apply_single_replacement(cx, idx) {
                            return close_fn;
                        }
                    }
                    // R: replace all selected results at once
                    key!('R') => {
                        self.apply_replacements(cx);
                        if self.results.is_empty() {
                            return close_fn;
                        }
                    }
                    ctrl!('s') => {
                        self.scope = match self.scope {
                            SearchScope::Buffer => SearchScope::Workspace,
                            SearchScope::Workspace => SearchScope::Buffer,
                        };
                        self.results.clear();
                        self.status = None;
                        self.focused = FocusedField::Search;
                    }
                    _ => {}
                }
            }
        }

        EventResult::Consumed(None)
    }

    fn render(&mut self, area: Rect, surface: &mut Surface, cx: &mut Context) {
        let theme = &cx.editor.theme;
        let text_style = theme.get("ui.text");
        let inactive_style = theme.get("ui.text.inactive");
        let cursor_row_style = theme.get("ui.selection");
        let _highlight_style = theme.get("ui.search");
        let background = theme.get("ui.background");
        let statusline_style = theme.get("ui.statusline");
        // Strip the statusline's bg so separators render on the modal background,
        // not with the statusline's contrasting background color.
        let separator_style = helix_view::graphics::Style {
            bg: background.bg,
            ..theme.get("ui.statusline.separator")
        };
        let diff_plus_style = theme.get("diff.plus");
        let diff_minus_style = theme.get("diff.minus");

        // Style used for an "active" option button: bold + underline on top of
        // the statusline style so it pops regardless of the colour scheme.
        let active_opt_style = statusline_style.add_modifier(Modifier::BOLD | Modifier::REVERSED);
        let inactive_opt_style = inactive_style;

        // Clear background
        surface.clear_with(area, background);

        // Outer border with title
        let title = match self.scope {
            SearchScope::Buffer => " Search & Replace — buffer ",
            SearchScope::Workspace => " Search & Replace — workspace ",
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .title(title)
            .border_style(text_style);
        block.render(area, surface);

        let inner = area.inner(Margin::all(1));
        if inner.height < 5 {
            return;
        }

        // ── Row 0: option toggles ──────────────────────────────────────────
        //
        // Each option is rendered as "[label](key)" when ON (bold highlight)
        // or " label (key)" when OFF (dimmed), so users can see both the
        // current state and how to toggle it at a glance.
        let opt_row = inner.y;

        // (display label, toggle key hint, current value)
        let opts: &[(&str, &str, bool)] = &[
            ("match-case", "alt-c", self.options.match_case),
            ("regex", "alt-r", self.options.regex_mode),
            ("whole-word", "alt-w", self.options.whole_word),
        ];

        let mut x = inner.x;
        for (label, key, active) in opts {
            let btn = if *active {
                format!("[{label}]({key}) ")
            } else {
                format!(" {label} ({key}) ")
            };
            let style = if *active { active_opt_style } else { inactive_opt_style };
            let (nx, _) = surface.set_stringn(x, opt_row, &btn, btn.len(), style);
            x = nx;
        }

        // Right-aligned: scope indicator
        let scope_text = match self.scope {
            SearchScope::Buffer => "buffer",
            SearchScope::Workspace => "workspace",
        };
        let scope_hint = format!("scope: {scope_text} (ctrl-s)");
        let hint_x = inner
            .x
            .saturating_add(inner.width)
            .saturating_sub(scope_hint.len() as u16);
        surface.set_stringn(hint_x, opt_row, &scope_hint, scope_hint.len() as usize, inactive_style);

        // ── Rows 1–2: search / replace inputs ─────────────────────────────
        let search_row = inner.y + 1;
        let replace_row = inner.y + 2;
        // Labels: "▶ Search: " / "  Search: " (10 chars wide)
        let label_width: u16 = 10;
        let input_x = inner.x + label_width;
        let input_width = inner.width.saturating_sub(label_width) as usize;

        let search_focused = matches!(self.focused, FocusedField::Search);
        let replace_focused = matches!(self.focused, FocusedField::Replace);

        // Focus arrow
        let arrow_on = "▶ ";
        let arrow_off = "  ";

        // "Search: " / "Replace:"
        {
            let prefix = if search_focused { arrow_on } else { arrow_off };
            let label_style = if search_focused { text_style } else { inactive_style };
            surface.set_stringn(inner.x, search_row, prefix, 2, label_style);
            surface.set_stringn(inner.x + 2, search_row, "Search: ", 8, label_style);
        }
        {
            let prefix = if replace_focused { arrow_on } else { arrow_off };
            let label_style = if replace_focused { text_style } else { inactive_style };
            surface.set_stringn(inner.x, replace_row, prefix, 2, label_style);
            surface.set_stringn(inner.x + 2, replace_row, "Replace:", 8, label_style);
        }

        // Input box background (slightly distinct — use statusline bg)
        let input_bg = statusline_style;
        surface.clear_with(Rect::new(input_x, search_row, input_width as u16, 1), input_bg);
        surface.clear_with(Rect::new(input_x, replace_row, input_width as u16, 1), input_bg);

        render_input(
            surface,
            input_x,
            search_row,
            input_width,
            &self.search_input,
            self.search_cursor,
            input_bg.patch(text_style),
        );
        render_input(
            surface,
            input_x,
            replace_row,
            input_width,
            &self.replace_input,
            self.replace_cursor,
            input_bg.patch(text_style),
        );

        // ── Row 3: divider ─────────────────────────────────────────────────
        let div_row = inner.y + 3;
        let divider = "─".repeat(inner.width as usize);
        surface.set_stringn(inner.x, div_row, &divider, inner.width as usize, separator_style);

        // ── Bottom 2 rows: separator + centered hint bar ──────────────────
        if inner.height <= 5 {
            return;
        }
        let sep_row = inner.y + inner.height - 2;
        let status_row = inner.y + inner.height - 1;

        surface.set_stringn(
            inner.x,
            sep_row,
            &divider,
            inner.width as usize,
            separator_style,
        );

        let selected_count = self.results.iter().filter(|r| r.selected).count();
        let total_count = self.results.len();
        let status_text = match &self.status {
            Some(s) => {
                if total_count > 0 {
                    format!(
                        "{s}  ({selected_count}/{total_count} sel)  \
                         <enter>:replace this  R:replace all selected  \
                         [space]:toggle  [a]ll  [n]one"
                    )
                } else {
                    s.clone()
                }
            }
            None => {
                "Tab:move focus  <enter>:replace hovered  R:replace selected  [a]/[n]:select all/none"
                    .to_string()
            }
        };
        // Center the hint text within inner.width
        let w = inner.width as usize;
        let text_len = status_text.len().min(w);
        let padding = (w.saturating_sub(text_len)) / 2;
        let centered = format!("{:>width$}", &status_text[..text_len], width = padding + text_len);
        surface.set_stringn(inner.x, status_row, &centered, w, inactive_style);

        // ── Rows 4..(height-3): results list (left) + diff preview (right) ─
        let list_y = inner.y + 4;
        // 4 header rows + 2 footer rows (separator + hints)
        let list_height = (inner.height - 6) as usize;

        // Split horizontally: list gets ~38%, preview gets ~62%
        // (minimum 20 chars for list, rest for preview)
        let list_width = ((inner.width as usize) * 38 / 100).max(20).min(inner.width as usize - 3);
        let preview_x = inner.x + list_width as u16 + 1; // +1 for divider
        let preview_width = (inner.x + inner.width).saturating_sub(preview_x) as usize;
        let divider_x = inner.x + list_width as u16;

        // Vertical divider
        for dy in 0..list_height {
            surface.set_stringn(divider_x, list_y + dy as u16, "│", 1, separator_style);
        }

        self.scroll_to_cursor(list_height);

        let results_focused = matches!(self.focused, FocusedField::Results);
        let scroll = self.scroll_offset;
        let cursor = self.result_cursor;

        // ── Left: results list ─────────────────────────────────────────────
        for (i, result) in self
            .results
            .iter()
            .enumerate()
            .skip(scroll)
            .take(list_height)
        {
            let row = list_y + (i - scroll) as u16;
            let is_cursor = results_focused && i == cursor;
            let row_bg = if is_cursor { cursor_row_style } else { background };

            surface.clear_with(Rect::new(inner.x, row, list_width as u16, 1), row_bg);

            // Toggle indicator ● / ○
            let (indicator, ind_style) = if result.selected {
                ("● ", text_style.add_modifier(Modifier::BOLD))
            } else {
                ("○ ", inactive_style)
            };
            let ind_style = if is_cursor { row_bg.patch(ind_style) } else { ind_style };
            surface.set_stringn(inner.x, row, indicator, 2, ind_style);

            // Path (truncated) + line number
            let rel_path = helix_stdx::path::get_relative_path(&result.path);
            let path_str = rel_path.to_string_lossy();
            let line_num_str = format!(":{}", result.line_num + 1);
            // Available width for path = list_width - 2 (indicator) - len(line_num_str) - 1 (space)
            let path_avail = (list_width)
                .saturating_sub(2 + line_num_str.len() + 1);
            let path_str = if path_str.len() > path_avail {
                // Truncate from left with "…"
                let keep = path_avail.saturating_sub(1);
                format!("…{}", &path_str[path_str.len().saturating_sub(keep)..])
            } else {
                path_str.to_string()
            };
            let path_style = if is_cursor { row_bg.patch(inactive_style) } else { inactive_style };
            let num_style = if is_cursor { row_bg.patch(text_style) } else { text_style };
            let (cx2, _) = surface.set_stringn(inner.x + 2, row, &path_str, path_avail, path_style);
            surface.set_stringn(cx2, row, &line_num_str, line_num_str.len(), num_style);
        }

        // Scrollbar on list panel right edge
        if self.results.len() > list_height && list_height > 0 {
            let total = self.results.len();
            let bar_h = (list_height * list_height / total).max(1);
            let bar_top = scroll * list_height / total;
            let bar_x = inner.x + list_width as u16 - 1;
            for dy in 0..list_height {
                let c = if dy >= bar_top && dy < bar_top + bar_h { "▐" } else { " " };
                surface.set_stringn(bar_x, list_y + dy as u16, c, 1, inactive_style);
            }
        }

        // ── Right: diff preview for cursor result ──────────────────────────
        if preview_width < 6 {
            return;
        }

        // Header — centered within the preview panel
        {
            let label = " Preview ";
            let label_len = label.len();
            let dashes_total = preview_width.saturating_sub(label_len);
            let left_dashes = dashes_total / 2;
            let right_dashes = dashes_total - left_dashes;
            let header = format!(
                "{}{}{}",
                "─".repeat(left_dashes),
                label,
                "─".repeat(right_dashes),
            );
            surface.set_stringn(preview_x, list_y, &header, preview_width, text_style);
        }

        if let Some(result) = self.results.get(cursor) {
            let line = &result.line_content;
            let ms = result.match_start_in_line.min(line.len());
            let me = result.match_end_in_line.min(line.len());
            let matched_text = &line[ms..me];
            let replacement = self.compute_replacement_for(matched_text);

            // Build "after" line by splicing replacement into the original line
            let after_line = format!("{}{}{}", &line[..ms], replacement, &line[me..]);

            // Row 0 (list_y): header (already written)
            // Row 1: file + line number
            if list_height > 1 {
                let rel_path = helix_stdx::path::get_relative_path(&result.path);
                let loc = format!("{}:{}", rel_path.display(), result.line_num + 1);
                let loc_style = text_style.add_modifier(Modifier::BOLD);
                surface.set_stringn(preview_x, list_y + 1, &loc, preview_width, loc_style);
            }

            // Row 2: empty separator
            // Row 3: Before line (minus / deletion highlight)
            if list_height > 3 {
                let before_prefix = "- ";
                let prefix_style = diff_minus_style.add_modifier(Modifier::BOLD);
                let (bx, _) = surface.set_stringn(
                    preview_x,
                    list_y + 3,
                    before_prefix,
                    before_prefix.len(),
                    prefix_style,
                );
                // Render line with the match portion highlighted more intensely
                let avail = (inner.x + inner.width).saturating_sub(bx) as usize;
                render_diff_line(
                    surface,
                    bx,
                    list_y + 3,
                    avail,
                    line,
                    ms,
                    me,
                    text_style,
                    diff_minus_style.add_modifier(Modifier::REVERSED),
                );
            }

            // Row 4: After line (plus / addition highlight)
            if list_height > 4 {
                let after_prefix = "+ ";
                let prefix_style = diff_plus_style.add_modifier(Modifier::BOLD);
                let (ax, _) = surface.set_stringn(
                    preview_x,
                    list_y + 4,
                    after_prefix,
                    after_prefix.len(),
                    prefix_style,
                );
                let avail = (inner.x + inner.width).saturating_sub(ax) as usize;
                // Highlight the replacement portion in the after line
                let repl_end = ms + replacement.len();
                render_diff_line(
                    surface,
                    ax,
                    list_y + 4,
                    avail,
                    &after_line,
                    ms,
                    repl_end.min(after_line.len()),
                    text_style,
                    diff_plus_style.add_modifier(Modifier::REVERSED),
                );
            }
        } else if !self.search_input.is_empty() {
            // No results yet
            let msg = if self.scope == SearchScope::Workspace {
                "Searching workspace…"
            } else {
                "No matches"
            };
            surface.set_stringn(preview_x, list_y + 1, msg, preview_width, inactive_style);
        }
    }

    fn cursor(&self, area: Rect, _editor: &Editor) -> (Option<Position>, CursorKind) {
        let inner = area.inner(Margin::all(1));
        let label_width: u16 = 10; // "▶ Search: " / "  Replace:"
        let input_x = inner.x + label_width;

        let (row, input, cursor_byte) = match self.focused {
            FocusedField::Search => (inner.y + 1, &self.search_input, self.search_cursor),
            FocusedField::Replace => (inner.y + 2, &self.replace_input, self.replace_cursor),
            FocusedField::Results => return (None, CursorKind::Hidden),
        };

        let cursor_byte = cursor_byte.min(input.len());
        let disp_width = input[..cursor_byte].width() as u16;
        let col = input_x + disp_width;
        (
            Some(Position::new(row as usize, col as usize)),
            CursorKind::Bar,
        )
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Rendering helpers
// ──────────────────────────────────────────────────────────────────────────────

/// Render a line of text with one highlighted span (ms..me byte range).
fn render_diff_line(
    surface: &mut Surface,
    x: u16,
    y: u16,
    avail: usize,
    line: &str,
    ms: usize,
    me: usize,
    normal_style: helix_view::theme::Style,
    highlight_style: helix_view::theme::Style,
) {
    // Trim trailing newline for display
    let line = line.trim_end_matches('\n').trim_end_matches('\r');
    let ms = ms.min(line.len());
    let me = me.min(line.len());

    let before = &line[..ms];
    let matched = &line[ms..me];
    let after = &line[me..];

    let (nx, _) = surface.set_stringn(x, y, before, avail, normal_style);
    let avail2 = avail.saturating_sub((nx - x) as usize);
    if avail2 > 0 {
        let (nx, _) = surface.set_stringn(nx, y, matched, avail2, highlight_style);
        let avail3 = avail.saturating_sub((nx - x) as usize);
        if avail3 > 0 {
            surface.set_stringn(nx, y, after, avail3, normal_style);
        }
    }
}

/// Render a single-line text input, scrolling to keep cursor visible.
fn render_input(
    surface: &mut Surface,
    x: u16,
    y: u16,
    width: usize,
    text: &str,
    cursor_byte: usize,
    style: helix_view::theme::Style,
) {
    if width == 0 {
        return;
    }
    let cursor_byte = cursor_byte.min(text.len());
    let cursor_disp = text[..cursor_byte].width();

    // Compute anchor (scroll offset in display cells)
    let anchor_byte = if cursor_disp >= width {
        // Find byte position such that text[anchor..] fits cursor at end
        let target = cursor_disp - width + 1;
        let mut disp = 0usize;
        let mut byte = 0usize;
        for (i, c) in text.char_indices() {
            if disp >= target {
                byte = i;
                break;
            }
            disp += c.width().unwrap_or(1) as usize;
            byte = i + c.len_utf8();
        }
        byte
    } else {
        0
    };

    surface.set_stringn(x, y, &text[anchor_byte..], width, style);
}
