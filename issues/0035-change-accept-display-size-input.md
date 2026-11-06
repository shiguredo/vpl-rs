# エンコーダーの入力で表示サイズ (crop サイズ) を受け付ける

- Priority: High
- Created: 2026-11-04
- Completed: {YYYY-MM-DD}
- Model: DeepSeek V4 Flash
- Branch: feature/change-accept-display-size-input
- Polished: {YYYY-MM-DD}

## 目的

vpl-rs はアラインメント後の coded サイズのバッファを要求するため、1920x1080 のような一般的な解像度でも 1920x1088 のバッファを用意してパディングを埋める必要がある。また、デコーダーの `DecodedFrame` はピッチ付きの Y/UV プレーンを公開しているが、表示サイズに再パックしないとエンコーダーに渡せない。

表示サイズのバッファを受け付け、各行のピッチを指定できるようにして、サーフェスの crop 領域にだけコピーする。

## 現状（実装前）

- `Encoder::encode()` は `frame_size(coded_width, coded_height)` バイト以上を要求する。`coded_width` / `coded_height` は `Encoder::coded_size()` が返すアラインメント後のサイズ。
- `FrameFormat::copy_to_surface_planes()` は `coded_width` x `coded_height` を前提に、各行 `coded_width` バイトをコピーする。
- `Encoder::new` は `FrameInfo.Width` / `Height` に coded サイズ、`CropW` / `CropH` に表示サイズを設定している。
- 既存のテスト `test_roundtrip_h264_nv12_alignment_mismatch` は 318x238 (coded 320x240) の入力を作る際に coded サイズのバッファを渡しており、パディングを含めている。
- デコーダーの `DecodedFrame` は crop サイズとピッチを公開しているが、エンコーダーに渡すには coded サイズへ再パックする必要がある。

## 設計方針

- `Encoder::encode()` の入力契約を表示サイズ (`config.width` x `config.height`) に変更する。
  - 入力は `FrameData` で渡す。
  - `FrameData::SemiPlanar` は NV12 / P010 の Y/UV プレーンを、`FrameData::Packed` は YUY2 / BGRA / AYUV / Y410 のパック形式を表す。
  - 各プレーンの `y_pitch` / `uv_pitch` / `pitch` で行間のバイト数（パディングを含む）を指定できる。
  - 必要な行の長さは `FrameFormat::bytes_per_row(config.width)` で求める。
- `FrameFormat::copy_to_surface_planes()` を表示サイズと入力ピッチでコピーするように変更する。
  - コピー元の行長は `bytes_per_row(config.width)`、コピー先の行長はサーフェスのピッチ。
  - Y プレーンは `config.height` 行、UV プレーンは `config.height / 2` 行をコピーする。
  - packed 形式は 1 プレーンで `config.height` 行をコピーする。
  - サーフェスの crop 領域外 (coded サイズとの差分) はコピーしない。パディングはそのままにする。
- `Encoder::encode()` は `FrameData` の形式と `pitch`、データ長を検証する。
- NV12 / P010 は幅と高さ、YUY2 は幅が偶数であることを `Encoder::new` で検証する。
- `Encoder::coded_size()` は残す。サーフェスのサイズが必要な利用者向け。
- `FrameFormat::bytes_per_row()` を公開し、表示サイズのバッファの行長を利用者が求められるようにする。

## 完了条件

1. 表示サイズ (例: 318x238、1920x1080) のバッファで `encode()` が動く。
2. `y_pitch` / `uv_pitch` / `pitch` が表示幅に応じた行長より大きいバッファで `encode()` が動く。
3. 形式と `FrameData` の variant の不一致、`pitch` 不足、データ長不足はエラーになる。
4. NV12 / P010 の奇数サイズと YUY2 の奇数幅は `Encoder::new` がエラーになる。
5. `test_roundtrip_h264_nv12_alignment_mismatch` を表示サイズのバッファに変更して通る。
6. NV12 / YUY2 / BGRA の各形式で表示サイズの入力をテストする。
7. `CHANGES.md` の `## develop` に `[CHANGE]` を追記し、`README.md` / `skills/shiguredo-vpl/SKILL.md` を更新する。

## 参考

- nvcodec-rs の `Encoder::encode()` と `BufferFormat::frame_size()`
- video-toolbox-rs の `Encoder::encode()` (width x height のフレーム)
- VPL の `mfxFrameInfo.CropW` / `CropH` と `mfxFrameData.Pitch`
