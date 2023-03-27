#![feature(round_char_boundary)]
#![feature(arbitrary_self_types)]

mod bot;
mod parsing;
mod throttling;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    setup_tracing()?;
    bot::bot_main().await
}

fn setup_tracing() -> Result<(), Box<dyn std::error::Error>> {
    // A bit of overkill, just cramming in everything from Tokio tutorial (see
    // https://tokio.rs/tokio/topics/tracing-next-steps); we will clean up
    // later, doesn't matter now.
    //
    // Note: this needs `--cfg tokio_unstable` (see .cargo/config.toml)

    use opentelemetry::global;
    use tracing_subscriber::prelude::*;

    let registry = tracing_subscriber::registry();

    // these could become features
    let enable_jaeger = true;
    let enable_fmt = false;
    let enable_tokio_console = true;

    let jaeger_layer = if enable_jaeger {
        // Allows you to pass along context (i.e., trace IDs) across services
        global::set_text_map_propagator(opentelemetry_jaeger::Propagator::new());
        let tracer = opentelemetry_jaeger::new_pipeline()
            .with_service_name("rusto")
            .install_simple()?;
        let opentelemetry = tracing_opentelemetry::layer().with_tracer(tracer);
        Some(opentelemetry)
    } else {
        None
    };

    let fmt_layer = if enable_fmt {
        Some(tracing_subscriber::fmt::layer())
    } else {
        None
    };

    let console_layer = if enable_tokio_console {
        Some(console_subscriber::spawn())
    } else {
        None
    };

    registry
        .with(jaeger_layer)
        .with(fmt_layer)
        .with(console_layer)
        .try_init()
        .map_err(|err| err.into())
}
