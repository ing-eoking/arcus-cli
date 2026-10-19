use super::Tool;

/// All tools exposed by `arcus-cli --mcp` (later tasks append here).
pub fn registry() -> Vec<Box<dyn Tool>> {
    Vec::new()
}
