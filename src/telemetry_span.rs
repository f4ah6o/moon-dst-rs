// SPDX-License-Identifier: MIT
//! Thin span wrapper for low-intrusion instrumentation.

use crate::telemetry::Telemetry;
use opentelemetry::trace::{Status, TraceContextExt, Tracer};
use opentelemetry::KeyValue;
use std::fmt::Display;

pub fn with_span<T, E, F>(
    telemetry: &Telemetry,
    name: &str,
    attrs: &[KeyValue],
    f: F,
) -> Result<T, E>
where
    E: Display,
    F: FnOnce() -> Result<T, E>,
{
    if !telemetry.enabled() {
        return f();
    }

    let Some(tracer) = telemetry.tracer() else {
        return f();
    };

    telemetry.with_runtime_context(|| {
        tracer.in_span(name.to_string(), |cx| {
            let span = cx.span();
            for attr in attrs {
                span.set_attribute(attr.clone());
            }

            let result = f();
            if let Err(err) = &result {
                span.set_status(Status::error(err.to_string()));
                span.add_event(
                    "exception".to_string(),
                    vec![
                        KeyValue::new("exception.message", err.to_string()),
                        KeyValue::new("exception.type", std::any::type_name::<E>()),
                    ],
                );
            }
            result
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_with_span_noop_passthrough() {
        let telemetry = Telemetry::disabled("cli.scan".to_string(), vec![]);

        let ok = with_span::<usize, &'static str, _>(&telemetry, "main_operation", &[], || Ok(7))
            .expect("no-op span should preserve Ok value");
        assert_eq!(ok, 7);

        let err =
            with_span::<usize, &'static str, _>(&telemetry, "main_operation", &[], || Err("boom"))
                .expect_err("no-op span should preserve Err value");
        assert_eq!(err, "boom");
    }
}
