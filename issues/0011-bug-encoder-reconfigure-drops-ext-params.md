# Encoder::reconfigure が ExtParam を送らず LookAheadDepth / QVBRQuality が消える

- Priority: High
- Created: 2026-07-01
- Model: Opus 4.7
- Branch: feature/fix-encoder-reconfigure-drops-ext-params
- Polished: 2026-08-02

## 目的

`Encoder::reconfigure` が `MFXVideoENCODE_Reset` を呼ぶ際に拡張バッファ（`mfxExtCodingOption2` / `mfxExtCodingOption3`）を送らないため、`Encoder::new` で設定した `LookAheadDepth` / `QVBRQuality` が Reset のたびにデフォルトへリセットされるバグを修正する。

## 優先度根拠

High。以下による。

- **サイレントな品質劣化**: エラーは発生せず、ユーザーは reconfigure 後に品質が変わったことに気付きにくい。
- **公開 API 契約違反**: `EncoderConfig::look_ahead_depth` / `EncoderConfig::qvbr_quality` を Some で設定できる API を提供しておきながら、reconfigure でリセットされることが doc に一切書かれていない。
- **`RateControlMode::La` / `LaHrd` / `LaIcq` / `Qvbr` を使うユーザーが影響を受ける**: これらのモードで reconfigure すると LookAheadDepth / QVBRQuality が消え、指定モードの動作が壊れる。

## 現状

### Init 時の ExtParam 設定

`src/encode.rs` の `Encoder::new` は、`config.look_ahead_depth` や `config.qvbr_quality` が Some の場合に `mfxExtCodingOption2` / `mfxExtCodingOption3` を組み立て、`video_param.ExtParam` と `NumExtParam` にセットして `MFXVideoENCODE_Init` を呼ぶ。

