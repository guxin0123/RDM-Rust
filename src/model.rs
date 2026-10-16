use std::collections::HashSet;

use serde::{Deserialize, Serialize};

pub const DEFAULT_GROUP: &str = "默认";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerEntry {
    /// 服务器唯一标识。历史数据里这个字段可能缺失或整批重复，读取时由
    /// [`Store::repair`] 兜底修复。
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub group: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default)]
    pub username: String,
    /// DPAPI 加密后的 base64，空字符串表示未保存密码
    #[serde(default)]
    pub password_enc: String,
}

fn default_port() -> u16 {
    3389
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Store {
    #[serde(default)]
    pub groups: Vec<String>,
    #[serde(default)]
    pub servers: Vec<ServerEntry>,
}

impl Store {
    /// 修复历史数据里的坏值，返回被修复的服务器数量。
    ///
    /// `id` 是服务器在增删改、连接、拖拽排序时唯一的身份依据。历史数据中出现过
    /// 整批服务器共用同一个 id 的情况，此时按 id 查找只会命中第一个，表现就是
    /// "把服务器拖到别的分组后，原来那个还在"。这里在读取时重新分配 id，并顺手
    /// 把空分组名归入默认分组。
    pub fn repair(&mut self) -> usize {
        let mut seen: HashSet<String> = HashSet::new();
        let mut repaired = 0;
        for s in self.servers.iter_mut() {
            if s.id.is_empty() || !seen.insert(s.id.clone()) {
                // 新 id 理论上不会撞车，仍然循环确认一次
                loop {
                    let candidate = new_id();
                    if seen.insert(candidate.clone()) {
                        s.id = candidate;
                        break;
                    }
                }
                repaired += 1;
            }
            if s.group.trim().is_empty() {
                s.group = DEFAULT_GROUP.to_string();
                repaired += 1;
            }
        }
        repaired
    }

    pub fn ensure_default_group(&mut self) {
        if !self.groups.iter().any(|g| g == DEFAULT_GROUP) {
            self.groups.insert(0, DEFAULT_GROUP.to_string());
        }
    }

    /// 显式分组 + 服务器引用到的隐式分组（按顺序去重）
    pub fn group_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.groups.clone();
        for s in &self.servers {
            if !names.contains(&s.group) {
                names.push(s.group.clone());
            }
        }
        names
    }

    pub fn find(&self, id: &str) -> Option<&ServerEntry> {
        self.servers.iter().find(|s| s.id == id)
    }

    pub fn add_group(&mut self, name: &str) -> bool {
        let name = name.trim();
        if name.is_empty() || self.groups.iter().any(|g| g == name) {
            return false;
        }
        self.groups.push(name.to_string());
        true
    }

    pub fn rename_group(&mut self, old: &str, new: &str) -> bool {
        let new = new.trim();
        if new.is_empty() || old == new || self.groups.iter().any(|g| g == new) {
            return false;
        }
        if let Some(g) = self.groups.iter_mut().find(|g| *g == old) {
            *g = new.to_string();
        }
        for s in self.servers.iter_mut().filter(|s| s.group == old) {
            s.group = new.to_string();
        }
        true
    }

    /// 删除分组，组内服务器移入默认分组
    pub fn delete_group(&mut self, name: &str) {
        self.groups.retain(|g| g != name);
        self.ensure_default_group();
        for s in self.servers.iter_mut().filter(|s| s.group == name) {
            s.group = DEFAULT_GROUP.to_string();
        }
    }

    pub fn upsert_server(&mut self, entry: ServerEntry) {
        if let Some(existing) = self.servers.iter_mut().find(|s| s.id == entry.id) {
            *existing = entry;
        } else {
            self.servers.push(entry);
        }
    }

    pub fn remove_server(&mut self, id: &str) {
        self.servers.retain(|s| s.id != id);
    }

    /// 把服务器移到其所在分组的最前面
    pub fn move_server_to_top(&mut self, id: &str) -> bool {
        let Some(pos) = self.servers.iter().position(|s| s.id == id) else {
            return false;
        };
        let group = self.servers[pos].group.clone();
        let entry = self.servers.remove(pos);
        let insert_at = self
            .servers
            .iter()
            .position(|s| s.group == group)
            .unwrap_or(self.servers.len());
        self.servers.insert(insert_at, entry);
        true
    }

    /// 把服务器移到其所在分组的最后面
    pub fn move_server_to_bottom(&mut self, id: &str) -> bool {
        let Some(pos) = self.servers.iter().position(|s| s.id == id) else {
            return false;
        };
        let group = self.servers[pos].group.clone();
        let entry = self.servers.remove(pos);
        let insert_at = self
            .servers
            .iter()
            .rposition(|s| s.group == group)
            .map(|i| i + 1)
            .unwrap_or(self.servers.len());
        self.servers.insert(insert_at, entry);
        true
    }

    /// 把服务器移动到 `target_group` 的指定槽位（`index` 按目标分组**当前**的成员顺序计，
    /// 即包含被拖动的服务器本身）。返回是否真的发生了改变。
    pub fn move_server_to(&mut self, id: &str, target_group: &str, index: usize) -> bool {
        let before: Vec<(String, String)> = self
            .servers
            .iter()
            .map(|s| (s.id.clone(), s.group.clone()))
            .collect();
        let Some(pos) = self.servers.iter().position(|s| s.id == id) else {
            return false;
        };
        let removed_slot = self
            .servers
            .iter()
            .filter(|s| s.group == target_group)
            .position(|s| s.id == id);
        let mut entry = self.servers.remove(pos);
        entry.group = target_group.to_string();
        // `index` 是按移除前的成员顺序给出的，移除后同一槽位要往前挪一格
        let slot = match removed_slot {
            Some(removed) if index > removed => index - 1,
            _ => index,
        };
        let member_positions: Vec<usize> = self
            .servers
            .iter()
            .enumerate()
            .filter(|(_, s)| s.group == target_group)
            .map(|(i, _)| i)
            .collect();
        let insert_at = if slot >= member_positions.len() {
            member_positions
                .last()
                .map(|i| i + 1)
                .unwrap_or(self.servers.len())
        } else {
            member_positions[slot]
        };
        self.servers.insert(insert_at, entry);

        let after: Vec<(String, String)> = self
            .servers
            .iter()
            .map(|s| (s.id.clone(), s.group.clone()))
            .collect();
        before != after
    }

    /// 把分组移动到分组列表的指定槽位（`index` 按分组列表**当前**的顺序计，
    /// 与 [`Store::move_server_to`] 保持一致）。隐式分组会一并固化进 `groups`。
    /// 返回是否真的发生了改变。
    pub fn move_group_to(&mut self, name: &str, index: usize) -> bool {
        let before = self.group_names();
        let mut names = before.clone();
        let Some(pos) = names.iter().position(|g| g == name) else {
            return false;
        };
        names.remove(pos);
        // index 是按移除前的顺序给出的，移除后同一槽位要往前挪一格
        let slot = if index > pos { index - 1 } else { index };
        let slot = slot.min(names.len());
        names.insert(slot, name.to_string());
        if names == before {
            return false;
        }
        self.groups = names;
        true
    }

    /// 把分组移到分组列表的最前面。隐式分组会一并固化进 `groups`。
    pub fn move_group_to_top(&mut self, name: &str) -> bool {
        let mut names = self.group_names();
        let Some(pos) = names.iter().position(|g| g == name) else {
            return false;
        };
        names.remove(pos);
        names.insert(0, name.to_string());
        self.groups = names;
        true
    }

    /// 把分组移到分组列表的最后面。隐式分组会一并固化进 `groups`。
    pub fn move_group_to_bottom(&mut self, name: &str) -> bool {
        let mut names = self.group_names();
        let Some(pos) = names.iter().position(|g| g == name) else {
            return false;
        };
        names.remove(pos);
        names.push(name.to_string());
        self.groups = names;
        true
    }
}

