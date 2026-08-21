# src/encode.rs (1765 行) をサブモジュールに分割する

- Priority: Medium
- Created: 2026-07-01
- Model: Opus 4.7
- Branch: feature/refactor-split-encode-module
- Polished: 2026-08-21

## 目的

`src/encode.rs` が 1,765 行のモノリシックファイルになっており、`Encoder::new` 単独で 357 行、その中の unsafe block だけで 100 行超と保守性が悪化している。役割別のサブモジュールに分割して、責務分離とレビュー効率を改善する。

## 優先度根拠

Medium。以下による。

- **既存の破壊的変更方針との相性**: `skills/shiguredo-vpl/SKILL.md` の「良い設計のためには破壊的変更を積極的に行う」方針に沿えば、内部モジュール構成の変更は問題ない。公開 API のパスは `pub use` で維持する。
- **将来の機能追加への布石**: reconfigure（0011 / 0012）などで `Encoder::new` を触る予定がある。0011 / 0012 を先に適用してから分割することで、各変更を安全に加える基盤ができる。
- **他モジュールとの規模差**: `src/decode.rs` (671 行) / `src/vpl.rs` (538 行) / `src/adapter.rs` (277 行) と比べて 1,765 行は明らかに突出している。
- **Priority は High ではない**: 現状動いており、直接的なバグはない。他の致命的バグ修正（0008 - 0015）を先に片付けたほうが良い。

## 現状

### `src/encode.rs` の構成

1,765 行の内訳（概算）:

- ファイル先頭: use 宣言
- `H264Profile` / `HevcProfile` / `Vp9Profile` / `Av1Profile` / `H264EncoderConfig` 等のプロファイル / エンコーダ設定型 4 コーデック × 2 種類
- `CodecConfig` enum
- `FrameFormat` enum + 関連メソッド + `copy_to_surface_planes`
- `RateControlMode` enum + 関連メソッド
- `EncoderConfig` 構造体 + `new` 8 引数コンストラクタ
- `ReconfigureParams` / `EncodeOptions` / `PictureType`
- `EncoderStats` / `EncodedFrame`
- `DEVICE_BUSY_MAX_RETRIES` / `PendingFrame` / `SyncData` / `SyncedBitstream` / `PendingFrameStore` / `WorkerCommand`
- `EncodeHandler` / `FnEncodeHandler` トレイト
- `Encoder<H>` 本体（`new` 357 行、`query` / `reconfigure` / `finish` / `encode` / `encode_frame_async` / `create_bitstream` / `stop_worker` / `Drop` など）
- `run_sync_worker` Worker ループ
- `sync_and_build_frame` / `sync_and_collect` / エラー生成 3 種（`mismatched_timestamp_error` / `finish_pending_error` / `canceled_error`）
- `picture_type_from_frame_type` / `codec_id` / `codec_profile` / `align_up` / `coding_option_name`
- `#[cfg(test)] mod tests`

### `Encoder::new` の内訳

- 寸法・framerate・pitch 検証
- セッション作成 → mfxFrameInfo 組立
- bitstream バッファサイズ計算
- mfxVideoParam 組立（100 行超の unsafe block）
- 拡張バッファ組立（ExtCodingOption2 / ExtCodingOption3 / ExtVP9Param）
- Init / GetVideoParam（読み戻し用拡張バッファの設定、VP9 `WriteIVFHeaders` の一致検証を含む。`coding_option_name` はこのエラーメッセージ表示に使用）
- Worker spawn + Encoder 構築

### 副次的問題

- `H264Profile` / `H264EncodingProfile` 重複（issue 0018 で解消予定）

## 設計方針

### モジュール分割案

shiguredo-rust 規約（「`mod.rs` を使わないこと。モジュールは `<module>/mod.rs` ではなく `<module>.rs` で書くこと。サブモジュールを持つ場合も `<module>.rs` + `<module>/<submodule>.rs` の構成にすること」）に従い、`src/encode.rs` を **維持したまま** サブモジュールを追加する構成にする:

