mod handshake;

use handshake::Harness;

#[test]
#[ignore = "needs network plus a working Reddit OAuth token"]
fn lists_subreddit_and_searches() {
	let mut h = Harness::spawn();
	h.initialize();

	h.send(r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#);
	let list = h.read_line();
	for name in ["reddit_search", "reddit_subreddit", "reddit_post", "reddit_user"] {
		assert!(list.contains(name), "tools/list missing {name}: {list}");
	}

	h.send(r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"reddit_subreddit","arguments":{"subreddit":"rust","limit":3}}}"#);
	let call = h.read_line();
	assert!(call.contains("result_count"), "unexpected call response: {call}");
	assert!(call.contains("\"result_count\":3"), "expected 3 posts: {call}");
}

#[test]
#[ignore = "needs network plus a working Reddit OAuth token"]
fn rejects_bad_input() {
	let mut h = Harness::spawn();
	h.initialize();
	h.send(r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"reddit_subreddit","arguments":{"subreddit":"bad/name"}}}"#);
	let call = h.read_line();
	assert!(call.contains("-32602") || call.contains("BAD_INPUT"), "expected a bad-input rejection: {call}");
}
