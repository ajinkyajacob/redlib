use std::sync::LazyLock;
mod normalize;

mod error;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
	LazyLock::force(&redlib::config::CONFIG);
	LazyLock::force(&redlib::client::OAUTH_CLIENT);
	eprintln!("redlib-mcp skeleton up");
	Ok(())
}