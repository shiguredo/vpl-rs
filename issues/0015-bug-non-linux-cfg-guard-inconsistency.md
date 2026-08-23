# 非 Linux 環境で cfg ガードが不統一のためリンカエラーや誤誘導が発生する

- Priority: High
- Created: 2026-07-01
- Model: Opus 4.7
- Branch: feature/fix-non-linux-cfg-guard-inconsistency
- Polished: 2026-08-02

## 目的

このクレートは Linux (x86_64) 前提だが、`Encoder` / `Decoder` の関数群に `#[cfg(target_os = "linux")]` ガードが付いていないため、非 Linux でビルドすると libvpl のシンボルが未解決のままになる。lib crate は `cargo build` ではリンカが走らないため一見成功するが、`cargo test` や下流バイナリのリンク時に生のリンカ未解決シンボルエラーで失敗する。加えて `list_adapters()` は非 Linux で空 `Vec` を返す実装が用意されていて「非 Linux でも使える」ような錯覚を与える。cfg ガードの方針を一貫させ、非 Linux では明確なコンパイルエラーで拒否する。

## 優先度根拠

High。以下による。

- **開発者体験の悪さ**: macOS / Windows で `cargo test` や下流ビルドを試みるとリンカ・エラーが返るが、原因が「Linux 前提のクレート」と伝わらない。undef sym をひとつずつ調べることになる。さらに `cargo build` 自体は成功するため、問題の存在に気付かないまま開発を進めてしまう。
- **API の一貫性の欠如**: `list_adapters` / `supported_codecs` は cfg 分岐しているのに、`Encoder::new` / `Decoder::new` は分岐なし。同じ crate で扱いが揃っていない。
- **誤誘導**: `list_adapters()` が非 Linux で `Ok(Vec::new())` を返す実装は「非 Linux でも API を触れる」と積極的に誤認させるバグであり、早期修正が必要。

## 現状

### cfg 分岐済み

- `src/adapter.rs` の `list_adapters`: Linux 実装と、非 Linux で `Ok(Vec::new())` を返す実装がある
- `src/codec_info.rs` の `supported_codecs`: `#[cfg(target_os = "linux")]` あり、非 Linux 版なし

### cfg 分岐なし

- `src/encode.rs` の `Encoder` 全体、`EncoderConfig` 全体、`Encoder::new` などの関数（`#[cfg(test)]` 以外の cfg なし）
- `src/decode.rs` の `Decoder` 全体、`DecoderConfig` 全体、`Decoder::new` などの関数（cfg なし）
- `src/vpl.rs` の `VplLibrary`（`pub(crate)` の内部型）と `create_session` などの関数（`#[cfg(test)]` 以外の cfg なし）
- `src/lib.rs` のモジュール宣言 / `pub use` は全 OS で公開

### build.rs の分岐

`build.rs` は以下の順で動作する:

1. 全 OS で libvpl ヘッダを git clone し、bindgen で `bindings.rs` を生成する（`DOCS_RS` チェックより前。OS 分岐なし）
2. `DOCS_RS` が設定されていれば早期 return
3. Linux のみ `cfg!(target_os = "linux")` で libvpl を CMake static build してリンクする

Linux 以外では libvpl 実体が build されずリンクもされないため、`sys::MFXVideoENCODE_Init` などの関数シンボルが未解決になる。

### ユーザー体験

非 Linux での実際の挙動:

- `cargo build`（lib のみ）: **成功する**（rlib はリンクしないため。問題が顕在化しない）
- `cargo test`（lib test バイナリのリンク）: リンカが「MFXVideoENCODE_Init が見つからない」と生のエラーを出す
- 下流クレートのバイナリをビルド: 同様にリンカエラー

`src/lib.rs` のドキュメントに「動作要件: Linux (x86_64)」と明記しているが、コンパイル時にエラー・メッセージで伝わらない。

### `list_adapters` の空 `Vec` 実装が誤解を招く

