use serde::Deserialize;
use std::time::Duration;

/// What Flokque's `/healthz` says about it.
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Health {
    pub active_sessions: u32,
}

/// None while Flokque doesn't answer, or doesn't run here at all.
pub fn health(url: &str) -> Option<Health> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(5)))
        .build()
        .into();
    agent
        .get(format!("{}/healthz", url.trim_end_matches('/')))
        .call()
        .ok()?
        .body_mut()
        .read_json()
        .ok()
}
