use anyhow::{anyhow, Result};
use base64::{engine::general_purpose::STANDARD as B64, Engine as _};

/// Windows DPAPI 加密，返回 base64 密文。密钥由系统按当前用户管理。
#[cfg(windows)]
pub fn dpapi_encrypt(plain: &str) -> Result<String> {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{LocalFree, HLOCAL};
    use windows::Win32::Security::Cryptography::{CryptProtectData, CRYPT_INTEGER_BLOB};

    let data = plain.as_bytes();
    unsafe {
        let mut input = CRYPT_INTEGER_BLOB {
            cbData: data.len() as u32,
            pbData: data.as_ptr() as *mut u8,
        };
        let mut output = CRYPT_INTEGER_BLOB::default();
        CryptProtectData(&mut input, PCWSTR::null(), None, None, None, 0, &mut output)?;
        let bytes = std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
        let _ = LocalFree(HLOCAL(output.pbData as *mut _));
        Ok(B64.encode(bytes))
    }
}

/// Windows DPAPI 解密 base64 密文。
#[cfg(windows)]
pub fn dpapi_decrypt(b64: &str) -> Result<String> {
    use windows::Win32::Foundation::{LocalFree, HLOCAL};
    use windows::Win32::Security::Cryptography::{CryptUnprotectData, CRYPT_INTEGER_BLOB};

    let data = B64.decode(b64.trim())?;
    unsafe {
        let mut input = CRYPT_INTEGER_BLOB {
            cbData: data.len() as u32,
            pbData: data.as_ptr() as *mut u8,
        };
        let mut output = CRYPT_INTEGER_BLOB::default();
        CryptUnprotectData(&mut input, None, None, None, None, 0, &mut output)?;
        let bytes = std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
        let _ = LocalFree(HLOCAL(output.pbData as *mut _));
        Ok(String::from_utf8(bytes)?)
    }
}

#[cfg(not(windows))]
pub fn dpapi_encrypt(_: &str) -> Result<String> {
    Err(anyhow!("DPAPI 仅在 Windows 上可用"))
}

#[cfg(not(windows))]
pub fn dpapi_decrypt(_: &str) -> Result<String> {
    Err(anyhow!("DPAPI 仅在 Windows 上可用"))
}

/// 用导出密码派生密钥并加密，返回 base64(nonce || ciphertext)
pub fn export_encrypt(plain: &str, password: &str, salt: &[u8]) -> Result<String> {
    use aes_gcm::aead::{Aead, KeyInit};
    use aes_gcm::{Aes256Gcm, Key, Nonce};

    let key = derive_key(password, salt)?;
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key));
    let nonce_bytes: [u8; 12] = rand::random();
    let ct = cipher
        .encrypt(Nonce::from_slice(&nonce_bytes), plain.as_bytes())
        .map_err(|e| anyhow!("加密失败: {e}"))?;
    let mut payload = nonce_bytes.to_vec();
    payload.extend_from_slice(&ct);
    Ok(B64.encode(payload))
}

pub fn export_decrypt(payload_b64: &str, password: &str, salt: &[u8]) -> Result<String> {
    use aes_gcm::aead::{Aead, KeyInit};
    use aes_gcm::{Aes256Gcm, Key, Nonce};

    let payload = B64.decode(payload_b64.trim())?;
    if payload.len() < 12 {
        return Err(anyhow!("密文长度无效"));
    }
    let key = derive_key(password, salt)?;
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key));
    let pt = cipher
        .decrypt(Nonce::from_slice(&payload[..12]), &payload[12..])
        .map_err(|_| anyhow!("解密失败（导出密码错误？）"))?;
    Ok(String::from_utf8(pt)?)
}

pub fn random_salt() -> [u8; 16] {
    rand::random()
}

fn derive_key(password: &str, salt: &[u8]) -> Result<[u8; 32]> {
    let mut key = [0u8; 32];
    argon2::Argon2::default()
        .hash_password_into(password.as_bytes(), salt, &mut key)
        .map_err(|e| anyhow!("密钥派生失败: {e}"))?;
    Ok(key)
}
