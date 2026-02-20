jaeger-up:
    docker compose up -d

jaeger-down:
    docker compose down

trace-scan:
    OTEL_SERVICE_NAME=moon-dst OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4317 cargo run -- scan --root .

trace-ui:
    if command -v open >/dev/null 2>&1; then open http://localhost:16686; elif command -v xdg-open >/dev/null 2>&1; then xdg-open http://localhost:16686; else echo "Open http://localhost:16686 in your browser"; fi
