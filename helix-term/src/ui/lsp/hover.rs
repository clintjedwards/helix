use std::sync::Arc;

use arc_swap::ArcSwap;
use helix_core::syntax;
use helix_lsp::lsp;
use helix_view::graphics::{Margin, Rect, Style};
use helix_view::input::{Event, MouseButton, MouseEvent, MouseEventKind};
use helix_view::keyboard::KeyModifiers;
use tui::buffer::Buffer;
use tui::widgets::{BorderType, Paragraph, Widget, Wrap};

use crate::compositor::{Component, Context, EventResult};

use crate::ui::Markdown;
use crate::{alt, key, shift};

pub struct Hover {
    active_index: usize,
    contents: Vec<(Option<Markdown>, Markdown)>,
    /// Link selected with tab/shift-tab in the active content, followed on enter.
    active_link: Option<usize>,
    /// The content area rendered with link markers, used to find which link was clicked.
    link_map: Option<Buffer>,
    /// Link under the mouse when a button went down, followed if it's released there, and
    /// whether to open it in the browser.
    pressed_link: Option<(usize, bool)>,
}

impl Hover {
    pub const ID: &'static str = "hover";

    pub fn new(
        hovers: Vec<(String, lsp::Hover)>,
        config_loader: Arc<ArcSwap<syntax::Loader>>,
    ) -> Self {
        let n_hovers = hovers.len();
        let contents = hovers
            .into_iter()
            .enumerate()
            .map(|(idx, (server_name, hover))| {
                let header = (n_hovers > 1).then(|| {
                    Markdown::new(
                        format!("**[{}/{}] {}**", idx + 1, n_hovers, server_name),
                        config_loader.clone(),
                    )
                });
                let body = Markdown::new(
                    hover_contents_to_string(hover.contents),
                    config_loader.clone(),
                );
                (header, body)
            })
            .collect();

        Self {
            active_index: usize::default(),
            contents,
            active_link: None,
            link_map: None,
            pressed_link: None,
        }
    }

    fn has_header(&self) -> bool {
        self.contents.len() > 1
    }

    fn content(&self) -> &(Option<Markdown>, Markdown) {
        &self.contents[self.active_index]
    }

    fn set_index(&mut self, index: usize) {
        assert!((0..self.contents.len()).contains(&index));
        self.set_active_link(None);
        self.active_index = index;
    }

    fn set_active_link(&mut self, link: Option<usize>) {
        self.active_link = link;
        self.contents[self.active_index].1.set_active_link(link);
    }

    /// Moves the link selection forward or backward, wrapping around. Returns
    /// false if there are no links to select.
    fn cycle_link(&mut self, forward: bool) -> bool {
        let count = self.content().1.links().len();
        if count == 0 {
            return false;
        }
        let next = match (self.active_link, forward) {
            (None, true) => 0,
            (None, false) => count - 1,
            (Some(i), true) => (i + 1) % count,
            (Some(i), false) => i.checked_sub(1).unwrap_or(count - 1),
        };
        self.set_active_link(Some(next));
        true
    }

    /// Closes the popup and follows the selected link, if there is one. Links go to the
    /// symbol's source unless `in_browser` is set, in which case their web page is opened.
    fn follow_active_link(&self, in_browser: bool) -> EventResult {
        let Some(target) = self
            .active_link
            .and_then(|i| self.content().1.links().into_iter().nth(i))
        else {
            return EventResult::Ignored(None);
        };
        EventResult::Consumed(Some(Box::new(move |compositor, cx| {
            compositor.remove(Self::ID);
            if in_browser {
                crate::commands::lsp::open_doc_link_in_browser(cx.editor, cx.jobs, target);
            } else {
                crate::commands::lsp::follow_doc_link(cx.editor, cx.jobs, target);
            }
        })))
    }
}

const PADDING_HORIZONTAL: u16 = 2;
const PADDING_TOP: u16 = 1;
const PADDING_BOTTOM: u16 = 1;
const HEADER_HEIGHT: u16 = 1;
const SEPARATOR_HEIGHT: u16 = 1;

impl Component for Hover {
    fn render(&mut self, area: Rect, surface: &mut Buffer, cx: &mut Context) {
        let margin = Margin::all(1);
        let area = area.inner(margin);

        let (header, contents) = self.content();

        // show header and border only when more than one results
        if let Some(header) = header {
            // header LSP Name
            let header = header.parse(Some(&cx.editor.theme));
            let header = Paragraph::new(&header);
            header.render(area.with_height(HEADER_HEIGHT), surface);

            // border
            let sep_style = Style::default();
            let borders = BorderType::line_symbols(BorderType::Plain);
            for x in area.left()..area.right() {
                if let Some(cell) = surface.get_mut(x, area.top() + HEADER_HEIGHT) {
                    cell.set_symbol(borders.horizontal).set_style(sep_style);
                }
            }
        }

        // hover content
        let markdown = contents;
        let contents = markdown.parse(Some(&cx.editor.theme));
        let contents_area = area.clip_top(if self.has_header() {
            HEADER_HEIGHT + SEPARATOR_HEIGHT
        } else {
            0
        });
        let scroll = (cx.scroll.unwrap_or_default() as u16, 0);
        let contents_para = Paragraph::new(&contents)
            .wrap(Wrap { trim: false })
            .scroll(scroll);
        contents_para.render(contents_area, surface);

        // Render the link markers exactly like the real text so wrapping and scrolling line up.
        let link_map = (!markdown.links().is_empty()).then(|| {
            let mut link_map = Buffer::empty(contents_area);
            Paragraph::new(&markdown.link_map())
                .wrap(Wrap { trim: false })
                .scroll(scroll)
                .render(contents_area, &mut link_map);
            link_map
        });
        self.link_map = link_map;
    }

