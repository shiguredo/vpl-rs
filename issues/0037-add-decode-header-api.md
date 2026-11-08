# ストリームのヘッダから解像度を取得する API を追加する

- Priority: Medium
- Created: 2026-11-04
- Completed: {YYYY-MM-DD}
- Model: DeepSeek V4 Flash
- Branch: feature/add-decode-header-api
- Polished: {YYYY-MM-DD}

## 目的

WebRTC の受信経路などで、新しい sequence のキーフレームをデコーダーに流す前に、そのストリームの解像度を取得できるようにする。

キーフレームの時点で解像度が分かるため、フレームを失う前にデコーダーの作り直しや受信経路の切り替えを判断できる。

## 現状

- `Decoder::initialize` は内部で `MFXVideoDECODE_DecodeHeader` を呼んでいるが、公開 API は無い
- `DecodedFrame` のサイズは出力フレームからのみ分かる
- `MFXVideoDECODE_GetVideoParam` を毎フレーム呼べば検出できるが、コストがかかる
- webrtc-rs の `EncodedImageRef::frame_type()` でキーフレームかどうかは分かるが、sora-rust-sdk の VPL デコーダーアダプターはそれを使っていない

## 実機確認

- 320x240 の先頭 AU を消費した時点で `GetVideoParam` が 320x240 を返した
- `MFXVideoDECODE_DecodeHeader` は SPS/PPS を含むキーフレーム (または out-of-band のパラメータセット) で解像度を返す
- `EncodedImageRef::frame_type()` は `VideoFrameType::Key` / `Delta` を返す (webrtc-rs)

## 設計方針

- `Decoder::decode_header(&self, data: &[u8]) -> Result<StreamInfo, Error>` を追加する
  - `MFXVideoDECODE_DecodeHeader` を呼び、結果を `StreamInfo` に変換する
  - 既存のセッションを使う
  - 入力を消費しないため、同じ AU をそのまま `decode()` に流せる
- `StreamInfo` は次の情報を持つ
  - `codec`
  - `coded_width` / `coded_height` (`FrameInfo.Width` / `Height`)
  - `width` / `height` (`CropW` / `CropH`)
  - `framerate: Option<Framerate>` (ビットストリームに情報が無い場合は `None`)
- SPS/PPS / sequence header を含まないデータ (P/B フレーム単体) ではエラーを返す。rustdoc に明記する
- 呼び出しはパラメータセットを含むデータ (通常はキーフレーム) のときに行う
  - WebRTC では `EncodedImage::frame_type() == VideoFrameType::Key` で判定できる
  - 毎フレーム呼ぶ必要はない
- `Decoder::initialize` の `DecodeHeader` 呼び出しを共通化し、`decode_header` と同じ変換を使う

## 完了条件

1. `Decoder::decode_header` が H.264 / HEVC / AV1 のキーフレームから `StreamInfo` を返す。
2. SPS/PPS / sequence header を含まないデータではエラーを返すことをテストで固定する。
3. `decode_header` が入力を消費せず、同じ AU を `decode()` に流せることをテストで固定する。
4. 256x160 / 320x240 と、coded サイズと表示サイズが異なる 318x238 のキーフレームで、それぞれの coded / 表示サイズが返ることをテストで固定する。
5. `CHANGES.md` と `README.md` / `skills/shiguredo-vpl/SKILL.md` を更新する。

## 参考

- VPL の `MFXVideoDECODE_DecodeHeader`
- webrtc-rs の `EncodedImageRef::frame_type()` と `VideoFrameType`
- sora-rust-sdk の `VplVideoDecoder::decode` (`EncodedImageRef` を受け取る)
