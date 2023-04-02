#![feature(round_char_boundary)]
#![feature(arbitrary_self_types)]

mod bot;
mod parsing;
mod throttling;
mod tracing;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(feature = "tracing")]
    let _tracing = tracing::setup_tracing()?;
    bot::bot_main().await
}
