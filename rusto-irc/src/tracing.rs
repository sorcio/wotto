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
        use opentelemetry::runtime::Tokio;
        // // Allows you to pass along context (i.e., trace IDs) across services
        global::set_text_map_propagator(opentelemetry_jaeger::Propagator::new());
        let tracer = opentelemetry_jaeger::new_agent_pipeline()
            .with_service_name("rusto")
            .with_max_packet_size(9216) // macos max, might need to be tweaked
            .with_auto_split_batch(true)
            .with_instrumentation_library_tags(false)
            .install_batch(Tokio)?;
        let opentelemetry = tracing_opentelemetry::layer().with_tracer(tracer);
        opentelemetry

        // Note: opentelemetry-jaeger export will be deprecated in the future,
        // and a migration to opentelemetry-otlp is encouraged:
        // https://github.com/open-telemetry/opentelemetry-specification/pull/2858
        // This is currently not easy because tracing-opentelemetry depends on
        // an older version of opentelemetry than the one required by the
        // latest opentelemetry-otlp version (yeah sorry I don't keep track
        // either). We could use an older release, but
        // open-telemetry/opentelemetry-rust#873 is a blocker. Basically, we
        // should wait until a new tracing-opentelemetry release supports
        // opentelemetry 0.19, which might be soon enough, or something else
        // changes. The new exporter would be used more or less like this:
        //
        //     let otlp_exporter = opentelemetry_otlp::new_exporter().tonic();
        //     let tracer = opentelemetry_otlp::new_pipeline()
        //         .tracing()
        //         .with_exporter(otlp_exporter)
        //         .install_batch(Tokio);

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
            .with_filter(
                tracing_subscriber::EnvFilter::builder()
                    .with_default_directive(tracing::metadata::LevelFilter::ERROR.into())
                    .from_env()?,
            )
    }
}

pub(crate) fn setup_tracing() -> Result<impl Drop, Box<dyn std::error::Error>> {
    // A bit of overkill, just cramming in everything from Tokio tutorial (see
    // https://tokio.rs/tokio/topics/tracing-next-steps); we will clean up
    // later, doesn't matter now.
    //
    // Note: this needs `--cfg tokio_unstable` (see .cargo/config.toml)

    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;
    tracing_subscriber::registry()
        .with(make_telemetry_layer()?)
        .with(make_stderr_tracing_layer()?)
        .with(make_tokio_console_layer()?)
        .try_init()?;

    Ok(Tracing)
}

/// Provides shutdown of tracing stuff when dropped. Returned by
/// [setup_tracing()].
struct Tracing;

impl Drop for Tracing {
    fn drop(&mut self) {
        // Add shutdown operations here. If we need to keep some reference
        // or track some state, we can use self.

        eprintln!("Shutting down tracing providers...");

        #[cfg(feature = "telemetry")]
        opentelemetry::global::shutdown_tracer_provider();
    }
}
