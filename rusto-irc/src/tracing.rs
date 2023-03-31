#![cfg(feature = "tracing")]

/// Helper to create layer setup functions conditionally on features.
/// See [crate::tracing] for examples.
macro_rules! layer_features {
    (
        #[cfg($cfg:meta)]
        fn $name:ident() { $($impl:tt)* };
    ) => {
        fn $name<T>(
        ) -> Result<impl ::tracing_subscriber::Layer<T>, Box<dyn std::error::Error>>
        where
            T: ::tracing::Subscriber + for<'a> ::tracing_subscriber::registry::LookupSpan<'a>,
        {
            #[cfg($cfg)]
            {Ok({ $($impl)* })}
            // if feature is not enabled, return a layer that does nothing:
            #[cfg(not($cfg))]
            {Ok(::tracing_subscriber::layer::Identity::new())}
        }
    };
    (
        $(#[cfg($cfg:meta)]
        fn $name:ident() { $($impl:tt)* })*
    ) => {
        $(layer_features!{
            #[cfg($cfg)]
            fn $name() { $($impl)* };
        })*
    };
}

layer_features! {
    #[cfg(feature = "telemetry")]
    fn make_telemetry_layer() {
        use opentelemetry::global;
        // Allows you to pass along context (i.e., trace IDs) across services
        global::set_text_map_propagator(opentelemetry_jaeger::Propagator::new());
        let tracer = opentelemetry_jaeger::new_pipeline()
            .with_service_name("rusto")
            .install_simple()?;
        let opentelemetry = tracing_opentelemetry::layer().with_tracer(tracer);
        opentelemetry
    }

    #[cfg(feature = "tokio-console")]
    fn make_tokio_console_layer() {
        console_subscriber::spawn()
    }

    #[cfg(feature = "stderr-tracing")]
    fn make_stderr_tracing_layer() {
        use tracing_subscriber::prelude::*;
        tracing_subscriber::fmt::Layer::new()
            .compact()
            .with_writer(std::io::stderr)
            .and_then(
                tracing_subscriber::EnvFilter::builder()
                    .with_default_directive(tracing::metadata::LevelFilter::ERROR.into())
                    .from_env()?,
            )
    }
}

pub(crate) fn setup_tracing() -> Result<(), Box<dyn std::error::Error>> {
    // A bit of overkill, just cramming in everything from Tokio tutorial (see
    // https://tokio.rs/tokio/topics/tracing-next-steps); we will clean up
    // later, doesn't matter now.
    //
    // Note: this needs `--cfg tokio_unstable` (see .cargo/config.toml)

    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;
    let registry = tracing_subscriber::registry();
    registry
        .with(make_telemetry_layer()?)
        .with(make_stderr_tracing_layer()?)
        .with(make_tokio_console_layer()?)
        .try_init()
        .map_err(|err| err.into())
}
