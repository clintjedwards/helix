use super::*;

use helix_term::{application::Application, config::ConfigRaw};

/// Build an app whose keymap is `test_config`'s default plus the bindings in
/// the given TOML `[keys]` table, keeping the test editor config (LSP off).
fn app_with_keys(keys_toml: &str) -> anyhow::Result<Application> {
    let raw: ConfigRaw = toml::from_str(keys_toml)?;
    let mut config = test_config();
    config.keys = raw.keys.expect("test keys toml must define [keys]");
    Ok(AppBuilder::new().with_config(config).build()?)
}

/// A single keybinding bound to several `:toggle` commands must toggle every
/// option, not just the last one. Each command reads the staged config so the
/// changes accumulate within one event-loop tick.
#[tokio::test(flavor = "multi_thread")]
async fn sequence_toggles_every_option() -> anyhow::Result<()> {
    let mut app = app_with_keys(
        r#"
[keys.normal]
C-y = [":toggle cursorline", ":toggle auto-format", ":toggle color-modes"]
"#,
    )?;

    let (cursorline, auto_format, color_modes) = {
        let c = app.editor.config();
        (c.cursorline, c.auto_format, c.color_modes)
    };

    test_key_sequences(
        &mut app,
        vec![
            // One press flips all three and leaves nothing staged.
            (
                Some("<C-y>"),
                Some(&|app| {
                    let c = app.editor.config();
                    assert_eq!(c.cursorline, !cursorline);
                    assert_eq!(c.auto_format, !auto_format);
                    assert_eq!(c.color_modes, !color_modes);
                    assert!(app.editor.pending_config.is_none());
                }),
            ),
            // A second press flips them back, proving the staged config was
            // cleared and does not accumulate stale state across ticks.
            (
                Some("<C-y>"),
                Some(&|app| {
                    let c = app.editor.config();
                    assert_eq!(c.cursorline, cursorline);
                    assert_eq!(c.auto_format, auto_format);
                    assert_eq!(c.color_modes, color_modes);
                    assert!(app.editor.pending_config.is_none());
                }),
            ),
        ],
        false,
    )
    .await
}

/// A sequence mixing `:set` and `:toggle` on different options must apply both,
/// and must not disturb unrelated options.
#[tokio::test(flavor = "multi_thread")]
async fn sequence_mixes_set_and_toggle() -> anyhow::Result<()> {
    let mut app = app_with_keys(
        r#"
[keys.normal]
C-y = [":set cursorline true", ":toggle color-modes"]
"#,
    )?;

    let (auto_format, color_modes) = {
        let c = app.editor.config();
        (c.auto_format, c.color_modes)
    };

    test_key_sequence(
        &mut app,
        Some("<C-y>"),
        Some(&|app| {
            let c = app.editor.config();
            assert!(c.cursorline);
            assert_eq!(c.color_modes, !color_modes);
            // unrelated option untouched
            assert_eq!(c.auto_format, auto_format);
            assert!(app.editor.pending_config.is_none());
        }),
        false,
    )
    .await
}

/// A standalone `:toggle` still works on its own, confirming no regression for
/// the single-command path.
#[tokio::test(flavor = "multi_thread")]
async fn single_toggle_still_works() -> anyhow::Result<()> {
    let mut app = AppBuilder::new().build()?;

    let cursorline = app.editor.config().cursorline;

    test_key_sequence(
        &mut app,
        Some(":toggle cursorline<ret>"),
        Some(&|app| {
            assert_eq!(app.editor.config().cursorline, !cursorline);
            assert!(app.editor.pending_config.is_none());
        }),
        false,
    )
    .await
}
