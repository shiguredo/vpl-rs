# エンコーダーの入力で表示サイズ (crop サイズ) を受け付ける

- Priority: High
- Created: 2026-11-04
- Completed: 2026-11-07
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

## 解決方法

`Encoder::encode()` が `FrameData` を受け取るように変更し、表示サイズと行ピッチを指定した入力をサーフェスの crop 領域へコピーできるようにした。

### 変更内容

- `FrameData` を追加し、`SemiPlanar`（NV12 / P010）と `Packed`（YUY2 / BGRA / AYUV / Y410）で入力を表現するようにした
- `Encoder::encode()` を `FrameData` 受け取りに変更し、`y_pitch` / `uv_pitch` / `pitch` で行間のバイト数を指定できるようにした
- `FrameFormat::copy_to_surface_planes()` で形式・ピッチ・データ長を検証し、表示サイズ分だけコピーするようにした。コピー先のピッチ不足もエラーにする
- `FrameFormat::bytes_per_row()` を公開し、`FrameFormat::frame_size()` を削除した
- NV12 / P010 は幅と高さ、YUY2 は幅が偶数であることを `Encoder::new` で検証するようにした。16 の倍数に切り上げたサイズが `u16::MAX` を超える場合もエラーにする
- `GetVideoParam` の実効フレーム情報を検証し、`CropX` / `CropY` / `CropW` / `CropH` / `Width` / `Height` / FourCC / ChromaFormat / BitDepth / Shift が要求値と一致しない場合はエラーにするようにした
- `FrameData` のデータは `encode` 内でコピーされるため、戻った後に再利用できることをドキュメント化した
- `Encoder::coded_size()` は内部サーフェスのサイズを返す API として残した
- `CHANGES.md` / `README.md` / `skills/shiguredo-vpl/SKILL.md` を更新した

### テスト

- `FrameFormat::copy_to_surface_planes()` の Y / UV / コピー先ピッチを合成 `mfxFrameData` で検証する単体テストを追加した
- カラーバー、strided NV12 / YUY2 / P010 の UV プレーン PSNR を検証した
- 318x238 / 1920x1080 の coded サイズ不変条件、pitch 不足、データ長不足、必要サイズのオーバーフロー、奇数サイズ拒否を検証した
- HEVC / AV1 / VP9 / P010 / AYUV / Y410 の 318x238 ラウンドトリップを追加した
- `DecodedFrame` の Y / UV / pitch をそのまま `FrameData` に渡す再エンコードを検証した
- `Encoder::get_video_param()` で実効フレーム情報を検証するテストを追加した

### 確認

Intel GPU を搭載した実機で確認した。

| 確認内容 | 結果 |
| --- | --- |
| `cargo fmt --all -- --check` | pass |
| `cargo clippy --workspace --all-targets -- -D warnings` | pass |
| `cargo test --workspace` | pass (lib 55 件、adapter 5 件、roundtrip 61 件、doctest 2 件) |
| `cargo doc --no-deps` | pass |

作業用の未追跡ファイル `tests/probe_444.rs` は一時退避してから実行し、確認後に元へ戻した。

## 参考

- nvcodec-rs の `Encoder::encode()` と `BufferFormat::frame_size()`
- video-toolbox-rs の `Encoder::encode()` (width x height のフレーム)
- VPL の `mfxFrameInfo.CropW` / `CropH` と `mfxFrameData.Pitch`
