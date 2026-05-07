use crate::adapter::{AdapterError, ProcessHandle, RuntimeAdapter};
use crate::adapters::create_adapter;
use crate::workspace::WorkspaceManager;
use rb_schemas::{AgentCommand, AgentSessionStatus, SessionInput, SessionStart, SessionTerminate};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;
use uuid::Uuid;

pub struct Session {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub runtime: rb_schemas::AgentRuntime,
    pub workspace_path: PathBuf,
    pub status: AgentSessionStatus,
    pub process_handle: Option<ProcessHandle>,
    pub adapter: Arc<Box<dyn RuntimeAdapter>>,
    pub trace_id: Option<String>,
}

#[derive(Default)]
pub struct SessionManager {
    sessions: RwLock<HashMap<Uuid, Session>>,
}

impl SessionManager {
    pub fn new() -> Self {
        Self {
            sessions: RwLock::new(HashMap::new()),
        }
    }

    pub async fn create_session(
        &self,
        cmd: &AgentCommand,
        workspace_mgr: &WorkspaceManager,
    ) -> Result<Uuid, AdapterError> {
        let session_id = Uuid::parse_str(&cmd.session_id)
            .map_err(|e| AdapterError::SpawnFailed(format!("Invalid session_id: {}", e)))?;
        let tenant_id = Uuid::parse_str(&cmd.tenant_id)
            .map_err(|e| AdapterError::SpawnFailed(format!("Invalid tenant_id: {}", e)))?;

        let workspace_path = workspace_mgr.create_workspace(&tenant_id, &session_id).await?;

        let adapter = create_adapter(cmd.runtime)?;

        let session = Session {
            id: session_id,
            tenant_id,
            runtime: rb_schemas::AgentRuntime::try_from(cmd.runtime).unwrap_or(rb_schemas::AgentRuntime::Unspecified),
            workspace_path,
            status: AgentSessionStatus::Pending,
            process_handle: None,
            adapter,
            trace_id: if cmd.traceparent.is_empty() {
                None
            } else {
                Some(cmd.traceparent.clone())
            },
        };

        self.sessions.write().await.insert(session_id, session);

        Ok(session_id)
    }

    pub async fn start_session(&self, session_id: Uuid, start: &SessionStart) -> Result<(), AdapterError> {
        let mut sessions = self.sessions.write().await;
        let session = sessions
            .get_mut(&session_id)
            .ok_or_else(|| AdapterError::NotRunning)?;

        let adapter = Arc::clone(&session.adapter);
        let workspace_path = session.workspace_path.clone();

        let prompt = if let Ok(content) = std::fs::read_to_string(&workspace_path.join("prompt.txt")) {
            content
        } else {
            String::new()
        };

        let handle = adapter
            .spawn(&workspace_path, &prompt, Some(&start.api_key))
            .await?;

        session.process_handle = Some(handle);
        session.status = AgentSessionStatus::Running;

        Ok(())
    }

    pub async fn send_input(&self, session_id: Uuid, input: &SessionInput) -> Result<(), AdapterError> {
        let mut sessions = self.sessions.write().await;
        let session = sessions
            .get_mut(&session_id)
            .ok_or_else(|| AdapterError::NotRunning)?;

        if let Some(ref mut handle) = session.process_handle {
            session.adapter.send_input(handle, &input.prompt).await?;
        }

        Ok(())
    }

    pub async fn terminate_session(&self, session_id: Uuid, term: &SessionTerminate) -> Result<(), AdapterError> {
        let mut sessions = self.sessions.write().await;
        let session = sessions
            .get_mut(&session_id)
            .ok_or_else(|| AdapterError::NotRunning)?;

        if let Some(ref mut handle) = session.process_handle {
            session.adapter.terminate(handle, term.force).await?;
        }

        session.status = AgentSessionStatus::Terminated;

        Ok(())
    }

    pub async fn remove_session(&self, session_id: Uuid) -> Option<Session> {
        let mut sessions = self.sessions.write().await;
        sessions.remove(&session_id)
    }
}
