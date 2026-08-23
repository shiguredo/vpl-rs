# 変更履歴

- CHANGE
  - 後方互換のない変更
- ADD
  - 後方互換がある追加
- UPDATE
  - 後方互換がある変更
- FIX
  - バグ修正

## develop

## 2026.4.0

**リリース日**: 2026-08-23

- [CHANGE] MSRV (rust-version) を 1.88 から 1.93 に上げる
  - @voluntas
- [CHANGE] `Vp9EncoderConfig` に `write_ivf_headers` を追加する
  - `Encoder` が oneVPL へ要求した値を返す `Encoder::write_ivf_headers` getter を追加する
  - 初期化時に `mfxExtVP9Param::WriteIVFHeaders` の実効値を読み戻し、要求値と一致しない場合はエラーを返す
  - `Vp9EncoderConfig` を構造体リテラルで構築する既存コードは `write_ivf_headers` の明示指定が必須になりコンパイル不能になる
  - Intel GPU の oneVPL は `WriteIVFHeaders` が既定で ON のため、従来の IVF 付き出力を維持するには `write_ivf_headers: true` を指定する
  - @melpon
- [UPDATE] libvpl を 2.16.0 から 2.17.0 に更新する
  - @voluntas
- [FIX] Decoder の DEVICE_BUSY / MORE_SURFACE リトライに上限を設ける
  - Encoder と同様に 1ms スリープで最大 30 回リトライし、上限超過時はエラーを返す
  - `finish` のドレインループの `FrameSurface::new` 呼び出し順序を `decode_bitstream` と統一する
  - @melpon

### misc

- [ADD] CI に ubuntu-26.04 を追加する
  - @voluntas

## 2026.3.0

**リリース日**: 2026-06-23

- [UPDATE] Guard 系の実装をやめてリソースに対するライフタイムで管理する
  - `VplLibrary` / `frame_type` / `gop_opt_flag` を `src/vpl.rs` に移動する
  - `SurfaceGuard` / `DecodedSurfaceGuard` を `FrameSurface` に統合する
  - `Session` 型を新設し、Encoder/Decoder の lib/loader/session フィールドを統合する
  - `CloseGuard` を廃止し、`Session` の Drop による RAII 解放に移行する
  - @melpon
- [CHANGE] Decoder の完了通知をハンドラートレイト方式に変更する
  - Decoder が型パラメータ `Decoder<H: DecodeHandler>` になり、コンストラクタでハンドラーを受け取るように変更
  - `DecodeHandler` トレイト (`on_decoded`) と `FnDecodeHandler<T, E>` ラッパーを追加
  - `Decoder::decode` のシグネチャが `decode(&mut self, data: &[u8], user_data: H::UserData)` に変更
  - デコード結果はハンドラー経由で `DecodedFrame<'_, T>` として受け取る
  - `DecodedFrame` は y()/uv() スライスと pitch() を提供し、ハンドラー呼び出し中のみ有効
  - ハンドラーの `Error` 関連型によりカスタムエラー型が利用可能
  - `Decoder::next_frame` を削除
  - `DecoderConfig` に `async_depth` フィールドを追加（None の場合は 4）
  - `surface_work=NULL` の VPL 内部割り当て方式に移行し、自前のサーフェスプール管理を廃止
  - @melpon
- [CHANGE] Encoder の完了通知をハンドラートレイト方式に変更する
  - Encoder が型パラメータ `Encoder<H: EncodeHandler>` になり、コンストラクタでハンドラーを受け取るように変更
  - `EncodeHandler` トレイト (`on_encoded`) と `FnEncodeHandler<T, E>` ラッパーを追加
  - `Encoder::encode` のシグネチャが `encode(&mut self, data: &[u8], user_data: H::UserData)` に変更
  - エンコード結果はハンドラー経由で `EncodedFrame<T>` として受け取る
  - `EncodedFrame` に `user_data()` / `into_user_data()` メソッドを追加
  - ハンドラーの `Error` 関連型によりカスタムエラー型が利用可能
  - `MFXMemory_GetSurfaceForEncode` による VPL 内部サーフェス利用に移行し、自前のサーフェスプール管理を廃止
  - `EncoderConfig` に `async_depth` フィールドを追加（None の場合は 4）
  - DEVICE_BUSY 最大リトライ回数を 10 → 30 に変更
  - ドレイン時の空ビットストリームをエラーではなく空データとして正常処理するよう修正
  - @melpon

### misc

- [ADD] container コマンドで macOS から clippy 検証ができるようにする
  - Apple Silicon Mac の arm64 ネイティブで x86_64-unknown-linux-gnu へクロスビルドする
  - `.devcontainer/Dockerfile` を VS Code Dev Container と prek の cargo-clippy フックで共用する
  - `make container-build` でイメージを用意し `prek run cargo-clippy` で実行する
  - @voluntas

## 2026.2.0

**リリース日**: 2026-05-13

- [ADD] shiguredo_vpl::list_adapters と AdapterSelector / AdapterInfo / PciAddress / MediaAdapterType を追加する
  - @voluntas
- [CHANGE] EncoderConfig::new / DecoderConfig::new / codec_info::supported_codecs にアダプタ指定を必須化する
  - @voluntas

## 2026.1.2

**リリース日**: 2026-04-08

- [FIX] 正しいアライメントのバッファを VPL エンコーダーに渡すように修正
  - 以前はアライメントされていないサイズでバッファを計算していたため、バッファサイズが足りずに SIGSEGV が発生していた
  - @melpon

## 2026.1.1

**リリース日**: 2026-04-08

- [FIX] パック済みフォーマットでも Y/U/V に値を設定する
  - mfxFrameData のドキュメントに、NV12 や YUY2 のようなフォーマットの場合であっても、Y/U/V をそれぞれ設定する必要があると書かれているため
  - @melpon

## 2026.1.0

**リリース日**: 2026-03-31
