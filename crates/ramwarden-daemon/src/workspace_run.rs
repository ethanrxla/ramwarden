//! One approved, bounded plan. Events describe observations, never new authority.
use crate::workspace_tools::{Operation, Workspace};
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::sync::mpsc;

async fn emit(events: &Option<mpsc::Sender<Value>>, event: Value) -> bool {
    match events {
        Some(tx) => tokio::time::timeout(std::time::Duration::from_secs(3), tx.send(event))
            .await
            .is_ok_and(|result| result.is_ok()),
        None => true,
    }
}

pub async fn execute(
    root: Arc<Workspace>,
    operation: Operation,
    events: Option<mpsc::Sender<Value>>,
) -> Value {
    if !operation.valid() {
        return json!({"executed":false,"succeeded":false,"reply":"The workspace plan is invalid."});
    }
    let steps = operation.steps();
    let total = steps.len();
    let mut results = vec![];
    let mut succeeded = true;
    if !emit(&events, json!({"kind":"plan","total":total,"steps":steps.iter().map(Operation::preview).collect::<Vec<_>>()})).await {
        return json!({"executed":false,"succeeded":false,"cancelled":true,"steps":results});
    }
    for (index, step) in steps.into_iter().enumerate() {
        let label = step.label();
        if !emit(
            &events,
            json!({"kind":"started","index":index,"total":total,"label":label}),
        )
        .await
        {
            break;
        }
        let workspace = root.clone();
        let run = async move {
            if step.command().is_some() {
                workspace.terminal(&step).await
            } else {
                tokio::task::spawn_blocking(move || workspace.execute(&step))
                    .await
                    .map_err(|_| "Workspace worker did not finish".to_string())?
            }
        };
        let result = if let Some(tx) = &events {
            // Dropping a terminal future kills its child. A file operation already
            // dispatched to the blocking pool may finish; later steps never start.
            tokio::select! { biased;
                _ = tx.closed() => return json!({"executed":true,"succeeded":false,"cancelled":true,"steps":results}),
                result = run => result,
            }
        } else {
            run.await
        };
        let (ok, value) = match result {
            Ok(value) => (value["succeeded"].as_bool().unwrap_or(true), value),
            Err(reason) => (false, json!({"error":reason})),
        };
        let event = json!({"kind":"result","index":index,"total":total,"label":label,"succeeded":ok,"result":value});
        results.push(event.clone());
        if !emit(&events, event).await {
            break;
        }
        if !ok {
            succeeded = false;
            break;
        }
    }
    let completed = results.iter().filter(|r| r["succeeded"] == true).count();
    succeeded &= results.len() == total;
    let reply = if succeeded {
        format!("Completed {completed} of {total} reviewed workspace steps.")
    } else {
        format!(
            "Stopped after {completed} successful step(s) of {total}. Completed changes remain; later steps did not run."
        )
    };
    let finished = json!({"kind":"finished","succeeded":succeeded,"completed":completed,"total":total,"reply":reply});
    emit(&events, finished).await;
    json!({"executed":true,"succeeded":succeeded,"reply":reply,"steps":results})
}

