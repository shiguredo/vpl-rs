# src/encode.rs (1653 行) をサブモジュールに分割する

- Priority: Medium
- Created: 2026-07-01
- Model: Opus 4.7
- Branch: feature/refactor-split-encode-module
- Polished: 2026-07-01

## 目的

`src/encode.rs` が 1,653 行のモノリシックファイルになっており、`Encoder::new` 単独で 275 行、その中の unsafe block だけで 100 行超と保守性が悪化している。役割別のサブモジュールに分割して、責務分離とレビュー効率を改善する。

## 優先度根拠

Medium。以下による。

- **既存の破壊的変更方針との相性**: SKILL.md L383「良い設計のためには破壊的変更を積極的に行う」に沿えば、モジュール分割時に公開 API のパスが変わっても問題ない（`pub use` で互換維持は可能）。
- **将来の機能追加への布石**: reconfigure (0011, 0012) や pending frame の buffer プール (別途改善) などで `Encoder::new` を触る予定があるなら、分割してからのほうが安全。
- **他モジュールとの規模差**: `src/decode.rs` (671 行) / `src/vpl.rs` (538 行) / `src/adapter.rs` (277 行) と比べて 1,653 行は明らかに突出している。
- **Priority は High ではない**: 現状動いており、直接的なバグはない。他の致命的バグ修正（0008 - 0015）を先に片付けたほうが良い。

## 現状

### `src/encode.rs` の構成

1,653 行の内訳（概算）:

- L1-8: use 宣言
- L9-88: プロファイル / エンコーダ設定型 4 個 × 2 種類
- L91-101: `CodecConfig` enum
- L104-214: `FrameFormat` enum + 関連メソッド + `copy_to_surface_planes`
- L217-256: `RateControlMode` enum + 関連メソッド
- L258-454: `EncoderConfig` 構造体 + `new` 8 引数コンストラクタ
- L457-495: `ReconfigureParams` / `EncodeOptions` / `PictureType`
- L497-546: `EncoderStats` / `EncodedFrame`
- L548-622: `DEVICE_BUSY_MAX_RETRIES` / `PendingFrame` / `SyncData` / `SyncedBitstream` / `PendingFrameStore` / `WorkerCommand`
- L624-680: `EncodeHandler` / `FnEncodeHandler` トレイト
- L682-1272: `Encoder<H>` 本体（`new` 275 行、`query` / `reconfigure` / `finish` / `encode` / `encode_frame_async` / `create_bitstream` / `stop_worker` / `Drop` など）
- L1274-1332: `run_sync_worker` Worker ループ
- L1334-1450: `sync_and_build_frame` / `sync_and_collect` / エラー生成 3 種
- L1451-1520: `picture_type_from_frame_type` / `codec_id` / `codec_profile` / `align_up`
- L1522-1653: `#[cfg(test)] mod tests`

### `Encoder::new` (L682-958) の内訳

- L686-724: 寸法・framerate・pitch 検証
- L726-750: セッション作成 → mfxFrameInfo 組立
- L752-761: bitstream バッファサイズ計算
- L763-873: mfxVideoParam 組立（100 行超の unsafe block）
- L875-914: 拡張バッファ（ExtCodingOption2/3）組立
- L916-928: Init / GetVideoParam
- L929-957: Worker spawn + Encoder 構築

### 副次的問題

- `H264Profile` / `H264EncodingProfile` 重複（issue 0018 で解消予定）
- `PendingFrameStore` の薄い抽象化（別 issue で対応可能）
- `Encoder::new` の unsafe block 100 行超

## 設計方針

### モジュール分割案

`src/encode.rs` を以下に分割:

- `src/encode/mod.rs` — 再エクスポート + 全体の doc comment
- `src/encode/config.rs` — `EncoderConfig` / `CodecConfig` / プロファイル関連 / `H264/HEVC/VP9/AV1EncoderConfig` / `RateControlMode` / `FrameFormat` / `EncodeOptions` / `ReconfigureParams`
- `src/encode/handler.rs` — `EncodeHandler` / `FnEncodeHandler` / `EncodedFrame` / `EncoderStats` / `PictureType`
- `src/encode/worker.rs` — `PendingFrame` / `SyncData` / `SyncedBitstream` / `PendingFrameStore` / `WorkerCommand` / `run_sync_worker` / `sync_and_*` 系
- `src/encode/encoder.rs` — `Encoder<H>` 本体
- `src/encode/mfx_param.rs` — `codec_id` / `codec_profile` / `align_up` / `picture_type_from_frame_type` / エラーヘルパー関数群（`mismatched_timestamp_error` / `finish_pending_error` / `canceled_error`）/ `EncoderConfig` → 初期 `mfxVideoParam` 変換ヘルパー

### `Encoder::new` の関数分割

`Encoder::new` 内部を以下に分割:

- `EncoderConfig::to_mfx_frame_info(&self) -> Result<sys::mfxFrameInfo, Error>` — 寸法検証 + mfxFrameInfo 組立
- `EncoderConfig::to_mfx_video_param(&self, frame_info: sys::mfxFrameInfo) -> Result<sys::mfxVideoParam, Error>` — 初期 mfxVideoParam 組立（`Init` 前のパラメータ）
- `EncoderConfig::build_ext_buffers(&self) -> ExtBuffers` — 拡張バッファ組立。`ExtBuffers` はローカル変数として使用し、`Init` 後は即破棄する（`Encoder` のフィールドとして保持しない）。

`Encoder::new` はこれらを組み立てて `Init` → `GetVideoParam`（実効パラメータ取得）→ Worker spawn を行うグルーコードになる。分割後の行数は自然に 50-80 行程度になる。

### 公開 API の維持

`src/lib.rs` の `pub use encode::{...}` を更新して既存 API パスを維持する。crate 利用者から見えるパスは変わらない。

## 完了条件

以下すべてを満たす。

1. `src/encode.rs` (単一ファイル) を `src/encode/mod.rs` + 上記 5 サブモジュールに分割する。
2. `Encoder::new` をグルーコード化し、`config.rs` / `mfx_param.rs` のヘルパーに委譲する。
3. `Encoder::new` 内の unsafe block を最小単位に分割し、それぞれに SAFETY コメントを付ける。
4. `src/lib.rs` の `pub use` を更新して既存 API パス（`shiguredo_vpl::Encoder` など）を維持する。
5. 既存のラウンドトリップテスト全てが pass する。
6. `#[cfg(test)] mod tests` を `src/encode/worker.rs` などに再配置する（Worker 単体テストは worker.rs に）。
7. `CHANGES.md` の `## develop` に `[UPDATE]` として追記する（内部リファクタで公開 API 変更なし）。

## 影響範囲

- `src/encode.rs` → `src/encode/*.rs` の 6 ファイル
- `src/lib.rs`（`pub use` 更新のみ）
- `CHANGES.md`

## 前提条件 / 依存関係

- issue 0011（reconfigure が ExtParam を送らない）を先に完了しておくと、`build_ext_buffers` を切り出す共通ヘルパーを両方で使える
- issue 0018（プロファイル / コーデック型統合）と同時進行するとコンフリクトしやすい

## 参考

- SKILL.md L383「破壊的変更を積極的に行う」（内部モジュール分割で公開 API パスは維持する）
