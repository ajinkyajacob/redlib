//! MCP server: tool definitions, the paced Reddit fetch they share, and (added
//! in later tasks) the stdio and Streamable HTTP transports.

pub mod fetch;
pub mod pace;
pub mod stdio;
pub mod tools;

pub use tools::RedlibServer;
