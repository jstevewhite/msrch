//! Tool handlers and transport startup. Completed in the rmcp task.

use anyhow::Result;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportKind {
    Stdio,
    Http,
}

#[derive(Debug)]
pub struct McpOptions {
    pub transport: TransportKind,
    /// Raw `--index` flag values (parsed by IndexRegistry::from_flags);
    /// empty → walk-up discovery from the current directory.
    pub index_flags: Vec<String>,
    /// http only; None → "127.0.0.1:7920".
    pub bind: Option<String>,
}

pub async fn serve(_options: McpOptions) -> Result<()> {
    anyhow::bail!("serve: implemented in the rmcp task")
}
