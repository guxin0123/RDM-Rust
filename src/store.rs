use std::path::{Path, PathBuf};

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

/// 读取数据。
///
/// 服务器 id 不落盘，读完后统一在内存里分配（见 [`Store::assign_ids`]）。
/// 文件里如果还留着旧版本的 `id` 字段，serde 会直接忽略，下次保存时自然就没了。
pub fn load() -> Store {
    let mut store = std::fs::read_to_string(store_path())
        .ok()
        .and_then(|text| serde_json::from_str::<Store>(&text).ok())
        .unwrap_or_default();
    store.ensure_default_group();
    store.assign_ids();
    store
}

pub fn save(store: &Store) -> Result<()> {
    let path = store_path();
    backup_legacy_ids_once(&path);
    let text = serde_json::to_string_pretty(store)?;
    std::fs::write(path, text)?;
    Ok(())
}

/// 第一次按新格式（不含 `id`）覆盖旧文件之前，把还带着 `id` 的旧文件备份一份，
/// 方便回滚到旧版本时数据不会缺字段。只备份一次。
fn backup_legacy_ids_once(path: &Path) {
    let backup = path.with_extension("json.bak");
    if backup.exists() {
        return;
    }
    let Ok(text) = std::fs::read_to_string(path) else {
        return;
    };
    // pretty 输出里字段是 `"id": `，用它判断是不是旧格式
    if text.contains("\"id\": ") {
        let _ = std::fs::write(&backup, text);
    }
}
