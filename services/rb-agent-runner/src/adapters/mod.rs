mod claude_code;
mod opencode;
mod pi;

pub use claude_code::ClaudeCodeAdapter;
pub use opencode::OpencodeAdapter;
pub use pi::PiAdapter;

use crate::adapter::RuntimeAdapter;
use crate::adapter::AdapterError;

pub fn create_adapter(runtime: rb_schemas::AgentRuntime) -> Result<Box<dyn RuntimeAdapter>, AdapterError> {
    match runtime {
        rb_schemas::AgentRuntime::ClaudeCode => Ok(Box::new(ClaudeCodeAdapter::new())),
        rb_schemas::AgentRuntime::Opencode => Ok(Box::new(OpencodeAdapter::new())),
        rb_schemas::AgentRuntime::Pi => Ok(Box::new(PiAdapter::new())),
        _ => Err(AdapterError::SpawnFailed(format!("Unsupported runtime: {:?}", runtime))),
    }
}
