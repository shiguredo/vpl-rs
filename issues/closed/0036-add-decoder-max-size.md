# デコーダーで max サイズを指定して解像度変更を扱う

- Priority: High
- Created: 2026-11-04
- Completed: 2026-11-08
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

## 解決方法

`DecoderConfig` に `max_width` / `max_height` を追加し、`MFXVideoDECODE_Init` の前に `mfxFrameInfo.Width` / `Height` を最大サイズへ上書きするようにした。最大サイズ内の解像度変更はデコーダーを再作成せずに処理し、`DecodedFrame::resolution_changed` で通知する。

### 変更内容

- `DecoderConfig` に `max_width` / `max_height` を追加した。表示サイズとして指定する。内部サーフェスは指定値以上のサイズで確保され、VPL のアラインメントにより指定値より大きくなることがある。未指定の場合は最初のストリームのサイズを使う
- `Decoder::initialize` で `MFXVideoDECODE_DecodeHeader` の後、`MFXVideoDECODE_Init` の前に `mfxFrameInfo.Width` / `Height` を最大サイズへ上書きするようにした。`CropW` / `CropH` はストリームの表示サイズのままにする
- `mfxFrameInfo.Width` / `Height` の実際のアラインメントは実装依存のため、`MFXEnumImplementations` が返す実装記述の `Width` / `Height` の `Step` で最大サイズを切り上げるようにした
- `DecodeHeader` が返した表示サイズと最大サイズを比較し、最大サイズの方が小さい場合は初期化エラーにするようにした。`Step` への切り上げ結果が coded サイズより小さい場合と、`u16` の範囲を超える場合も初期化エラーになる
- ワーカースレッドで出力サーフェスの `CropW` / `CropH` を直前の出力と比較し、`DecodedFrame::resolution_changed` として通知するようにした。最初の出力は `false`、サイズが変わった最初のフレームで `true`、以降は `false` になる
- `DecodedFrame::resolution_changed` のドキュメントに、最初の出力は `false`、サイズが変わった最初のフレームで `true`、以降は `false` になることを追記した
- `DecoderConfig` のドキュメントに、最大サイズの指定方法・内部サーフェスは指定値以上になること・メモリ消費量・エラー条件を追記した
- `Decoder` のドキュメントに、最大サイズ内の解像度変更を再作成なしで処理することを追記した
- `CHANGES.md` / `README.md` / `skills/shiguredo-vpl/SKILL.md` を更新した

### テスト

- `max_width=640` / `max_height=480` で 256x160 の 10 フレーム → 320x240 の 10 フレームの 20 フレームがすべて出力され、`pitch` が 640 以上になることを検証した
- `max_width=640` / `max_height=480` で 320x240 → 256x160 → 320x240 の 30 フレームがすべて出力されることを検証した
- 30 フレームの `width` / `height` が入力ストリームの表示サイズと一致し、縮小・拡大の両方で `resolution_changed` が `true` になり、`pitch` が 640 以上になることを検証した
- HEVC でも 256x160 → 320x240 の 20 フレームが最大サイズ 640x480 で出力されることを検証した
- 最大サイズを 320x240 にして 256x160 → 640x480 に解像度が上がるストリームを入力し、`decode()` が `MFX_ERR_DEVICE_FAILED` または `MFX_ERR_INCOMPATIBLE_VIDEO_PARAM` を返し、`finish()` もエラーになり、最大サイズを超えるフレームが正常フレームとして通知されないことを検証した
- 最大サイズ未指定で 256x160 から 320x240 に解像度が上がるストリームを入力し、同じくエラーになることを検証した
- B フレーム有効時でも 256x160 → 320x240 の解像度変更を扱え、B ピクチャが生成されることと、`DecoderStats` の decode / output / error / in-flight が矛盾しないことを検証した
- 638x478 のストリームに対して `max_width=638` / `max_height=478` を指定し、実装記述の `Step` への切り上げを経て表示サイズをそのまま最大サイズに指定でき、`pitch` が 640 以上になることを検証した
- 320x240 のストリームに対して `max_width=256` / `max_height=160` を指定し、最初のストリームの表示サイズより小さい最大サイズが初期化エラーになることを検証した
- 最大サイズの 0、表示サイズより小さい場合、`Step` への切り上げ、coded サイズより小さい場合、`u16` の範囲を超える場合を単体テストで検証した
- `decoder_frame_step` が指定プロファイルの System メモリの Step を返すことを単体テストで検証した

### 確認

Intel GPU を搭載した実機で確認した。

| 確認内容 | 結果 |
| --- | --- |
| `cargo fmt --all -- --check` | pass |
| `cargo clippy --workspace --all-targets -- -D warnings` | pass |
| `cargo test --workspace` | pass (lib 61 件、adapter 5 件、roundtrip 69 件、doctest 2 件) |
| `cargo doc --no-deps` | pass |

## 参考

- VPL の `MFXVideoDECODE_DecodeHeader` / `MFXVideoDECODE_Init` / `mfxFrameInfo.CropW` / `CropH`
- Intel 実装 (vpl-gpu-rt) の H.264 / HEVC デコーダーの新しい解像度の分岐 (`MFX_ERR_INCOMPATIBLE_VIDEO_PARAM`)
- 0037 (decode_header API)
