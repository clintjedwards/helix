//! `:diff-review` walks every file with unstaged changes so it can be read, touched up and
//! approved. Approving stages the file with `git add`, which takes it out of the queue. The
//! queue is re-read from `git status` on every step instead of being kept around, so changes made
//! outside the editor (a `git add` in a terminal, another tool editing a file that was
//! already approved) show up in the review as they happen.

use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context as _};
use helix_core::{char_idx_at_visual_offset, Range, Rope, RopeSlice, Selection};
use helix_stdx::path::{canonicalize, get_relative_path};
use helix_vcs::FileChange;
use helix_view::{
    current, current_ref, editor::Action, editor::DiffReview, Document, DocumentId, Editor,
};

use super::{hunk_range, push_jump, Context};

struct Entry {
    /// Canonical path, used to match against open documents and the review's current file.
    path: PathBuf,
    change: FileChange,
}

impl Entry {
    /// The text for the placeholder buffer shown in place of a file that can't be opened and
    /// walked like text. Approving and moving on still work for these.
    fn unopenable_note(&self) -> Option<String> {
        let rel = display(&self.path);
        let (what, approve) = if let FileChange::Deleted { .. } = self.change {
            (format!("{rel} was deleted."), "stage the deletion")
        } else if self.path.is_dir() {
            (
                format!("{rel} is not a regular file, so it isn't shown."),
                "stage it",
            )
        } else if is_binary(&self.path) {
            (
                format!("{rel} is a binary file, so it isn't shown."),
                "stage it",
            )
        } else {
            return None;
        };
        Some(format!(
            "{what}\n\n\
             \x20   space v              approve and {approve}\n\
             \x20   ]r / [r              next / previous file without approving\n\
             \x20   :diff-review-stop    stop reviewing\n"
        ))
    }

    fn paths_to_stage(&self) -> Vec<&Path> {
        match &self.change {
            // Stage both sides so the index doesn't end up holding half a rename.
            FileChange::Renamed { from_path, to_path } => vec![from_path, to_path],
            change => vec![change.path()],
        }
    }
}

fn display(path: &Path) -> String {
    get_relative_path(path).display().to_string()
}

/// Checks for a NUL byte near the start of the file, the same heuristic git uses.
fn is_binary(path: &Path) -> bool {
    let mut buf = [0u8; 8000];
    let Ok(mut file) = std::fs::File::open(path) else {
        return false;
    };
    let n = file.read(&mut buf).unwrap_or(0);
    buf[..n].contains(&0)
}

fn load_queue(editor: &Editor) -> anyhow::Result<Vec<Entry>> {
    let cwd = helix_stdx::env::current_working_dir();
    let changes = editor
        .diff_providers
        .changed_files(&cwd)
        .context("couldn't read git status")?;
    let mut queue: Vec<Entry> = changes
        .into_iter()
        .map(|change| Entry {
            path: canonicalize(change.path()),
            change,
        })
        .collect();
    queue.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(queue)
}

pub(crate) fn start(editor: &mut Editor) -> anyhow::Result<()> {
    let queue = load_queue(editor)?;
    if queue.is_empty() {
        editor.diff_review = None;
        editor.set_status("Nothing to review: no unstaged changes");
        return Ok(());
    }
    show(editor, &queue, 0);
    Ok(())
}

pub(crate) fn stop(editor: &mut Editor) -> anyhow::Result<()> {
    if editor.diff_review.is_none() {
        bail!("No diff review is running");
    }
    end(editor);
    editor.set_status("Diff review stopped");
    Ok(())
}

fn end(editor: &mut Editor) {
    if let Some(placeholder) = editor.diff_review.take().and_then(|r| r.placeholder) {
        close_placeholder(editor, placeholder);
    }
}

fn open_placeholder(editor: &mut Editor, note: String) -> DocumentId {
    let mut doc = Document::from(
        Rope::from(note),
        None,
        editor.config.clone(),
        editor.syn_loader.clone(),
    );
    doc.readonly = true;
    editor.new_file_from_document(Action::Replace, doc)
}

fn close_placeholder(editor: &mut Editor, doc_id: DocumentId) {
    // Forced since it's only a note; any stray edits made to it don't matter.
    if editor.documents.contains_key(&doc_id) {
        let _ = editor.close_document(doc_id, true);
    }
}

fn current_review(editor: &Editor) -> anyhow::Result<&DiffReview> {
    editor
        .diff_review
        .as_ref()
        .context("No diff review is running; start one with :diff-review")
}

