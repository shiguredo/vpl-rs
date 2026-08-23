# ci ジョブが cargo test を実行しないため GPU 非依存テストの回帰検知が実機 CI 依存になっている

- Priority: High
- Created: 2026-07-01
- Model: Opus 4.7
- Branch: feature/fix-ci-run-cargo-test-in-non-gpu-job
- Polished: 2026-08-02

## 目的

`.github/workflows/ci.yml` の `ci` ジョブ（GPU 不要、Ubuntu 22.04 / 24.04 / 26.04）が `cargo test` を実行しておらず、GPU 非依存のユニットテスト・統合テストが実機セルフホストランナー (`test-intel-vpl`) が動くまで検証されない。実機ランナー障害時にプルリクを弾けない状態を解消する。段階 1 では GPU 非依存の単体テスト（`cargo test --lib`）の回帰検知と、issue 0021 の PBT 基盤（`cargo test -p pbt`）の CI 実行を `ci` ジョブで実現する（`tests/` 込みの段階 2 は別 issue）。

## 優先度根拠

High。以下による。

- **リグレッション検知の穴**: `src/error.rs::test_*` / `src/vpl.rs::frame_surface_new_rejects_null` / `src/encode.rs::pending_frame_store_takes_by_frame_seq` / `src/encode.rs::worker_wait_idle_returns_error_when_pending_remains` / `src/encode.rs::worker_stop_returns_aborted_for_all_pending` / `tests/test_adapter.rs::test_*_rejects_render_node_zero` などの GPU 非依存テストが GPU 不要の `ci` ジョブでは実行されない（実機ランナー稼働中は `test-intel-vpl` ジョブの `cargo test --workspace` で実行される）。
- **セルフホストランナー障害への耐性ゼロ**: 実機ランナーが落ちるとテスト全体が回らないため、PR のマージ判定ができなくなる（現状は無視して人力判断になっている）。
- **CLAUDE.md 規約違反**: 「Don't live with broken windows」「If it hurts, do it more often」の観点で、テストを走らせない CI ジョブは「壊れた窓」に該当する。
- **修正コスト小**: ci.yml に `cargo test --lib` を 1 行追加するだけで段階 1 の大部分が改善する。

## 現状

### ci ジョブの実装

`.github/workflows/ci.yml` の `ci` ジョブ:

```yaml
ci:
  name: CI
  strategy:
    matrix:
      os:
        - ubuntu-26.04
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

`.github/workflows/ci.yml` の `test-intel-vpl` ジョブ:

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

Linux ホストで GPU なしでもコンパイル・実行できるテストは 15 件（issue 0023 適用前の現状コードに基づく。0023 の実装結果によって内訳は変動し得る）:

- `src/error.rs::test_new_custom_display`
- `src/error.rs::test_check_mfx_success`
- `src/error.rs::test_check_mfx_error`
- `src/error.rs::test_check_mfx_allow_warn_success`
- `src/error.rs::test_check_mfx_allow_warn_warning`
- `src/error.rs::test_check_mfx_allow_warn_error`
- `src/vpl.rs::frame_surface_new_rejects_null`
- `src/encode.rs::pending_frame_store_takes_by_frame_seq`
- `src/encode.rs::worker_wait_idle_returns_error_when_pending_remains`
- `src/encode.rs::worker_stop_returns_aborted_for_all_pending`
- `tests/test_adapter.rs::test_list_adapters_sorted_and_deduped`（issue 0023 の silent pass 問題あり）
- `tests/test_adapter.rs::test_encoder_rejects_render_node_zero`
- `tests/test_adapter.rs::test_decoder_rejects_render_node_zero`
- `tests/test_adapter.rs::test_supported_codecs_rejects_render_node_zero`
- `tests/test_adapter.rs::test_encoder_not_found_for_invalid_render_node`

これらは `#[cfg(intel_vpl)]` ガードが付いていないので、GPU なし Linux でも実行される。なお `src/vpl.rs::frame_surface_gpu_required` は GPU 必須テストのため一覧外であり、GPU なしでは silent skip する挙動を持つ（issue 0023 で `#[cfg(intel_vpl)]` を付与する）。また `tests/test_roundtrip.rs` の全テストは実機必須（GPU なしでは panic）のため一覧外であり、段階 2 の `#[cfg(intel_vpl)]` 分離の対象に含まれる。