- `src/encode.rs` — モジュール doc + `mod config;` / `mod handler;` / `mod worker;` / `mod encoder;` / `mod mfx_param;` の宣言 + 公開項目の再エクスポート
- `src/encode/config.rs` — `EncoderConfig` / `CodecConfig` / プロファイル関連 / `H264/HEVC/VP9/AV1EncoderConfig` / `RateControlMode` / `FrameFormat` / `EncodeOptions` / `ReconfigureParams` / `EncoderConfig` の初期化ヘルパー（`to_mfx_frame_info` / `to_mfx_video_param` / `build_ext_buffers`。`pub(crate)` メソッド）
- `src/encode/handler.rs` — `EncodeHandler` / `FnEncodeHandler` / `EncodedFrame` / `PictureType`
- `src/encode/worker.rs` — `PendingFrame` / `SyncData` / `SyncedBitstream` / `PendingFrameStore` / `WorkerCommand` / `run_sync_worker` / `sync_and_*` 系 / エラーヘルパー関数群（`mismatched_timestamp_error` / `finish_pending_error` / `canceled_error`、および 0012 適用後の `reconfigure_canceled_error`）/ `picture_type_from_frame_type`（worker 専用のため）
- `src/encode/encoder.rs` — `Encoder<H>` 本体 / `EncoderStats`（`Encoder::get_encode_stat` の戻り値型。ハンドラーとは無関係のため）/ `DEVICE_BUSY_MAX_RETRIES`（`encode_frame_async` からのみ使用）
- `src/encode/mfx_param.rs` — `align_up` 等の純粋ユーティリティ関数群

`mod.rs` は使わない（shiguredo-rust 規約）。`src/lib.rs` の `mod encode;` 宣言は `encode.rs` を解決するため、現状のまま成立する。

**`codec_id` / `codec_profile` は config.rs に配置する**（`CodecConfig` / プロファイル型に対する写像であり、config.rs 側の `to_mfx_video_param` から呼ばれる。mfx_param.rs に置くと config.rs → mfx_param.rs → config.rs の設計上の循環依存になるため。0018 適用後はプロファイル型の `to_mfx_profile()` メソッド（config.rs 側）を呼ぶ）。

**エラーヘルパー関数群と `picture_type_from_frame_type` は worker.rs に配置する**（いずれも `run_sync_worker` / `sync_and_build_frame` / `sync_and_collect` からのみ呼ばれるため。mfx_param.rs に置くと worker 関連コードが複数モジュールに分散し、責務分離を弱める）。

### `Encoder::new` の関数分割

`Encoder::new` 内部を以下に分割:

- `EncoderConfig::to_mfx_frame_info(&self) -> Result<sys::mfxFrameInfo, Error>` — 寸法検証 + mfxFrameInfo 組立（`pub(crate)`）
- `EncoderConfig::to_mfx_video_param(&self, frame_info: sys::mfxFrameInfo) -> Result<(sys::mfxVideoParam, u16), Error>` — 初期 mfxVideoParam 組立（`Init` 前のパラメータ。`pub(crate)`）。bitstream バッファサイズ計算（`buffer_size_in_kb`）もこの中で行い、`(sys::mfxVideoParam, u16)` として `Encoder` の `bitstream_buffer_size` の計算に使う値を返す（GetVideoParam 後の `video_param` から読み戻すとドライバが補正した実効値になり現行の挙動と異なるため、Init 前の設定値ベースの値を直接返す）
- `EncoderConfig::build_ext_buffers(&self) -> ExtBuffers` — 拡張バッファ組立（`pub(crate)`）。`ExtBuffers` は 0011 で定める `ext_co2: Option<Box<sys::mfxExtCodingOption2>>` / `ext_co3: Option<Box<sys::mfxExtCodingOption3>>` / `ext_bufs: Vec<*mut sys::mfxExtBuffer>` の 3 要素を保持する構造体（0011 の完了条件 1 と対応。構造体名と戻り値型は 0020 側の設計提案であり、0011 側はフィールド追加のみを定めている）
- VP9 の `mfxExtVP9Param`（`write_ivf_headers`）は reconfigure で再利用しないため、`build_ext_buffers` の `ExtBuffers` には含めず、`Encoder::new` のグルーコード側で構築する（Init 後の読み戻し用 `ext_vp9_readback`・`WriteIVFHeaders` 一致検証も同様）。**Init 用の一時配列と保持用 `ext_bufs` は分離する**: Init 時に VPL へ渡す `ExtParam` は、`ExtBuffers` が保持する `ext_bufs`（ポインタ配列）を複製して VP9 のポインタを追加した一時配列とし、`Encoder` に保存する `ext_bufs` には VP9 を混ぜない（VP9 のバッファはローカル変数のため、保存用配列に混入すると `Encoder::new` 終了後に dangling ポインタになり、0011 適用後の reconfigure で UB を招く）。Box 化された co2 / co3 へのポインタはヒープアドレスが安定しているため、一時配列の破棄後も有効である。
- `coding_option_name` は `Encoder::new` のエラーメッセージ（VP9 `WriteIVFHeaders` 不一致検証）専用のため、純粋ユーティリティとして mfx_param.rs に配置し、`pub(crate)` とする（encoder.rs のグルーコードから使用）

