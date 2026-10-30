//! A scheduled task, enabled or disabled.
//!
//! Through the Task Scheduler COM API rather than `schtasks.exe`, whose status
//! column is localised and so cannot be parsed reliably.

use ramwarden_platform::{Error, Result};
use serde_json::{Value, json};
use windows::Win32::Foundation::{VARIANT_FALSE, VARIANT_TRUE};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
};
use windows::Win32::System::TaskScheduler::{IRegisteredTask, ITaskService, TaskScheduler};
use windows::Win32::System::Variant::VARIANT;
use windows::core::BSTR;

use crate::tweak::{Scope, State, Tweak};

/// Split `\Folder\Sub\Name` into (`\Folder\Sub`, `Name`).
pub fn split(path: &str) -> (String, String) {
    match path.rsplit_once('\\') {
        Some((folder, name)) if !folder.is_empty() => (folder.to_string(), name.to_string()),
        Some((_, name)) => ("\\".to_string(), name.to_string()),
        None => ("\\".to_string(), path.to_string()),
    }
}

fn open(path: &str) -> Result<IRegisteredTask> {
    let (folder, name) = split(path);
    // SAFETY: COM calls on interfaces from the system Task Scheduler.
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let service: ITaskService = CoCreateInstance(&TaskScheduler, None, CLSCTX_INPROC_SERVER)
            .map_err(|e| Error::os(format!("Task Scheduler: {e}")))?;
        let empty = VARIANT::default();
        service
            .Connect(&empty, &empty, &empty, &empty)
            .map_err(|e| Error::os(format!("Task Scheduler connect: {e}")))?;
        let f = service
            .GetFolder(&BSTR::from(folder.as_str()))
            .map_err(|_| Error::missing(format!("task folder {folder}")))?;
        f.GetTask(&BSTR::from(name.as_str()))
            .map_err(|_| Error::missing(format!("task {path}")))
    }
}

fn enabled(task: &IRegisteredTask) -> Result<bool> {
    // SAFETY: property read on a live interface.
    unsafe { task.Enabled() }
        .map(|v| v.as_bool())
        .map_err(|e| Error::os(format!("task state: {e}")))
}

fn set_enabled(path: &str, on: bool) -> Result<()> {
    let task = open(path)?;
    // SAFETY: property write on a live interface.
    unsafe { task.SetEnabled(if on { VARIANT_TRUE } else { VARIANT_FALSE }) }.map_err(|e| {
        if e.code().0 as u32 == 0x8007_0005 {
            Error::denied(format!("task {path}"), "needs administrator — use ramwarden-service")
        } else {
            Error::os(format!("task {path}: {e}"))
        }
    })
}

/// Disables a scheduled task.
pub struct TaskTweak {
    pub id: String,
    pub description: String,
    pub optional: bool,
    pub task: String,
}

impl Tweak for TaskTweak {
    fn id(&self) -> &str {
        &self.id
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn scope(&self) -> Scope {
        Scope::Machine
    }

    fn optional(&self) -> bool {
        self.optional
    }

    fn target(&self) -> String {
        format!("task {} -> disabled", self.task)
    }

    fn detect(&self) -> Result<State> {
        match open(&self.task) {
            Ok(t) => Ok(if enabled(&t)? { State::Default } else { State::Applied }),
            Err(e) if e.is_missing() => Ok(State::Missing),
            Err(e) => Err(e),
        }
    }

    fn snapshot(&self) -> Result<Value> {
        Ok(json!({"enabled": enabled(&open(&self.task)?)?}))
    }

    fn apply(&self) -> Result<()> {
        set_enabled(&self.task, false)
    }

    fn revert(&self, snapshot: &Value) -> Result<()> {
        set_enabled(&self.task, snapshot["enabled"].as_bool().unwrap_or(true))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_paths_split_into_folder_and_name() {
        assert_eq!(
            split(r"\Microsoft\Windows\Application Experience\Microsoft Compatibility Appraiser"),
            (r"\Microsoft\Windows\Application Experience".into(), "Microsoft Compatibility Appraiser".into())
        );
        assert_eq!(split(r"\Top"), (r"\".into(), "Top".into()));
    }

    #[test]
    fn detection_reads_the_live_scheduler() {
        let real = TaskTweak {
            id: "t".into(),
            description: "t".into(),
            optional: false,
            task: r"\Microsoft\Windows\Customer Experience Improvement Program\Consolidator".into(),
        };
        assert!(real.detect().is_ok());
        let missing = TaskTweak { task: r"\RamWarden\NoSuchTask".into(), ..real };
        assert_eq!(missing.detect().unwrap(), State::Missing);
    }
}
