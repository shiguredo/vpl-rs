# Encoder::get_video_param() に ExtParam を渡す手段を追加する

- Priority: Medium
- Created: 2026-08-02
- Completed: {YYYY-MM-DD}
- Model: DeepSeek V4 Flash
- Branch: feature/add-get-video-param-ext-param
- Polished: {YYYY-MM-DD}

## 目的

`Encoder::get_video_param()` で拡張パラメータ（`mfxExtCodingOption2` / `mfxExtCodingOption3`）の実効値を取得できるようにする。現行の公開 API は `ExtParam` を渡す手段がないため、reconfigure 後の拡張パラメータの実効値を検証できない。issue 0011 の設計方針で「公開 API の拡張が必要になる」と明記され、「別 issue として対応する」とされたものである。

## 現状

`src/encode.rs` の `get_video_param()` は `MFXVideoENCODE_GetVideoParam` を呼び出して `mfxVideoParam` を返す。しかし、`video_param.ExtParam` は `Encoder::new` の Init 直後に `std::ptr::null_mut()` / `NumExtParam = 0` に戻されており、拡張パラメータの実効値は取得できない。

`GetVideoParam` に `mfxExtCodingOption2` / `mfxExtCodingOption3` を `ExtParam` として渡せば、VPL が実効値を書き戻してくれる。しかし現行の公開 API には `ExtParam` を渡す手段がなく、拡張パラメータの実効値確認ができない。

## 設計方針

- `Encoder::get_video_param()` に拡張パラメータを渡す手段を追加する（例: 引数で `mfxExtCodingOption2` / `mfxExtCodingOption3` を受け取り、`ExtParam` として設定して `MFXVideoENCODE_GetVideoParam` を呼び出す）。
- 追加する API のシグネチャは、利用側（reconfigure 後の実効値検証）の使いやすさと、`EncoderConfig` の設定値（`look_ahead_depth` / `qvbr_quality` 等）との対応を考慮して設計する。
- 設計の詳細は、issue 0012（reconfigure が pending frame を drain しない問題）のテスト実装での利用方法と合わせて確定する。

## 完了条件

以下すべてを満たす。

1. `Encoder::get_video_param()`（または同等の新規公開 API）で、拡張パラメータ（`mfxExtCodingOption2` / `mfxExtCodingOption3`）の実効値を取得できる。
2. 取得した実効値が、reconfigure で設定した `EncoderConfig` の値と整合することを確認できる（テストで検証可能であること）。
3. `CHANGES.md` の `## develop` に `[ADD]` として追記する。

## 解決方法

- `src/encode.rs` の `get_video_param()` を拡張するか、新規メソッドを追加する。
- 拡張パラメータを `ExtParam` として渡す際は、`mfxExtCodingOption2` / `mfxExtCodingOption3` の `Header` 設定（`MFX_EXTBUFF_CODING_OPTION2` / `MFX_EXTBUFF_CODING_OPTION3`）を正しく初期化する。

## 参考

- 関連 issue: 0011（Encoder::reconfigure が ExtParam を送らない問題。本 issue を「公開 API の拡張が必要」として切り出した）、0012（Encoder::reconfigure が pending frame を drain しない問題。本 issue の API をテスト検証に使う）
