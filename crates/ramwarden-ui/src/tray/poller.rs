//! Talks to the daemon off the UI thread.
//!
//! egui redraws from whatever [`Snapshot`] holds; every network call happens
//! on a tokio runtime owned by the [`Poller`], which asks egui to repaint when
//! something changes. The window therefore never stalls on the daemon, and a
//! daemon that stops answering shows up as a message rather than a frozen UI.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::mpsc;

use super::launch::DaemonLauncher;
use crate::client::{Client, DebloatState, Error, GpuReport, State, TidyState};

/// Everything the window shows, as last fetched.
#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub state: Option<State>,
    pub tidy: Option<TidyState>,
    pub gpu: Option<GpuReport>,
    pub debloat: Option<DebloatState>,
    /// Why the last refresh failed, if it did.
    pub error: Option<String>,
    /// What is being done about it: starting the daemon, or where to look.
    pub daemon: Option<String>,
    /// The outcome of the last user action, shown until the next one.
    pub message: Option<String>,
    /// An action is in flight.
    pub busy: bool,
}

/// What the user asked for.
#[derive(Clone, Debug)]
pub enum Command {
    Suspend { name: String, force: bool },
    Resume(String),
    Kill { name: String, force: bool },
    Reclaim(String),
    DebloatApply { name: String, include_optional: bool },
    DebloatRevert(String),
    GpuUnload(Option<String>),
}

pub struct Poller {
    shared: Arc<Mutex<Snapshot>>,
    tx: mpsc::UnboundedSender<Command>,
    _runtime: tokio::runtime::Runtime,
}

impl Poller {
    /// `full` polls everything the window shows; without it only `/state`
    /// is fetched, which is all the tray gauge needs.
    pub fn start(client: Client, full: bool, repaint: impl Fn() + Send + Sync + 'static) -> Self {
        let runtime = crate::runtime::network_runtime().expect("tokio runtime");
        let shared = Arc::new(Mutex::new(Snapshot::default()));
        let repaint = Arc::new(repaint);
        let client = Arc::new(client);
        let (tx, mut rx) = mpsc::unbounded_channel::<Command>();

        // Fast loop: memory and Tidy, every two seconds.
        {
            let (shared, client, repaint) = (shared.clone(), client.clone(), repaint.clone());
            runtime.spawn(async move {
                // The tray is first in line to start the daemon; the window
                // only does it when it runs without a tray.
                let mut launcher = DaemonLauncher::new(Duration::from_secs(if full { 20 } else { 6 }));
                loop {
                    let (state, tidy) = if full {
                        tokio::join!(client.state(), client.tidy())
                    } else {
                        (client.state().await, Err(crate::client::Error::Transport(String::new())))
                    };
                    {
                        let mut s = shared.lock().unwrap();
                        match state {
                            Ok(st) => {
                                s.state = Some(st);
                                s.error = None;
                                s.daemon = None;
                                launcher.connected();
                            }
                            Err(e) => {
                                s.daemon = Some(match &e {
                                    // Nothing listening, or it did not answer in time.
                                    Error::Transport(_) => launcher.unreachable(),
                                    // Something answered, but not with RamWarden's /state.
                                    _ => format!(
                                        "Something other than ramwarden-daemon is answering on {} \
                                         (an old v1 daemon, or a port forwarded from WSL). Find it with \
                                         `netstat -ano | findstr :{}` and stop it.",
                                        client.base(),
                                        client.base().rsplit(':').next().unwrap_or("7823"),
                                    ),
                                });
                                s.error = Some(format!("daemon at {}: {e}", client.base()));
                            }
                        }
                        if let Ok(t) = tidy {
                            s.tidy = Some(t);
                        }
                    }
                    repaint();
                    tokio::time::sleep(Duration::from_secs(if full { 2 } else { 5 })).await;
                }
            });
        }
        // Slow loop: the GPU and debloat status are costlier to compute.
        if full {
            let (shared, client, repaint) = (shared.clone(), client.clone(), repaint.clone());
            runtime.spawn(async move {
                loop {
                    Self::refresh_slow(&client, &shared).await;
                    repaint();
                    tokio::time::sleep(Duration::from_secs(20)).await;
                }
            });
        }
        // Commands, one at a time.
        {
            let (shared, client, repaint) = (shared.clone(), client.clone(), repaint.clone());
            runtime.spawn(async move {
                while let Some(cmd) = rx.recv().await {
                    shared.lock().unwrap().busy = true;
                    repaint();
                    let message = Self::run(&client, cmd).await;
                    Self::refresh_slow(&client, &shared).await;
                    let mut s = shared.lock().unwrap();
                    s.message = Some(message);
                    s.busy = false;
                    drop(s);
                    repaint();
                }
            });
        }

        Poller {
            shared,
            tx,
            _runtime: runtime,
        }
    }