/// Index of the entry after (or before) `current`, wrapping around. `current` may have already
/// left the queue, so this goes by sort order rather than looking it up.
fn neighbor(queue: &[Entry], current: &Path, forward: bool) -> usize {
    if forward {
        queue
            .iter()
            .position(|entry| entry.path.as_path() > current)
            .unwrap_or(0)
    } else {
        queue
            .iter()
            .rposition(|entry| entry.path.as_path() < current)
            .unwrap_or(queue.len() - 1)
    }
}

fn step(editor: &mut Editor, forward: bool) -> anyhow::Result<()> {
    let current = current_review(editor)?.current.clone();
    let queue = load_queue(editor)?;
    if queue.is_empty() {
        finish(editor);
        return Ok(());
    }
    let idx = neighbor(&queue, &current, forward);
    show(editor, &queue, idx);
    Ok(())
}

fn finish(editor: &mut Editor) {
    end(editor);
    editor.set_status("Diff review done: all changes are staged");
}

fn show(editor: &mut Editor, queue: &[Entry], idx: usize) {
    let entry = &queue[idx];
    let old_placeholder = editor.diff_review.as_ref().and_then(|r| r.placeholder);

    let (placeholder, opened) = match entry.unopenable_note() {
        Some(note) => (Some(open_placeholder(editor, note)), Ok(())),
        None => (None, editor.open(&entry.path, Action::Replace).map(|_| ())),
    };
    // Closed after the next file is up so the view never falls back to an empty scratch buffer.
    if let Some(old) = old_placeholder {
        close_placeholder(editor, old);
    }
    editor.diff_review = Some(DiffReview {
        current: entry.path.clone(),
        unreviewed: queue.len(),
        pending_approve: None,
        placeholder,
    });

    if let Err(err) = opened {
        editor.set_error(format!("Couldn't open {}: {err}", display(&entry.path)));
        return;
    }
    if placeholder.is_some() {
        return;
    }

    if !goto_first_change(editor) {
        editor.set_status(format!(
            "No content changes in {} (likely a file mode change)",
            display(&entry.path)
        ));
    } else if let FileChange::Renamed { from_path, .. } = &entry.change {
        editor.set_status(format!("Renamed from {}", display(from_path)));
    } else if let FileChange::Conflict { .. } = &entry.change {
        editor.set_status("This file has merge conflicts; approving marks them resolved");
    }
}

/// Moves the cursor to the first change in the current document. Returns false when the file has
/// no content changes.
fn goto_first_change(editor: &mut Editor) -> bool {
    let Some(range) = first_change(editor) else {
        return false;
    };
    let scrolloff = editor.config().scrolloff;
    let (view, doc) = current!(editor);
    push_jump(view, doc);
    let start = range.from();
    doc.set_selection(view.id, Selection::point(start));

    // Center the whole change rather than the cursor. A change taller than the window is pinned
    // near the top instead, so the cursor at its start stays on screen.
    let text = doc.text().slice(..);
    // A deletion at the end of the file is a one char range starting at the end of the text, so
    // its end is past the last char.
    let end = range.to().min(text.len_chars());
    let change_lines = text.char_to_line(end) - text.char_to_line(start);
    let viewport = view.inner_area(doc);
    let rows_above = (viewport.height.saturating_sub(1) as usize / 2)
        .saturating_sub(change_lines / 2)
        .max(scrolloff);
    let text_fmt = doc.text_format(viewport.width, None);
    let mut view_offset = doc.view_offset(view.id);
    (view_offset.anchor, view_offset.vertical_offset) = char_idx_at_visual_offset(
        text,
        start,
        -(rows_above as isize),
        0,
        &text_fmt,
        &view.text_annotations(doc, None),
    );
    doc.set_view_offset(view.id, view_offset);
    true
}

fn first_change(editor: &Editor) -> Option<Range> {
    let (_, doc) = current_ref!(editor);
    let text = doc.text().slice(..);

    // An already-open document has an up to date diff. A freshly opened one is still being
    // diffed in the background, so fall back to finding the first line that differs from the
    // diff base. That's where the first hunk starts in all but unusual cases.
    if let Some(handle) = doc.diff_handle() {
        let diff = handle.load();
        if !diff.is_empty() {
            return Some(hunk_range(diff.nth_hunk(0), text));
        }
    }
    let base = doc
        .path()
        .and_then(|path| editor.diff_providers.get_diff_base(path));
    let Some(base) = base else {
        // No base means the file is untracked, so all of it is new.
        return Some(Range::point(0));
    };
    let line = first_differing_line(&base, text)?;
    Some(Range::point(text.line_to_char(line)))
}

