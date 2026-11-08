# Widgets

`services-widgets` holds the App's generic UI components (Yew/WebAssembly): the view
that renders a module's UI tree, the text editor (`editor::View` and its `Handle`), the
find bar, the check rail, the status bar, the JSON tree, the pane resizer and the state
indicator. They know nothing of the Kernel or the application: the Shell gives the
editor its language through `editor::language::install`.

Each widget's CSS lies next to it and is exported as `CSS`; colours come only from the
variables of the Shell's theme (`app/shell/src/theme.css`).

`cargo test -p services-widgets` runs the pure logic tests (`tests/editor/`).
