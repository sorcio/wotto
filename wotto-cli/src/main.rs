use anyhow::Result;
use wotto_engine::repl;

#[tokio::main]
async fn main() -> Result<()> {
    repl::repl().await
}
