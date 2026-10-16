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

/// 读取数据并修复坏值，返回 (数据, 被修复的条目数)。
///
/// 修好了就立刻写回，并把修复前的原文另存一份 `.bak`，避免每次启动都要再修一遍，
/// 同时保留原始数据可回溯。
pub fn load() -> (Store, usize) {
    let path = store_path();
    let original = std::fs::read_to_string(&path).ok();
    let mut store = original
        .as_deref()
        .and_then(|text| serde_json::from_str::<Store>(text).ok())
        .unwrap_or_default();
    store.ensure_default_group();
    let repaired = store.repair();
    if repaired > 0 {
        let backup = path.with_extension("json.bak");
        if !backup.exists() {
            if let Some(text) = original.as_deref() {
                let _ = std::fs::write(&backup, text);
            }
        }
        if let Err(e) = save(&store) {
            eprintln!("修复服务器 ID 后写回 connections.json 失败: {e}");
        }
    }
    (store, repaired)
}

pub fn save(store: &Store) -> Result<()> {
    let path = store_path();
    let text = serde_json::to_string_pretty(store)?;
    std::fs::write(path, text)?;
    Ok(())
}
