# Encoder::reconfigure が ExtParam を送らず LookAheadDepth / QVBRQuality が消える

- Priority: High
- Created: 2026-07-01
- Completed: {YYYY-MM-DD}
- Model: Opus 4.7
- Branch: feature/fix-encoder-reconfigure-drops-ext-params
- Polished: {YYYY-MM-DD}

## 目的

`Encoder::reconfigure` が `MFXVideoENCODE_Reset` を呼ぶ際に拡張バッファ（`mfxExtCodingOption2` / `mfxExtCodingOption3`）を送らないため、`Encoder::new` で設定した `LookAheadDepth` / `QVBRQuality` が Reset のたびにデフォルトへリセットされるバグを修正する。ユーザーは「ビットレートだけ変えたつもり」なのに、Look Ahead depth や QVBR quality まで silent に消される。

## 優先度根拠

High。以下による。

- **サイレントな品質劣化**: エラーは発生せず、ユーザーは reconfigure 後に品質が変わったことに気付きにくい。
- **公開 API 契約違反**: `EncoderConfig::look_ahead_depth` / `EncoderConfig::qvbr_quality` を Some で設定できる API を提供しておきながら、reconfigure でリセットされることが doc に一切書かれていない。
- **`RateControlMode::La` / `LaHrd` / `LaIcq` / `Qvbr` を使うユーザーが影響を受ける**: これらのモードで reconfigure すると LookAheadDepth / QVBRQuality が消え、指定モードの動作が壊れる。特に低ビットレートストリーミングで Look Ahead は品質に大きく効くので、実運用での劣化が目立つ。

## 現状

### Init 時の ExtParam 設定

`src/encode.rs:876-914` の `Encoder::new` は、`config.look_ahead_depth` や `config.qvbr_quality` が Some の場合に `mfxExtCodingOption2` / `mfxExtCodingOption3` を組み立て、`video_param.ExtParam` と `NumExtParam` にセットして `MFXVideoENCODE_Init` を呼ぶ。

```rust
// L903-914
let mut ext_bufs: Vec<*mut sys::mfxExtBuffer> = Vec::new();
if let Some(ref mut co2) = ext_co2 {
    ext_bufs.push(co2 as *mut sys::mfxExtCodingOption2 as *mut sys::mfxExtBuffer);
}
if let Some(ref mut co3) = ext_co3 {
    ext_bufs.push(co3 as *mut sys::mfxExtCodingOption3 as *mut sys::mfxExtBuffer);
}
if !ext_bufs.is_empty() {
    video_param.ExtParam = ext_bufs.as_mut_ptr();
    video_param.NumExtParam = ext_bufs.len() as u16;
}

lib.mfx_video_encode_init(session.as_ptr(), &mut video_param)?;
```

### Init 後に ExtParam がクリアされる

`src/encode.rs:922-923`:

```rust
// Init 後に ExtParam ポインタをクリアする（ローカルの ext_bufs が drop されるため）
video_param.ExtParam = std::ptr::null_mut();
video_param.NumExtParam = 0;
```

`ext_bufs` はローカル変数で関数終了時に drop されるため、`video_param.ExtParam` の生ポインタを保持し続けると use-after-free になる。そのため Init 直後に null に戻すのは正しい。しかし `video_param` は Encoder に保存され、あとで `reconfigure()` から再利用される。

### reconfigure() は ExtParam なしで Reset を呼ぶ

`src/encode.rs:986-1022`:

```rust
pub fn reconfigure(&mut self, params: ReconfigureParams) -> Result<(), Error> {
    // ...
    // 現在の video_param をベースに変更を適用する
    unsafe {
        let enc = &mut self.video_param.__bindgen_anon_1.mfx.__bindgen_anon_1.__bindgen_anon_1;
        if let Some(target_kbps) = params.target_kbps {
            enc.__bindgen_anon_2.TargetKbps = target_kbps;
        }
        // ...
    }

    self.session
        .lib()
        .mfx_video_encode_reset(self.session.as_ptr(), &mut self.video_param)
}
```

