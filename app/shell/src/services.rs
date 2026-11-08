//! Console services: the service modules the Kernel runs, one tab each.

/// A console service: a module the Kernel runs or knows from the module list.
pub fn contains(id: &str) -> bool {
    crate::kernel::is_service(id)
}

/// Service ids in load order.
pub fn ids() -> Vec<String> {
    crate::kernel::declared()
        .into_iter()
        .map(|manifest| manifest.id)
        .collect()
}

/// The tab title: the manifest name, or the id of an unknown service.
pub fn label(id: &str) -> String {
    crate::kernel::manifest(id).map_or_else(|| id.to_owned(), |manifest| manifest.name)
}

/// Tab order without a saved one: by the modules' `order`, then the rest in load order.
pub fn default_order() -> Vec<String> {
    let mut ordered: Vec<(Option<u32>, String)> = crate::kernel::declared()
        .into_iter()
        .map(|manifest| (manifest.order, manifest.id))
        .collect();
    ordered.sort_by_key(|(order, _)| order.unwrap_or(u32::MAX));
    ordered.into_iter().map(|(_, id)| id).collect()
}

/// The service a new console opens on.
pub fn default_service() -> String {
    default_order().into_iter().next().unwrap_or_default()
}

/// Template library sections of a service: `check` (whole checks), then its `#marker`
/// sections as its module completes them.
pub fn template_sections(id: &str) -> Vec<String> {
    let markers = crate::console::markers(id);
    if markers.is_empty() {
        return Vec::new();
    }
    std::iter::once("check".to_owned())
        .chain(markers.into_iter().filter(|marker| marker != "check"))
        .collect()
}

pub fn has_template_section(id: &str, section: &str) -> bool {
    template_sections(id).iter().any(|name| name == section)
}
