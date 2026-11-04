# エンコーダー / デコーダーの統計 API を追加する

- Priority: Medium
- Created: 2026-11-04
- Completed: 2026-11-04
- Model: DeepSeek V4 Flash
- Branch: feature/add-encoder-decoder-stats-api
- Polished: {YYYY-MM-DD}

## 目的

利用側がバックエンドを問わず同じ観測性を得られるようにする。

nvcodec-rs は `Encoder::stats()` / `Decoder::stats()` で共有カウンタ (`Counter` / `Gauge`) を返し、video-toolbox-rs も同じ形の API を持つ。vpl-rs はエンコーダーのスナップショット (`Encoder::get_encode_stat`) しか持たず、デコーダーの統計は取得できない。この差があると、vpl バックエンドだけメトリクス収集・in-flight 制御・テストでの挙動検証ができず、バックエンド差し替えの障害になる。

## 現状

- `src/encode.rs` の `Encoder::get_encode_stat()` は `MFXVideoENCODE_GetEncodeStat` を呼び、`EncoderStats` (`mfxEncodeStat` の `NumFrame` / `NumBit` / `NumCachedFrame`) を返す。呼び出しごとのスナップショットであり、共有参照ではない。
- `src/decode.rs` の `Decoder` には統計 API が無い。
- エラー通知回数、reconfigure 成功回数、in-flight フレーム数、出力ビット総数を利用側から観測できない。
- nvcodec-rs と video-toolbox-rs の統計型は、いずれも `Counter` / `Gauge` を公開し、`stats()` が共有参照を返す。in-flight は `finish()` を挟む間隔の判断に使われている。

## 設計方針

- `src/stats.rs` を追加し、`Counter` / `Gauge` を公開する。nvcodec-rs / video-toolbox-rs と同じ API にする (`new()` / `get()` を公開し、`inc()` / `dec()` は `pub(crate)`)。
- `Encoder::stats(&self) -> &EncoderStats` / `Decoder::stats(&self) -> &DecoderStats` を追加する。統計は `Arc` でワーカースレッドと共有し、`Clone` はスナップショットを返す。
- 既存の `Encoder::get_encode_stat()` と `EncoderStats` (VPL の生統計) は削除し、`EncoderStats` を共有統計型として定義し直す。`mfxEncodeStat` の値は共有統計で代替できる。
  - `NumFrame` → `total_output_frame_count`
  - `NumBit` → `total_output_bit_count`
  - `NumCachedFrame` → `in_flight_frames`
- デコーダー側に生統計 (`MFXVideoDECODE_GetDecodeStat`) の API は追加しない。`NumSkippedFrame` は skip mode を公開していないため常に 0、`NumError` は「回復したエラー数」で共有統計の `total_error_count` (通知したエラー数) とは意味が違う。必要になった時点で別 issue にする。
- 共有統計の項目:
  - `EncoderStats`: `total_encode_count` (`encode()` が受理した回数)、`total_output_frame_count` (Ok 通知回数)、`total_error_count` (Err 通知回数)、`total_reconfigure_count` (reconfigure 成功回数)、`total_output_bit_count` (出力ビットストリームの総ビット数)、`in_flight_frames` (Gauge)
  - `DecoderStats`: `total_decode_count` (`decode()` が受理した回数)、`total_output_frame_count`、`total_error_count`、`in_flight_frames` (Gauge)
- 更新規則:
  - `in_flight_frames` は、`encode()` / `decode()` が pending を登録した時点で増やし、ワーカーが対応する結果を通知する直前に減らす。`finish()` と `reconfigure()` の drain 完了後に 0 になることを保証する。Drop 時の中断通知でも減らす。
  - `total_error_count` は `Err` を通知するたびに増やす。ドレイン時に残留 pending をエラー通知する場合も含む。
  - `total_encode_count` / `total_decode_count` は、VPL に受理された時点で増やす。入力検証エラーで拒否した場合は増やさない。
  - `total_output_bit_count` は、同期後のビットストリーム長 (`DataLength * 8`) を加算する。
- 統計は `AtomicU64` (`Ordering::Relaxed`) で保持する。フィールド間の一貫性は保証しない (`clone()` は各フィールドの現在値を個別にコピーする) ことを rustdoc に明記する。

## 完了条件

1. `Encoder::stats()` / `Decoder::stats()` から上記の値が読める。
2. `Counter` / `Gauge` が公開され、nvcodec-rs / video-toolbox-rs と同名・同義の API になっている。
3. `Encoder::get_encode_stat()` を削除し、旧 `EncoderStats` を同名の共有統計型として定義し直している。`Decoder` に生統計 API は追加しない。
4. `finish()` 完了後に `in_flight_frames` が 0 になることをテストで固定する。
5. エラー通知時に `total_error_count` が増え、`in_flight_frames` が減ることをテストで固定する。
6. `reconfigure` 成功時に `total_reconfigure_count` が増えることをテストで固定する。
7. 入力検証で拒否したフレームが `total_encode_count` / `total_decode_count` に計上されないことをテストで固定する。
8. `total_output_bit_count` が出力ビットストリームの総ビット数と一致することをテストで固定する。
9. `CHANGES.md` の `## develop` に `[ADD]` (統計 API) と `[CHANGE]` (`get_encode_stat` と旧 `EncoderStats` の削除) を追記する。
10. `skills/shiguredo-vpl/SKILL.md` の公開 API 一覧を更新する。

## 解決方法

`Counter` / `Gauge` と共有統計型 `EncoderStats` / `DecoderStats` を追加し、`Encoder::stats()` / `Decoder::stats()` から統計を取得できるようにした。`Encoder::get_encode_stat()` と VPL の生統計を返す `EncoderStats` は削除した。

### 変更内容

- `src/stats.rs` に `Counter` / `Gauge` を追加した
- `Encoder::stats()` / `Decoder::stats()` と `EncoderStats` / `DecoderStats` を追加した
- `Encoder::get_encode_stat()` を削除し、VPL の生統計を返す `EncoderStats` を共有統計型に置き換えた
- in-flight・エラー・reconfigure 成功・出力ビット数の計上を実装した
- `total_decode_count` は pending 登録時点で計上し、登録後のデコード失敗と空入力も計上するようにした
- 統計の単体テストと実機テストを追加した
- `CHANGES.md` と `skills/shiguredo-vpl/SKILL.md` を更新した

### 確認

Intel GPU を搭載した実機で確認した。

| 確認内容 | 結果 |
| --- | --- |
| `cargo fmt --all -- --check` | pass |
| `cargo clippy --workspace --all-targets -- -D warnings` | pass |
| `INTEL_VPL=1 cargo test --workspace` | pass（lib 27 件、adapter 6 件、roundtrip 30 件、doctest 2 件） |
| `DOCS_RS=1 cargo doc --no-deps` | pass |

### 参考

- PR: https://github.com/shiguredo/vpl-rs/pull/20