## 設計方針

### ci ジョブで `cargo test --lib` を実行する（段階 1・推奨）

`cargo test --lib` は `src/` 配下の `#[cfg(test)] mod tests` のみを走らせる（`tests/` は走らせない）。GPU 非依存の単体テスト（`src/error.rs` / `src/vpl.rs` / `src/encode.rs` 配下の 10 件）がカバーされる。

```yaml
- run: cargo test --lib
```

注: `tests/test_adapter.rs` 配下の GPU 非依存テスト 5 件は `cargo test --lib` では実行されない。`cargo test` 全体（`tests/` 込み）の CI 組込み（段階 2）は、実機必須テストの `#[cfg(intel_vpl)]` 分離が必要になるため、issue 0023 完了後に別 issue で対応する（分離候補）。0023 の完了条件 3 は `test_list_adapters_sorted_and_deduped` に `#[cfg(intel_vpl)]` を付与するため、0023 完了後は `tests/test_adapter.rs` の GPU 非依存テスト数が 4 件になる。段階 2 の別 issue では 4 件を前提に構成する。

### workspace オプション

現状の `--workspace` は単一クレートで意味を持たない。本 issue の前提である issue 0021（PBT 基盤 + workspace 化）適用後は `pbt/` を含む workspace に対して意味を持つため、`--workspace` は変更しない（本 issue のスコープ外）。

## 完了条件

以下すべてを満たす（段階 1）。

前提: issue 0023（silent pass テスト修正）が完了していること。`frame_surface_gpu_required` に `#[cfg(intel_vpl)]` が付与済みであること。また、`cargo test -p pbt` は issue 0021（PBT 基盤）適用後に追加するため、0021 が先に完了していること（0022 は 0021 完了が前提である一方、0021 側は 0022 の状態（open / closed）に依存せず完了条件 8 で分岐を定義している）。

1. `.github/workflows/ci.yml` の `ci` ジョブに `cargo test --lib` ステップを追加する（`cargo clippy` の後が適切）。
2. `cargo test --lib` が Ubuntu 22.04 / 24.04 / 26.04 のすべてで pass する。
3. `.github/workflows/ci.yml` の `ci` ジョブに `cargo test -p pbt` ステップを追加し、Ubuntu 22.04 / 24.04 / 26.04 のすべてで pass する（issue 0021 の依頼事項。0021 適用後の PBT が CI で実行される。配置は `cargo test --lib` の直後が適切）。
4. `CHANGES.md` の `## develop` の `### misc` サブセクションに `[ADD]` として追記する（CI の変更はライブラリ機能に直接影響しないため）。

注: `--workspace` オプションは変更しない（issue 0021 適用後は意味を持つため。「設計方針」参照）。

段階 2（`cargo test` 全体の CI 組込み）は本 issue のスコープ外であり、issue 0023 完了後に別 issue で対応する（分離候補）。

## 影響範囲

- `.github/workflows/ci.yml`
- `CHANGES.md`

## 前提条件 / 依存関係

- issue 0023（silent pass テスト）: `frame_surface_gpu_required` への `#[cfg(intel_vpl)]` 付与が完了条件の前提
- issue 0021（PBT 基盤）: `cargo test -p pbt` の CI 追加は 0021 適用後（0021 の完了条件 8 で定義された分岐）

## 参考

- CLAUDE.md「Don't live with broken windows」「If it hurts, do it more often」
- 関連 issue: 0023（silent pass テスト）、0021（PBT/Fuzz と workspace 構成。`cargo test -p pbt` の CI 実行を本 issue へ依頼している）
