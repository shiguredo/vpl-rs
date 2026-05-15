# sync_and_collect で空ビットストリームをエラー扱いにしている

Created: 2026-05-01
Model: kimi-k2.6

## 問題

`sync_and_collect` で `bitstream.DataLength == 0` の場合にエラーを返しているが、VPL のドレイン処理（flush）では `syncp` は返るが `DataLength == 0` となるケースが一部の実装やドライバで発生する可能性がある。

## 再現手順

1. `Encoder::finish()` を呼び出す
2. `encode_frame_async` に `surface = null` でドレインを要求する
3. `Some(syncp)` が返るが、ビットストリームのデータ長が 0 になる
4. `sync_and_collect` で `encoded bitstream is empty` エラーが発生する
5. `pending_store` からフレームが取り除かれないため、`WaitIdle` で失敗する

## 影響

`finish()` が正常に完了すべきケースで意図せず失敗する。

## 解決方法

`sync_and_collect` で `DataLength == 0` の場合にエラーを返すのではなく、空データの `SyncedBitstream` を正常に返すようにした。これにより、VPL のドレイン処理で空ビットストリームが返ってきた場合でも `pending_store` からフレームが正常に取り除かれ、`finish()` が正常に完了するようになる。