/// 生成新的服务器 id。
///
/// 除毫秒时间戳和随机数外，再加一个进程内自增序号：同一毫秒内连续生成也绝不会重复。
pub fn new_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);

    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    format!(
        "{:x}{:04x}{:04x}",
        millis,
        rand::random::<u16>(),
        seq as u16
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server(id: &str, group: &str, name: &str) -> ServerEntry {
        ServerEntry {
            id: id.to_string(),
            group: group.to_string(),
            name: name.to_string(),
            host: format!("{name}.example.com"),
            port: 3389,
            username: String::new(),
            password_enc: String::new(),
        }
    }

    /// 回归：历史数据里整批服务器共用同一个 id 时，按 id 查找只会命中第一个，
    /// 拖拽就会"把 A 组的服务器拖到 B 组后，A 组那个还在"。读取时必须先修好 id。
    #[test]
    fn repair_gives_every_duplicate_id_a_new_one() {
        let mut store = Store {
            groups: vec![DEFAULT_GROUP.to_string(), "B".to_string()],
            servers: vec![
                server("same", DEFAULT_GROUP, "a1"),
                server("same", "B", "b1"),
                server("same", "B", "b2"),
                server("", "B", "no-id"),
                server("unique", DEFAULT_GROUP, "a2"),
            ],
        };

        let repaired = store.repair();

        // "same" 出现 3 次 → 后 2 个要重新分配；空 id 1 个 → 共 3 条
        assert_eq!(repaired, 3);
        assert_eq!(store.servers[0].id, "same", "第一个出现的不动");
        let ids: HashSet<&str> = store.servers.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids.len(), store.servers.len(), "修复后 id 必须两两不同");
        assert!(store.servers[4].id == "unique");
    }

    #[test]
    fn repair_fills_empty_group_and_is_idempotent() {
        let mut store = Store {
            groups: vec![DEFAULT_GROUP.to_string()],
            servers: vec![server("x", "   ", "a")],
        };
        assert_eq!(store.repair(), 1);
        assert_eq!(store.servers[0].group, DEFAULT_GROUP);
        assert_eq!(store.repair(), 0, "修好之后再修应该无事可做");
    }

    #[test]
    fn move_server_to_another_group_removes_it_from_the_source() {
        let mut store = Store {
            groups: vec![DEFAULT_GROUP.to_string(), "B".to_string()],
            servers: vec![
                server("a1", DEFAULT_GROUP, "a1"),
                server("a2", DEFAULT_GROUP, "a2"),
                server("b1", "B", "b1"),
            ],
        };

        assert!(store.move_server_to("a1", "B", 0));

        assert_eq!(store.find("a1").map(|s| s.group.as_str()), Some("B"));
        assert_eq!(
            store
                .servers
                .iter()
                .filter(|s| s.group == "B")
                .map(|s| s.id.as_str())
                .collect::<Vec<_>>(),
            vec!["a1", "b1"]
        );
        assert_eq!(
            store
                .servers
                .iter()
                .filter(|s| s.group == DEFAULT_GROUP)
                .map(|s| s.id.as_str())
                .collect::<Vec<_>>(),
            vec!["a2"]
        );
    }

    #[test]
    fn move_server_to_slot_counts_the_dragged_server_itself() {
        // 组内 [s0, s1, s2, s3]，把 s0 放到 s2 之后（槽位 3）
        let mut store = Store {
            groups: vec![DEFAULT_GROUP.to_string()],
            servers: (0..4)
                .map(|i| server(&format!("s{i}"), DEFAULT_GROUP, "n"))
                .collect(),
        };
        assert!(store.move_server_to("s0", DEFAULT_GROUP, 3));
        assert_eq!(
            store.servers.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
            vec!["s1", "s2", "s0", "s3"]
        );

        // 落到自己原来的位置应当是空操作
        assert!(!store.move_server_to("s0", DEFAULT_GROUP, 2));
    }

    #[test]
    fn move_server_to_end_of_group_appends() {
        let mut store = Store {
            groups: vec![DEFAULT_GROUP.to_string(), "B".to_string()],
            servers: vec![
                server("a1", DEFAULT_GROUP, "a1"),
                server("a2", DEFAULT_GROUP, "a2"),
                server("b1", "B", "b1"),
            ],
        };
        // 槽位 2 == 默认分组当前的成员数，即追加到末尾
        assert!(store.move_server_to("b1", DEFAULT_GROUP, 2));
        assert_eq!(
            store
                .servers
                .iter()
                .filter(|s| s.group == DEFAULT_GROUP)
                .map(|s| s.id.as_str())
                .collect::<Vec<_>>(),
            vec!["a1", "a2", "b1"]
        );
    }

    #[test]
    fn move_server_into_empty_group_appends_at_end() {
        let mut store = Store {
            groups: vec![DEFAULT_GROUP.to_string(), "empty".to_string()],
            servers: vec![server("a1", DEFAULT_GROUP, "a1")],
        };
        assert!(store.move_server_to("a1", "empty", 0));
        assert_eq!(store.find("a1").map(|s| s.group.as_str()), Some("empty"));
    }

    #[test]
    fn move_group_to_uses_the_index_before_removal() {
        let mut store = Store {
            groups: vec!["A".to_string(), "B".to_string(), "C".to_string()],
            servers: Vec::new(),
        };

        // 把 C 放到 A 之前
        assert!(store.move_group_to("C", 0));
        assert_eq!(store.group_names(), vec!["C", "A", "B"]);

        // 把 C 放到 A 之后（当前顺序 C A B 中 A 的槽位 1，之后是 2）
        assert!(store.move_group_to("C", 2));
        assert_eq!(store.group_names(), vec!["A", "C", "B"]);

        // 落到自己原来的位置是空操作
        assert!(!store.move_group_to("C", 1));
    }

    #[test]
    fn move_group_to_end_uses_the_group_count() {
        let mut store = Store {
            groups: vec!["A".to_string(), "B".to_string(), "C".to_string()],
            servers: Vec::new(),
        };
        assert!(store.move_group_to("A", 3));
        assert_eq!(store.group_names(), vec!["B", "C", "A"]);
    }

    #[test]
    fn new_id_is_unique_within_a_tight_loop() {
        let ids: HashSet<String> = (0..10_000).map(|_| new_id()).collect();
        assert_eq!(ids.len(), 10_000);
    }
}
