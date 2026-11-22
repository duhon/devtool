//! The `#target`: `k8s://<context>/<namespace>`. Owned by the language and run part.

/// A place: one kubeconfig context and one namespace.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Target {
    pub context: String,
    /// Required; cluster-scoped resources ignore it.
    pub namespace: String,
}

const FORM: &str = "k8s://<context>/<namespace>";

/// Reads the `#target` text; `{{…}}` variables are already resolved. The namespace is what
/// follows the last `/`, so a context name may contain slashes (an EKS cluster ARN does).
pub fn parse(text: &str) -> Result<Target, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err(format!("#target is required: {FORM}"));
    }
    let Some(rest) = text.strip_prefix("k8s://") else {
        return Err(format!("#target must be {FORM}"));
    };
    if rest.contains(char::is_whitespace) {
        return Err(format!("#target must be {FORM}, without spaces"));
    }
    let Some((context, namespace)) = rest.rsplit_once('/') else {
        return Err(format!(
            "A namespace is required: put it after the context ({FORM})"
        ));
    };
    if context.is_empty() {
        return Err(format!("A context is required in #target ({FORM})"));
    }
    if namespace.is_empty() {
        return Err(format!(
            "A namespace is required: put it after the context ({FORM})"
        ));
    }
    crate::resources::check_namespace(namespace)?;
    Ok(Target {
        context: context.into(),
        namespace: namespace.into(),
    })
}
