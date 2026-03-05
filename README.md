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

This is clint's personal fork with the following features added on top of upstream:

## LSP status picker

Run `:lsp-info` to open a picker showing all language servers for the current file — their status (initializing, running, stopped), root path, and PID. Pressing Enter on a server restarts it.

## Docked file explorer

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

## Interactive search & replace

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

## Auto file reload (Linux only)

Automatically reloads open buffers when their files change on disk. Disabled by default. When a buffer has unsaved changes, a prompt is shown before reloading.

Enable in `~/.config/helix/config.toml`:

```toml
[editor.auto-reload]
enable = true
prompt-if-modified = true  # ask before reloading buffers with unsaved changes
```

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
