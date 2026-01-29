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

## ライセンス

MIT


