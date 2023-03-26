#![feature(round_char_boundary)]
#![feature(arbitrary_self_types)]

mod bot;
mod parsing;
mod throttling;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    bot::bot_main().await
}
