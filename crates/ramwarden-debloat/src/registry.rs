//! A registry value set to a fixed DWORD or string.

use ramwarden_platform::{Error, Result};
use serde_json::{Value, json};
use winreg::enums::{
    HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, KEY_SET_VALUE, KEY_WOW64_64KEY, REG_BINARY,
    REG_DWORD, REG_EXPAND_SZ, REG_MULTI_SZ, REG_QWORD, REG_SZ, RegType,
};
use winreg::{RegKey, RegValue};

use crate::tweak::{Scope, State, Tweak};

/// The data a registry tweak writes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Data {
    Dword(u32),
    String(String),
}

impl Data {
    fn to_reg(&self) -> RegValue<'static> {
        match self {
            Data::Dword(v) => RegValue {
                bytes: v.to_le_bytes().to_vec().into(),
                vtype: REG_DWORD,
            },
            Data::String(s) => {
                let mut wide: Vec<u8> = s.encode_utf16().chain([0]).flat_map(u16::to_le_bytes).collect();
                wide.shrink_to_fit();
                RegValue {
                    bytes: wide.into(),
                    vtype: REG_SZ,
                }
            }
        }
    }
}

/// `HKLM\...` or `HKCU\...`, split into hive and subkey.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyPath {
    pub machine: bool,
    pub subkey: String,
}

impl KeyPath {
    pub fn parse(path: &str) -> Result<Self> {
        let (hive, rest) = path
            .split_once('\\')
            .ok_or_else(|| Error::os(format!("registry path without a hive: {path}")))?;
        let machine = match hive.to_ascii_uppercase().as_str() {
            "HKLM" | "HKEY_LOCAL_MACHINE" => true,
            "HKCU" | "HKEY_CURRENT_USER" => false,
            other => return Err(Error::os(format!("unsupported hive {other} in {path}"))),
        };
        Ok(KeyPath {
            machine,
            subkey: rest.trim_matches('\\').to_string(),
        })
    }

    fn hive(&self) -> RegKey {
        RegKey::predef(if self.machine { HKEY_LOCAL_MACHINE } else { HKEY_CURRENT_USER })
    }

    pub fn display(&self) -> String {
        format!("{}\\{}", if self.machine { "HKLM" } else { "HKCU" }, self.subkey)
    }

    fn open(&self, write: bool) -> std::io::Result<RegKey> {
        let flags = KEY_READ | KEY_WOW64_64KEY | if write { KEY_SET_VALUE } else { 0 };
        self.hive().open_subkey_with_flags(&self.subkey, flags)
    }

    fn create(&self) -> std::io::Result<RegKey> {
        self.hive().create_subkey(&self.subkey).map(|(k, _)| k)
    }
}

pub(crate) fn io(what: &str, e: std::io::Error) -> Error {
    match e.kind() {
        std::io::ErrorKind::PermissionDenied => Error::denied(what, "needs administrator — use ramwarden-service"),
        std::io::ErrorKind::NotFound => Error::missing(what),
        _ => Error::os(format!("{what}: {e}")),
    }
}

/// Encode a registry type for a snapshot.
pub(crate) fn vtype_code(t: &RegType) -> u32 {
    match t {
        REG_SZ => 1,
        REG_EXPAND_SZ => 2,
        REG_BINARY => 3,
        REG_DWORD => 4,
        REG_MULTI_SZ => 7,
        REG_QWORD => 11,
        _ => 3,
    }
}

pub(crate) fn vtype_from(code: u64) -> RegType {
    match code {
        1 => REG_SZ,
        2 => REG_EXPAND_SZ,
        4 => REG_DWORD,
        7 => REG_MULTI_SZ,
        11 => REG_QWORD,
        _ => REG_BINARY,
    }
}

/// Snapshot of one value: whether the key and value existed, and the raw
/// bytes and type if so.
pub(crate) fn snapshot_value(key: &KeyPath, name: &str) -> Value {
    match key.open(false) {
        Err(_) => json!({"key_existed": false, "exists": false}),
        Ok(k) => match k.get_raw_value(name) {
            Ok(v) => json!({
                "key_existed": true,
                "exists": true,
                "vtype": vtype_code(&v.vtype),
                "bytes": v.bytes.to_vec(),
            }),
            Err(_) => json!({"key_existed": true, "exists": false}),
        },
    }
}

