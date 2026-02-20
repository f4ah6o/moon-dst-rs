# moon-dst
<!-- bdg:begin -->
[![crates.io](https://img.shields.io/crates/v/moon-dst.svg)](https://crates.io/crates/moon-dst)
<!-- bdg:end -->

MoonBit プロジェクトの依存を強制的に更新する CLI ツール。

読み: **moon dust**（依存を"払う"イメージ）

## インストール

```bash
cargo install --path .
```

## 使い方

### scan - 依存一覧を表示

```bash
moon-dst scan --root .
moon-dst scan --json
```

### apply - 依存を更新

```bash
# 基本（moon update → moon add を実行、justfile も追加）
moon-dst apply

# dry-run で確認
moon-dst apply --dry-run --verbose

# 特定パッケージのみ
moon-dst apply --package moonbitlang/core

# justfile を追加しない
moon-dst apply --no-justfile
```

### just - justfile のみ追加

```bash
moon-dst just --root .
```

## オプション

### 共通

| オプション | 説明 |
|-----------|------|
| `--root <PATH>` | 探索ルート（デフォルト: `.`） |
| `--ignore <NAME>` | 無視するディレクトリ（複数可） |
| `--no-default-ignore` | デフォルト除外ルールを無効化 |
| `--jobs <N>` | 並列数 |
| `--dry-run` | 実行せずコマンドのみ表示 |
| `--verbose` | 詳細ログ |

### apply 専用

| オプション | 説明 |
|-----------|------|
| `--skip-update` | `moon update` をスキップ |
| `--repeat <N>` | `moon add` の繰り返し回数 |
| `--package <NAME>` | 特定パッケージのみ対象 |
| `--fail-fast` | 失敗時に即終了 |
| `--no-justfile` | justfile を追加しない |

## デフォルト除外

以下は自動的に除外される:

- `.` で始まるフォルダ（`.git`, `.moon`, `.mooncakes` など）
- `target`, `node_modules`, `dist`, `build`, `vendor`, `skills`

## Tracing (OpenTelemetry / Jaeger)

`moon-dst` は OTLP endpoint が設定されているときだけ trace を export します。  
`OTEL_EXPORTER_OTLP_ENDPOINT` 未設定時は no-op で動作し、従来の挙動を維持します。

### ローカルで確認

```bash
# Jaeger 起動
just jaeger-up

# trace 付きで scan 実行
just trace-scan

# UI を開く
just trace-ui
```

Jaeger UI: `http://localhost:16686`  
Search で service=`moon-dst`（または `OTEL_SERVICE_NAME`）を選ぶと span が確認できます。

### 主な環境変数

| 環境変数 | 説明 |
|---------|------|
| `OTEL_EXPORTER_OTLP_ENDPOINT` | OTLP exporter の endpoint（例: `http://localhost:4317`） |
| `OTEL_SERVICE_NAME` | `service.name` の上書き |
| `OTEL_TRACES_SAMPLER` | sampler 名（例: `parentbased_traceidratio`, `always_on`） |
| `OTEL_TRACES_SAMPLER_ARG` | sampler 引数（例: trace id ratio の `1.0`） |

## ライセンス

MIT

