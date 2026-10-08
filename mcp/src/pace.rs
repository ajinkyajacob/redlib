use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Single-user min-interval gate: one upstream call at a time, spaced by `min_ms`.
/// Serialized through a mutex so overlapping tool calls cannot burst Reddit.
pub struct Pacer {
	min: Duration,
	state: Mutex<Option<Instant>>,
}

impl Pacer {
	pub fn new(min_ms: u64) -> Self {
		Self { min: Duration::from_millis(min_ms), state: Mutex::new(None) }
	}

	pub async fn wait(&self) {
		let sleep_for = {
			let mut slot = self.state.lock().unwrap_or_else(|e| e.into_inner());
			let now = Instant::now();
			let start = slot.map(|next_allowed| next_allowed.max(now)).unwrap_or(now);
			*slot = Some(start + self.min);
			start.saturating_duration_since(now)
		};
		if !sleep_for.is_zero() {
			tokio::time::sleep(sleep_for).await;
		}
	}
}

pub fn from_env() -> Pacer {
	let ms = std::env::var("REDLIB_MCP_MIN_INTERVAL_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(300);
	Pacer::new(ms)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[tokio::test]
	async fn spaces_calls_by_minimum_interval() {
		let pacer = Pacer::new(40);
		let start = Instant::now();
		pacer.wait().await;
		pacer.wait().await;
		pacer.wait().await;
		assert!(start.elapsed().as_millis() >= 80, "expected two 40ms gaps, got {:?}", start.elapsed());
	}

	#[tokio::test]
	async fn zero_interval_never_sleeps() {
		let pacer = Pacer::new(0);
		let start = Instant::now();
		pacer.wait().await;
		pacer.wait().await;
		pacer.wait().await;
		assert!(start.elapsed().as_millis() < 50, "zero interval must not sleep, took {:?}", start.elapsed());
	}
}