use std::{collections::HashSet, time::Duration};

use helix_core::{syntax::config::LanguageServerFeature, Assoc};
use helix_event::{cancelable_future, register_hook};
use helix_lsp::lsp;
use helix_view::{
    document::SemanticToken,
    events::{DocumentDidChange, DocumentDidOpen, LanguageServerExited, LanguageServerInitialized},
    handlers::{lsp::SemanticTokensEvent, Handlers},
    DocumentId, Editor,
};
use tokio::time::Instant;

use crate::job;

#[derive(Default)]
pub(super) struct SemanticTokensHandler {
    docs: HashSet<DocumentId>,
}

const DOCUMENT_CHANGE_DEBOUNCE: Duration = Duration::from_millis(250);

impl helix_event::AsyncHook for SemanticTokensHandler {
    type Event = SemanticTokensEvent;

    fn handle_event(&mut self, event: Self::Event, _timeout: Option<Instant>) -> Option<Instant> {
        let SemanticTokensEvent(doc_id) = event;
        self.docs.insert(doc_id);
        Some(Instant::now() + DOCUMENT_CHANGE_DEBOUNCE)
    }

    fn finish_debounce(&mut self) {
        let docs = std::mem::take(&mut self.docs);

        job::dispatch_blocking(move |editor, _compositor| {
            for doc in docs {
                request_semantic_tokens(editor, doc);
            }
        });
    }
}

/// Map a server's token type and modifiers onto a theme scope.
///
/// Returning `None` leaves the span to tree-sitter. We skip lexical types like keywords,
/// strings and comments on purpose: tree-sitter already gets those right and usually with
/// finer scopes than the server gives us. Where semantic tokens actually help is identifiers,
/// since only the server knows whether `Foo` is an enum or a module, or whether `x` is a
/// parameter or a constant.
fn token_scope(token_type: &str, modifiers: &[&str]) -> Option<&'static str> {
    let has = |modifier: &str| modifiers.contains(&modifier);

    let scope = match token_type {
        "namespace" | "toolModule" => "namespace",
        "type" | "class" | "interface" | "struct" | "typeAlias" | "union" => "type",
        "builtinType" => "type.builtin",
        "enum" => "type.enum",
        "typeParameter" => "type.parameter",
        "enumMember" => "type.enum.variant",
        "parameter" => "variable.parameter",
        "variable" if has("readonly") => "constant",
        "variable" => "variable",
        "property" => "variable.other.member",
        "const" | "static" | "constParameter" => "constant",
        "function" => "function",
        "method" => "function.method",
        "macro" | "derive" | "procMacro" => "function.macro",
        _ => return None,
    };

    // Standard library items. This keeps things like Go's `int` and `len` looking the same
    // as they do under tree-sitter. Themes that don't define a `.builtin` variant fall back
    // to the parent scope.
    if has("defaultLibrary") && !scope.ends_with(".builtin") {
        return Some(match scope {
            "namespace" => "namespace.builtin",
            "type" => "type.builtin",
            "type.enum" => "type.enum.builtin",
            "type.enum.variant" => "type.enum.variant.builtin",
            "variable" => "variable.builtin",
            "variable.other.member" => "variable.other.member.builtin",
            "constant" => "constant.builtin",
            "function" => "function.builtin",
            "function.method" => "function.method.builtin",
            "function.macro" => "function.macro.builtin",
            other => other,
        });
    }

    Some(scope)
}

/// Decode the server's relative token encoding into absolute character ranges.
fn decode_tokens(
    text: &helix_core::Rope,
    data: &[lsp::SemanticToken],
    legend: &lsp::SemanticTokensLegend,
    offset_encoding: helix_lsp::OffsetEncoding,
) -> Vec<SemanticToken> {
    let mut tokens = Vec::with_capacity(data.len());
    let mut modifiers = Vec::new();
    let mut line = 0;
    let mut character = 0;

    for token in data {
        if token.delta_line != 0 {
            line += token.delta_line;
            character = token.delta_start;
        } else {
            character += token.delta_start;
        }

        let Some(token_type) = legend.token_types.get(token.token_type as usize) else {
            continue;
        };

        modifiers.clear();
        modifiers.extend(
            legend
                .token_modifiers
                .iter()
                .enumerate()
                .filter(|(i, _)| *i < 32 && token.token_modifiers_bitset & (1 << i) != 0)
                .map(|(_, modifier)| modifier.as_str()),
        );

        let Some(scope) = token_scope(token_type.as_str(), &modifiers) else {
            continue;
        };

        let start = lsp::Position::new(line, character);
        let end = lsp::Position::new(line, character + token.length);
        let (Some(start), Some(end)) = (
            helix_lsp::util::lsp_pos_to_pos(text, start, offset_encoding),
            helix_lsp::util::lsp_pos_to_pos(text, end, offset_encoding),
        ) else {
            continue;
        };

        // The overlay renderer requires sorted, non-overlapping ranges. Well behaved servers
        // already guarantee this, but drop anything that would break it.
        let after_previous = tokens
            .last()
            .is_none_or(|prev: &SemanticToken| prev.end <= start);
        if start < end && after_previous {
            tokens.push(SemanticToken { start, end, scope });
        }
    }

    tokens
}

