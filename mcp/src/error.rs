use rmcp::model::ErrorData;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
	BadInput,
	RateLimited,
	TokenStale,
	Down,
	Unavailable,
	Parse,
}

impl ErrorKind {
	pub fn as_str(self) -> &'static str {
		match self {
			Self::BadInput => "BAD_INPUT",
			Self::RateLimited => "RATE_LIMITED",
			Self::TokenStale => "UPSTREAM_TOKEN_STALE",
			Self::Down => "REDLIB_DOWN",
			Self::Unavailable => "CONTENT_UNAVAILABLE",
			Self::Parse => "PARSE_ERROR",
		}
	}

	pub fn hint(self) -> &'static str {
		match self {
			Self::TokenStale => "Reddit rejected the OAuth token. Restart the MCP server to obtain a fresh one.",
			Self::RateLimited => "Back off a few seconds before retrying.",
			_ => "",
		}
	}
}

#[derive(Debug)]
pub struct FetchError {
	pub kind: ErrorKind,
	pub detail: String,
}

impl FetchError {
	pub fn new(kind: ErrorKind, detail: impl Into<String>) -> Self {
		Self { kind, detail: detail.into() }
	}

	pub fn to_error_data(&self) -> ErrorData {
		let hint = self.kind.hint();
		let message = if hint.is_empty() {
			format!("{}: {}", self.kind.as_str(), self.detail)
		} else {
			format!("{}: {} — {hint}", self.kind.as_str(), self.detail)
		};
		match self.kind {
			ErrorKind::BadInput => ErrorData::invalid_params(message, None),
			_ => ErrorData::internal_error(message, None),
		}
	}
}

pub fn map_client_err(message: String) -> FetchError {
	let lower = message.to_lowercase();
	let kind = if lower.contains("rate limit") || lower.contains("too many requests") {
		ErrorKind::RateLimited
	} else if lower.contains("oauth") || lower.contains("unauthorized") || lower.contains("401") || lower.contains("token") {
		ErrorKind::TokenStale
	} else if lower.contains("removed") || lower.contains("404") {
		ErrorKind::Unavailable
	} else if lower.contains("error sending request") || lower.contains("connection") || lower.contains("dns") || lower.contains("timed out") {
		ErrorKind::Down
	} else {
		ErrorKind::Parse
	};
	FetchError::new(kind, message)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn maps_rate_limit_text() {
		assert_eq!(map_client_err("Rate limit exceeded, please retry".into()).kind, ErrorKind::RateLimited);
	}

	#[test]
	fn maps_token_stale_text() {
		assert_eq!(map_client_err("Unauthorized: oauth token expired".into()).kind, ErrorKind::TokenStale);
	}

	#[test]
	fn maps_transport_text() {
		assert_eq!(map_client_err("error sending request for url (https://www.reddit.com/): connection closed".into()).kind, ErrorKind::Down);
	}

	#[test]
	fn unknown_text_falls_back_to_parse() {
		assert_eq!(map_client_err("something odd".into()).kind, ErrorKind::Parse);
	}

	#[test]
	fn bad_input_maps_to_invalid_params() {
		let e = FetchError::new(ErrorKind::BadInput, "query must not be empty");
		assert_eq!(e.to_error_data().code.0, -32602);
	}

	#[test]
	fn internal_kinds_map_to_internal_error() {
		let e = FetchError::new(ErrorKind::Down, "no route to host");
		assert_eq!(e.to_error_data().code.0, -32603);
	}
}