非 Linux で `list_adapters()` が `Ok(Vec::new())` を返す実装（`src/adapter.rs` の非 Linux 版）は「非 Linux でも API を触れる」印象を与える。しかし `Encoder::new` / `Decoder::new` を呼ぼうとすると（テストや下流ビルドで）リンカエラーになるため、実際には触れない。

## 設計方針

### 案 A: crate 全体を `#[cfg(all(target_os = "linux", target_arch = "x86_64"))]` でガードする（推奨）

`src/lib.rs` のドキュメントコメント直後（`mod` 宣言の前）に `compile_error!` を配置する:

```rust
#[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
compile_error!("shiguredo_vpl requires Linux x86_64. Other targets are not supported.");
```

- 長所: 非 Linux では codegen / リンク前に明確なエラーメッセージで失敗する（リンカ未解決シンボルエラーが消える）。動作要件（「Linux (x86_64)」）とガード条件が一致する。
- 短所: `compile_error!` はマクロ展開時にエラーを報告するだけで、モジュールの名前解決・型検査は継続する（モジュール内に型エラーがあれば compile_error! と同時に報告される。非 Linux で bindings.rs を含む全モジュールの型チェックが通る場合は compile_error! のみが報告される）。また `build.rs` は rustc より先に必ず実行されるため、非 Linux でも git clone + bindgen は走る（libvpl のリンクのみスキップされる）。

**build.rs のガードも合わせて変更する**: `build.rs` の libvpl ビルド分岐は現在 `cfg!(target_os = "linux")` のため、aarch64 Linux では compile_error! より先に CMake ビルドが実行され、CMake ビルドが失敗する場合は panic が先に出る（compile_error! のメッセージに到達しない）。これを防ぐため、`build.rs` の分岐を `cfg!(all(target_os = "linux", target_arch = "x86_64"))` に変更し、非 x86_64 では libvpl のビルド・リンクをスキップする。

**案 A の付随対応**: 非 Linux 版 `list_adapters` を削除すると、`src/lib.rs` の `pub use adapter::{..., list_adapters}` が非 Linux で `unresolved import` エラー（E0432）になる。この二次エラーを防ぐため、`list_adapters` の `pub use` に `#[cfg(target_os = "linux")]` を付けて分岐する（`adapter` モジュール内の他の公開型 `AdapterSelector` / `AdapterInfo` / `MediaAdapterType` / `PciAddress` は sys 型に依存しない純粋データ型であり、非 Linux でもコンパイル可能なため分岐不要。`codec_info.rs` の非 gated 型定義 `VideoCodecType` / `CodecInfo` 等も同じ）。

### 案 B: 非 Linux でも API を空実装で提供する

`Encoder` / `Decoder` の全 API を非 Linux で `Err(Error::new_custom("...", "not supported on this platform"))` を返す実装にする。

- 長所: 非 Linux でも cargo test は通り、ドキュメント生成もどの OS でもできる。
- 短所: 実装コスト大。使えない API を用意する意味が薄い。

推奨は **案 A**。crate の目的が「Intel VPL の Rust バインディング」であり、本 crate では Linux x86_64 のみをサポートする（Intel VPL は Windows でも動くが本 crate では対応していない）以上、明確に拒否するのが正しい。

### `list_adapters` の非 Linux 実装をどうするか

- **案 A 採用**: `list_adapters` の非 Linux 実装を削除し、`src/lib.rs` の `pub use` を cfg 分岐する（詳細は前述の「案 A の付随対応」のとおり）。

### docs.rs 対応

CI の `docs-rs` ジョブ（ubuntu-24.04、ネットワークあり）は `DOCS_RS=1 cargo doc --no-deps` を実行しており、Linux 上で通ることを確認できる（`build.rs` の `DOCS_RS` 分岐で libvpl のビルド・リンクをスキップしつつ bindings.rs は生成されるため）。macOS での `DOCS_RS=1 cargo doc --no-deps` は `compile_error!` で拒否されるが、これは意図通り（macOS 向けのドキュメント生成は不要）。

なお、実 docs.rs サービス（docs.rs のビルド環境）はネットワークが遮断されており、本 crate の `build.rs` が `DOCS_RS` チェックより先に git clone を実行するため、実 docs.rs でのビルドは失敗する既存の問題がある。これは本 issue のスコープ外とする（別 issue で対応予定）。

