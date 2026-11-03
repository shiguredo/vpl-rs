# Encoder::get_video_param() に ExtParam を渡す手段を追加する

- Priority: Medium
- Created: 2026-08-02
- Completed: 2026-11-03
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

`Encoder::get_video_param()` の戻り値を `VideoParam` に変更し、要求した拡張バッファの実効値を `ExtParams` として返すようにした。

### 変更内容

#### `src/encode.rs`

- `VideoParam` を追加した。基本パラメータの `mfxVideoParam` と、拡張バッファの実効値を持つ `ExtParams` を保持する
- `ExtParams` を追加した。`mfxExtCodingOption2` / `mfxExtCodingOption3` / `mfxExtVP9Param` を、要求した場合のみ `Some` で保持する
- `get_video_param()` は、要求したバッファと同じ種類のゼロ初期化バッファを構築して `ExtParam` として attach し、`MFXVideoENCODE_GetVideoParam` を呼ぶようにした
- 読み戻し用のバッファは、保持している要求値のバッファと分けた。要求値のバッファを読み戻し先に使うと、要求値が実効値で上書きされ、以降の `Reset` が要求値ではなくなる
- `ExtParam` は VPL 呼び出しの直後に、成否にかかわらず必ずクリアする
- `Header` の初期化を `coding_option2()` / `coding_option3()` / `vp9_param()` に切り出し、要求時と読み戻し時で共用するようにした

#### テスト

- `read_coding_option_ext_params()` から private フィールドへのアクセスを削除し、公開 API だけで読み戻すようにした
- GPU を必要としない単体テストを 3 件追加した。読み戻し用バッファの `Header` と値、要求していないバッファを構築しないこと、`ExtParams` への取り出しを検証する

#### ドキュメント

- `CHANGES.md` の `## develop` に追記した。戻り値の型変更は後方互換がないため、完了条件の `[ADD]` ではなく `[CHANGE]` とした
- `skills/shiguredo-vpl/SKILL.md` の公開 API 一覧と説明を更新した

### 確認

Intel GPU 実機で以下を実行し、すべて成功した。

- `cargo fmt --all --check`
- `INTEL_VPL=1 cargo clippy --workspace --all-targets -- -D warnings`
- `INTEL_VPL=1 cargo test --workspace -- --test-threads=1 --nocapture`

`reconfigure_preserves_coding_option_ext_params` が公開 API 経由で成功することを確認した。

## 参考

- 関連 issue: 0011（Encoder::reconfigure が ExtParam を送らない問題。本 issue を「公開 API の拡張が必要」として切り出した）、0012（Encoder::reconfigure が pending frame を drain しない問題。本 issue の API をテスト検証に使う）
