# VPL の値のチェックを一貫性のある方針にする

- Priority: Medium
- Created: 2026-11-08
- Completed: {YYYY-MM-DD}
- Model: DeepSeek V4 Flash
- Branch: feature/refactor-unify-vpl-value-checking
- Polished: {YYYY-MM-DD}

## 目的

VPL が返す値の検証方針が場所によって異なっている。`CODEBASE.md` に「VPL の値の扱い」を追記し、その方針に沿ってユーザー入力だけを検証する形に統一する。

## 現状

- `CODEBASE.md` に VPL の値の扱いの方針が無い
- `Encoder::new` は `MFXVideoENCODE_GetVideoParam` の実効 `FrameInfo` と VP9 `WriteIVFHeaders` を読み戻して要求値と比較している
- `read_decoded_surface` は出力サーフェスの null ポインタ、`crop = 0`、`pitch < crop_w` を検証している
- `sync_and_collect` は `mfxBitstream` の `DataOffset` / `DataLength` をバッファ範囲と照合している
- `copy_to_surface_planes` はユーザー入力の `FrameData` とコピー先の pitch を検証している
- `frame.rs`、`verify_effective_frame_info`、max サイズ関連のコードは現在のコードに無い

## 設計方針

本 issue で `CODEBASE.md` に次を追記する。

- ユーザーが渡す値は検証する
- VPL が返すメタデータ (`mfxFrameInfo`、`mfxVideoParam`、FourCC、ChromaFormat など) は仕様どおりと信頼し、健全性の検証をしない
- VPL が返すメモリ記述 (`mfxFrameData` のポインタや Pitch、`mfxBitstream` の `DataOffset` / `DataLength`) は、Rust の安全境界を満たすために必要な確認だけ行う。VPL の値の正しさを検証するためのものではない。確認内容は例えば次の項目を含むが、これらに限らない
  - 生ポインタの null、参照・書き込みの前提
  - スライス生成と読み書きの範囲
  - 長さ・オフセットの計算とオーバーフロー
  - FFI の関数ポインタ呼び出しの前提
- API エラー、機能の欠落、対応外フォーマットは検証ではなくエラー処理や分岐として扱う

現状のコードへの適用:

- `Encoder::new` の実効 `FrameInfo` と VP9 `WriteIVFHeaders` の読み戻し検証を削除する
- `read_decoded_surface` から `crop = 0` と `pitch < crop_w` の検証を削除する。null ポインタとサイズ計算のオーバーフローは Rust の安全境界として残す
- `sync_and_collect` の `DataOffset` / `DataLength` の範囲検証は安全境界として残す
- `copy_to_surface_planes` のユーザー入力検証とコピー先 pitch の検証は、ユーザー入力と Rust の安全境界として残す
- `is_supported_decoded_format` は対応形式の分岐として残す
- `FrameData` のデータ長 `rows * pitch` の検証はユーザー入力の検証として残す

## 完了条件

- `CODEBASE.md` に「VPL の値の扱い」が追記されている
- VPL の出力メタデータに対する健全性検証が、Rust の安全境界以外に残っていない
- 実機で `cargo test --workspace` が通る
- 変更内容が `CODEBASE.md` の方針と一致している

## 参考

- VPL の `MFXVideoENCODE_GetVideoParam` / `mfxFrameSurface1.Info` / `mfxFrameData.Pitch`
- VPL の `MFXVideoDECODE_DecodeFrameAsync` と `mfxBitstream` の `DataOffset` / `DataLength`
