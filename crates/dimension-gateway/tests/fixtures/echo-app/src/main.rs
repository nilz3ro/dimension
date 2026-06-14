//! Echo-app fixture for e2e testing.
//!
//! Reads a JSON-serialized `MessageRequest` from stdin (written by
//! dimension-agent), extracts text content blocks, and prints them
//! to stdout. This proves the full data path: HTTP request → gateway
//! → vsock → dimension-agent → app stdin → app stdout → vsock → SSE.

use std::io::{self, Read};

/// Minimal MessageRequest parser — only the fields we care about.
#[derive(serde::Deserialize)]
struct MessageRequest {
    content: Vec<ContentBlock>,
}

#[derive(serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ContentBlock {
    Text { text: String },
    #[serde(other)]
    Other,
}

fn main() {
    // dimension-agent writes the full payload to stdin then closes it.
    let mut input = String::new();
    io::stdin()
        .read_to_string(&mut input)
        .expect("failed to read stdin");

    // Parse the MessageRequest JSON.
    let request: MessageRequest = match serde_json::from_str(&input) {
        Ok(req) => req,
        Err(e) => {
            eprintln!("echo-app: failed to parse MessageRequest: {e}");
            eprintln!("echo-app: raw input: {input}");
            std::process::exit(1);
        }
    };

    // Extract and print text content blocks — this is what a real agent
    // (e.g., Strands) would do to get the user's message.
    for block in &request.content {
        if let ContentBlock::Text { text } = block {
            println!("{text}");
        }
    }
}
