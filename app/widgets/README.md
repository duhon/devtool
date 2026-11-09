# Widgets

`services-widgets` holds the App's generic UI components (Yew/WebAssembly): the view
that renders a module's UI tree (`view`), the text editor (`editor::View` and its
`Handle`), the find bar, the check rail, the status bar, the JSON tree, the pane
resizer and the state indicator. They know nothing of the Kernel, modules or checks.

The editor gets its language through the neutral `editor::extension::Extension` trait:
tokens, folds, completions, hints, output ranges and line bands. An owner passes one
per editor (`Handle::with_extension`); editors made from a UI tree get theirs from the
`Languages` provider the Shell installs (`editor::extension::install`). The Shell's
`language::Language` implements both over the Kernel.

Each widget's CSS lies next to it and is exported as `CSS`; classes carry the widget's
name (`editor-*`, `find-bar-*`, `check-rail-*`, `status-bar-*`, `indicator-*`,
`resizer`), and colours come only from the variables of the Shell's theme
(`app/shell/src/theme.css`).

`cargo test -p services-widgets` runs the pure logic tests (`tests/editor/`).
