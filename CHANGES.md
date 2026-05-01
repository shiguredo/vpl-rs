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

- [CHANGE] Decoder のデコード完了通知を非同期 callback 型 API に変更する
  - Decoder がジェネリクス `Decoder<T>` になり、コンストラクタでコールバックを受け取るように変更
  - `Decoder::decode` のシグネチャが `decode(&mut self, data: &[u8], value: T)` に変更
  - デコード結果はコールバック経由で `DecodedFrame<'a, T>` として受け取る
  - `DecodedFrame` は y()/uv() スライスと pitch() を提供し、コールバック呼び出し中のみ有効
  - データコピー不要で、VPL 内部サーフェスから直接読み取ったデータを借用として渡す
  - `Decoder::next_frame` を削除
  - `DecoderConfig` に `async_depth` フィールドを追加（None の場合は 4）
  - `surface_work=NULL` の VPL 内部割り当て方式に移行し、自前のサーフェスプール管理を廃止
  - @melpon
- [CHANGE] Encoder のエンコード完了通知を非同期 callback 型 API に変更する
  - Encoder がジェネリクス `Encoder<T>` になり、コンストラクタでコールバックを受け取るように変更
  - `Encoder::encode` のシグネチャが `encode(&mut self, data: &[u8], value: T)` に変更
  - エンコード結果はコールバック経由で `EncodedFrame<T>` として受け取る
  - `EncodedFrame` に `value()` / `into_value()` メソッドを追加
  - `MFXMemory_GetSurfaceForEncode` による VPL 内部サーフェス利用に移行し、自前のサーフェスプール管理を廃止
  - `SurfaceGuard` を導入し、エラーパスでの内部サーフェス解放漏れを防止
  - `surface_work=NULL` の VPL 内部割り当て方式に移行し、Row-by-Row コピーでフレームデータを書き込む
  - `EncoderConfig` に `async_depth` フィールドを追加（None の場合は 4）
  - DEVICE_BUSY 最大リトライ回数を 10 → 30 に変更
  - ドレイン時の空ビットストリームをエラーではなく空データとして正常処理するよう修正
  - @melpon

### misc

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
