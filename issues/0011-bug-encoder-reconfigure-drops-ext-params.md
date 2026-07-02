# Encoder::reconfigure が ExtParam を送らず LookAheadDepth / QVBRQuality が消える

- Priority: High
- Created: 2026-07-01
- Model: Opus 4.7
- Branch: feature/fix-encoder-reconfigure-drops-ext-params
- Polished: 2026-07-01

## 目的

`Encoder::reconfigure` が `MFXVideoENCODE_Reset` を呼ぶ際に拡張バッファ（`mfxExtCodingOption2` / `mfxExtCodingOption3`）を送らないため、`Encoder::new` で設定した `LookAheadDepth` / `QVBRQuality` が Reset のたびにデフォルトへリセットされるバグを修正する。ユーザーは「ビットレートだけ変えたつもり」なのに、Look Ahead depth や QVBR quality までサイレントに消される。

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

`ext_bufs` はローカル変数で関数終了時に drop されるため、`video_param.ExtParam` の生ポインタを保持し続けると use-after-free になる。そのため Init 直後に null に戻すのは正しい。しかし `video_param` は Encoder に保存され、あとで `reconfigure()` から再利用される。さらに Init 後の `GetVideoParam` 呼び出し（L926）も `ExtParam = null` の状態で呼ばれるため、`self.video_param` には拡張パラメータの実効値は反映されていない。

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

現状 `ReconfigureParams` は `target_kbps` / `max_kbps` / `framerate_num` / `framerate_den` の 4 フィールドのみ。LookAheadDepth や QVBRQuality を reconfigure で変える API はなく、本 issue では対応しない（動的変更の需要が確認された時点で別 issue として対応）。

## 設計方針

### 案 A: Encoder が元の EncoderConfig を保持し、reconfigure 時に再構築

`Encoder` 構造体に `config: EncoderConfig` を保持し、`reconfigure()` の直前に:

1. `EncoderConfig` から ExtParam バッファ（`co2` / `co3`）とそのポインタ配列 `ext_bufs` を再構築する（Init 時と同じ手順）
2. `self.video_param.ExtParam` と `NumExtParam` を再セットする
3. `MFXVideoENCODE_Reset` を呼ぶ
4. Reset 後に `ExtParam = null` / `NumExtParam = 0` に戻す（use-after-free 防止）

- 長所: Init 時と同じ組み立て手順を再利用できる。
- 短所: reconfigure のたびに ExtParam バッファの再構築が発生する。

### 案 B: ExtParam バッファを Encoder のフィールドとして保持する

`Encoder` に `ext_co2: Option<Box<sys::mfxExtCodingOption2>>` / `ext_co3: Option<Box<sys::mfxExtCodingOption3>>` と `ext_bufs: Vec<*mut sys::mfxExtBuffer>` を保持し、Init 後もライフタイムを維持する。

- 長所: reconfigure ごとに再構築が不要。ポインタの再生成も不要なため Init/Reset 呼び出しだけの単純なコードになる。
- 短所: 生ポインタを Encoder に長期保持することになり、unsafe impl Send の安全性検証が増える。ただし Encoder は既に `video_param`（生ポインタを内包する FFI 型）を保持し unsafe impl Send されているため、追加複雑さは軽微。

**案 B を採用する**。案 A の「毎回再構築」はコード重複とランタイムコストの両面で劣る。案 B では ExtParam 構築ロジックは Init 時と同一であり、Encoder のフィールドとして `Box` でヒープに保持することでポインタの有効性をライフタイムで管理できる。

### 実装上の注意

`ext_bufs` は `ext_co2` / `ext_co3` への生ポインタを保持する自己参照構造のため、単純に「値を返すヘルパー関数」に抽出できない。代わりに構築コードをクロージャまたはインラインのまま `new` と `reconfigure` の両方から使える形に整理する。具体的には、`Encoder` のフィールドに保存した `ext_co2` / `ext_co3` へのポインタを `self.ext_bufs` として再構成するメソッドを実装する（Reset の直前に毎回呼ぶ）。

## 完了条件

以下すべてを満たす。

1. `Encoder` 構造体に以下のフィールドを追加する:
   - `ext_co2: Option<Box<sys::mfxExtCodingOption2>>`
   - `ext_co3: Option<Box<sys::mfxExtCodingOption3>>`
2. `Encoder::new` で ExtParam 構築後、`ext_co2` / `ext_co3` を `Encoder` に保存する（`video_param.ExtParam` の null クリアは従来通り行う）。
3. `Encoder::reconfigure` 内で、`self.ext_co2` / `self.ext_co3` から `ext_bufs` を再構成し `self.video_param.ExtParam` / `NumExtParam` にセットしてから `MFXVideoENCODE_Reset` を呼ぶ。Reset 後は `ExtParam = null` / `NumExtParam = 0` に戻す。
4. `CHANGES.md` の `## develop` に `[FIX]` として「reconfigure 時に ExtParam（LookAheadDepth / QVBRQuality）を再構築するよう修正」を追記する。

注: reconfigure 後の挙動を検証する結合テストは実 GPU 依存のため本 issue のスコープ外とする（既存の `test-intel-vpl` ジョブで手動確認）。

## 影響範囲

- `src/encode.rs`（`Encoder` 構造体に `ext_co2` / `ext_co3` フィールド追加、`Encoder::new`: ExtParam 構築後にフィールド保存、`Encoder::reconfigure`: `ext_co2` / `ext_co3` からポインタ配列を再構成して Reset 呼び出し）
- `CHANGES.md`

## 参考

- Init 時の ExtParam 組み立て: `src/encode.rs:876-914`
- reconfigure の実装: `src/encode.rs:986-1022`
- 関連 issue: 0012（`Encoder::reconfigure` が pending frame を drain しない別問題。reconfigure を同時に修正するため実装順序に注意）
