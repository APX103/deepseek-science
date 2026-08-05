use dss_core::A2aAgentConfig;
use serde::{Deserialize, Serialize};

use crate::{validate_configs, A2aClient, A2aError, CardSnapshot};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentRuntimeStatus {
    Unchecked,
    Ready,
    Offline,
    Invalid,
    Unsupported,
    Disabled,
}

/// Immutable per-Agent runtime state captured by an application/run snapshot.
///
/// This type intentionally has no `Serialize` implementation because `config` may contain a
/// Bearer token. API layers must expose an explicitly redacted public view.
#[derive(Debug, Clone)]
pub struct AgentRuntime {
    pub config: A2aAgentConfig,
    pub status: AgentRuntimeStatus,
    pub card: Option<CardSnapshot>,
    pub last_error: Option<String>,
    pub last_refreshed_at: Option<String>,
}

impl AgentRuntime {
    pub fn tool_name(&self) -> String {
        crate::stable_tool_name(&self.config.id)
    }
}

/// A coherent, cloneable A2A configuration/card snapshot. Calling `refresh_all` returns a new
/// snapshot and never mutates a snapshot already captured by an in-flight run.
#[derive(Debug, Clone)]
pub struct A2aRuntimeSnapshot {
    pub revision: u64,
    pub agents: Vec<AgentRuntime>,
}

impl A2aRuntimeSnapshot {
    pub fn unrefreshed(revision: u64, configs: Vec<A2aAgentConfig>) -> Result<Self, A2aError> {
        validate_configs(&configs)?;
        Ok(Self {
            revision,
            agents: configs
                .into_iter()
                .map(|config| AgentRuntime {
                    status: if config.enabled {
                        AgentRuntimeStatus::Unchecked
                    } else {
                        AgentRuntimeStatus::Disabled
                    },
                    config,
                    card: None,
                    last_error: None,
                    last_refreshed_at: None,
                })
                .collect(),
        })
    }

    pub fn enabled(&self) -> impl Iterator<Item = &AgentRuntime> {
        self.agents.iter().filter(|agent| agent.config.enabled)
    }

    pub fn find(&self, config_id: &str) -> Option<&AgentRuntime> {
        self.agents
            .iter()
            .find(|agent| agent.config.id == config_id)
    }

    /// Best-effort discovery for settings/status display. Offline or incompatible Agents remain
    /// in the returned snapshot and still can recover on a future mandatory call-time refresh.
    pub async fn refresh_all(&self, client: &A2aClient) -> Self {
        let agents =
            futures::future::join_all(self.agents.iter().cloned().map(|mut agent| async move {
                if !agent.config.enabled {
                    agent.status = AgentRuntimeStatus::Disabled;
                    agent.last_error = None;
                    return agent;
                }
                match client
                    .refresh_card(&agent.config, agent.card.as_ref())
                    .await
                {
                    Ok(card) => {
                        agent.last_refreshed_at = Some(card.fetched_at.clone());
                        agent.card = Some(card);
                        agent.status = AgentRuntimeStatus::Ready;
                        agent.last_error = None;
                    }
                    Err(error) => {
                        agent.status = status_for_refresh_error(&error);
                        agent.last_error = Some(error.to_string());
                    }
                }
                agent
            }))
            .await;
        Self {
            revision: self.revision,
            agents,
        }
    }
}

fn status_for_refresh_error(error: &A2aError) -> AgentRuntimeStatus {
    match error {
        A2aError::UnsupportedCard(_) => AgentRuntimeStatus::Unsupported,
        A2aError::InvalidConfig(_)
        | A2aError::InvalidEndpoint(_)
        | A2aError::CardTooLarge { .. }
        | A2aError::InvalidCard(_)
        | A2aError::CrossOrigin => AgentRuntimeStatus::Invalid,
        A2aError::CardRefresh(_)
        | A2aError::ResponseTooLarge { .. }
        | A2aError::TotalResponseTooLarge { .. }
        | A2aError::Protocol(_)
        | A2aError::Timeout
        | A2aError::Transport(_) => AgentRuntimeStatus::Offline,
    }
}

#[cfg(test)]
mod tests {
    use dss_core::A2aAgentConfig;

    use super::*;

    #[test]
    fn unrefreshed_snapshot_preserves_disabled_agents() {
        let snapshot = A2aRuntimeSnapshot::unrefreshed(
            7,
            vec![A2aAgentConfig {
                id: "disabled".into(),
                name: "Disabled".into(),
                endpoint: "http://127.0.0.1:9999".into(),
                enabled: false,
                bearer_token: None,
                timeout_seconds: 120,
            }],
        )
        .unwrap();
        assert_eq!(snapshot.revision, 7);
        assert_eq!(snapshot.agents[0].status, AgentRuntimeStatus::Disabled);
        assert_eq!(snapshot.enabled().count(), 0);
    }

    #[test]
    fn refresh_errors_keep_invalid_unsupported_and_network_statuses_distinct() {
        assert_eq!(
            status_for_refresh_error(&A2aError::InvalidCard("bad shape".into())),
            AgentRuntimeStatus::Invalid
        );
        assert_eq!(
            status_for_refresh_error(&A2aError::UnsupportedCard("grpc only".into())),
            AgentRuntimeStatus::Unsupported
        );
        assert_eq!(
            status_for_refresh_error(&A2aError::CardRefresh("connection refused".into())),
            AgentRuntimeStatus::Offline
        );
    }
}
