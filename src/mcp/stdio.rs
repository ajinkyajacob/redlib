//! stdio transport: `redlib mcp` speaks MCP on stdin/stdout.

pub async fn serve() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
	use rmcp::ServiceExt;
	std::sync::LazyLock::force(&crate::config::CONFIG);
	// Do NOT force OAUTH_CLIENT here: its initialiser does a live Reddit round
	// trip (and can take tens of seconds), which would delay `initialize` past a
	// client's timeout. The first tool call forces it lazily instead — safe
	// because `client.rs` uses `futures_lite::block_on`, which can run inside a
	// tokio task.
	// stdout carries MCP protocol frames only. Never println! on this path.
	let service = match crate::mcp::tools::RedlibServer::new().serve((tokio::io::stdin(), tokio::io::stdout())).await {
		Ok(service) => service,
		// A client that closes stdin before initialising is a normal shutdown,
		// not a server error (e.g. `redlib mcp` with stdin at EOF).
		Err(rmcp::service::ServerInitializeError::ConnectionClosed(_)) => return Ok(()),
		Err(e) => return Err(e.into()),
	};
	service.waiting().await?;
	Ok(())
}
