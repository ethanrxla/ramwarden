//! The unit of change: one setting, with a way back.

use ramwarden_platform::Result;
use serde::Serialize;
use serde_json::Value;

/// Whose settings a tweak touches, which decides who may apply it.
///
/// User tweaks live in the user's own registry hive and must run as the user:
/// the service runs as SYSTEM, whose `HKCU` is not the user's. Machine tweaks
/// need administrator rights and go through `ramwarden-service`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    User,
    Machine,
    /// The user's own hive, but a part only administrators may write:
    /// `HKCU\Software\Policies`. Applied by the service (or an elevated
    /// caller) into `HKEY_USERS\<the user's SID>`.
    UserPolicy,
}

/// What a tweak finds on this machine right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    /// The setting is as Windows (or the app) left it.
    Default,
    /// The setting is as the tweak would make it.
    Applied,
    /// The thing the tweak targets does not exist here — a service not
    /// installed, an app with no startup entry. Nothing to do, nothing wrong.
    Missing,
}

/// One reversible setting.
///
/// The contract the engine relies on: `snapshot` captures everything `revert`
/// needs, and is taken *before* `apply` runs; `revert(snapshot)` restores the
/// machine to exactly what `snapshot` saw, including "this value did not
/// exist".
pub trait Tweak: Send + Sync {
    fn id(&self) -> &str;
    fn description(&self) -> &str;
    fn scope(&self) -> Scope;
    /// Not applied by "apply profile"; only when named explicitly.
    fn optional(&self) -> bool;
    /// What it changes, for display: `service DiagTrack -> disabled`.
    fn target(&self) -> String;

    /// For a [`Scope::UserPolicy`] tweak: the same change bound to one user's
    /// hive under `HKEY_USERS`, with its own journal id. `None` otherwise.
    fn for_user(&self, _sid: &str) -> Option<Box<dyn Tweak>> {
        None
    }

    fn detect(&self) -> Result<State>;
    fn snapshot(&self) -> Result<Value>;
    fn apply(&self) -> Result<()>;
    fn revert(&self, snapshot: &Value) -> Result<()>;
}

impl std::fmt::Debug for dyn Tweak {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Tweak({} — {})", self.id(), self.target())
    }
}