## 完了条件

以下すべてを満たす。

1. `src/lib.rs` の先頭（ドキュメントコメント直後、`mod` 宣言の前）に `#[cfg(not(all(target_os = "linux", target_arch = "x86_64")))] compile_error!(...)` を追加する。
2. `src/adapter.rs` の非 Linux 版 `list_adapters`（空 `Vec` を返す実装）を削除し、`src/lib.rs` の `pub use adapter::{..., list_adapters}` の `list_adapters` を `#[cfg(target_os = "linux")]` で分岐する（非 Linux で `unresolved import` の二次エラーが出ないようにする）。
3. `build.rs` の libvpl ビルド分岐を `cfg!(target_os = "linux")` から `cfg!(all(target_os = "linux", target_arch = "x86_64"))` に変更する（aarch64 Linux で compile_error! より先に CMake ビルド失敗の panic が出ないようにする）。
4. 非 Linux 環境（例: macOS）で `cargo build` と `cargo test --no-run` を実行し、`compile_error!` のメッセージで失敗し、リンカ未解決シンボルエラーが出ないことを確認する（CI は全ジョブ Linux のため手動確認）。なお、`cargo test --no-run` では、0023 適用前は `frame_surface_gpu_required`（`src/vpl.rs` のテストモジュール）の `crate::list_adapters()` 参照由来の `E0425`（cannot find function `list_adapters`）が `compile_error!` と同時に報告される。これは 0023 が `#[cfg(intel_vpl)]` を付与することで解消される（本 issue のスコープ外）。
5. `skills/shiguredo-vpl/SKILL.md` の「動作 OS の制約」（「macOS / Windows でビルドできるとしても `list_adapters()` は空を返し」等）、「Linux 以外では `list_adapters()` は常に空 `Vec` を返す」、および「docs.rs 向けに `DOCS_RS=1 cargo doc --no-deps` で libvpl なしでもドキュメントだけは生成できる」（非 Linux では compile_error! で拒否されるため「Linux 上で」に限定）の記述を更新する。`README.md` の「libvpl がない環境では、docs.rs 向けのドキュメント生成のみ可能です。」（`DOCS_RS=1 cargo doc --no-deps` の手順）にも同じ「Linux 上で」の限定を適用する。
6. CI の `docs-rs` ジョブ（`DOCS_RS=1`）が Linux 上で通ることを確認する。
7. `CHANGES.md` の `## develop` に `[FIX]` として追記する（非 Linux で `compile_error!` によりビルドを拒否する / 非 Linux 版 `list_adapters` を削除する。非 Linux では `cargo build` が新たに失敗するようになるが、Linux 利用者には影響がなく誤誘導の修正であるため `[FIX]` で扱う）。

## 影響範囲

- `src/lib.rs`（`compile_error!` 追加、`pub use` の `list_adapters` を cfg 分岐）
- `src/adapter.rs`（非 Linux 版 `list_adapters` 削除）
- `build.rs`（libvpl ビルド分岐を `cfg!(all(target_os = "linux", target_arch = "x86_64"))` に変更）
- `skills/shiguredo-vpl/SKILL.md`（動作 OS の制約の記述更新）
- `README.md`（docs.rs 向けビルド記述の「Linux 上で」限定）
- `CHANGES.md`

## 参考

- `build.rs` の `DOCS_RS` 対応と Linux 分岐
- 関連 issue 0018（コーデック識別型の統合。本 issue が先に適用されれば非 Linux は到達不能になり、0018 の非 Linux 向け `to_codec_id()` の検討は不要になる）
- 関連 issue 0019（VPL ローダー初期化シーケンスの `LoaderBuilder` 化。同じ `src/adapter.rs` を書き換えるため、適用順序に注意）
- 関連 issue 0023（silent pass テストの修正。0023 は GPU なし環境でのビルド可能性を前提とした記述を含むため、本 issue 適用後は非 Linux ビルド自体が拒否される前提に変わる）
