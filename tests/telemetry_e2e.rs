// SPDX-License-Identifier: MIT

use opentelemetry_proto::tonic::collector::trace::v1::trace_service_server::{
    TraceService, TraceServiceServer,
};
use opentelemetry_proto::tonic::collector::trace::v1::{
    ExportTraceServiceRequest, ExportTraceServiceResponse,
};
use opentelemetry_proto::tonic::trace::v1::status::StatusCode;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tempfile::TempDir;
use tokio::net::TcpListener;
use tokio::sync::{oneshot, Mutex};
use tokio_stream::wrappers::TcpListenerStream;
use tonic::{Request, Response, Status};

#[derive(Clone, Default)]
struct MockCollector {
    requests: Arc<Mutex<Vec<ExportTraceServiceRequest>>>,
}

#[tonic::async_trait]
impl TraceService for MockCollector {
    async fn export(
        &self,
        request: Request<ExportTraceServiceRequest>,
    ) -> Result<Response<ExportTraceServiceResponse>, Status> {
        self.requests.lock().await.push(request.into_inner());
        Ok(Response::new(ExportTraceServiceResponse {
            partial_success: None,
        }))
    }
}

struct CollectorHandle {
    endpoint: String,
    requests: Arc<Mutex<Vec<ExportTraceServiceRequest>>>,
    shutdown_tx: Option<oneshot::Sender<()>>,
    join: Option<tokio::task::JoinHandle<()>>,
}

impl CollectorHandle {
    async fn shutdown(mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        if let Some(join) = self.join.take() {
            let _ = join.await;
        }
    }
}