**0011 適用後の整合**: 0011 は「`ext_co2` / `ext_co3` / `ext_bufs` を `Box` フィールドとして `Encoder` に保持し、reconfigure で再利用する」設計を採用済みである（0011 の設計方針）。**本 issue (0020) は 0011 適用後の設計に合わせ、`build_ext_buffers` ヘルパーの戻り値は `Encoder` のフィールド（`ext_co2` / `ext_co3` / `ext_bufs`）として保持する**（0011 と共通のヘルパーを両方で使う）。「`ext_co2` / `ext_co3` / `ext_bufs` をローカル変数として Init 後即破棄」という設計は 0011 適用後は成立しないため採用しない（VP9 の `mfxExtVP9Param` は reconfigure で再利用しないためローカル変数のまま残す。上記「Init 用の一時配列と保持用 `ext_bufs` の分離」参照）。

`Encoder::new` はこれらを組み立てて `Init` → `GetVideoParam`（実効パラメータ取得）→ Worker spawn を行うグルーコードになる。

### 公開 API の維持

`src/encode.rs`（親モジュール）が全公開項目を再エクスポートするため、`src/lib.rs` の `pub use encode::{...}` は現状のまま成立する（確認のみで足りる）。crate 利用者から見えるパスは変わらない。

再エクスポートは shiguredo-rust 規約「re-export は基本的にやらないこと」の対象外である（`src/lib.rs` が既に `pub use encode::{...}` で公開 API パスを確立しており、本 issue は既存の公開パスを維持するために親モジュールでの再エクスポートが必要。公開 API のパスを保つための必要最小限の再エクスポートであり、新規の公開拡張ではない）。

### 新設ヘルパーの可視性

`to_mfx_frame_info` / `to_mfx_video_param` / `build_ext_buffers` はすべて `pub(crate)` とする（`pub` にすると `sys::mfxFrameInfo` / `sys::mfxVideoParam` が公開 API に露出し、完了条件 7 の「公開 API 変更なし」と矛盾するため）。

分割に伴いモジュール境界を越えて呼ばれる既存の private メソッドも `pub(crate)` 化する（例: `FrameFormat::copy_to_surface_planes`（config.rs から encoder.rs で使用）/ `align_up` フリー関数（mfx_param.rs から config.rs で使用））。

分割に伴いモジュール境界を越えて構築・使用される構造体の private フィールドと型も `pub(crate)` 化する（例: `EncodedFrame<T>` のフィールド（handler.rs 定義、worker.rs の `sync_and_build_frame` で構築）/ `PendingFrame<T>` と `SyncData` のフィールド（worker.rs 定義、encoder.rs の `encode` / `finish` で構築）/ `WorkerCommand<T>` 型自体（worker.rs 定義、encoder.rs で構築）/ `run_sync_worker`（worker.rs 定義、encoder.rs の `Encoder::new` で呼出）。フィールドの `pub(crate)` 化とコンストラクタ新設のどちらを選ぶかは実装時に判断するが、公開 API には露出させない）。

## 完了条件

以下すべてを満たす。

1. `src/encode.rs` (単一ファイル) を、`src/encode.rs`（モジュール doc + `mod config;` 等の宣言 + 再エクスポート）と `src/encode/<submodule>.rs` の 5 サブモジュール（config / handler / worker / encoder / mfx_param）に分割する。`mod.rs` は使わない（shiguredo-rust 規約）。
2. `Encoder::new` をグルーコード化し、`config.rs` / `mfx_param.rs` のヘルパー（`pub(crate)`）に委譲する。0011 適用後は `build_ext_buffers` の戻り値を `Encoder` のフィールドとして保持する設計に整合させる。VP9 の `mfxExtVP9Param` / 読み戻し検証はグルーコード側に残す。
3. 分割後の各ヘルパー（`to_mfx_frame_info` / `to_mfx_video_param` 等）内の unsafe block を最小単位に分割し、それぞれに SAFETY コメント（日本語）を付ける。
4. `src/encode.rs` の再エクスポートにより、`src/lib.rs` の `pub use` を通した既存 API パス（`shiguredo_vpl::Encoder` など）を維持する。
5. 既存のラウンドトリップテスト全てが pass する。なお、issue 0023 適用後（GPU 非依存テストの検証力向上後）も pass することを確認する。
6. `#[cfg(test)] mod tests` の 3 テスト（`pending_frame_store_takes_by_frame_seq` / `worker_wait_idle_returns_error_when_pending_remains` / `worker_stop_returns_aborted_for_all_pending`）をすべて `src/encode/worker.rs` に再配置する。0012 適用後に `DrainPending` アームの単体テストが追加される場合は、それも `src/encode/worker.rs` へ移動する。
7. `CHANGES.md` の `## develop` の `### misc` サブセクションに `[UPDATE]` として追記する（内部リファクタで公開 API 変更なし。機能に直接影響しない変更のため `### misc` に記載する）。

