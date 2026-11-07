//! Coloring of the App's own editors: exported shell commands.

use crate::support::tokens;
use services_ui::syntax::{Language, State};

#[test]
fn shell_commands_variables_and_flags() {
    let t = tokens(
        Language::Shell,
        "echo \"$HOME\" --flag && printf {{:NAME}}",
        &mut State::default(),
    );
    assert!(t.contains(&("echo".into(), Some("builtin"))));
    assert!(t.contains(&("$HOME".into(), Some("variable-2"))));
    assert!(t.contains(&("--flag".into(), Some("attribute"))));
    assert!(t.contains(&(":".into(), Some("gw-placeholder-colon"))));
}
