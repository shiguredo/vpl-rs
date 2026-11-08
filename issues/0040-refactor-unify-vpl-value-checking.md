# VPL の値のチェックを一貫性のある方針にする

- Priority: Medium
- Created: 2026-11-08
- Completed: {YYYY-MM-DD}
- Model: DeepSeek V4 Flash
- Branch: feature/refactor-unify-vpl-value-checking
- Polished: {YYYY-MM-DD}

## 目的

VPL が返す値の検証方針が場所によって異なっている。`CODEBASE.md` の「VPL の値の扱い」に従い、ユーザー入力だけを検証し、VPL の値は信頼する形に統一する。検証の重複と、実サーフェスと一致しない読み戻し検証を除去する。

## 現状

- `Decoder::initialize` の `verify_effective_frame_info` は `MFXVideoDECODE_GetVideoParam` の `FrameInfo.Width` / `Height` を実サーフェスサイズとして検証しているが、実際の `mfxFrameSurface1.Info` と一致する保証はない
- `frame.rs` の `validate_mfx_frame_surface` は VPL の出力サーフェスを検証している
- `Encoder::new` は `MFXVideoENCODE_GetVideoParam` の実効 `FrameInfo` と VP9 `WriteIVFHeaders` を読み戻して検証している
- `read_decoded_surface` と `read_bitstream` は VPL の出力するポインタ、pitch、offset、length を検証している
- 場所によって VPL を信頼する箇所と検証する箇所が混在している

## 設計方針

`CODEBASE.md` の「VPL の値の扱い」に従う。

- ユーザーが渡す値は検証する
- VPL が返すメタデータは仕様どおりと信頼し、健全性の検証をしない
- VPL が返すメモリ記述は Rust のスライス生成に必要な最小限の安全境界だけ確認する
- API エラー、機能の欠落、対応外フォーマットはエラー処理や分岐として扱う

具体的な変更:

- `Decoder::initialize` の `verify_effective_frame_info` と `mfx_video_decode_get_video_param` を削除する
- `frame.rs` の `validate_mfx_frame_surface` / `frame_data_from_mfx` / `plane_slice` を削除し、`FrameData` と `validate_frame_data` / `frame_data_pitch` だけにする
- `validate_frame_data` から `coded_width` / `coded_height` を削除し、ユーザー入力の検証に限定する
- `Encoder::new` の実効 `FrameInfo` と VP9 `WriteIVFHeaders` の読み戻し検証を削除する
- エンコーダーのコピー先サーフェスの検証と、デコーダー出力サーフェスの max / crop / pitch / null 検証を削除する
- `mfxBitstream` の `DataOffset` / `DataLength` は Rust の安全境界として残す
- `is_supported_decoded_format` は対応形式の分岐として残す

## 完了条件

- VPL の出力値に対する健全性検証が、Rust の安全境界以外に残っていない
- `frame.rs` がユーザー入力の検証だけを持っている
- 実機で `cargo test --workspace` が通る
- 変更内容が `CODEBASE.md` の方針と一致している

## 参考

- `CODEBASE.md` の「VPL の値の扱い」
- VPL の `MFXVideoDECODE_GetVideoParam` / `MFXVideoENCODE_GetVideoParam` / `mfxFrameSurface1.Info` / `mfxFrameData.Pitch`
