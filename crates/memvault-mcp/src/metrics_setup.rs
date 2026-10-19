use std::sync::OnceLock;

use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};

/// The Prometheus recorder can only be installed once per process. Share a
/// single handle across every call — production installs it once at startup,
/// and tests routinely boot several REST servers in one test binary.
static RECORDER: OnceLock<PrometheusHandle> = OnceLock::new();

/// Install the global Prometheus recorder (idempotent) and return the handle
/// used to render the `/metrics` response.
pub fn install_recorder() -> PrometheusHandle {
    RECORDER
        .get_or_init(|| {
            PrometheusBuilder::new()
                .install_recorder()
                .expect("failed to install Prometheus metrics recorder")
        })
        .clone()
}
