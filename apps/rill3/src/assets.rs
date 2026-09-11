use sha2::{Digest, Sha256};

pub(crate) const CSS: &str = include_str!("../../../static/css/app.css");

/// Changes the browser cache key whenever the bundled stylesheet changes.
pub(crate) fn stylesheet_path(base: &str) -> String {
    let digest = hex::encode(Sha256::digest(CSS.as_bytes()));
    format!("{base}/static/app.css?v={digest:.16}")
}
