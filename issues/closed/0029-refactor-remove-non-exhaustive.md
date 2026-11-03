# 公開型の #[non_exhaustive] を削除して shiguredo-rust 規約に準拠する

- Priority: Medium
- Created: 2026-08-02
- Completed: 2026-11-04
- Model: DeepSeek V4 Flash
- Branch: feature/refactor-remove-non-exhaustive
- Polished: {YYYY-MM-DD}

## 目的

公開型に付与されている `#[non_exhaustive]` を削除し、shiguredo-rust 規約に準拠する。規約は「`#[non_exhaustive]` を使わないこと。どうしても必要な場合は許可を得ること」と定めている。issue 0018 で「分離候補」として切り出されたものである。

## 現状

`#[non_exhaustive]` が付与されている公開型は以下。

- `src/decode.rs` の `DecoderConfig`
- `src/encode.rs` の `EncoderConfig`
- `src/adapter.rs` の `AdapterSelector` / `MediaAdapterType` / `PciAddress` / `AdapterInfo`

これらの型は `src/lib.rs` で `pub use` されており、外部クレートから利用される公開 API である。`#[non_exhaustive]` により、利用側は `match` の網羅性チェックの恩恵を失い、ワイルドカードパターンや構造体リテラルの構築制約を強制される。

issue 0018 は「`DecoderConfig` の `#[non_exhaustive]` 削除は本 issue の目的（型の統合）と無関係な別目的の作業であるため、分離候補として『公開型の `#[non_exhaustive]` を削除して shiguredo-rust 規約に準拠する』別 issue に切り出すことを検討する（`EncoderConfig` も含めて対応）」としている。

## 設計方針

- 公開型の `#[non_exhaustive]` をすべて削除する。
- 将来 variant や field を追加するときは、規約どおり素直に破壊的変更（`[CHANGE]`）として扱う。
- 削除に伴う外部 API の破壊的変更はない（`#[non_exhaustive]` の削除は利用側の制約を緩める方向であり、後方互換を壊さない）。

## 完了条件

以下すべてを満たす。

1. 公開型（`DecoderConfig` / `EncoderConfig` / `AdapterSelector` / `MediaAdapterType` / `PciAddress` / `AdapterInfo`）の `#[non_exhaustive]` がすべて削除されている。
2. `cargo test` と `cargo clippy --workspace --all-targets -- -D warnings` が pass する。
3. `CHANGES.md` の `## develop` の `### misc` サブセクションに `[UPDATE]` として追記する（ライブラリ機能に直接影響しないため）。

## 解決方法

公開型から `#[non_exhaustive]` をすべて削除し、shiguredo-rust 規約に準拠した。

### 変更内容

- `src/decode.rs` の `DecoderConfig` から `#[non_exhaustive]` を削除した
- `src/encode.rs` の `EncoderConfig` から `#[non_exhaustive]` を削除した
- `src/adapter.rs` の `AdapterSelector` / `MediaAdapterType` / `PciAddress` / `AdapterInfo` から `#[non_exhaustive]` を削除した
- `CHANGES.md` の `## develop` の `### misc` に `[UPDATE]` として追記した

### 確認

`shiguredo-gmktec-k9` の `~/melpon/vpl-rs` で確認した。外部クレートと同じ制約で公開型を利用できることも、`examples/` に一時的な確認用ファイルを置いて `cargo check --example verify_0029` で確認した（確認後に削除）。

| 確認内容 | 結果 |
| --- | --- |
| `cargo fmt --all -- --check` | pass |
| `cargo clippy --workspace --all-targets -- -D warnings` | pass |
| `cargo test --workspace -- --test-threads=1` | pass（単体テスト 17 件、`tests/test_adapter.rs` 5 件、`tests/test_roundtrip.rs` 21 件、doc テスト 2 件） |
| `INTEL_VPL=1 cargo test --workspace -- --test-threads=1` | pass（単体テスト 18 件、`tests/test_adapter.rs` 6 件、`tests/test_roundtrip.rs` 21 件、doc テスト 2 件） |
| 外部クレート相当の構造体リテラル構築・構造体更新記法・網羅 match | pass |

## 参考

- 関連 issue: 0018（コーデック識別型とプロファイル enum の統合。本 issue を分離候補として切り出した）
