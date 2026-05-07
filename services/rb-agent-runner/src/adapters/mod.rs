mod claude_code;
mod opencode;
mod pi;

pub use claude_code::ClaudeCodeAdapter;
pub use opencode::OpencodeAdapter;
pub use pi::PiAdapter;

use crate::adapter::RuntimeAdapter;
use crate::adapter::AdapterError;
use crate::config::AdapterConfig;

/// Creates a runtime adapter with environment configuration.
pub fn create_adapter(runtime: rb_schemas::AgentRuntime) -> Result<Box<dyn RuntimeAdapter>, AdapterError> {
    let config = AdapterConfig::from_env();
    create_adapter_with_config(runtime, config)
}

/// Creates a runtime adapter with explicit configuration.
pub fn create_adapter_with_config(
    runtime: rb_schemas::AgentRuntime,
    config: AdapterConfig,
) -> Result<Box<dyn RuntimeAdapter>, AdapterError> {
    match runtime {
        rb_schemas::AgentRuntime::ClaudeCode => Ok(Box::new(ClaudeCodeAdapter::new(config))),
        rb_schemas::AgentRuntime::Opencode => Ok(Box::new(OpencodeAdapter::new(config))),
        rb_schemas::AgentRuntime::Pi => Ok(Box::new(PiAdapter::new(config))),
        _ => Err(AdapterError::SpawnFailed(format!("Unsupported runtime: {:?}", runtime))),
    }
}