#[derive(Debug, Clone)]
struct SpanView {
    name: String,
    span_id: Vec<u8>,
    parent_span_id: Vec<u8>,
    status_code: i32,
    event_names: Vec<String>,
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exports_expected_spans_for_scan() {
    let collector = start_collector().await;
    let fixture = TestFixture::new().expect("failed to create fixture");

    let output = run_cli(
        fixture.root(),
        fixture.moon_bin_dir(),
        &collector.endpoint,
        &[
            "scan",
            "--root",
            fixture.project_dir().to_string_lossy().as_ref(),
        ],
    );

    assert!(
        output.status.success(),
        "scan should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let spans = wait_for_spans(&collector.requests, Duration::from_secs(5)).await;
    assert!(!spans.is_empty(), "no spans exported");

    let root = spans
        .iter()
        .find(|s| s.name == "cli.scan")
        .expect("missing cli.scan root span");
    assert_eq!(
        root.parent_span_id.len(),
        0,
        "root span should not have parent"
    );

    for child in [
        "parse_args",
        "load_config",
        "load_inputs",
        "main_operation",
        "write_outputs",
    ] {
        let span = spans
            .iter()
            .find(|s| s.name == child)
            .unwrap_or_else(|| panic!("missing child span: {child}"));
        assert_eq!(
            span.parent_span_id, root.span_id,
            "child span {child} should be attached to root"
        );
    }

    collector.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn marks_error_span_when_scan_fails() {
    let collector = start_collector().await;
    let fixture = TestFixture::new().expect("failed to create fixture");
    let missing_root = fixture.root().join("does-not-exist");

    let output = run_cli(
        fixture.root(),
        fixture.moon_bin_dir(),
        &collector.endpoint,
        &["scan", "--root", missing_root.to_string_lossy().as_ref()],
    );

    assert_eq!(
        output.status.code(),
        Some(1),
        "failure should preserve non-zero exit code"
    );

    let spans = wait_for_spans(&collector.requests, Duration::from_secs(5)).await;
    assert!(!spans.is_empty(), "no spans exported on error path");

    let root = spans
        .iter()
        .find(|s| s.name == "cli.scan")
        .expect("missing cli.scan root span on error");
    assert_eq!(
        root.status_code,
        StatusCode::Error as i32,
        "root span should be marked error"
    );
    assert!(
        root.event_names.iter().any(|name| name == "exception"),
        "root span should include exception event"
    );

    let load_inputs = spans
        .iter()
        .find(|s| s.name == "load_inputs")
        .expect("missing load_inputs span on error");
    assert_eq!(
        load_inputs.status_code,
        StatusCode::Error as i32,
        "load_inputs should be marked error"
    );
    assert!(
        load_inputs
            .event_names
            .iter()
            .any(|name| name == "exception"),
        "load_inputs should include exception event"
    );

    collector.shutdown().await;
}

async fn start_collector() -> CollectorHandle {
    let collector = MockCollector::default();
    let requests = collector.requests.clone();
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("failed to bind OTLP test server");
    let addr = listener.local_addr().expect("missing local addr");
    let incoming = TcpListenerStream::new(listener);
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();

    let join = tokio::spawn(async move {
        tonic::transport::Server::builder()
            .add_service(TraceServiceServer::new(collector))
            .serve_with_incoming_shutdown(incoming, async {
                let _ = shutdown_rx.await;
            })
            .await
            .expect("failed to run OTLP test server");
    });

    CollectorHandle {
        endpoint: format!("http://{addr}"),
        requests,
        shutdown_tx: Some(shutdown_tx),
        join: Some(join),
    }
}

async fn wait_for_spans(
    requests: &Arc<Mutex<Vec<ExportTraceServiceRequest>>>,
    timeout: Duration,
) -> Vec<SpanView> {
    let deadline = Instant::now() + timeout;
    loop {
        let snapshot = requests.lock().await.clone();
        let spans = flatten_spans(&snapshot);
        if !spans.is_empty() {
            return spans;
        }

        if Instant::now() >= deadline {
            return spans;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn flatten_spans(requests: &[ExportTraceServiceRequest]) -> Vec<SpanView> {
    let mut spans = Vec::new();
    for request in requests {
        for resource_spans in &request.resource_spans {
            for scope_spans in &resource_spans.scope_spans {
                for span in &scope_spans.spans {
                    spans.push(SpanView {
                        name: span.name.clone(),
                        span_id: span.span_id.clone(),
                        parent_span_id: span.parent_span_id.clone(),
                        status_code: span.status.as_ref().map(|s| s.code).unwrap_or_default(),
                        event_names: span.events.iter().map(|e| e.name.clone()).collect(),
                    });
                }
            }
        }
    }
    spans
}

struct TestFixture {
    _tmp: TempDir,
    root: PathBuf,
    project_dir: PathBuf,
    moon_bin_dir: PathBuf,
}

impl TestFixture {
    fn new() -> std::io::Result<Self> {
        let tmp = tempfile::tempdir()?;
        let root = tmp.path().to_path_buf();
        let project_dir = root.join("project");
        fs::create_dir_all(&project_dir)?;
        fs::write(
            project_dir.join("moon.mod.json"),
            r#"{"deps":{"moonbitlang/core":"0.1.0"}}"#,
        )?;

        let moon_bin_dir = root.join("bin");
        fs::create_dir_all(&moon_bin_dir)?;
        create_mock_moon_binary(&moon_bin_dir)?;

        Ok(Self {
            _tmp: tmp,
            root,
            project_dir,
            moon_bin_dir,
        })
    }

    fn root(&self) -> &Path {
        &self.root
    }

    fn project_dir(&self) -> &Path {
        &self.project_dir
    }

    fn moon_bin_dir(&self) -> &Path {
        &self.moon_bin_dir
    }
}

fn create_mock_moon_binary(bin_dir: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let script_path = bin_dir.join("moon");
        fs::write(
            &script_path,
            r#"#!/bin/sh
if [ "$1" = "version" ]; then
  echo "moon 0.0.0"
  exit 0
fi
echo "unsupported in test: $@" >&2
exit 1
"#,
        )?;
        let mut perms = fs::metadata(&script_path)?.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(script_path, perms)?;
    }

    #[cfg(windows)]
    {
        let script_path = bin_dir.join("moon.bat");
        fs::write(
            &script_path,
            r#"@echo off
if "%1"=="version" (
  echo moon 0.0.0
  exit /b 0
)
echo unsupported in test: %* 1>&2
exit /b 1
"#,
        )?;
    }

    Ok(())
}

fn run_cli(root: &Path, moon_bin_dir: &Path, endpoint: &str, args: &[&str]) -> Output {
    let bin = env!("CARGO_BIN_EXE_moon-dst");
    let mut cmd = std::process::Command::new(bin);
    cmd.args(args)
        .current_dir(root)
        .env("OTEL_EXPORTER_OTLP_ENDPOINT", endpoint)
        .env("OTEL_SERVICE_NAME", "moon-dst-e2e")
        .env("PATH", build_path_with_prefix(moon_bin_dir));

    cmd.output().expect("failed to run moon-dst")
}

fn build_path_with_prefix(prefix: &Path) -> OsString {
    let existing = std::env::var_os("PATH").unwrap_or_default();
    let mut paths = vec![prefix.to_path_buf()];
    paths.extend(std::env::split_paths(&existing));
    std::env::join_paths(paths).expect("failed to build PATH for e2e test")
}
