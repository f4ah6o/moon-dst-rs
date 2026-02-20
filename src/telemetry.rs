// SPDX-License-Identifier: MIT
//! OpenTelemetry bootstrap for moon-dst CLI.

use opentelemetry::trace::TracerProvider;
use opentelemetry::KeyValue;
use opentelemetry_otlp::{SpanExporter, WithExportConfig};
use opentelemetry_sdk::trace::{Sampler, SdkTracer, SdkTracerProvider};
use opentelemetry_sdk::Resource;
use std::time::Duration;

pub struct RootAttrs {
    pub cli_command: String,
    pub cli_args: String,
    pub cwd: String,
}

pub struct Telemetry {
    enabled: bool,
    tracer: Option<SdkTracer>,
    provider: Option<SdkTracerProvider>,
    runtime: Option<tokio::runtime::Runtime>,
    root_span_name: String,
    root_attributes: Vec<KeyValue>,
}

impl Telemetry {
    pub fn disabled(root_span_name: String, root_attributes: Vec<KeyValue>) -> Self {
        Self {
            enabled: false,
            tracer: None,
            provider: None,
            runtime: None,
            root_span_name,
            root_attributes,
        }
    }

    pub fn tracer(&self) -> Option<&SdkTracer> {
        self.tracer.as_ref()
    }

    pub fn root_span_name(&self) -> &str {
        &self.root_span_name
    }

    pub fn root_attributes(&self) -> &[KeyValue] {
        &self.root_attributes
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn shutdown(&mut self) {
        let Some(provider) = self.provider.take() else {
            return;
        };

        if let Some(runtime) = &self.runtime {
            let _guard = runtime.enter();
            if let Err(err) = provider.shutdown() {
                eprintln!("Warning: failed to flush telemetry: {err}");
            }
        } else if let Err(err) = provider.shutdown() {
            eprintln!("Warning: failed to flush telemetry: {err}");
        }
    }

    pub fn with_runtime_context<R>(&self, f: impl FnOnce() -> R) -> R {
        if let Some(runtime) = &self.runtime {
            let _guard = runtime.enter();
            f()
        } else {
            f()
        }
    }
}

pub fn init(root_span_name: &str, attrs: RootAttrs) -> Telemetry {
    let root_attributes = vec![
        KeyValue::new("cli.command", attrs.cli_command),
        KeyValue::new("cli.args", attrs.cli_args),
        KeyValue::new("cwd", attrs.cwd),
    ];

    let endpoint = match std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT") {
        Ok(endpoint) if !endpoint.trim().is_empty() => endpoint,
        _ => return Telemetry::disabled(root_span_name.to_string(), root_attributes),
    };

    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => {
            eprintln!("Warning: failed to create telemetry runtime: {err}");
            return Telemetry::disabled(root_span_name.to_string(), root_attributes);
        }
    };

    let exporter = {
        let _guard = runtime.enter();
        match SpanExporter::builder()
            .with_tonic()
            .with_endpoint(endpoint)
            .with_timeout(Duration::from_millis(500))
            .build()
        {
            Ok(exporter) => exporter,
            Err(err) => {
                eprintln!("Warning: failed to initialize OTLP exporter: {err}");
                return Telemetry::disabled(root_span_name.to_string(), root_attributes);
            }
        }
    };

    let service_name =
        std::env::var("OTEL_SERVICE_NAME").unwrap_or_else(|_| env!("CARGO_PKG_NAME").to_string());
    let resource = Resource::builder_empty()
        .with_attributes(vec![
            KeyValue::new("service.name", service_name),
            KeyValue::new("service.version", env!("CARGO_PKG_VERSION")),
        ])
        .build();

    let provider = SdkTracerProvider::builder()
        .with_resource(resource)
        .with_sampler(resolve_sampler())
        .with_simple_exporter(exporter)
        .build();

    let tracer = provider.tracer(env!("CARGO_PKG_NAME"));
    Telemetry {
        enabled: true,
        tracer: Some(tracer),
        provider: Some(provider),
        runtime: Some(runtime),
        root_span_name: root_span_name.to_string(),
        root_attributes,
    }
}

pub fn mask_cli_args(args: &[String]) -> String {
    let mut masked = Vec::with_capacity(args.len());
    let mut redact_next = false;

    for arg in args {
        if redact_next {
            masked.push("***".to_string());
            redact_next = false;
            continue;
        }

        let lower = arg.to_ascii_lowercase();
        if let Some((key, _)) = lower.split_once('=') {
            if is_sensitive_key(key) {
                masked.push(format!("{}=***", &arg[..key.len()]));
                continue;
            }
        }

        if is_sensitive_key(&lower) {
            masked.push(arg.clone());
            redact_next = true;
        } else {
            masked.push(arg.clone());
        }
    }

    masked.join(" ")
}

fn resolve_sampler() -> Sampler {
    let sampler_name = std::env::var("OTEL_TRACES_SAMPLER")
        .unwrap_or_else(|_| "parentbased_traceidratio".to_string())
        .to_ascii_lowercase();

    let sampler_arg = std::env::var("OTEL_TRACES_SAMPLER_ARG").ok();
    let ratio = sampler_arg
        .as_deref()
        .and_then(|value| value.parse::<f64>().ok())
        .unwrap_or(1.0)
        .clamp(0.0, 1.0);

    match sampler_name.as_str() {
        "always_on" => Sampler::AlwaysOn,
        "always_off" => Sampler::AlwaysOff,
        "traceidratio" => Sampler::TraceIdRatioBased(ratio),
        "parentbased_always_on" => Sampler::ParentBased(Box::new(Sampler::AlwaysOn)),
        "parentbased_always_off" => Sampler::ParentBased(Box::new(Sampler::AlwaysOff)),
        "parentbased_traceidratio" => {
            Sampler::ParentBased(Box::new(Sampler::TraceIdRatioBased(ratio)))
        }
        _ => Sampler::ParentBased(Box::new(Sampler::TraceIdRatioBased(1.0))),
    }
}

fn is_sensitive_key(key: &str) -> bool {
    matches!(
        key,
        "--token"
            | "-t"
            | "--password"
            | "-p"
            | "--secret"
            | "--api-key"
            | "--apikey"
            | "--auth"
            | "--access-token"
            | "--bearer-token"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mask_cli_args_sensitive_flags() {
        let args = vec![
            "apply".to_string(),
            "--token".to_string(),
            "abc123".to_string(),
            "--package".to_string(),
            "moonbitlang/core".to_string(),
            "--api-key=xyz".to_string(),
        ];

        let masked = mask_cli_args(&args);
        assert!(masked.contains("--token ***"));
        assert!(masked.contains("--api-key=***"));
        assert!(masked.contains("--package moonbitlang/core"));
    }
}