fn first_differing_line(base: &[u8], text: RopeSlice) -> Option<usize> {
    let mut base_lines = base.split_inclusive(|&b| b == b'\n');
    let mut line_buf = String::new();
    for (idx, line) in text.lines().enumerate() {
        line_buf.clear();
        line_buf.extend(line.chunks());
        match base_lines.next() {
            Some(base_line) if base_line == line_buf.as_bytes() => {}
            // Ropes always yield a final empty line, which matches running out of base lines.
            None if line_buf.is_empty() => return None,
            _ => return Some(idx.min(text.len_lines().saturating_sub(1))),
        }
    }
    base_lines
        .next()
        .map(|_| text.len_lines().saturating_sub(1))
}

/// Picks the file approve acts on: the focused buffer if it's in the queue, which covers files
/// opened by hand during the review, and otherwise the review's current file. Placeholders have
/// no path, so they always fall through to the current file.
fn approve_target<'a>(editor: &Editor, queue: &'a [Entry], current: &Path) -> Option<&'a Entry> {
    let focused = doc_path(editor);
    queue
        .iter()
        .find(|entry| Some(&entry.path) == focused.as_ref())
        .or_else(|| queue.iter().find(|entry| entry.path == current))
}

fn doc_path(editor: &Editor) -> Option<PathBuf> {
    let (_, doc) = current_ref!(editor);
    doc.path().cloned()
}

fn approve(editor: &mut Editor) -> anyhow::Result<()> {
    let current = current_review(editor)?.current.clone();
    let queue = load_queue(editor)?;
    let Some(entry) = approve_target(editor, &queue, &current) else {
        // Already staged from somewhere else, so there's nothing left to do for it.
        return step(editor, true);
    };

    // Stage what's in the buffer, not an older copy on disk. Saving is async, so staging finishes
    // in `on_document_saved`. The save refuses to clobber a file changed on disk since it was
    // loaded, which keeps someone else's newer edit from being overwritten and then approved unseen.
    if let Some(doc) = editor.document_by_path(&entry.path) {
        if doc.is_modified() {
            let doc_id = doc.id();
            editor.save::<PathBuf>(doc_id, None, false)?;
            if let Some(review) = editor.diff_review.as_mut() {
                review.pending_approve = Some(doc_id);
            }
            return Ok(());
        }
    }

    stage_and_advance(editor, entry)
}

fn stage_and_advance(editor: &mut Editor, entry: &Entry) -> anyhow::Result<()> {
    let cwd = helix_stdx::env::current_working_dir();
    helix_vcs::stage_paths(&cwd, &entry.paths_to_stage())?;
    let approved = entry.path.clone();

    let queue = load_queue(editor)?;
    if queue.is_empty() {
        finish(editor);
        return Ok(());
    }
    editor.set_status(format!("Approved {}", display(&approved)));
    show(editor, &queue, neighbor(&queue, &approved, true));
    Ok(())
}

/// Finishes an approve that was waiting on its buffer to be saved.
pub(crate) fn on_document_saved(editor: &mut Editor, doc_id: DocumentId) {
    let Some(review) = editor.diff_review.as_mut() else {
        return;
    };
    if review.pending_approve != Some(doc_id) {
        return;
    }
    review.pending_approve = None;

    let Some(doc) = editor.documents.get(&doc_id) else {
        return;
    };
    if doc.is_modified() {
        editor.set_error("Buffer changed while saving; approve again to stage it");
        return;
    }
    let Some(path) = doc.path().cloned() else {
        return;
    };

    let result = load_queue(editor).and_then(|queue| {
        match queue.iter().find(|entry| entry.path == path) {
            Some(entry) => stage_and_advance(editor, entry),
            // The save brought the file back in line with the index.
            None => step(editor, true),
        }
    });
    if let Err(err) = result {
        editor.set_error(err.to_string());
    }
}

/// A save failed, so any approve waiting on it is dropped. The save error is already shown.
pub(crate) fn on_save_failed(editor: &mut Editor) {
    if let Some(review) = editor.diff_review.as_mut() {
        review.pending_approve = None;
    }
}

fn report(cx: &mut Context, result: anyhow::Result<()>) {
    if let Err(err) = result {
        cx.editor.set_error(err.to_string());
    }
}

pub fn diff_review_approve(cx: &mut Context) {
    let result = approve(cx.editor);
    report(cx, result);
}

pub fn diff_review_next_file(cx: &mut Context) {
    let result = step(cx.editor, true);
    report(cx, result);
}

pub fn diff_review_prev_file(cx: &mut Context) {
    let result = step(cx.editor, false);
    report(cx, result);
}
