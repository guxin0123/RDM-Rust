use serde::{Deserialize, Serialize};

pub const DEFAULT_GROUP: &str = "默认";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerEntry {
    pub id: String,
    pub group: String,
    pub name: String,
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

    /// 把服务器移动到目标分组的指定位置（组内序号，0 表示该组第一个）。
    ///
    /// 列表按 `servers` 的顺序渲染、按 `group` 字段归属分组，
    /// 所以只需在 Vec 中重排，即可同时实现「换组」与「组内排序」。
    pub fn move_server(&mut self, id: &str, target_group: &str, index_in_group: usize) -> bool {
        let Some(pos) = self.servers.iter().position(|s| s.id == id) else {
            return false;
        };
        let old_group = self.servers[pos].group.clone();
        let old_index = self.servers[..pos]
            .iter()
            .filter(|s| s.group == old_group)
            .count();

        let mut entry = self.servers.remove(pos);
        entry.group = target_group.to_string();

        // 目标序号基于「移除拖动项之后」的序列：同组且原位置在前时需要回退一位
        let index = if old_group == target_group && old_index < index_in_group {
            index_in_group - 1
        } else {
            index_in_group
        };

        let insert_at = self.insert_index_of(target_group, index);
        self.servers.insert(insert_at, entry);
        true
    }

    /// 把分组移动到指定序号（0 表示第一个）。隐式分组会一并固化进 `groups`。
    pub fn move_group(&mut self, name: &str, index: usize) -> bool {
        let mut names = self.group_names();
        let Some(pos) = names.iter().position(|g| g == name) else {
            return false;
        };
        names.remove(pos);
        // 移除后，原位置之后的元素整体前移一位
        let index = if pos < index { index - 1 } else { index };
        names.insert(index.min(names.len()), name.to_string());
        self.groups = names;
        true
    }

    /// 目标分组中第 index 个服务器在 `servers` 中的位置；不存在则插到该组末尾之后
    fn insert_index_of(&self, group: &str, index: usize) -> usize {
        let mut seen = 0;
        for (i, s) in self.servers.iter().enumerate() {
            if s.group == group {
                if seen == index {
                    return i;
                }
                seen += 1;
            }
        }
        self.servers
            .iter()
            .rposition(|s| s.group == group)
            .map(|i| i + 1)
            .unwrap_or(self.servers.len())
    }
}

pub fn new_id() -> String {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("{:x}{:04x}", millis, rand::random::<u16>())
}
