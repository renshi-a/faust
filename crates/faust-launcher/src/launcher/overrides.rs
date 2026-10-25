//! Environment variables for developers and tests (`FAUST_SUPPORT_DIR`, `FAUST_TEST_ANSWERS`,
//! …). They change what the launcher trusts and asks, so a release launcher ignores them: they
//! count only in a bundle `faust dev` made, or in a `faust` built with the `test-hooks` feature.

use std::sync::OnceLock;

/// The `Info.plist` key `faust dev` sets on the bundles it makes.
pub(crate) const DEVELOPMENT_KEY: &str = "FaustDevelopment";

/// The variable's value, when overrides are allowed here.
pub(crate) fn get(name: &str) -> Option<String> {
    allowed().then(|| std::env::var(name).ok()).flatten()
}

/// Is the variable set to `1`, and allowed?
pub(crate) fn flag(name: &str) -> bool {
    get(name).as_deref() == Some("1")
}

fn allowed() -> bool {
    static ALLOWED: OnceLock<bool> = OnceLock::new();
    *ALLOWED.get_or_init(|| cfg!(feature = "test-hooks") || development_bundle())
}

/// Was this bundle made by `faust dev`?
fn development_bundle() -> bool {
    let Some(app) = super::shipped::bundle_path() else { return false };
    let plist = std::fs::read_to_string(app.join("Contents/Info.plist")).unwrap_or_default();
    plist.contains(&format!("<key>{DEVELOPMENT_KEY}</key><true/>"))
}