/// Put one value back exactly as a snapshot recorded it.
pub(crate) fn restore_value(key: &KeyPath, name: &str, snap: &Value) -> Result<()> {
    let what = format!("{}\\{name}", key.display());
    if snap["exists"].as_bool() == Some(true) {
        let bytes: Vec<u8> = serde_json::from_value(snap["bytes"].clone())
            .map_err(|e| Error::os(format!("corrupt snapshot for {what}: {e}")))?;
        let value = RegValue {
            bytes: bytes.into(),
            vtype: vtype_from(snap["vtype"].as_u64().unwrap_or(3)),
        };
        return key
            .create()
            .and_then(|k| k.set_raw_value(name, &value))
            .map_err(|e| io(&what, e));
    }

    // It did not exist: remove what we added.
    match key.open(true) {
        Err(_) => return Ok(()),
        Ok(k) => match k.delete_value(name) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(io(&what, e)),
        },
    }
    // And the key, if we created it and nothing else has since used it.
    if snap["key_existed"].as_bool() == Some(false)
        && let Ok(k) = key.open(false)
        && let Ok(info) = k.query_info()
        && info.values == 0
        && info.sub_keys == 0
    {
        drop(k);
        let _ = key.hive().delete_subkey(&key.subkey);
    }
    Ok(())
}

/// Sets `value` under `key` to fixed data.
pub struct RegistryTweak {
    pub id: String,
    pub description: String,
    pub optional: bool,
    pub key: KeyPath,
    pub value: String,
    pub data: Data,
}

impl Tweak for RegistryTweak {
    fn id(&self) -> &str {
        &self.id
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn scope(&self) -> Scope {
        if self.key.machine { Scope::Machine } else { Scope::User }
    }

    fn optional(&self) -> bool {
        self.optional
    }

    fn target(&self) -> String {
        let data = match &self.data {
            Data::Dword(v) => v.to_string(),
            Data::String(s) => format!("{s:?}"),
        };
        format!("{}\\{} = {data}", self.key.display(), self.value)
    }

    fn detect(&self) -> Result<State> {
        let want = self.data.to_reg();
        Ok(match self.key.open(false).and_then(|k| k.get_raw_value(&self.value)) {
            Ok(v) if v.vtype == want.vtype && v.bytes == want.bytes => State::Applied,
            _ => State::Default,
        })
    }

    fn snapshot(&self) -> Result<Value> {
        Ok(snapshot_value(&self.key, &self.value))
    }

    fn apply(&self) -> Result<()> {
        let what = format!("{}\\{}", self.key.display(), self.value);
        self.key
            .create()
            .and_then(|k| k.set_raw_value(&self.value, &self.data.to_reg()))
            .map_err(|e| io(&what, e))
    }

    fn revert(&self, snapshot: &Value) -> Result<()> {
        restore_value(&self.key, &self.value, snapshot)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch key under HKCU, removed afterwards.
    fn scratch() -> String {
        format!(
            r"HKCU\Software\RamWardenTest\{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        )
    }

    fn tweak(path: &str, data: Data) -> RegistryTweak {
        RegistryTweak {
            id: "test".into(),
            description: "test".into(),
            optional: false,
            key: KeyPath::parse(path).unwrap(),
            value: "Value".into(),
            data,
        }
    }

    #[test]
    fn paths_parse_and_scope_follows_the_hive() {
        let k = KeyPath::parse(r"HKLM\SOFTWARE\Policies\Microsoft\Windows\DataCollection").unwrap();
        assert!(k.machine);
        assert_eq!(k.subkey, r"SOFTWARE\Policies\Microsoft\Windows\DataCollection");
        assert!(!KeyPath::parse(r"HKCU\Software\X").unwrap().machine);
        assert!(KeyPath::parse(r"HKCR\x").is_err());
        assert!(KeyPath::parse("nohive").is_err());
    }

    /// The revert guarantee for the common case: the key did not exist, so
    /// after revert it does not exist again.
    #[test]
    fn apply_then_revert_leaves_no_trace_when_nothing_existed() {
        let path = scratch();
        let t = tweak(&path, Data::Dword(0));
        assert_eq!(t.detect().unwrap(), State::Default);
        let snap = t.snapshot().unwrap();
        t.apply().unwrap();
        assert_eq!(t.detect().unwrap(), State::Applied);
        t.revert(&snap).unwrap();
        assert_eq!(t.detect().unwrap(), State::Default);
        assert!(t.key.open(false).is_err(), "the key we created must be gone");
    }

    #[test]
    fn revert_restores_a_previous_value_byte_for_byte() {
        let path = scratch();
        let before = tweak(&path, Data::String("original".into()));
        before.apply().unwrap();

        let t = tweak(&path, Data::Dword(1));
        let snap = t.snapshot().unwrap();
        t.apply().unwrap();
        assert_eq!(t.detect().unwrap(), State::Applied);
        t.revert(&snap).unwrap();
        assert_eq!(before.detect().unwrap(), State::Applied, "the string is back, type and all");
        let _ = RegKey::predef(HKEY_CURRENT_USER).delete_subkey_all(&t.key.subkey);
    }
}
