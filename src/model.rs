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
}

pub fn new_id() -> String {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("{:x}{:04x}", millis, rand::random::<u16>())
}
