//! Fixture loading and pre-timing verification.

use std::fs;
use std::path::{Path, PathBuf};

use crate::ops::*;

pub struct Fixture {
    pub name: String,
    pub text: String,
}

/// RedisJSON's benchmark datasets, read from the pinned submodule.
pub fn load_fixtures(dir: &Path) -> Vec<Fixture> {
    let mut paths: Vec<PathBuf> = fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("cannot read fixture dir {}: {e}", dir.display()))
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    paths.sort();
    paths
        .into_iter()
        .map(|p| Fixture {
            name: p.file_stem().unwrap().to_string_lossy().into_owned(),
            text: fs::read_to_string(&p).unwrap(),
        })
        .collect()
}

pub fn default_fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../RedisJSON/tests/benchmarks/datasets")
}

/// Check, before anything is timed, that `ijson` and the baseline agree on a
/// fixture: parse with both, serialise the `ijson` value, and require that it
/// reads back as the same document the baseline parsed.
pub fn verify(fx: &Fixture) -> Result<(), String> {
    let reference = serde_parse(&fx.text);
    let out = ijson_serialize(&ijson_parse(&fx.text));
    let back: serde_json::Value =
        serde_json::from_str(&out).map_err(|e| format!("ijson output does not parse: {e}"))?;
    if back != reference {
        return Err("ijson round trip differs from serde_json::Value".into());
    }
    Ok(())
}