## 影響範囲

- `src/encode.rs` → `src/encode.rs` + `src/encode/*.rs` の 6 ファイル（`mod.rs` は使わない）
- `src/lib.rs`（`pub use` の確認のみ。`mod encode;` 宣言は `encode.rs` を解決するため変更不要）
- `CHANGES.md`

## 前提条件 / 依存関係

適用順序は次のとおり（各 issue の適用順序記述と整合させる）:

- **issue 0011**（reconfigure が ExtParam を送らない）: `Encoder::new` の ExtParam 構築部を変更し、`build_ext_buffers` ヘルパーを切り出す。**0011 を先に適用する**（0011 の Box フィールド保持設計に合わせて本 issue の設計を調整済み）。
- **issue 0010**（デバイスエラー伝搬と Drop 経路の検証）: 0010 は「`SyncData` への `frame_seq` 追加・`sync_and_collect` / `sync_and_build_frame` のシグネチャ変更」を**廃案**とし、プロダクションコード変更なしで **closed 済み**である（0010 の設計方針 2・解決方法参照）。`stopping` 引数・`Encoder` 構造体の `stopping` フィールド・有限タイムアウト化も廃案。したがって本 issue が 0010 の適用差分を前提にする必要はない（0010 の調査結果・方針を参照するのみ）。
- **issue 0012**（reconfigure が pending frame を drain しない）: `WorkerCommand::DrainPending` 追加、`run_sync_worker` のアーム追加、`reconfigure_canceled_error` 新設を変更する。**0012 を先に適用する**（0012 側も「0020 は本 issue 適用後に調整する」と明記）。
- **issue 0013**（frame_seq=0 衝突）: **0013 は closed で「`frame_count` 1 スタート化」は不採用**（`src/encode.rs` の `Encoder::new` は `frame_count: 0` のまま）。適用すべき差分は存在しないため、本 issue の前提から除外する。
- **issue 0014**（Drop 経路のエラー観測）: `Encoder::Drop` を変更する。0014 の変更は本 issue の分割と独立に適用可能。
- **issue 0018**（型統合）: プロファイル / コーデック型を変更する。**0018 を先に適用し、その差分の上に本 issue の変更を重ねる**（0018 側も「本 issue (0018) を先に適用し、その差分の上に 0020 の変更を重ねる」と明記）。
- **issue 0023**（テストの silent pass 修正）: 完了条件 5 の「0023 適用後も pass する」確認に関連する。0023 の変更は本 issue の分割と独立に適用可能（実装時の確認事項）。
- **issue 0025**（エンコーダのドレイン空フレーム）: `src/encode.rs` の `finish()` のドレインループと worker 側の Sync 処理（`sync_and_collect` 等）を変更する。これは 0020 が worker.rs / encoder.rs へ移動するコード領域と重なるため、0025 を先に適用してから本 issue の分割を重ねる（0025 は 0013 の「`frame_count` 1 スタート」前提で書かれており陳腐化している点に注意。0025 の実装時に整合を取る）。
- **issue 0027**（`get_video_param()` 拡張）: `src/encode.rs` の `get_video_param()` を拡張する。分割に独立に適用可能。
- **issue 0029**（`#[non_exhaustive]` 削除）: `src/encode.rs` の `EncoderConfig` 等の公開型から `#[non_exhaustive]` を削除する。分割に独立に適用可能（`EncoderConfig` の配置は config.rs へ移るため、分割後はその差分に合わせる）。

## 参考

- `skills/shiguredo-vpl/SKILL.md` の「チェンジログ・破壊的変更の方針」節（「良い設計のためには破壊的変更を積極的に行う」「互換シムは作らない」。`pub use` による再エクスポートはシムに該当しない）
- 関連 issue: 0011（ExtParam の Box フィールド保持。本 issue の `build_ext_buffers` 設計の前提）
- 関連 issue: 0010 / 0012 / 0014 / 0018 / 0023 / 0025 / 0027 / 0029（適用順序は「前提条件 / 依存関係」参照。0010・0013 は closed 済みで適用差分なし）