`self.video_param.ExtParam = null / NumExtParam = 0` の状態でそのまま `MFXVideoENCODE_Reset` を呼ぶ。VPL は「ExtParam が空 = デフォルトを使う」と解釈するため、Init 時に設定した LookAheadDepth や QVBRQuality が失われる。

### 影響を受けるフィールド

- `mfxExtCodingOption2.LookAheadDepth`（`RateControlMode::La` / `LaIcq` / `LaHrd` で使用）
- `mfxExtCodingOption3.QVBRQuality`（`RateControlMode::Qvbr` で使用）

将来 ExtCodingOption を増やす場合も同じ問題を踏むため、汎用的な対応が必要。

### `ReconfigureParams` の設計上の限界

現状 `ReconfigureParams` は `target_kbps` / `max_kbps` / `framerate_num` / `framerate_den` の 4 フィールドのみ。LookAheadDepth や QVBRQuality を reconfigure で変える API はない。しかし、変えないとしても Init 時の値を維持する必要がある。

## 設計方針

### 案 A: Encoder が元の EncoderConfig を保持し、reconfigure 時に再構築

`Encoder` 構造体に `config: EncoderConfig` を保持し、`reconfigure()` の直前に:

1. `EncoderConfig` から ExtParam バッファ（`co2` / `co3`）を再構築する（Init 時と同じ手順）
2. `self.video_param.ExtParam` と `NumExtParam` を再セットする
3. `MFXVideoENCODE_Reset` を呼ぶ
4. Reset 後に `ExtParam = null` / `NumExtParam = 0` に戻す（use-after-free 防止）

- 長所: Init 時と同じ組み立て手順を再利用できる。
- 短所: `EncoderConfig` を Encoder に持たせるためメモリを少し食う（数百 bytes 程度）。

### 案 B: ExtParam バッファを Encoder のフィールドとして保持する

`Encoder` に `ext_co2: Option<Box<sys::mfxExtCodingOption2>>` / `ext_co3: Option<Box<sys::mfxExtCodingOption3>>` と `ext_bufs: Vec<*mut sys::mfxExtBuffer>` を保持し、Init 後もライフタイムを維持する。`reconfigure` では `video_param.ExtParam = self.ext_bufs.as_mut_ptr()` として使う。

- 長所: reconfigure ごとに再構築が不要。
- 短所: 生ポインタを Encoder に長期保持することになり Send 実装や Drop 順序の設計が複雑になる。

推奨は **案 A**。EncoderConfig 保持のコストは限定的で、実装が理解しやすい。

## 完了条件

以下すべてを満たす。

1. `Encoder::reconfigure` 後も、Init 時に設定した `LookAheadDepth` / `QVBRQuality` が VPL 側で維持されることを確認する。
2. 上記を検証するテストを追加する。実 GPU がない環境でも、`Encoder::get_video_param` の返り値を突き合わせるか、少なくとも `Encoder` が保持する `EncoderConfig` の状態から想定される ExtParam が組み立てられていることを検証する。実 GPU が必要なテストは `#[cfg(intel_vpl)]` に置く。
3. Reset 後に `video_param.ExtParam = null` に戻す処理が入っており、`ext_bufs` の生ポインタが dangling しないことを検証する（コード検査で確認 + Drop 順序のドキュメント化）。
4. `ReconfigureParams` に `look_ahead_depth` / `qvbr_quality` を追加するかは本 issue のスコープ外とする（機能追加は別 issue）。本 issue は「Init 時の値を維持する」ことのみが目的。
5. `CHANGES.md` の `## develop` に `[FIX]` として追記する。

## 影響範囲

- `src/encode.rs`（`Encoder` 構造体、`Encoder::new`、`Encoder::reconfigure`）
- `tests/test_roundtrip.rs`（reconfigure 後の LookAheadDepth 維持テスト。実 GPU 依存）
- `CHANGES.md`

## 参考

- `/review-code` の致命的指摘 F3
- 関連 issue: 0012（Encoder::reconfigure が pending frame を drain しない別問題）
