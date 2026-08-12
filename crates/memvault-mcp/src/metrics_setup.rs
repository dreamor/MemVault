use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};

/// Install the global Prometheus recorder and return a handle used to
/// render the `/metrics` response. Call once at process startup before
/// any `metrics::counter!`/`histogram!` calls are made.
pub fn install_recorder() -> PrometheusHandle {
    PrometheusBuilder::new()
        .install_recorder()
        .expect("failed to install Prometheus metrics recorder")
}