#[cfg(test)]
mod tests {
    use super::*;
    fn create(path: &str) -> Operation {
        Operation::CreateText {
            path: path.into(),
            content: "reviewed".into(),
        }
    }
    #[tokio::test]
    async fn ordered_results_and_partial_failure_stop_later_writes() {
        let temp = tempfile::tempdir().unwrap();
        let root = Arc::new(Workspace::open(temp.path()).unwrap());
        let plan = Operation::Workflow {
            steps: vec![
                Operation::CreateDirectory {
                    path: "notes".into(),
                },
                create("notes/one"),
                create("notes/one"),
                create("must-not-exist"),
            ],
        };
        let result = execute(root, plan, None).await;
        assert_eq!(result["succeeded"], false);
        assert_eq!(result["steps"].as_array().unwrap().len(), 3);
        assert_eq!(result["steps"][1]["index"], 1);
        assert_eq!(
            std::fs::read_to_string(temp.path().join("notes/one")).unwrap(),
            "reviewed"
        );
        assert!(!temp.path().join("must-not-exist").exists());
    }
    #[tokio::test]
    async fn stream_emits_start_result_and_finish_in_order() {
        let temp = tempfile::tempdir().unwrap();
        let root = Arc::new(Workspace::open(temp.path()).unwrap());
        let (tx, mut rx) = mpsc::channel(4);
        let job = tokio::spawn(execute(root, create("one"), Some(tx)));
        let mut kinds = vec![];
        while let Some(event) = rx.recv().await {
            kinds.push(event["kind"].as_str().unwrap().to_string());
        }
        assert_eq!(kinds, vec!["plan", "started", "result", "finished"]);
        assert_eq!(job.await.unwrap()["succeeded"], true);
    }
    #[tokio::test]
    async fn closed_stream_never_starts_a_step() {
        let temp = tempfile::tempdir().unwrap();
        let root = Arc::new(Workspace::open(temp.path()).unwrap());
        let (tx, rx) = mpsc::channel(1);
        drop(rx);
        assert_eq!(
            execute(root, create("never"), Some(tx)).await["succeeded"],
            false
        );
        assert!(!temp.path().join("never").exists());
    }
    #[tokio::test(start_paused = true)]
    async fn unread_stream_times_out_without_holding_the_executor_forever() {
        let temp = tempfile::tempdir().unwrap();
        let root = Arc::new(Workspace::open(temp.path()).unwrap());
        let (tx, _rx) = mpsc::channel(1);
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(7),
            execute(root, create("never"), Some(tx)),
        )
        .await
        .unwrap();
        assert_eq!(result["succeeded"], false);
        assert!(!temp.path().join("never").exists());
    }
    #[tokio::test]
    async fn terminal_failure_stops_the_remaining_plan() {
        let (git, _) = Operation::GitLog.command().unwrap();
        if !std::path::Path::new(git).exists() {
            return;
        }
        let temp = tempfile::tempdir().unwrap();
        let null = if cfg!(windows) { "NUL" } else { "/dev/null" };
        let status = std::process::Command::new(git)
            .args(["init", "--quiet"])
            .arg(temp.path())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", null)
            .status()
            .unwrap();
        assert!(status.success());
        let root = Arc::new(Workspace::open(temp.path()).unwrap());
        // An empty repository has no log. Git's nonzero exit is an observed
        // failure, not a successful command merely because the process ran.
        let result = execute(
            root.clone(),
            Operation::Workflow {
                steps: vec![Operation::GitLog, create("never")],
            },
            None,
        )
        .await;
        assert_eq!(result["succeeded"], false);
        assert_eq!(result["steps"].as_array().unwrap().len(), 1);
        assert_eq!(result["steps"][0]["result"]["succeeded"], false);
        assert!(!temp.path().join("never").exists());
        std::fs::write(temp.path().join("fixture.txt"), "fixture").unwrap();
        for args in [
            vec!["add", "--", "fixture.txt"],
            vec![
                "-c",
                "commit.gpgSign=false",
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "commit",
                "--quiet",
                "-m",
                "fixture commit",
            ],
        ] {
            assert!(
                std::process::Command::new(git)
                    .args(args)
                    .current_dir(temp.path())
                    .env("GIT_CONFIG_NOSYSTEM", "1")
                    .env("GIT_CONFIG_GLOBAL", null)
                    .status()
                    .unwrap()
                    .success()
            );
        }
        let log = root.terminal(&Operation::GitLog).await.unwrap();
        assert_eq!(log["succeeded"], true);
        assert!(log["stdout"].as_str().unwrap().contains("fixture commit"));
    }
    #[tokio::test]
    async fn disconnect_between_steps_preserves_completed_work_only() {
        let temp = tempfile::tempdir().unwrap();
        let root = Arc::new(Workspace::open(temp.path()).unwrap());
        let (tx, mut rx) = mpsc::channel(1);
        let job = tokio::spawn(execute(
            root,
            Operation::Workflow {
                steps: vec![create("first"), create("later")],
            },
            Some(tx),
        ));
        assert_eq!(rx.recv().await.unwrap()["kind"], "plan");
        assert_eq!(rx.recv().await.unwrap()["kind"], "started");
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while !temp.path().join("first").exists() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        // The result fills the one-event channel. The second start cannot pass
        // until it is read; closing the receiver prevents that second write.
        rx.close();
        drop(rx);
        job.await.unwrap();
        assert!(temp.path().join("first").exists());
        assert!(!temp.path().join("later").exists());
    }
    #[test]
    fn workflow_bounds_and_auto_approval_are_not_recursive() {
        let valid = Operation::Workflow {
            steps: vec![create("one")],
        };
        assert!(valid.valid());
        assert!(!valid.auto_eligible("create one"));
        assert!(!Operation::Workflow { steps: vec![valid] }.valid());
        assert!(
            !Operation::Workflow {
                steps: vec![create("a"); 7]
            }
            .valid()
        );
        assert!(!Operation::Workflow { steps: vec![] }.valid());
        assert!(
            !Operation::Workflow {
                steps: vec![create("../escape")]
            }
            .valid()
        );
        assert!(
            !Operation::Workflow {
                steps: vec![
                    Operation::CreateText {
                        path: "one".into(),
                        content: "x".repeat(9000)
                    },
                    Operation::CreateText {
                        path: "two".into(),
                        content: "x".repeat(9000)
                    }
                ]
            }
            .valid()
        );
        assert!(Operation::GitLog.preview().contains("--no-show-signature"));
        assert!(
            serde_json::from_value::<Operation>(json!({"tool":"shell","command":"touch bad"}))
                .is_err()
        );
    }
}