    fn required_size(&mut self, viewport: (u16, u16)) -> Option<(u16, u16)> {
        let max_text_width = viewport.0.saturating_sub(PADDING_HORIZONTAL).clamp(10, 120);

        let (header, contents) = self.content();

        let header_width = header
            .as_ref()
            .map(|header| {
                let header = header.parse(None);
                let (width, _height) = crate::ui::text::required_size(&header, max_text_width);
                width
            })
            .unwrap_or_default();

        let contents = contents.parse(None);
        let (content_width, content_height) =
            crate::ui::text::required_size(&contents, max_text_width);

        let width = PADDING_HORIZONTAL + header_width.max(content_width);
        let height = if self.has_header() {
            PADDING_TOP + HEADER_HEIGHT + SEPARATOR_HEIGHT + content_height + PADDING_BOTTOM
        } else {
            PADDING_TOP + content_height + PADDING_BOTTOM
        };

        Some((width, height))
    }

    fn handle_event(&mut self, event: &Event, _ctx: &mut Context) -> EventResult {
        if let Event::Mouse(MouseEvent {
            kind,
            column,
            row,
            modifiers,
        }) = *event
        {
            let (is_press, button) = match kind {
                MouseEventKind::Down(button) => (true, button),
                MouseEventKind::Up(button) => (false, button),
                _ => return EventResult::Ignored(None),
            };
            // Middle-click and ctrl-click open the link's web page instead of jumping to the
            // source. Ctrl-click alone isn't enough since some terminals keep it for themselves.
            let in_browser = match button {
                MouseButton::Left => modifiers.contains(KeyModifiers::CONTROL),
                MouseButton::Middle => true,
                MouseButton::Right => return EventResult::Ignored(None),
            };
            let link = self
                .link_map
                .as_ref()
                .and_then(|link_map| link_map.get(column, row))
                .and_then(|cell| Markdown::link_at(cell.fg));
            // Links are followed on release rather than press. Following one closes the popup,
            // and if the release then reached the editor it would yank the new selection to the
            // mouse register. Both halves of the click are consumed, also when they miss a link,
            // so the popup stays open.
            if is_press {
                self.pressed_link = link.map(|link| (link, in_browser));
                return EventResult::Consumed(None);
            }
            return match (link, self.pressed_link.take()) {
                (Some(link), Some((pressed, in_browser))) if link == pressed => {
                    self.set_active_link(Some(link));
                    self.follow_active_link(in_browser)
                }
                _ => EventResult::Consumed(None),
            };
        }

        let Event::Key(event) = event else {
            return EventResult::Ignored(None);
        };

        match event {
            alt!('p') => {
                let index = self
                    .active_index
                    .checked_sub(1)
                    .unwrap_or(self.contents.len() - 1);
                self.set_index(index);
                EventResult::Consumed(None)
            }
            alt!('n') => {
                self.set_index((self.active_index + 1) % self.contents.len());
                EventResult::Consumed(None)
            }
            key!(Tab) | shift!(Tab) => {
                if self.cycle_link(*event == key!(Tab)) {
                    EventResult::Consumed(None)
                } else {
                    EventResult::Ignored(None)
                }
            }
            key!(Enter) => self.follow_active_link(false),
            alt!(Enter) => self.follow_active_link(true),
            _ => EventResult::Ignored(None),
        }
    }
}

fn hover_contents_to_string(contents: lsp::HoverContents) -> String {
    fn marked_string_to_markdown(contents: lsp::MarkedString) -> String {
        match contents {
            lsp::MarkedString::String(contents) => contents,
            lsp::MarkedString::LanguageString(string) => {
                if string.language == "markdown" {
                    string.value
                } else {
                    format!("```{}\n{}\n```", string.language, string.value)
                }
            }
        }
    }
    match contents {
        lsp::HoverContents::Scalar(contents) => marked_string_to_markdown(contents),
        lsp::HoverContents::Array(contents) => contents
            .into_iter()
            .map(marked_string_to_markdown)
            .collect::<Vec<_>>()
            .join("\n\n"),
        lsp::HoverContents::Markup(contents) => contents.value,
    }
}
