//! 凭据安全存储（Vault）
//!
//! 把 API key、密码等敏感值经 Windows DPAPI 加密后落库（base64 密文），
//! 避免明文写进配置文件。非 Windows 平台降级为明文存储（仅编译兼容，运行时会告警）。

use std::sync::Arc;

use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use serde_json::{json, Value};

use crate::memory::MemoryStore;
use crate::tools::{PermissionClass, Tool};

// ───────────────── DPAPI ─────────────────

#[cfg(windows)]
mod dpapi {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::HLOCAL;
    use windows::Win32::Security::Cryptography::{
        CryptProtectData, CryptUnprotectData, CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN,
    };
    use windows::Win32::System::Memory::LocalFree;

    fn copy_blob(blob: &CRYPT_INTEGER_BLOB) -> Vec<u8> {
        if blob.pbData.is_null() || blob.cbData == 0 {
            return Vec::new();
        }
        let slice = unsafe { std::slice::from_raw_parts(blob.pbData, blob.cbData as usize) };
        slice.to_vec()
    }

    fn blob_from(data: &[u8]) -> CRYPT_INTEGER_BLOB {
        CRYPT_INTEGER_BLOB {
            cbData: data.len() as u32,
            pbData: data.as_ptr() as *mut u8,
        }
    }

    /// DPAPI 加密（entropy 为附加熵，绑定 key 名，防挪库后解密）
    pub fn protect(data: &[u8], entropy: &[u8]) -> Result<Vec<u8>, String> {
        let input = blob_from(data);
        let ent = blob_from(entropy);
        let mut output: CRYPT_INTEGER_BLOB = unsafe { std::mem::zeroed() };

        let ok = unsafe {
            CryptProtectData(
                &input,
                PCWSTR::null(),
                if entropy.is_empty() { None } else { Some(&ent) },
                None,
                None,
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        };
        if !ok.as_bool() {
            return Err("DPAPI 加密失败".to_string());
        }
        let result = copy_blob(&output);
        unsafe {
            let _ = LocalFree(HLOCAL(output.pbData as isize));
        }
        if result.is_empty() {
            return Err("DPAPI 加密结果为空".to_string());
        }
        Ok(result)
    }

    /// DPAPI 解密
    pub fn unprotect(data: &[u8], entropy: &[u8]) -> Result<Vec<u8>, String> {
        let input = blob_from(data);
        let ent = blob_from(entropy);
        let mut output: CRYPT_INTEGER_BLOB = unsafe { std::mem::zeroed() };

        let ok = unsafe {
            CryptUnprotectData(
                &input,
                None,
                if entropy.is_empty() { None } else { Some(&ent) },
                None,
                None,
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        };
        if !ok.as_bool() {
            return Err("DPAPI 解密失败（数据可能被其他用户/机器加密，或已损坏）".to_string());
        }
        let result = copy_blob(&output);
        unsafe {
            let _ = LocalFree(HLOCAL(output.pbData as isize));
        }
        Ok(result)
    }
}

#[cfg(not(windows))]
mod dpapi {
    pub fn protect(data: &[u8], _entropy: &[u8]) -> Result<Vec<u8>, String> {
        Ok(data.to_vec())
    }
    pub fn unprotect(data: &[u8], _entropy: &[u8]) -> Result<Vec<u8>, String> {
        Ok(data.to_vec())
    }
}

// ───────────────── 加解密辅助 ─────────────────

fn encrypt(key: &str, value: &str) -> Result<String, String> {
    let cipher = dpapi::protect(value.as_bytes(), key.as_bytes())?;
    Ok(STANDARD.encode(&cipher))
}

fn decrypt(key: &str, stored: &str) -> Result<String, String> {
    let cipher = STANDARD
        .decode(stored)
        .map_err(|e| format!("Vault 数据损坏（base64 解码失败）: {e}"))?;
    let plain = dpapi::unprotect(&cipher, key.as_bytes())?;
    String::from_utf8(plain).map_err(|e| format!("Vault 存储值非 UTF-8 文本: {e}"))
}

/// 加密一条敏感值（模型 API Key 等走此路径落库），返回 base64 密文。
pub(crate) fn seal(key: &str, value: &str) -> Result<String, String> {
    encrypt(key, value)
}

/// 解密 vault 中的密文，返回明文。
pub(crate) fn open(key: &str, stored: &str) -> Result<String, String> {
    decrypt(key, stored)
}

// ───────────────── 工具 ─────────────────

pub struct VaultSetTool {
    store: Arc<MemoryStore>,
}

impl VaultSetTool {
    pub fn new(store: Arc<MemoryStore>) -> Self {
        Self { store }
    }
}

impl Tool for VaultSetTool {
    fn name(&self) -> &str {
        "vault_set"
    }
    fn description(&self) -> &str {
        "安全存储一条敏感凭据（API key、密码等），经 Windows DPAPI 加密后落库，不落明文。key 为凭据名称，value 为明文值"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "key": { "type": "string", "description": "凭据名称（如 openai_api_key）" },
                "value": { "type": "string", "description": "凭据明文值" }
            },
            "required": ["key", "value"]
        })
    }
    fn permission(&self) -> PermissionClass {
        // 写入凭据会改动凭据库（可能覆盖已有密钥），需要用户确认
        PermissionClass::HighRisk
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let key = args["key"].as_str().ok_or("缺少参数 key")?.trim();
        let value = args["value"].as_str().ok_or("缺少参数 value")?;
        if key.is_empty() {
            return Err("key 不能为空".into());
        }
        if value.is_empty() {
            return Err("value 不能为空".into());
        }
        let cipher = encrypt(key, value)?;
        self.store.vault_set(key, &cipher)?;
        #[cfg(not(windows))]
        {
            eprintln!("[Vault] 警告：非 Windows 平台，凭据以明文存储");
        }
        Ok(json!({ "ok": true, "key": key }))
    }
}

