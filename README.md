<div align="center">

<h1>
<picture>
  <source media="(prefers-color-scheme: dark)" srcset="logo_dark.svg">
  <source media="(prefers-color-scheme: light)" srcset="logo_light.svg">
  <img alt="Helix" height="128" src="logo_light.svg">
</picture>
</h1>

[![Build status](https://github.com/helix-editor/helix/actions/workflows/build.yml/badge.svg)](https://github.com/helix-editor/helix/actions)
[![GitHub Release](https://img.shields.io/github/v/release/helix-editor/helix)](https://github.com/helix-editor/helix/releases/latest)
[![Documentation](https://shields.io/badge/-documentation-452859)](https://docs.helix-editor.com/)
[![GitHub contributors](https://img.shields.io/github/contributors/helix-editor/helix)](https://github.com/helix-editor/helix/graphs/contributors)
[![Matrix Space](https://img.shields.io/matrix/helix-community:matrix.org)](https://matrix.to/#/#helix-community:matrix.org)

</div>

![Screenshot](./screenshot.png)

A [Kakoune](https://github.com/mawww/kakoune) / [Neovim](https://github.com/neovim/neovim) inspired editor, written in Rust.

The editing model is very heavily based on Kakoune; during development I found
myself agreeing with most of Kakoune's design decisions.

For more information, see the [website](https://helix-editor.com) or
[documentation](https://docs.helix-editor.com/).

All shortcuts/keymaps can be found [in the documentation on the website](https://docs.helix-editor.com/keymap.html).

[Troubleshooting](https://github.com/helix-editor/helix/wiki/Troubleshooting)

# Features

- Vim-like modal editing
- Multiple selections
- Built-in language server support
- Smart, incremental syntax highlighting and code editing via tree-sitter

Although it's primarily a terminal-based editor, I am interested in exploring
a custom renderer (similar to Emacs) using wgpu.

Note: Only certain languages have indentation definitions at the moment. Check
`runtime/queries/<lang>/` for `indents.scm`.

# Fork additions

This is my personal fork of helix adding to the base codebase the things I personally need and use. Click any feature below to expand its documentation.

<details>
<summary><strong>LSP status picker</strong> — view and restart language servers (<code>:lsp-info</code>)</summary>

<br>

Run `:lsp-info` to open a picker showing all language servers for the current file — their status (initializing, running, stopped), root path, and PID. Pressing Enter on a server restarts it.

</details>

<details>
<summary><strong>Docked file explorer</strong> — sidebar file tree with vim-style navigation (<code>&lt;space&gt;e</code> / <code>&lt;space&gt;E</code>)</summary>

<br>

A sidebar file explorer with vim-style navigation. Press `<space>e` to reveal the current file in the explorer, or `<space>E` to open/focus it at the workspace root.

Inside the explorer:
- `j`/`k` — move up/down
- `<ret>` — open file
- `r` — rename
- `a` — new file or folder
- `d` — delete
- `]` — change root to current folder
- `[` — go to previous root
- `?` — toggle help

Configure in `~/.config/helix/config.toml`:

```toml
[editor.explorer]
position = "left"   # or "right"
column-width = 36
```

</details>

<details>
<summary><strong>Interactive search &amp; replace</strong> — VSCode-style panel with live diff preview (<code>&lt;space&gt;Alt-/</code>)</summary>

<br>

A VSCode-style search and replace panel with a live diff preview. Press `<space>Alt-/` to open it.

```
┌─ Search & Replace — buffer ──────────────────────────────────────────────────┐
│  match-case (alt-c)   regex (alt-r)  [whole-word](alt-w)   scope: buffer (ctrl-s) │
│ ▶ Search:  foo                                                               │
│   Replace: bar                                                               │
├──────────────────────────────────────────────────────────────────────────────┤
│ ● src/main.rs:42   │ ──────────── Preview ─────────────────────────────     │
│ ● src/lib.rs:17    │ src/main.rs:42                                          │
│ ○ tests/test.rs:5  │                                                         │
│                    │ - let foo = "hello world"                               │
│                    │ + let bar = "hello world"                               │
├──────────────────────────────────────────────────────────────────────────────┤
│          <enter>:replace this  R:replace all selected  [a]ll  [n]one         │
└──────────────────────────────────────────────────────────────────────────────┘
```

**Options** (toggle from any field):
- `alt-c` — **match-case**: case-sensitive matching
- `alt-r` — **regex**: treat search as a regular expression (supports `$1`/`$2` capture group references in the replacement)
- `alt-w` — **whole-word**: only match complete words, not substrings

**Scope**: `ctrl-s` toggles between the current buffer and the entire workspace.

**Results list** (focus with `Tab`):
- `j`/`k` — move up/down
- `space` — toggle a result on/off
- `a`/`n` — select all / deselect all
- `enter` — replace only the hovered match
- `R` — replace all selected matches at once

Replacements are applied as normal transactions and can be undone with `u`.

</details>

<details>
<summary><strong>Line drag</strong> — move selected lines up/down (<code>Ctrl-k</code> / <code>Ctrl-j</code>)</summary>

<br>

Move selected lines up or down without cutting and pasting. Works with multiple cursors/selections.

| Key                           | Description              |
| ----------------------------- | ------------------------ |
| `Ctrl-k`, `Ctrl-Shift-Up`     | Move selected lines up   |
| `Ctrl-j`, `Ctrl-Shift-Down`   | Move selected lines down |

</details>

<details>
<summary><strong>GitHub permalink</strong> — copy a permalink for the cursor/selection (<code>Space+l</code>)</summary>

<br>

Press `Space+l` to generate a GitHub permalink for the current cursor position or selection and copy it to the system clipboard. The URL points to the exact commit, file, and line range.

Single-line cursor produces `#L42`; a visual selection produces `#L42-L55`.

The URL is displayed in the status bar after copying.

</details>

<details>
<summary><strong>Global indentation settings</strong> — set a default tab width and indent style for all files</summary>

<br>

Set a global tab width and indent style that applies to all files without a language-specific override. Language config in `languages.toml` and `.editorconfig` files take precedence.

```toml
[editor]
indent-style = "spaces"   # "spaces" or "tabs"
tab-width = 4             # number of spaces per indent level
```

Priority (highest wins): `.editorconfig` → language config (`languages.toml`) → global editor config → built-in default (tabs, width 4).

</details>

<details>
<summary><strong>Auto file reload</strong> (Linux only) — reload buffers when files change on disk</summary>

<br>

Automatically reloads open buffers when their files change on disk. Reloads are instantaneous — uses inotify directory watching, so atomic saves (vim, emacs, most editors) are detected correctly. Disabled by default. When a buffer has unsaved changes, a prompt is shown before reloading.

Enable in `~/.config/helix/config.toml`:

```toml
[editor.auto-reload]
enable = true
prompt-if-modified = true  # ask before reloading buffers with unsaved changes
```

</details>

<details>
<summary><strong>Chained config commands in one keybinding</strong> — toggle/set several options with a single key</summary>

<br>

Bind a single key to a sequence of `:set` and `:toggle` commands and they all take effect. In upstream Helix, only the last command in such a sequence applied: each command read the same pre-change config snapshot and sent a full-config update over an async channel, so the last update clobbered the earlier ones.

This fork stages config changes so each command in the sequence sees the result of the ones before it, and all of them are applied.

```toml
[keys.normal]
C-space = [":toggle lsp.display-inlay-hints", ":toggle rainbow-brackets", ":toggle indent-guides.render"]
```

</details>

# Installation

[Installation documentation](https://docs.helix-editor.com/install.html).

[![Packaging status](https://repology.org/badge/vertical-allrepos/helix-editor.svg?exclude_unsupported=1)](https://repology.org/project/helix-editor/versions)

# Contributing

Contributing guidelines can be found [here](./docs/CONTRIBUTING.md).

# Getting help

Your question might already be answered on the [FAQ](https://github.com/helix-editor/helix/wiki/FAQ).

Discuss the project on the community [Matrix Space](https://matrix.to/#/#helix-community:matrix.org) (make sure to join `#helix-editor:matrix.org` if you're on a client that doesn't support Matrix Spaces yet).

# Credits

Thanks to [@jakenvac](https://github.com/jakenvac) for designing the logo!
