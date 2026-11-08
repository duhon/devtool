//! The editor's [`Language`]: what the Kernel's modules and the console add to its text.
use crate::{completion, console, kernel, template_completion};
use serde_json::Value;
use services_sdk::{Check, ColumnType, Fold, Line, LineColumns};
use services_widgets::editor::language::{self, Completion};
use services_widgets::editor::model::Context;

struct Modules;

impl language::Language for Modules {
    fn is_service(&self, service: &str) -> bool {
        kernel::is_service(service)
    }

    fn markers(&self, service: &str) -> Vec<String> {
        console::markers(service)
    }

    fn highlight(
        &self,
        service: &str,
        state: Vec<u8>,
        lines: Vec<String>,
        columns: Vec<LineColumns>,
    ) -> Option<Vec<Line>> {
        kernel::with(|modules| modules.highlight(service, state, lines, columns)).ok()
    }

    fn fold(&self, service: &str, text: &str, columns: Vec<ColumnType>) -> Vec<Fold> {
        let mut check = kernel::check(service, text, &Value::Null);
        check.columns = columns;
        kernel::with(|modules| modules.fold(service, check)).unwrap_or_default()
    }

    fn suggest(
        &self,
        service: &str,
        field: &str,
        source: &str,
        line: usize,
        ch: usize,
        context: &Context,
    ) -> Completion {
        template_completion::suggest(service, field, source, line, ch, &context.templates)
            .unwrap_or_else(|| {
                completion::suggest(service, field, source, line, ch, &context.hints)
            })
    }

    fn hints(&self, service: &str, source: &str, line: usize, ch: usize) -> Option<(Check, u32)> {
        completion::kernel_check(service, source, line, ch)
    }

    fn variables(&self) -> Vec<(String, String)> {
        kernel::variables()
    }
}

/// Makes the editor highlight, fold and complete through the Kernel's modules.
pub fn install() {
    language::install(&Modules);
}
