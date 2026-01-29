# moon-dst (moon dust) — Rust CLI 仕様書
SPDX-License-Identifier: MIT

## 1. 目的
MoonBit プロジェクト群に対して、`moon.mod.json` の依存を **強制的に更新（上げる）**ための CLI。

- `moon add <package>` を繰り返し実行し、新しいバージョンがあれば上げる
- 初回に `moon update` を実行
- `moon.mod.json` を **再帰的に探索**
- おまけ機能：`justfile` がない repo にはテンプレ `justfile` を追加

CLI 名称：
- **コマンド名**：`moon-dst`
- **読み**：moon dust（依存を“払う”イメージ）

---

## 2. 対象・前提
- 対象ファイル：`moon.mod.json`
- 依存定義：`deps` オブジェクトのキー
- `moon` CLI が PATH 上に存在すること

---

## 3. CLI 概要

### サブコマンド
- `scan` : 対象 `moon.mod.json` を再帰探索し、依存一覧を出力
- `apply`: 実際に `moon update` / `moon add` を実行
- `just` : `justfile` 追加のみ

---

## 4. オプション仕様

### 共通オプション
- `--root <PATH>`: 探索ルート（デフォルト: `.`）
- `--ignore <PATH|NAME>`: 無視パス/ディレクトリ（複数指定可）
- `--no-default-ignore`: デフォルト除外ルールを無効化
- `--jobs <N>`: 並列数（デフォルト: 論理 CPU / 2）
- `--dry-run`: 実行予定コマンドのみ表示
- `--verbose`: 詳細ログ
- `--json`: JSON 出力（scan 用）

### `apply` 専用
- `--skip-update`: 初回 `moon update` をスキップ
- `--repeat <N>`: `moon add` の繰り返し回数（デフォルト: 1）
- `--package <NAME>`: 特定 package のみ対象（複数可）
- `--fail-fast`: 失敗時に即終了
- `--write-justfile`: `justfile` がない repo に追加
- `--justfile-mode <skip|create|merge>`: デフォルト `create`

### `just` 専用
- `--mode <create|merge>`: デフォルト `create`

---

## 5. デフォルト除外ルール
探索時に以下は自動で除外（`--no-default-ignore` で無効化）

- `.git/`
- `.moon/`
- `.mooncakes/`
- `target/`
- `node_modules/`
- `dist/`
- `build/`
- `vendor/`

---

## 6. フロー定義

### 6.1 scan フロー
1. `--root` から再帰的に `moon.mod.json` を探索
2. JSON としてパース
3. `deps` オブジェクトのキーを抽出
4. 以下を出力：
   - repo ルート
   - `moon.mod.json` のパス
   - package 一覧

---

### 6.2 apply フロー（基本）
repo 単位で以下を実行：

1. 初回 `moon update`
2. `moon.mod.json` を再帰的に取得
3. `deps` のキーを列挙
4. 各 package に対して `moon add <package>` を実行
5. （任意）`justfile` を追加

---

## 7. repo ルート判定ルール
1. `moon.mod.json` のディレクトリから親方向に `.git/` を探索
2. 見つかればそこを repo ルート
3. 見つからなければ `moon.mod.json` のあるディレクトリを repo とする

---

## 8. 実行コマンド仕様

### 8.1 moon update
- 実行場所：repo ルート
- コマンド：`moon update`

### 8.2 moon add
- 実行場所：repo ルート
- コマンド：`moon add <package>`
- 既存依存があっても **更新目的で実行**

---

## 9. justfile 追加仕様（おまけ）

### 9.1 条件
- repo ルートに `justfile` が存在しない場合のみ（create）

### 9.2 追加テンプレ
```just
# https://github.com/mizchi/moonbit-template
# SPDX-License-Identifier: MIT
# MoonBit Project Commands

target := "js"

default: check test

fmt:
    moon fmt

check:
    moon check --deny-warn --target {{target}}

test:
    moon test --target {{target}}

test-update:
    moon test --update --target {{target}}

run:
    moon run src/main --target {{target}}

info:
    moon info

clean:
    moon clean

release-check: fmt info check test
```

---

## 10. 出力仕様

### 標準出力（人間向け）
- repo 単位の成功/失敗サマリ
- 実行した package 数
- 失敗 package 一覧

### JSON 出力（--json）
```json
{
  "repos": [
    {
      "repo_root": "/path/to/repo",
      "moon_mods": [
        {
          "path": "moon.mod.json",
          "deps": ["moonbitlang/core"]
        }
      ]
    }
  ]
}
```

---

## 11. エラーハンドリング
- JSON パース失敗：repo 単位でスキップ（fail-fast なら終了）
- `moon` 不在：起動時に即エラー
- `moon add` 失敗：package 名と exit code を記録

---

## 12. Rust 実装方針（非コード）
- CLI: `clap` v4
- JSON: `serde`, `serde_json`
- 探索: `walkdir`
- 並列: `rayon`（repo 単位）
- プロセス実行: `std::process::Command`

---

## 13. 受け入れ基準
- [ ] `scan` が依存一覧を正しく列挙できる
- [ ] `apply` が `moon update` → `moon add` を実行できる
- [ ] `.mooncakes` が探索対象から除外されている
- [ ] `justfile` がない repo にテンプレが追加される
- [ ] 失敗があれば終了コードが非 0 になる

---

## 14. 利用例
- 探索のみ  
  `moon-dst scan --root .`

- 実行  
  `moon-dst apply --root .`

- dry-run  
  `moon-dst apply --dry-run --verbose`

- 特定 package  
  `moon-dst apply --package moonbitlang/core`

- justfile 追加込み  
  `moon-dst apply --write-justfile`
