use std::path::PathBuf;

use anyhow::Result;

use crate::model::Store;

/// connections.json 位于 exe 同目录
pub fn store_path() -> PathBuf {
    let dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."));
    dir.join("connections.json")
}

pub fn load() -> Store {
    let path = store_path();
    let mut store = std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| serde_json::from_str::<Store>(&text).ok())
        .unwrap_or_default();
    store.ensure_default_group();
    store
}

pub fn save(store: &Store) -> Result<()> {
    let path = store_path();
    let text = serde_json::to_string_pretty(store)?;
    std::fs::write(path, text)?;
    Ok(())
}
