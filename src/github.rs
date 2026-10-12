//! The GitHub client. All calls go through the `gh` CLI, so the auth comes
//! from the user's `gh` login (keyring) and no token is stored here.

use std::io::Write;
use std::process::{Command, Stdio};

use serde::Deserialize;

pub type Error = Box<dyn std::error::Error + Send + Sync>;

/// One workflow run. `workflow` (JSON `name`) is the name of the workflow,
/// `display_title` is the commit subject.
#[derive(Debug, Clone, Deserialize)]
pub struct Run {
    pub id: u64,
    #[serde(default, rename = "name")]
    pub workflow: Option<String>,
    #[serde(default)]
    pub display_title: Option<String>,
    #[serde(default)]
    pub head_branch: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub conclusion: Option<String>,
    #[serde(default)]
    pub html_url: Option<String>,
    #[serde(default)]
    pub created_at: Option<String>,
}

impl Run {
    /// The workflow name, with a fallback for display.
    pub fn name(&self) -> &str {
        self.workflow.as_deref().unwrap_or("run")
    }

    pub fn title(&self) -> &str {
        self.display_title.as_deref().unwrap_or("")
    }

    pub fn conclusion(&self) -> &str {
        self.conclusion.as_deref().unwrap_or("-")
    }
}

/// One environment of a run that waits for approval.
#[derive(Debug, Clone)]
pub struct PendingEnv {
    pub id: u64,
    pub name: String,
    /// False when the environment disallows self-approval for this user.
    pub can_approve: bool,
}

#[derive(Debug, Deserialize)]
struct RawPending {
    environment: RawEnv,
    #[serde(default, rename = "current_user_can_approve")]
    can_approve: bool,
}

#[derive(Debug, Deserialize)]
struct RawEnv {
    #[serde(default)]
    id: u64,
    #[serde(default)]
    name: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Client {
    repo: String,
}

impl Client {
    pub fn new(repo: &str) -> Self {
        Self {
            repo: repo.to_string(),
        }
    }

    fn api(&self, args: &[&str]) -> Result<serde_json::Value, Error> {
        let out = Command::new("gh")
            .args(["api", "-H", "Accept: application/vnd.github+json"])
            .args(args)
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("gh api: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "gh api failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            )
            .into());
        }
        Ok(serde_json::from_slice(&out.stdout)?)
    }

    fn runs(&self, query: &str) -> Result<Vec<Run>, Error> {
        let path = format!("repos/{}/actions/runs?{query}", &self.repo);
        let value = self.api(&[&path])?;
        let mut out = Vec::new();
        if let Some(list) = value.get("workflow_runs").and_then(|w| w.as_array()) {
            for item in list {
                if let Ok(run) = serde_json::from_value::<Run>(item.clone()) {
                    out.push(run);
                }
            }
        }
        Ok(out)
    }

    /// The runs that wait on a deployment review.
    pub fn waiting(&self) -> Result<Vec<Run>, Error> {
        self.runs("status=waiting&per_page=50")
    }

    /// The latest runs of any state, newest first.
    pub fn recent(&self, count: usize) -> Result<Vec<Run>, Error> {
        self.runs(&format!("per_page={count}"))
    }

    /// One run by id.
    pub fn run(&self, run_id: u64) -> Result<Run, Error> {
        let path = format!("repos/{}/actions/runs/{run_id}", &self.repo);
        let value = self.api(&[&path])?;
        Ok(serde_json::from_value(value)?)
    }

    /// The environments the run waits on. An error means the run has no
    /// pending deployments left (already reviewed, or not an approval run).
    pub fn pending(&self, run_id: u64) -> Result<Vec<PendingEnv>, Error> {
        let path = format!(
            "repos/{}/actions/runs/{run_id}/pending_deployments",
            &self.repo
        );
        let value = self.api(&[&path])?;
        let mut out = Vec::new();
        if let Some(list) = value.as_array() {
            for item in list {
                if let Ok(raw) = serde_json::from_value::<RawPending>(item.clone()) {
                    out.push(PendingEnv {
                        id: raw.environment.id,
                        name: raw
                            .environment
                            .name
                            .unwrap_or_else(|| raw.environment.id.to_string()),
                        can_approve: raw.can_approve,
                    });
                }
            }
        }
        Ok(out)
    }

    /// Approve or reject the environments of one run. `state` is
    /// `approved` or `rejected`; the comment is required by the API and
    /// shows up in the deployment history.
    pub fn review(
        &self,
        run_id: u64,
        env_ids: &[u64],
        state: &str,
        comment: &str,
    ) -> Result<(), Error> {
        let ids = env_ids
            .iter()
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join(",");
        let body = format!(
            "{{\"environment_ids\":[{ids}],\"state\":\"{state}\",\"comment\":\"{comment}\"}}"
        );
        let path = format!(
            "repos/{}/actions/runs/{run_id}/pending_deployments",
            &self.repo
        );
        let mut child = Command::new("gh")
            .args([
                "api",
                "-H",
                "Content-Type: application/json",
                "-X",
                "POST",
                "--input",
                "-",
                &path,
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("gh api: {e}"))?;
        child
            .stdin
            .take()
            .ok_or("gh api: stdin closed")?
            .write_all(body.as_bytes())?;
        let out = child.wait_with_output()?;
        if out.status.success() {
            Ok(())
        } else {
            Err(format!(
                "review of run {run_id} failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            )
            .into())
        }
    }

    /// A run action: `cancel`, `rerun` or `rerun-failed-jobs`.
    pub fn action(&self, run_id: u64, action: &str) -> Result<(), Error> {
        let path = format!("repos/{}/actions/runs/{run_id}/{action}", &self.repo);
        let out = Command::new("gh")
            .args(["api", "-X", "POST"])
            .arg(&path)
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("gh api: {e}"))?;
        if out.status.success() {
            Ok(())
        } else {
            Err(format!(
                "{action} of run {run_id} failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            )
            .into())
        }
    }
}
