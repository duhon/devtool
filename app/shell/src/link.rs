//! The modules that are part of the App: the built-in Settings panel and, natively, every
//! module linked in so tests run them without a browser. The Kernel gets them from here.

use services_kernel::modules::registry::{self, Link, Module};
use services_sdk::Manifest;

/// Links the App's modules into the Kernel; call it before anything uses the Kernel.
pub fn link() {
    registry::link(Link { builtin, linked });
}

/// The built-in Settings panel: part of the App on every target, never declared by the
/// Host, removed or updated.
fn builtin() -> Module {
    (
        Manifest::parse(include_str!(
            "../../../modules/panels/settings/manifest.json"
        ))
        .expect("Settings manifest"),
        Box::new(services_sdk::Linked::<services_module_settings::Settings>::default()),
    )
}

#[cfg(target_arch = "wasm32")]
fn linked() -> Vec<Module> {
    Vec::new()
}

#[cfg(not(target_arch = "wasm32"))]
fn linked() -> Vec<Module> {
    use services_sdk::Linked;
    let manifest = |json: &str| Manifest::parse(json).expect("linked module manifest");
    macro_rules! linked {
        ($($path:literal => $module:ty),* $(,)?) => {
            vec![$((
                manifest(include_str!(concat!("../../../modules/", $path, "/manifest.json"))),
                Box::new(Linked::<$module>::default()) as Box<dyn services_kernel::Instance>,
            )),*]
        };
    }
    linked![
        "services/rest" => services_module_rest::Rest,
        "services/mysql" => services_module_mysql::Mysql,
        "services/graphql" => services_module_graphql::Graphql,
        "services/grpc" => services_module_grpc::Grpc,
        "services/splunk" => services_module_splunk::Splunk,
        "services/terminal" => services_module_terminal::Terminal,
        "services/artemis" => services_module_artemis::Artemis,
        "services/opensearch" => services_module_opensearch::OpenSearch,
        "panels/variables" => services_module_variables::Variables,
        "panels/history" => services_module_history::History,
        "panels/templates" => services_module_templates::Templates,
        "panels/hints" => services_module_hints::Hints,
        "panels/dictionary" => services_module_dictionary::Dictionary,
        "panels/sessions" => services_module_sessions::Sessions,
        "panels/diff" => services_module_diff::Diff,
        "panels/agent" => services_module_agent::Agent,
    ]
}
