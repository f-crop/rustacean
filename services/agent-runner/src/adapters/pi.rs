use anyhow::{Result, bail};
use async_trait::async_trait;

use super::{AgentProcess, ParsedLine, RuntimeAdapter, SessionCtx};

pub struct PiAdapter;

impl PiAdapter {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl RuntimeAdapter for PiAdapter {
    async fn spawn(&self, _ctx: &SessionCtx) -> Result<AgentProcess> {
        bail!("PiAdapter not implemented: pi runtime evaluation pending (ADR-009 Phase 3)")
    }

    async fn send_input(&self, _proc: &mut AgentProcess, _input: &str) -> Result<()> {
        bail!("PiAdapter not implemented")
    }

    async fn terminate(&self, _proc: &mut AgentProcess, _force: bool) -> Result<()> {
        bail!("PiAdapter not implemented")
    }

    fn parse_stdout_line(&self, _line: &str) -> Option<ParsedLine> {
        None
    }
}
