# エンコーダーの入力で表示サイズ (crop サイズ) を受け付ける

- Priority: High
- Created: 2026-11-04
- Completed: {YYYY-MM-DD}
- Model: DeepSeek V4 Flash
- Branch: feature/change-accept-display-size-input
- Polished: {YYYY-MM-DD}

## 目的

nvcodec-rs / video-toolbox-rs は `width x height` (表示サイズ) のバッファをそのまま入力として受け付ける。vpl-rs はアラインメント後の coded サイズのバッファを要求するため、1920x1080 のような一般的な解像度でも 1920x1088 のバッファを用意してパディングを埋める必要がある。

表示サイズのバッファを受け付け、サーフェスの crop 領域にだけコピーするようにする。

## 現状

- `Encoder::encode()` は `frame_size(coded_width, coded_height)` バイト以上を要求する。`coded_width` / `coded_height` は `Encoder::coded_size()` が返すアラインメント後のサイズ。
- `FrameFormat::copy_to_surface_planes()` は `coded_width` x `coded_height` を前提に、各行 `coded_width` バイトをコピーする。
- `Encoder::new` は `FrameInfo.Width` / `Height` に coded サイズ、`CropW` / `CropH` に表示サイズを設定している。
- 既存のテスト `test_roundtrip_h264_nv12_alignment_mismatch` は 318x238 (coded 320x240) の入力を作る際に coded サイズのバッファを渡しており、パディングを含めている。
- デコーダーの `DecodedFrame` は crop サイズとピッチを公開しているが、エンコーダーに渡すには coded サイズへ再パックする必要がある。

## 設計方針

- `Encoder::encode()` の入力契約を表示サイズ (`config.width` x `config.height`) に変更する。
  - 必要なバイト数は `frame_size(config.width, config.height)`。
  - 入力の行ストライドは表示幅から計算する (`row_bytes(config.width)`)。
  - バッファは表示サイズで密に詰めたものとする。coded サイズのバッファは行ストライドが異なるため受け付けない
- `FrameFormat::copy_to_surface_planes()` を表示サイズでコピーするように変更する。
  - コピー元の行長は表示幅、コピー先の行長はサーフェスのピッチ。
  - Y プレーンは `config.height` 行、UV プレーンは `config.height / 2` 行をコピーする。
  - packed 形式は 1 プレーンで `config.height` 行をコピーする。
  - サーフェスの crop 領域外 (coded サイズとの差分) はコピーしない。パディングはそのままにする。
- `Encoder::encode()` のサイズ検証を表示サイズに変更する。
- `Encoder::coded_size()` は残す。サーフェスのサイズが必要な利用者向け。
- `Encoder::new` のピッチ検証はサーフェスの coded サイズのまま変更しない。
- 入力の行ストライドを明示したい場合 (パディング付きのバッファ) は別 issue とする。

## 完了条件

1. 表示サイズ (例: 318x238、1920x1080) のバッファで `encode()` が動く。
2. 表示サイズ未満のバッファはエラーになる。
3. `test_roundtrip_h264_nv12_alignment_mismatch` を表示サイズのバッファに変更して通る。
4. NV12 / YUY2 / BGRA の各形式で表示サイズの入力をテストする。
5. `CHANGES.md` の `## develop` に `[CHANGE]` を追記し、`README.md` / `skills/shiguredo-vpl/SKILL.md` を更新する。

## 参考

- nvcodec-rs の `Encoder::encode()` と `BufferFormat::frame_size()`
- video-toolbox-rs の `Encoder::encode()` (width x height のフレーム)
- VPL の `mfxFrameInfo.CropW` / `CropH` と `mfxFrameData.Pitch`
