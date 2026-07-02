# ci ジョブが cargo test を実行しないため GPU 非依存テストの回帰検知が実機 CI 依存になっている

- Priority: High
- Created: 2026-07-01
- Model: Opus 4.7
- Branch: feature/fix-ci-run-cargo-test-in-non-gpu-job
- Polished: 2026-07-01

## 目的

`.github/workflows/ci.yml` の `ci` ジョブ（GPU 不要、Ubuntu 22.04 / 24.04）が `cargo test` を実行しておらず、GPU 非依存のユニットテスト・統合テストが実機セルフホストランナー (`test-intel-vpl`) が動くまで検証されない。実機ランナー障害時にプルリクを弾けない状態を解消する。

## 優先度根拠

High。以下による。

- **リグレッション検知の穴**: `src/error.rs::test_*` / `src/vpl.rs::frame_surface_new_rejects_null` / `src/encode.rs::pending_frame_store_takes_by_frame_seq` / `src/encode.rs::worker_wait_idle_returns_error_when_pending_remains` / `src/encode.rs::worker_stop_returns_aborted_for_all_pending` / `tests/test_adapter.rs::test_*_rejects_render_node_zero` などの GPU 非依存テストが CI で機械的に走らない。
- **セルフホストランナー障害への耐性ゼロ**: 実機ランナーが落ちるとテスト全体が回らないため、PR のマージ判定ができなくなる（現状は無視して人力判断になっている）。
- **CLAUDE.md 規約違反**: 「Don't live with broken windows」「If it hurts, do it more often」の観点で、テストを走らせない CI ジョブは「壊れた窓」に該当する。
- **修正コスト小**: ci.yml に `cargo test` を 1 行追加するだけで大部分が改善する。

## 現状

### ci ジョブの実装

`.github/workflows/ci.yml:19-37`:

```yaml
ci:
  name: CI
  strategy:
    matrix:
      os:
        - ubuntu-24.04
        - ubuntu-22.04
  runs-on: ${{ matrix.os }}
  timeout-minutes: 20
  steps:
    - uses: actions/checkout@...
    - run: rustup update stable
    - uses: shiguredo/github-actions/.github/actions/rust-cache@main
      with:
        os: ${{ matrix.os }}
        toolchain: stable
    - run: cargo fmt --all --check
    - run: cargo build --workspace
    - run: cargo clippy --workspace -- -D warnings
```

`cargo test` が呼ばれていない。

### test-intel-vpl ジョブは GPU 必要

`.github/workflows/ci.yml:40-58`:

```yaml
test-intel-vpl:
  name: Test (Intel GPU)
  runs-on:
    group: Self
    labels: [self-hosted, linux, x64, Intel-VPL]
  timeout-minutes: 15
  env:
    INTEL_VPL: 1
  steps:
    # ...
    - run: cargo test --workspace -- --test-threads=1 --nocapture
```

`INTEL_VPL=1` で `#[cfg(intel_vpl)]` テストも含めて全て実行するが、セルフホストランナーが必要。

### GPU 非依存テストの一覧

Linux ホストで GPU なしでもコンパイル・実行できるテスト:

- `src/error.rs::test_new_custom_display`
- `src/error.rs::test_check_mfx_success`
- `src/error.rs::test_check_mfx_error`
- `src/error.rs::test_check_mfx_allow_warn_*`
- `src/vpl.rs::frame_surface_new_rejects_null`
- `src/encode.rs::pending_frame_store_takes_by_frame_seq`
- `src/encode.rs::worker_wait_idle_returns_error_when_pending_remains`
- `src/encode.rs::worker_stop_returns_aborted_for_all_pending`
- `tests/test_adapter.rs::test_list_adapters_sorted_and_deduped`（issue 0023 の silent pass 問題あり）
- `tests/test_adapter.rs::test_encoder_rejects_render_node_zero`
- `tests/test_adapter.rs::test_decoder_rejects_render_node_zero`
- `tests/test_adapter.rs::test_supported_codecs_rejects_render_node_zero`
- `tests/test_adapter.rs::test_encoder_not_found_for_invalid_render_node`

これらは `#[cfg(intel_vpl)]` ガードが付いていないので、GPU なし Linux でも実行される（`vpl.rs::frame_surface_gpu_required` は silent skip する挙動、issue 0023 参照）。

## 設計方針

### 案 A: ci ジョブで `cargo test --lib` を実行する（推奨・段階 1）

`cargo test --lib` は `src/` 配下の `#[cfg(test)] mod tests` のみを走らせる（`tests/` は走らせない）。GPU 非依存の単体テストがカバーされる。

```yaml
- run: cargo test --lib
```

### 案 B: ci ジョブで `cargo test` を実行する（段階 2）

`tests/test_adapter.rs` の一部（`test_*_rejects_render_node_zero` など）も GPU 非依存で走る。ただし `tests/test_roundtrip.rs` は実機必須のテストが多数含まれるので、`#[cfg(intel_vpl)]` で分離する必要がある。

```yaml
- run: cargo test
```

現状のテストコードでは `tests/test_adapter.rs` は `#![cfg(target_os = "linux")]` のみで、`intel_vpl` cfg 分離が不完全（`test_real_adapter_session` のみ `#[cfg(intel_vpl)]`）。実機必須テストと GPU 非依存テストの整理が必要。

### 案 C: `cargo test --lib` と `cargo test --tests test_adapter -- --skip test_real_adapter_session` を組み合わせる

- 短所: 実行テスト名の管理が煩雑

推奨は **段階 1 で案 A、段階 2 で案 B**。段階 2 は issue 0023（silent pass テスト）とセットで対応する。

### workspace オプション

現状の `--workspace` は単一クレートで意味を持たないので、この機会に削除する。

## 完了条件

以下すべてを満たす（段階 1）。

前提: issue 0023（silent pass テスト修正）が完了していること。`frame_surface_gpu_required` に `#[cfg(intel_vpl)]` が付与済みであること。

1. `.github/workflows/ci.yml` の `ci` ジョブに `cargo test --lib` ステップを追加する（`cargo clippy` の後が適切）。
2. `cargo test --lib` が Ubuntu 22.04 / 24.04 の両方で pass する。
3. `CHANGES.md` の `## develop` に `[UPDATE]` として追記する。

注: `--workspace` オプションの削除は issue 0021（workspace 化）の進捗に依存するため本 issue のスコープ外とする。

段階 2 の完了条件（`cargo test` 全体の CI 組み込み）は issue 0023 の完了後に別途検討する。

## 影響範囲

- `.github/workflows/ci.yml`
- `Makefile` / `prek.toml`（`--workspace` 削除する場合）
- `CHANGES.md`

## 前提条件 / 依存関係

- issue 0023（silent pass テスト）と併せて段階 2 を進めると効率的

## 参考

- CLAUDE.md「Don't live with broken windows」「If it hurts, do it more often」
- 関連 issue: 0023（silent pass テスト）、0021（PBT/Fuzz と workspace 構成）
