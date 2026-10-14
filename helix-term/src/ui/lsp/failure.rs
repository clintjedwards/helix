use helix_lsp::LanguageServerFailure;
use helix_view::{
    document::Mode,
    graphics::{Margin, Rect},
    input::Event,
};
use tui::{
    buffer::Buffer as Surface,
    text::Text,
    widgets::{Block, Paragraph, Widget, Wrap},
};

use crate::{
    compositor::{Callback, Component, Context, EventResult},
    key,
};

/// Box in the bottom right corner, just above the statusline, explaining why a language
/// server died. Unlike a popup it isn't tied to the cursor and doesn't close when you
/// move, so it's hard to miss even if it shows up while you're busy. Every key except a
/// normal mode Esc passes through to the editor, so it never gets in the way of editing.
pub struct LspFailureNotice {
    name: String,
    title: String,
    text: String,
}

impl LspFailureNotice {
    pub const ID: &'static str = "lsp-failure";

    pub fn new(name: String, failure: &LanguageServerFailure) -> Self {
        let mut text = String::new();
        if failure.stderr.is_empty() {
            text.push_str("It didn't write anything to stderr.\n");
        } else {
            for line in &failure.stderr {
                text.push_str(line);
                text.push('\n');
            }
        }
        text.push_str("\nEsc to dismiss, :lsp-restart to try again, :log-open for the full log");

        Self {
            title: format!(" {name} {} ", failure.reason),
            name,
            text,
        }
    }

    /// The notice is moot once the server comes back or the user restarts or stops it.
    fn is_stale(&self, cx: &Context) -> bool {
        cx.editor.language_servers.failure(&self.name).is_none()
    }
}

impl Component for LspFailureNotice {
    fn handle_event(&mut self, event: &Event, cx: &mut Context) -> EventResult {
        let close: Callback = Box::new(|compositor, _| {
            compositor.remove(Self::ID);
        });

        if self.is_stale(cx) {
            return EventResult::Ignored(Some(close));
        }

        // Only claim Esc in normal mode. In insert and select mode the editor needs it to
        // get back to normal mode.
        match event {
            Event::Key(key) if *key == key!(Esc) && cx.editor.mode() == Mode::Normal => {
                EventResult::Consumed(Some(close))
            }
            _ => EventResult::Ignored(None),
        }
    }

    fn render(&mut self, viewport: Rect, surface: &mut Surface, cx: &mut Context) {
        if self.is_stale(cx) {
            return;
        }

        let text_style = cx.editor.theme.get("ui.text.info");
        let popup_style = cx.editor.theme.get("ui.popup.info");
        let border_style = cx.editor.theme.get("error");

        let text = Text::from(self.text.as_str());
        // Stderr lines can be very long (panic messages, paths), so wrap at two thirds of
        // the screen instead of letting the box take over the whole width.
        let max_text_width = (viewport.width * 2 / 3).max(20);
        let (text_width, text_height) = crate::ui::text::required_size(&text, max_text_width);
        let title_width = self.title.chars().count() as u16;

        let width = text_width.max(title_width) + 2 + 2; // +2 for border, +2 for margin
        let height = text_height + 2; // +2 for border
        let area = viewport.intersection(Rect::new(
            viewport.width.saturating_sub(width),
            viewport.height.saturating_sub(height + 2), // +2 for statusline and command line
            width,
            height,
        ));
        surface.clear_with(area, popup_style);

        let block = Block::bordered()
            .title(self.title.as_str())
            .border_style(border_style);
        let inner = block.inner(area).inner(Margin::horizontal(1));
        block.render(area, surface);

        Paragraph::new(&text)
            .style(text_style)
            .wrap(Wrap { trim: false })
            .render(inner, surface);
    }

    fn id(&self) -> Option<&'static str> {
        Some(Self::ID)
    }
}
