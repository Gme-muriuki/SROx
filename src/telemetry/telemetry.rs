use anyhow::{Context, Result};
use opentelemetry::trace::TracerProvider;
use opentelemetry_otlp::{SpanExporter, WithExportConfig};
use opentelemetry_sdk::{Resource, trace::SdkTracerProvider};
use tracing_opentelemetry::OpenTelemetryLayer;
use tracing_subscriber::{EnvFilter, fmt::format::FmtSpan, layer::SubscriberExt};

pub fn init() -> Result<SdkTracerProvider> {
    // 1. Build OTel tracer that exports to Jaeger via OTLP
    let exporter = SpanExporter::builder()
        .with_tonic()
        .with_endpoint("http://localhost:4317")
        .build()
        .context("failed to build OTLP exporter")?;

    // Resource with service name
    let resource = Resource::builder().with_service_name("srox-proxy").build();

    // Tracer provider with batch exporter and service name.
    let tracer_provider = SdkTracerProvider::builder()
        .with_resource(resource)
        .with_batch_exporter(exporter)
        .build();

    opentelemetry::global::set_tracer_provider(tracer_provider.clone());

    // 2. Build tracing subscriber: JSON format + OTel layer
    let filter_layer = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    let json_layer = tracing_subscriber::fmt::layer()
        .json()
        .with_span_list(true)
        .with_ansi(false)
        .with_current_span(true)
        .with_span_events(FmtSpan::NONE);

    let tracer = tracer_provider.tracer("srox");
    let otel_layer = OpenTelemetryLayer::new(tracer);

    let subscriber = tracing_subscriber::Registry::default()
        .with(filter_layer)
        .with(json_layer)
        .with(otel_layer);

    // 3. Set a global default
    tracing::subscriber::set_global_default(subscriber)
        .context("failed to set global subscriber")?;

    // 4. Return Ok(())
    Ok(tracer_provider)
}

pub fn shutdown(provider: SdkTracerProvider) {
    // Flush remaining spans before process exits.
    provider.shutdown().ok();
}
