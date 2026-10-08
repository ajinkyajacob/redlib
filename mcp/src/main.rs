use std::sync::LazyLock;

use rmcp::ServiceExt;

mod normalize;
mod params;
mod tools;

mod error;
mod pace;
mod fetch;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
	for arg in std::env::args().skip(1) {
		match arg.as_str() {
			"--version" => {
				eprintln!("redlib-mcp {}", env!("CARGO_PKG_VERSION"));
				return Ok(());
			}
			other => {
				eprintln!("unknown argument: {other}");
				std::process::exit(2);
			}
		}
	}

	LazyLock::force(&redlib::config::CONFIG);
	LazyLock::force(&redlib::client::OAUTH_CLIENT);

	// stdout carries MCP protocol frames only. Never println! in this crate.
	let service = tools::RedlibServer::new().serve((tokio::io::stdin(), tokio::io::stdout())).await?;
	service.waiting().await?;
	Ok(())
}