    async fn refresh_slow(client: &Client, shared: &Mutex<Snapshot>) {
        let (gpu, debloat) = tokio::join!(client.gpu(), client.debloat());
        let mut s = shared.lock().unwrap();
        if let Ok(g) = gpu {
            s.gpu = Some(g);
        }
        if let Ok(d) = debloat {
            s.debloat = Some(d);
        }
    }

    /// Execute one command and describe what happened.
    async fn run(client: &Client, cmd: Command) -> String {
        match cmd {
            Command::Suspend { name, force } => match client.suspend(&name, force).await {
                Ok(r) => r.describe("suspend"),
                Err(e) => format!("suspend {name}: {e}"),
            },
            Command::Resume(name) => match client.resume(&name).await {
                Ok(r) => r.describe("resume"),
                Err(e) => format!("resume {name}: {e}"),
            },
            Command::Kill { name, force } => match client.kill(&name, force).await {
                Ok(r) => r.describe("kill"),
                Err(e) => format!("kill {name}: {e}"),
            },
            Command::Reclaim(scope) => match client.reclaim(&scope).await {
                Ok(v) => format!(
                    "reclaimed {} MB from {scope}",
                    v.get("freed_mb").and_then(|f| f.as_f64()).unwrap_or(0.0)
                ),
                Err(e) => format!("reclaim {scope}: {e}"),
            },
            Command::DebloatApply { name, include_optional } => {
                Self::report("apply", &name, client.debloat_apply(&name, include_optional).await)
            }
            Command::DebloatRevert(name) => Self::report("revert", &name, client.debloat_revert(&name).await),
            Command::GpuUnload(name) => match client.gpu_unload(name.as_deref()).await {
                Ok(v) => {
                    let n = v.get("results").and_then(|r| r.as_array()).map(Vec::len).unwrap_or(0);
                    if n == 0 { "no local models were loaded".into() } else { format!("unloaded {n} model(s)") }
                }
                Err(e) => format!("unload: {e}"),
            },
        }
    }

    fn report(verb: &str, name: &str, r: crate::client::Result<crate::client::TweakReport>) -> String {
        match r {
            Err(e) => format!("{verb} {name}: {e}"),
            Ok(rep) if rep.error.is_some() => format!("{verb} {name}: {}", rep.error.unwrap_or_default()),
            Ok(rep) => {
                let failed: Vec<String> = rep
                    .results
                    .iter()
                    .filter(|l| !l.ok)
                    .map(|l| format!("{} ({})", l.id, l.result))
                    .collect();
                let ok = rep.results.len() - failed.len();
                if failed.is_empty() {
                    format!("{verb} {name}: {ok} done")
                } else {
                    format!("{verb} {name}: {ok} done, {} failed — {}", failed.len(), failed.join("; "))
                }
            }
        }
    }

    pub fn snapshot(&self) -> Snapshot {
        self.shared.lock().unwrap().clone()
    }

    pub fn send(&self, cmd: Command) {
        let _ = self.tx.send(cmd);
    }
}
