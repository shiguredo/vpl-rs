# デコーダーで max サイズを指定して解像度変更を扱う

- Priority: High
- Created: 2026-11-04
- Completed: {YYYY-MM-DD}
- Model: DeepSeek V4 Flash
- Branch: feature/add-decoder-max-size
- Polished: {YYYY-MM-DD}

## 目的

WebRTC などで解像度が途中で変わるストリームを、デコーダーの再作成なしで扱えるようにする。

`DecoderConfig` に max サイズを追加し、そのサイズで内部サーフェスを確保する。max 内の解像度変更は、出力フレームのサイズと `resolution_changed` フラグで通知する。

## 現状

- `DecoderConfig` に max サイズの指定は無い
- `Decoder::initialize` は `MFXVideoDECODE_DecodeHeader` が返したサイズで `MFXVideoDECODE_Init` を呼び、内部サーフェスを確保する
- ストリームの解像度が大きくなると、デコーダーは数 AU を消費した後に `MFX_ERR_DEVICE_FAILED` または `MFX_ERR_INCOMPATIBLE_VIDEO_PARAM` を返す
- 消費された AU は失われ、コールバックも来ない
- `DecodedFrame` に解像度変更のフラグは無い

## 実機確認

`Init` 時に `FrameInfo.Width` / `Height` を 640x480 に上書きして、256x160 の 10 フレーム → 320x240 の 10 フレームを入力した。

- 20 フレームすべてコールバックされた
- 最初の 10 フレームが 256x160、11 フレーム目から 320x240
- `MFX_ERR_DEVICE_FAILED` は出なかった
- 前フレームと比較するフラグは 11 フレーム目で `true`
- VPL は `Init` 時の `FrameInfo.Width` / `Height` の上書きを尊重し、max サイズの内部サーフェスを確保する

## 設計方針

- `DecoderConfig` に `max_width` / `max_height` を追加する
  - `None` の場合は最初のストリームのサイズ (現状と同じ)
  - 表示サイズで指定し、クレートが 16 バイト境界に切り上げる
- `initialize` で `DecodeHeader` の後、`Init` の前に `FrameInfo.Width` / `Height` を max に上書きする
  - `CropW` / `CropH` はストリームの表示サイズのまま
  - max がストリームの coded サイズより小さい場合は `Init` がエラーになる
- `DecodedFrame` に `resolution_changed: bool` を追加する
  - 出力サーフェスの `CropW` / `CropH` を前回の出力と比較する
  - 最初のフレームは `false`
  - サイズが変わった最初のフレームで `true`
  - 以降のフレームは `false`
  - 判定はワーカー側で行う
- max 内の解像度変更は、デコーダーの再作成なしで続行する
- max を超える変更は、0037 の `decode_header` でキーフレームの時点で事前に検出し、より大きい max で `Decoder` を作り直す
  - 検出せずに流した場合は、デコーダーがエラーを返す。`decode()` の Ok だけで成功とみなさないことを rustdoc に明記する
- 外部サーフェス方式 (`MFX_ERR_REALLOC_SURFACE`) は本 issue の対象外とする
- max のメモリ消費は max に比例することを rustdoc に明記する

## 完了条件

1. `DecoderConfig` に `max_width` / `max_height` が追加され、`initialize` が `FrameInfo` を max に設定する。
2. `max_width=640`, `max_height=480` で 256x160 → 320x240 の 20 フレームがすべてデコードできることをテストで固定する。
3. `DecodedFrame.resolution_changed` が追加され、最初のフレームは `false`、サイズが変わったフレームで `true`、以降は `false` になることをテストで固定する。
4. max を超えるストリームを流した場合に、黙って成功せずエラーになることをテストで固定する。
5. `CHANGES.md` と `README.md` / `skills/shiguredo-vpl/SKILL.md` を更新する。

## 参考

- VPL の `MFXVideoDECODE_DecodeHeader` / `MFXVideoDECODE_Init` / `mfxFrameInfo.CropW` / `CropH`
- Intel 実装 (vpl-gpu-rt) の H.264 / HEVC デコーダーの新しい解像度の分岐 (`MFX_ERR_INCOMPATIBLE_VIDEO_PARAM`)
- 0037 (decode_header API)
