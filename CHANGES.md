# 変更履歴

- UPDATE
  - 後方互換がある変更
- ADD
  - 後方互換がある追加
- CHANGE
  - 後方互換のない変更
- FIX
  - バグ修正

## develop

- [CHANGE] Decoder の完了通知をハンドラートレイト方式に変更する
  - Decoder が型パラメータ `Decoder<H: DecodeHandler>` になり、コンストラクタでハンドラーを受け取るように変更
  - `DecodeHandler` トレイト (`on_decoded`) と `FnDecodeHandler<T, E>` ラッパーを追加
  - `Decoder::decode` のシグネチャが `decode(&mut self, data: &[u8], value: H::UserData)` に変更
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
  - `Encoder::encode` のシグネチャが `encode(&mut self, data: &[u8], value: H::UserData)` に変更
  - エンコード結果はハンドラー経由で `EncodedFrame<T>` として受け取る
  - `EncodedFrame` に `value()` / `into_value()` メソッドを追加
  - ハンドラーの `Error` 関連型によりカスタムエラー型が利用可能
  - `MFXMemory_GetSurfaceForEncode` による VPL 内部サーフェス利用に移行し、自前のサーフェスプール管理を廃止
  - `SurfaceGuard` を導入し、エラーパスでの内部サーフェス解放漏れを防止
  - `surface_work=NULL` の VPL 内部割り当て方式に移行し、Row-by-Row コピーでフレームデータを書き込む
  - `EncoderConfig` に `async_depth` フィールドを追加（None の場合は 4）
  - DEVICE_BUSY 最大リトライ回数を 10 → 30 に変更
  - ドレイン時の空ビットストリームをエラーではなく空データとして正常処理するよう修正
  - @melpon

### misc

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
