# ci ジョブで tests/ 込みの cargo test 全体を実行できるようにする

- Priority: Medium
- Created: 2026-08-02
- Completed: {YYYY-MM-DD}
- Model: DeepSeek V4 Flash
- Branch: feature/fix-ci-run-full-cargo-test-in-non-gpu-job
- Polished: {YYYY-MM-DD}

## 目的

`.github/workflows/ci.yml` の `ci` ジョブ（GPU 不要）で `cargo test` 全体（`tests/` 込み）を実行し、`tests/test_adapter.rs` 配下の GPU 非依存テストも機械的に検証できるようにする。現状は `cargo test --lib`（段階 1）では `src/` 配下の単体テストしか実行されず、`tests/` 配下の統合テストは実機ジョブ `test-intel-vpl` が動くまで検証されない。これは issue 0022 で「段階 2」として本 issue に切り出された作業である。

## 現状

- `ci` ジョブは issue 0022 の段階 1 により `cargo test --lib` を実行する（0022 適用後）。`tests/` 込みの `cargo test` は実行されない。
- `tests/test_roundtrip.rs` には `#[test]` が 13 件あるが、すべて実機 GPU 必須（GPU なしでは panic）であり、`#[cfg(intel_vpl)]` ガードが付いていない。
- `tests/test_adapter.rs` の GPU 非依存テストは issue 0023 適用後 4 件（`test_list_adapters_sorted_and_deduped` は 0023 で `#[cfg(intel_vpl)]` 付与により実機テスト化される）。実機テストは `test_real_adapter_session` の 1 件のみ `#[cfg(intel_vpl)]` 付き。
- 実機必須テストと GPU 非依存テストの cfg 分離が不完全なため、`cargo test` 全体を `ci` ジョブで実行すると実機必須テストが GPU なし Ubuntu で panic して落ちる。

## 設計方針

1. `tests/test_roundtrip.rs` の実機必須テスト 13 件に `#[cfg(intel_vpl)]` を付与して分離する（`test-intel-vpl` ジョブでのみ実行されるようにする）。
2. `.github/workflows/ci.yml` の `ci` ジョブのステップを `cargo test --lib` から `cargo test` に変更する（あるいは `cargo test --lib` と `cargo test --tests` を組み合わせる）。issue 0023 適用後は `tests/test_adapter.rs` の GPU 非依存テスト 4 件が `ci` ジョブで実行されることを前提に構成する。
3. `src/` 配下の GPU 非依存テスト（`src/error.rs` / `src/vpl.rs` / `src/encode.rs` 配下）は `cargo test` でも引き続き実行される。

## 完了条件

以下すべてを満たす。

1. `tests/test_roundtrip.rs` の実機必須テスト 13 件に `#[cfg(intel_vpl)]` が付与されている。
2. `.github/workflows/ci.yml` の `ci` ジョブで `cargo test`（`tests/` 込み）が Ubuntu 22.04 / 24.04 / 26.04 のすべてで pass する（GPU なし環境での実機必須テストの panic が発生しない）。
3. `tests/test_adapter.rs` の GPU 非依存テスト 4 件が `ci` ジョブで実行される。
4. `CHANGES.md` の `## develop` の `### misc` サブセクションに `[ADD]` として追記する（CI の変更はライブラリ機能に直接影響しないため）。

## 解決方法

- `tests/test_roundtrip.rs` の全テスト関数に `#[cfg(intel_vpl)]` を付与する。
- `ci` ジョブの `cargo test` 実行ステップを変更する。
- `test-intel-vpl` ジョブの `cargo test --workspace -- --test-threads=1 --nocapture` は従来どおり実機テストを含めて実行する（変更不要）。

## 参考

- 関連 issue: 0022（ci ジョブの cargo test 未実行。段階 1 を実装し、本 issue を段階 2 として切り出した）、0023（silent pass テストの修正。`test_list_adapters_sorted_and_deduped` の cfg 分離を含む。本 issue の前提）
- 前提: issue 0023 が完了していること
