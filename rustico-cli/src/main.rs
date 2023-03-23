use anyhow::Result;
use rustico::repl;

#[tokio::main]
async fn main() -> Result<()> {
    repl::repl().await
}