```rust
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

`Encoder::new` は Init 直後に `video_param.ExtParam = std::ptr::null_mut()` / `NumExtParam = 0` を実行する。`ext_bufs` はローカル変数で関数終了時に drop されるため、`video_param.ExtParam` の生ポインタを保持し続けると use-after-free になる。そのため Init 直後に null に戻すのは正しい。しかし `video_param` は Encoder に保存され、あとで `reconfigure()` から再利用される。さらに Init 後の `GetVideoParam` 呼び出しも `ExtParam = null` の状態で呼ばれるため、`self.video_param` には拡張パラメータの実効値は反映されていない（実効値の取得は本 issue のスコープ外。reconfigure で送るのは `EncoderConfig` 由来の入力値で目的は達成される）。

### reconfigure() は ExtParam なしで Reset を呼ぶ

`src/encode.rs` の `reconfigure` は `self.video_param` に `ReconfigureParams` を適用した後、そのまま `MFXVideoENCODE_Reset` を呼ぶ。

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

`self.video_param.ExtParam = null / NumExtParam = 0` の状態でそのまま `MFXVideoENCODE_Reset` を呼ぶ。VPL は「ExtParam が空 = デフォルトを使う」と解釈するため、Init 時に設定した LookAheadDepth や QVBRQuality が失われる（この解釈は一次資料で明示されておらず、実機確認を完了条件に含める）。

### 影響を受けるフィールド

- `mfxExtCodingOption2.LookAheadDepth`（`RateControlMode::La` / `LaIcq` / `LaHrd` で使用）
- `mfxExtCodingOption3.QVBRQuality`（`RateControlMode::Qvbr` で使用）

将来 ExtCodingOption を増やす場合も同じ問題を踏むため、対応は「Box フィールド保持 + Reset 前にポインタ配列をセット」のパターンとして、将来の拡張時も同じパターンで対応する。

### `ReconfigureParams` の設計上の限界

現状 `ReconfigureParams` は `target_kbps` / `max_kbps` / `framerate_num` / `framerate_den` の 4 フィールドのみ。LookAheadDepth や QVBRQuality を reconfigure で変える API はなく、本 issue では対応しない。

## 設計方針

### 案 A: Encoder が元の EncoderConfig を保持し、reconfigure 時に再構築

`Encoder` 構造体に `config: EncoderConfig` を保持し、`reconfigure()` の直前に `EncoderConfig` から ExtParam バッファ（`co2` / `co3`）とそのポインタ配列 `ext_bufs` を再構築する。

- 長所: Init 時と同じ組み立て手順を再利用できる。
- 短所: reconfigure のたびに ExtParam バッファの組み立てコードが走る。

### 案 B: ExtParam バッファを Encoder のフィールドとして保持する

`Encoder` に `ext_co2: Option<Box<sys::mfxExtCodingOption2>>` / `ext_co3: Option<Box<sys::mfxExtCodingOption3>>` と、それらへのポインタ配列 `ext_bufs: Vec<*mut sys::mfxExtBuffer>` をフィールドとして保持し、Init 後もライフタイムを維持する。

- 長所: バッファ本体もポインタ配列も一度だけ構築すればよく、reconfigure ごとの再構築が不要。`Box` のヒープアドレスは Encoder の move で不変なため、ポインタの有効性は Encoder の生存期間を通じて維持される。
- 短所: 生ポインタを Encoder に長期保持することになるが、`mfxExtCodingOption2` / `mfxExtCodingOption3` は全整数フィールドの POD（生ポインタを含まない）であり、`Box<...>` は自動で `Send`。既存の `unsafe impl Send` の Safety コメントにフィールドの追記を行うだけで済み、追加の安全性検証は不要。

**案 B を採用する。**

- `Encoder::new`: ExtParam 構築後、`ext_co2` / `ext_co3` を `Box` に包んで `Encoder` に保存し、`ext_bufs` も一度だけ構築して保存する（`video_param.ExtParam` の null クリアは従来通り行う。`ext_bufs` フィールド自体は保持し続ける）。
- `Encoder::reconfigure`: `ext_bufs` が空でない場合のみ、`self.ext_bufs.as_mut_ptr()` を `self.video_param.ExtParam` に、`self.ext_bufs.len() as u16` を `NumExtParam` にセットしてから `MFXVideoENCODE_Reset` を呼ぶ（`look_ahead_depth` / `qvbr_quality` が両方 None の場合は従来どおり `ExtParam = null` のままにする）。Reset の成否にかかわらず、関数の最後に `ExtParam = null` / `NumExtParam = 0` に戻す（「`reconfigure` の外では `video_param.ExtParam` が常にクリア済み」の不変条件を維持するため）。
- 構築コード（バッファの組み立てとポインタ配列の構築）は `Box` 化によりヒープアドレスが安定するため、値を返すヘルパー関数（例: `build_ext_buffers`）に抽出可能である。`Encoder::new` はこのヘルパーの戻り値をフィールドに保存し、`reconfigure` はフィールドを参照するだけになる。

### 適用順序（他 issue との関係）

- **issue 0012** (`0012-bug-encoder-reconfigure-does-not-drain-pending`): reconfigure が pending frame を drain しない問題。本 issue (0011) と 0012 は同一の `Encoder::reconfigure` を修正するため、**0011 を先に適用し、その差分の上に 0012 の変更を重ねる**。0012 側は 0011 の ExtParam セット処理を前提とした設計になっている。また 0012 の reconfigure 処理（パラメータ適用 → DrainPending → Reset）で早期リターン経路が挟まるため、「`video_param.ExtParam` のクリアを関数の最後で行う」ことを 0012 の実装でも維持する（0011 の完了条件 3 の不変条件を 0012 のエラー経路でも破らないこと）。
- **issue 0020** (`0020-refactor-split-encode-module`): encode.rs のサブモジュール分割。0020 は本 issue (0011) の「Box フィールド保持 + `build_ext_buffers` ヘルパー」設計に合わせて調整済みである（0020 の設計方針・完了条件 2 参照。「ExtBuffers はローカル変数として使用し、Init 後は即破棄する」という旧設計は 0020 側で採用していない）。0020 の `build_ext_buffers` 切り出しは本 issue のヘルパー関数と共通化できる。

## 完了条件

以下すべてを満たす。

1. `Encoder` 構造体に以下のフィールドを追加する:
   - `ext_co2: Option<Box<sys::mfxExtCodingOption2>>`
   - `ext_co3: Option<Box<sys::mfxExtCodingOption3>>`
   - `ext_bufs: Vec<*mut sys::mfxExtBuffer>`
2. `Encoder::new` で ExtParam 構築後、`ext_co2` / `ext_co3` / `ext_bufs` を `Encoder` に保存する（`video_param.ExtParam` の null クリアは従来通り行う。クリア理由のコメントは「ローカルの `ext_bufs` が drop されるため」から「`video_param.ExtParam` は `reconfigure` の外では常にクリア済みという不変条件を維持するため」に更新する）。構築コードはヘルパー関数（例: `build_ext_buffers`）に抽出し、`Encoder::new` で使用する（issue 0020 の共通ヘルパー切り出しと共通化できる形にする）。
3. `Encoder::reconfigure` 内で、`ext_bufs` が空でない場合のみ `self.ext_bufs.as_mut_ptr()` を `self.video_param.ExtParam` に、`self.ext_bufs.len() as u16` を `NumExtParam` にセットしてから `MFXVideoENCODE_Reset` を呼ぶ（`look_ahead_depth` / `qvbr_quality` が両方 None の場合は従来どおり `ExtParam = null` のままにする。空 `Vec` の `as_mut_ptr()` は dangling ポインタを返すため）。Reset の成否にかかわらず、関数の最後に `ExtParam = null` / `NumExtParam = 0` に戻す。
4. `Encoder` の `unsafe impl Send` の Safety コメントに `ext_co2` / `ext_co3` / `ext_bufs` の保持を追記する。Safety 根拠は、`ext_co2` / `ext_co3` は全整数フィールドの POD で `Box` は自動 `Send` であること、`ext_bufs` は「`Box` のヒープアドレスが Encoder の move / スレッド移動で不変なため、ポインタの指す先が常に有効」であることを書く。
5. `Encoder::reconfigure` の doc に「reconfigure 後も `LookAheadDepth` / `QVBRQuality` は維持される」ことを明記する。
6. Reset 後に LookAheadDepth / QVBRQuality が維持されることを実機で確認する（既存の `test-intel-vpl` ジョブの self-hosted GPU ランナー上で確認）。検証手段は次の 2 つ:
   - `GetVideoParam` に `ExtParam`（`mfxExtCodingOption2` / `mfxExtCodingOption3`）を渡して実効値を確認する方法。**現行の公開 API `Encoder::get_video_param()` は `ExtParam` を渡す手段がないため、この方法を使うには公開 API の拡張（ExtParam を渡す手段の追加）が必要になる**。拡張は 0012 のテスト実装時に合わせて対応するか、別 issue として対応する。
   - テストコードによる自動検証は **issue 0012 の reconfigure テストに「reconfigure 後も LookAheadDepth / QVBRQuality が保持される」検証を含める形で連携し、0012 の完了条件にもこの検証項目の追加を依頼する**（0012 の現行完了条件にはこの検証が含まれていないため、0011 側から 0012 への連携事項として明示する）。
7. `CHANGES.md` の `## develop` に `[FIX]` として「reconfigure 時に ExtParam（LookAheadDepth / QVBRQuality）を再送するよう修正」を追記する。

## 影響範囲

- `src/encode.rs`（`Encoder` 構造体に `ext_co2` / `ext_co3` / `ext_bufs` フィールド追加、`Encoder::new`: ExtParam 構築後にフィールド保存、`Encoder::reconfigure`: フィールドの `ext_bufs` をセットして Reset 呼び出し、`unsafe impl Send` の Safety コメント更新、`reconfigure` の doc 更新）
- `CHANGES.md`

## 参考

- Init 時の ExtParam 組み立て: `src/encode.rs` の `Encoder::new` の拡張バッファ設定部
- reconfigure の実装: `src/encode.rs` の `Encoder::reconfigure`
- 関連 issue: 0012（reconfigure が pending frame を drain しない別問題。適用順序は「設計方針」の「適用順序」を参照）、0020（encode.rs のサブモジュール分割。本 issue 適用後の設計に合わせて調整する）
