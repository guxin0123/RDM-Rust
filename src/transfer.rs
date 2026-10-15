use anyhow::{anyhow, Result};
use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use serde::{Deserialize, Serialize};

use crate::crypto;
use crate::model::{new_id, ServerEntry, Store};

pub const FORMAT: &str = "rdm-rust-export";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportServer {
    pub group: String,
    pub name: String,
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default)]
    pub username: String,
    /// encrypted=false 时为空；encrypted=true 时为 base64(nonce || AES-GCM 密文)
    #[serde(default)]
    pub password: String,
}

fn default_port() -> u16 {
    3389
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportFile {
    pub format: String,
    pub version: u32,
    /// 密码字段是否用导出密码加密
    pub encrypted: bool,
    /// argon2 盐（base64），encrypted=true 时存在
    #[serde(default)]
    pub salt: Option<String>,
    #[serde(default)]
    pub groups: Vec<String>,
    #[serde(default)]
    pub servers: Vec<ExportServer>,
}

/// 构建导出文件。export_password 为空则不导出密码；否则用其加密所有密码字段。
pub fn build_export(store: &Store, export_password: &str) -> Result<ExportFile> {
    let encrypted = !export_password.is_empty();
    let salt = if encrypted {
        Some(crypto::random_salt())
    } else {
        None
    };

    let mut servers = Vec::with_capacity(store.servers.len());
    for s in &store.servers {
        let password = match (&salt, s.password_enc.is_empty()) {
            (Some(salt), false) => {
                let plain = crypto::dpapi_decrypt(&s.password_enc)
                    .map_err(|e| anyhow!("解密服务器「{}」密码失败: {e}", s.name))?;
                crypto::export_encrypt(&plain, export_password, salt)?
            }
            _ => String::new(),
        };
        servers.push(ExportServer {
            group: s.group.clone(),
            name: s.name.clone(),
            host: s.host.clone(),
            port: s.port,
            username: s.username.clone(),
            password,
        });
    }

    Ok(ExportFile {
        format: FORMAT.to_string(),
        version: 1,
        encrypted,
        salt: salt.map(|s| B64.encode(s)),
        groups: store.groups.clone(),
        servers,
    })
}

pub fn parse_export(json: &str) -> Result<ExportFile> {
    let file: ExportFile = serde_json::from_str(json)?;
    if file.format != FORMAT {
        return Err(anyhow!("不是有效的 RDM Rust 导出文件"));
    }
    if file.version > 1 {
        return Err(anyhow!("导出文件版本过高，无法导入"));
    }
    if file.encrypted && file.salt.is_none() {
        return Err(anyhow!("导出文件缺少加密盐"));
    }
    Ok(file)
}

/// 解密导出文件中的密码字段（encrypted=false 时返回全空）。
/// 返回与 file.servers 对齐的明文密码列表。
pub fn decrypt_passwords(file: &ExportFile, export_password: &str) -> Result<Vec<String>> {
    if !file.encrypted {
        return Ok(vec![String::new(); file.servers.len()]);
    }
    let salt_b64 = file.salt.as_ref().ok_or_else(|| anyhow!("缺少加密盐"))?;
    let salt = B64.decode(salt_b64)?;
    file.servers
        .iter()
        .map(|s| {
            if s.password.is_empty() {
                Ok(String::new())
            } else {
                crypto::export_decrypt(&s.password, export_password, &salt)
            }
        })
        .collect()
}

/// 将导出内容合并进 store。overwrite=true 时覆盖重名服务器，否则跳过。
/// 返回 (新增数, 覆盖数, 跳过数)。
pub fn apply_import(
    store: &mut Store,
    file: &ExportFile,
    passwords: &[String],
    overwrite: bool,
) -> Result<(usize, usize, usize)> {
    for g in &file.groups {
        store.add_group(g);
    }
    for s in &file.servers {
        store.add_group(&s.group);
    }
    store.ensure_default_group();

    let mut added = 0;
    let mut overwritten = 0;
    let mut skipped = 0;

    for (s, password) in file.servers.iter().zip(passwords.iter()) {
        let password_enc = if password.is_empty() {
            String::new()
        } else {
            crypto::dpapi_encrypt(password)?
        };
        let existing = store
            .servers
            .iter()
            .position(|x| x.name == s.name)
            .map(|i| store.servers[i].id.clone());
        match (existing, overwrite) {
            (Some(id), true) => {
                store.upsert_server(ServerEntry {
                    id,
                    group: s.group.clone(),
                    name: s.name.clone(),
                    host: s.host.clone(),
                    port: s.port,
                    username: s.username.clone(),
                    password_enc,
                });
                overwritten += 1;
            }
            (Some(_), false) => skipped += 1,
            (None, _) => {
                store.upsert_server(ServerEntry {
                    id: new_id(),
                    group: s.group.clone(),
                    name: s.name.clone(),
                    host: s.host.clone(),
                    port: s.port,
                    username: s.username.clone(),
                    password_enc,
                });
                added += 1;
            }
        }
    }
    Ok((added, overwritten, skipped))
}

/// 计算导入数据中与现有列表重名的服务器数量
pub fn count_conflicts(store: &Store, file: &ExportFile) -> usize {
    file.servers
        .iter()
        .filter(|s| store.servers.iter().any(|x| x.name == s.name))
        .count()
}