/// 供其他模块按名读取解密后的凭据（如邮件工具取 SMTP/IMAP 配置）。
/// 注意：返回明文，调用方不得把明文写进日志/审计。
pub fn get_plain(store: &MemoryStore, key: &str) -> Result<String, String> {
    let stored = store
        .vault_get(key)?
        .ok_or_else(|| format!("凭据不存在: {key}"))?;
    decrypt(key, &stored)
}

pub struct VaultGetTool {
    store: Arc<MemoryStore>,
}

impl VaultGetTool {
    pub fn new(store: Arc<MemoryStore>) -> Self {
        Self { store }
    }
}

impl Tool for VaultGetTool {
    fn name(&self) -> &str {
        "vault_get"
    }
    fn description(&self) -> &str {
        "读取已存储的敏感凭据（DPAPI 解密后返回明文，供调用外部 API 时使用）"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "key": { "type": "string", "description": "凭据名称" }
            },
            "required": ["key"]
        })
    }
    fn permission(&self) -> PermissionClass {
        // 返回的是解密后的明文密钥：泄露面大，每次读取都必须经用户确认
        PermissionClass::HighRisk
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let key = args["key"].as_str().ok_or("缺少参数 key")?.trim();
        let stored = self
            .store
            .vault_get(key)?
            .ok_or_else(|| format!("凭据不存在: {key}"))?;
        let plain = decrypt(key, &stored)?;
        Ok(json!({ "key": key, "value": plain }))
    }
}

pub struct VaultListTool {
    store: Arc<MemoryStore>,
}

impl VaultListTool {
    pub fn new(store: Arc<MemoryStore>) -> Self {
        Self { store }
    }
}

impl Tool for VaultListTool {
    fn name(&self) -> &str {
        "vault_list"
    }
    fn description(&self) -> &str {
        "列出已存储的凭据名称（不含明文值）"
    }
    fn schema(&self) -> Value {
        json!({ "type": "object", "properties": {} })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::ReadOnly
    }
    fn run(&self, _args: Value) -> Result<Value, String> {
        let keys = self.store.vault_list()?;
        Ok(json!({ "keys": keys }))
    }
}

pub struct VaultDeleteTool {
    store: Arc<MemoryStore>,
}

impl VaultDeleteTool {
    pub fn new(store: Arc<MemoryStore>) -> Self {
        Self { store }
    }
}

impl Tool for VaultDeleteTool {
    fn name(&self) -> &str {
        "vault_delete"
    }
    fn description(&self) -> &str {
        "删除一条已存储的凭据（用于轮换或清理）"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "key": { "type": "string", "description": "凭据名称" }
            },
            "required": ["key"]
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::Write
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let key = args["key"].as_str().ok_or("缺少参数 key")?.trim();
        let removed = self.store.vault_delete(key)?;
        Ok(json!({ "ok": true, "key": key, "removed": removed }))
    }
}