pub(crate) fn request_semantic_tokens(editor: &mut Editor, doc_id: DocumentId) {
    if !editor.config().lsp.display_semantic_tokens {
        return;
    }

    let Some(doc) = editor.document_mut(doc_id) else {
        return;
    };

    // Tokens from two servers would overlap, so only ask the first one that supports them.
    let Some(language_server) = doc
        .language_servers_with_feature(LanguageServerFeature::SemanticTokens)
        .next()
    else {
        return;
    };

    let Some(legend) = language_server.semantic_tokens_legend().cloned() else {
        return;
    };
    let Some(future) = language_server.text_document_semantic_tokens_full(doc.identifier()) else {
        return;
    };
    let offset_encoding = language_server.offset_encoding();
    let text = doc.text().clone();
    let cancel = doc.semantic_tokens_controller.restart();

    tokio::spawn(async move {
        let response = match cancelable_future(future, &cancel).await {
            Some(Ok(response)) => response,
            Some(Err(err)) => {
                log::error!("semantic tokens request failed: {err}");
                return;
            }
            // The request was cancelled.
            None => return,
        };

        let data = match response {
            Some(lsp::SemanticTokensResult::Tokens(tokens)) => tokens.data,
            Some(lsp::SemanticTokensResult::Partial(partial)) => partial.data,
            None => Vec::new(),
        };

        let tokens = decode_tokens(&text, &data, &legend, offset_encoding);
        job::dispatch(move |editor, _| {
            if let Some(doc) = editor.documents.get_mut(&doc_id) {
                doc.semantic_tokens = tokens;
            }
        })
        .await;
    });
}

pub(super) fn register_hooks(handlers: &Handlers) {
    register_hook!(move |event: &mut DocumentDidOpen<'_>| {
        request_semantic_tokens(event.editor, event.doc);
        Ok(())
    });

    let tx = handlers.semantic_tokens.clone();
    register_hook!(move |event: &mut DocumentDidChange<'_>| {
        // Shift existing tokens so they stay roughly in place until fresh ones arrive.
        event
            .changes
            .update_positions(event.doc.semantic_tokens.iter_mut().flat_map(|token| {
                std::iter::once((&mut token.start, Assoc::After))
                    .chain(std::iter::once((&mut token.end, Assoc::Before)))
            }));

        // Ghost transactions (completion previews) aren't sent to the server, so asking it
        // for tokens would give us positions for the wrong text.
        if !event.ghost_transaction {
            event.doc.semantic_tokens_controller.cancel();
            helix_event::send_blocking(&tx, SemanticTokensEvent(event.doc.id()));
        }

        Ok(())
    });

    register_hook!(move |event: &mut LanguageServerInitialized<'_>| {
        let doc_ids: Vec<_> = event.editor.documents().map(|doc| doc.id()).collect();

        for doc_id in doc_ids {
            request_semantic_tokens(event.editor, doc_id);
        }

        Ok(())
    });

    register_hook!(move |event: &mut LanguageServerExited<'_>| {
        for doc in event.editor.documents_mut() {
            if doc.supports_language_server(event.server_id) {
                doc.semantic_tokens.clear();
            }
        }

        let doc_ids: Vec<_> = event.editor.documents().map(|doc| doc.id()).collect();

        for doc_id in doc_ids {
            request_semantic_tokens(event.editor, doc_id);
        }

        Ok(())
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token(
        delta_line: u32,
        delta_start: u32,
        length: u32,
        ty: u32,
        mods: u32,
    ) -> lsp::SemanticToken {
        lsp::SemanticToken {
            delta_line,
            delta_start,
            length,
            token_type: ty,
            token_modifiers_bitset: mods,
        }
    }

    #[test]
    fn decodes_relative_positions() {
        let text = helix_core::Rope::from("fn main() {\n    let x = FOO;\n}\n");
        let legend = lsp::SemanticTokensLegend {
            token_types: vec!["function".into(), "variable".into(), "keyword".into()],
            token_modifiers: vec!["declaration".into(), "readonly".into()],
        };
        let data = [
            token(0, 0, 2, 2, 0), // `fn`, keyword so skipped
            token(0, 3, 4, 0, 1), // `main`
            token(1, 8, 1, 1, 1), // `x`
            token(0, 4, 3, 1, 2), // `FOO`, readonly
        ];

        let tokens = decode_tokens(&text, &data, &legend, helix_lsp::OffsetEncoding::Utf16);
        let got: Vec<_> = tokens.iter().map(|t| (t.start, t.end, t.scope)).collect();
        assert_eq!(
            got,
            vec![
                (3, 7, "function"),
                (20, 21, "variable"),
                (24, 27, "constant"),
            ]
        );
    }

    #[test]
    fn drops_overlapping_tokens() {
        let text = helix_core::Rope::from("abcdef\n");
        let legend = lsp::SemanticTokensLegend {
            token_types: vec!["variable".into()],
            token_modifiers: vec![],
        };
        let data = [token(0, 0, 4, 0, 0), token(0, 2, 2, 0, 0)];

        let tokens = decode_tokens(&text, &data, &legend, helix_lsp::OffsetEncoding::Utf16);
        assert_eq!(tokens.len(), 1);
    }

    #[test]
    fn default_library_maps_to_builtin() {
        assert_eq!(
            token_scope("type", &["defaultLibrary"]),
            Some("type.builtin")
        );
        assert_eq!(
            token_scope("function", &["defaultLibrary"]),
            Some("function.builtin")
        );
        assert_eq!(
            token_scope("variable", &["readonly", "defaultLibrary"]),
            Some("constant.builtin")
        );
        assert_eq!(
            token_scope("builtinType", &["defaultLibrary"]),
            Some("type.builtin")
        );
        assert_eq!(token_scope("keyword", &[]), None);
    }
}
