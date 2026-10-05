//! Local model runtimes, and the VRAM their loaded models hold.
//!
//! A model left loaded in LM Studio or Ollama holds gigabytes of VRAM (and,
//! for LM Studio, RAM too) while nobody is using it. RamWarden cannot shrink a
//! loaded model, but both runtimes can *unload* one on request, cleanly: the
//! next request just loads it again. That makes it the one VRAM lever that
//! is safe to pull.

use std::path::PathBuf;
use std::time::Duration;

use serde::Serialize;
use serde_json::Value;

use crate::client::{Error, Ollama, Result};

/// One model resident in a runtime.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct LoadedModel {
    /// `"ollama"` or `"lmstudio"`.
    pub host: &'static str,
    /// The identifier the runtime uses to unload it.
    pub name: String,
    /// VRAM (Ollama) or total size (LM Studio), when reported.
    pub bytes: Option<u64>,
}

/// LM Studio, through its own `lms` CLI.
///
/// The CLI is used rather than the HTTP API because the API server is off by
/// default, while the CLI talks to the running app directly.
#[derive(Clone, Debug)]
pub struct LmStudio {
    lms: PathBuf,
}

impl LmStudio {
    /// The CLI LM Studio installs into the user's profile, if present.
    pub fn find() -> Option<Self> {
        let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))?;
        let exe = if cfg!(windows) { "lms.exe" } else { "lms" };
        let lms = PathBuf::from(home).join(".lmstudio").join("bin").join(exe);
        lms.exists().then_some(LmStudio { lms })
    }

    async fn run(&self, args: &[&str]) -> Result<String> {
        let mut cmd = tokio::process::Command::new(&self.lms);
        cmd.args(args).kill_on_drop(true);
        #[cfg(windows)]
        {
            // CREATE_NO_WINDOW: no console flash from a background daemon.
            cmd.creation_flags(0x0800_0000);
        }
        let out = tokio::time::timeout(Duration::from_secs(20), cmd.output())
            .await
            .map_err(|_| Error::Transport("lms timed out".into()))?
            .map_err(|e| Error::Transport(format!("lms: {e}")))?;
        if !out.status.success() {
            return Err(Error::Reply(String::from_utf8_lossy(&out.stderr).trim().to_string()));
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    pub async fn loaded(&self) -> Result<Vec<LoadedModel>> {
        parse_lms_ps(&self.run(&["ps", "--json"]).await?)
    }

    pub async fn unload(&self, identifier: &str) -> Result<()> {
        self.run(&["unload", identifier]).await.map(|_| ())
    }
}

/// `lms ps --json`: an array of loaded models. Tolerant of field names, since
/// the CLI's JSON shape is not a published contract.
pub fn parse_lms_ps(text: &str) -> Result<Vec<LoadedModel>> {
    let v: Value = serde_json::from_str(text.trim()).map_err(|e| Error::Reply(format!("lms ps: {e}")))?;
    let Some(items) = v.as_array() else {
        return Err(Error::Reply("lms ps: expected a JSON array".into()));
    };
    Ok(items
        .iter()
        .filter_map(|m| {
            let name = ["identifier", "modelKey", "path"]
                .iter()
                .find_map(|k| m.get(*k).and_then(Value::as_str))?
                .to_string();
            let bytes = ["sizeBytes", "size"].iter().find_map(|k| m.get(*k).and_then(Value::as_u64));
            Some(LoadedModel {
                host: "lmstudio",
                name,
                bytes,
            })
        })
        .collect())
}

/// Every local runtime RamWarden knows how to ask.
#[derive(Clone, Debug, Default)]
pub struct ModelHosts {
    pub ollama: Option<Ollama>,
    pub lmstudio: Option<LmStudio>,
}

impl ModelHosts {
    pub fn new(ollama: Option<Ollama>) -> Self {
        ModelHosts {
            ollama,
            lmstudio: LmStudio::find(),
        }
    }

    /// Everything loaded, across runtimes. A runtime that is not running
    /// contributes nothing rather than an error.
    pub async fn loaded(&self) -> Vec<LoadedModel> {
        let mut out = Vec::new();
        if let Some(o) = &self.ollama
            && let Ok(m) = o.loaded().await
        {
            out.extend(m);
        }
        if let Some(l) = &self.lmstudio
            && let Ok(m) = l.loaded().await
        {
            out.extend(m);
        }
        out
    }

    pub async fn unload(&self, model: &LoadedModel) -> Result<()> {
        match model.host {
            "ollama" => match &self.ollama {
                Some(o) => o.unload(&model.name).await,
                None => Err(Error::Transport("ollama is not configured".into())),
            },
            "lmstudio" => match &self.lmstudio {
                Some(l) => l.unload(&model.name).await,
                None => Err(Error::Transport("lms not found".into())),
            },
            other => Err(Error::Reply(format!("unknown model host {other}"))),
        }
    }

    /// Unload every loaded model; reports each by name.
    pub async fn unload_all(&self) -> Vec<(LoadedModel, std::result::Result<(), String>)> {
        let mut out = Vec::new();
        for m in self.loaded().await {
            let r = self.unload(&m).await.map_err(|e| e.to_string());
            out.push((m, r));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lms_ps_parses_whatever_identifier_field_it_finds() {
        assert!(parse_lms_ps("[]").unwrap().is_empty());
        let m = parse_lms_ps(r#"[{"identifier":"qwen3-8b","sizeBytes":5000000000},{"path":"x/y.gguf"}]"#).unwrap();
        assert_eq!(m.len(), 2);
        assert_eq!(m[0].name, "qwen3-8b");
        assert_eq!(m[0].bytes, Some(5_000_000_000));
        assert_eq!(m[1].name, "x/y.gguf");
        assert!(parse_lms_ps("not json").is_err());
    }
}
