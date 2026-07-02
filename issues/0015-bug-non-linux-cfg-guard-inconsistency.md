# 非 Linux 環境で Encoder / Decoder の cfg ガードが欠落しリンカエラーになる

- Priority: High
- Created: 2026-07-01
- Model: Opus 4.7
- Branch: feature/fix-non-linux-cfg-guard-inconsistency
- Polished: 2026-07-02

## 目的

このクレートは Linux (x86_64) 前提だが、`Encoder` / `Decoder` / `VplLibrary` の関数群に `#[cfg(target_os = "linux")]` ガードが付いていないため、非 Linux で `cargo build` すると **生のリンカ未解決シンボルエラー** で失敗する。加えて `list_adapters()` は非 Linux で空 `Vec` を返す実装が用意されていて「非 Linux でも使える」ような錯覚を与える。cfg ガードの方針を一貫させ、非 Linux では明確なコンパイルエラーで拒否する。

## 優先度根拠

High。以下による。

- **開発者体験の悪さ**: macOS / Windows で `cargo build` を試みるとリンカ・エラーが返るが、原因が「Linux 前提のクレート」と伝わらない。undef sym をひとつずつ調べることになる。
- **API の一貫性の欠如**: `list_adapters` / `supported_codecs` は cfg 分岐しているのに、`Encoder::new` / `Decoder::new` は分岐なし。同じ crate で扱いが揃っていない。
- **誤誘導**: `list_adapters()` が非 Linux で `Ok(Vec::new())` を返す実装は「非 Linux でも API を触れる」と積極的に誤認させるバグであり、早期修正が必要。

## 現状

### cfg 分岐済み

- `src/adapter.rs:104-215` の `list_adapters`: Linux 実装
- `src/adapter.rs:217-221` の `list_adapters`: 非 Linux で `Ok(Vec::new())` を返す
- `src/codec_info.rs:146` の `supported_codecs`: `#[cfg(target_os = "linux")]` あり、非 Linux 版なし

### cfg 分岐なし

- `src/encode.rs` の `Encoder` 全体、`EncoderConfig` 全体、`Encoder::new` などの関数
- `src/decode.rs` の `Decoder` 全体、`DecoderConfig` 全体、`Decoder::new` などの関数
- `src/vpl.rs` の `VplLibrary::create_session` などの関数
- `src/lib.rs:86-91` のモジュール宣言 / `pub use` は全 OS で公開

### build.rs の分岐

`build.rs:63-65`:

```rust
if cfg!(target_os = "linux") {
    build_and_link_libvpl(&clone_dir);
}
```

Linux 以外では libvpl 実体が build されずリンクもされないため、`sys::MFXVideoENCODE_Init` などの関数シンボルが未解決になる。

### ユーザー体験

非 Linux で `cargo build`:

1. `build.rs` は bindings.rs を生成するが libvpl のリンクをスキップ
2. `Encoder::new` などの関数が bindings.rs の extern シンボルを参照
3. リンカが「MFXVideoENCODE_Init が見つからない」と生のエラーを出す

lib.rs L21 に「動作要件: Linux (x86_64)」と明記しているが、コンパイル時にエラー・メッセージで伝わらない。

### `list_adapters` の空 `Vec` 実装が誤解を招く

非 Linux で `list_adapters()` が `Ok(Vec::new())` を返す実装（`src/adapter.rs:217-221`）は「非 Linux でも API を触れる」印象を与える。しかし `Encoder::new` / `Decoder::new` を呼ぼうとするとリンカエラーで爆発するため、実際には触れない。この対称性の欠如が「非 Linux で使えるように見える」錯覚を強めている。

## 設計方針

### 案 A: crate 全体を `#[cfg(target_os = "linux")]` でガードする（推奨）

`src/lib.rs` のドキュメントコメント直後（`mod` 宣言の前）に `compile_error!` を配置する:

```rust
#[cfg(not(target_os = "linux"))]
compile_error!("shiguredo_vpl requires Linux x86_64. Non-Linux targets are not supported.");
```

- 長所: 非 Linux ではドキュメントコメントのパースのみで即座に拒否され、モジュールの型検査まで走らない（コンパイル時間が無駄にならない）。
- 短所: `docs.rs` ビルドを Linux で走らせる必要がある。`docs.rs` のデフォルトビルド環境は Ubuntu であり、本 crate の `build.rs` の `DOCS_RS` 分岐とも整合するため問題なし。
- macOS での `DOCS_RS=1 cargo doc --no-deps` は `compile_error!` で拒否されるが、これは意図通り（macOS 向けのドキュメント生成は不要）。

### 案 B: 非 Linux でも API を空実装で提供する

`Encoder` / `Decoder` の全 API を非 Linux で `Err(Error::new_custom("...", "not supported on this platform"))` を返す実装にする。

- 長所: 非 Linux でも cargo test は通り、ドキュメント生成もどの OS でもできる。
- 短所: 実装コスト大。使えない API を用意する意味が薄い。

推奨は **案 A**。crate の目的が「Intel VPL の Rust バインディング」で、Intel VPL は Linux 専用（正確には Windows でも動くが本 crate では対応していない）である以上、明確に拒否するのが正しい。

### `list_adapters` の非 Linux 実装をどうするか

- **案 A 採用**: `list_adapters` の非 Linux 実装も削除する（crate 全体が拒否されるので不要）。
- `codec_info.rs` の非 gated 型定義（`VideoCodecType`, `CodecInfo` 等）は `sys` 型に依存しない純粋データ型であり、`compile_error!` 導入後はモジュールごと到達不能になるため影響なし。cfg 追記は不要。

`docs.rs` 対応は `build.rs:57-60` の `if std::env::var("DOCS_RS").is_ok()` で処理しており、bindings.rs は生成されるので、案 A 採用時も docs.rs ビルドは通る（Linux 上で）。

## 完了条件

以下すべてを満たす。

1. `src/lib.rs` の先頭（ドキュメントコメント直後、`mod` 宣言の前）に `#[cfg(not(target_os = "linux"))] compile_error!(...)` を追加する。
2. `src/adapter.rs` の非 Linux 版 `list_adapters`（空 `Vec` を返す実装）を削除する。
3. `CHANGES.md` の `## develop` に `[FIX]` として追記する（非 Linux で誤誘導するバグの修正）。
4. `docs.rs` ビルド（`DOCS_RS=1`）が Linux 上で通ることを確認する（現在の CI の `docs-rs` ジョブまたは手動確認）。

## 影響範囲

- `src/lib.rs`（`compile_error!` 追加）
- `src/adapter.rs`（非 Linux 版 `list_adapters` 削除）
- `CHANGES.md`

## 参考

- `build.rs:57-60` の `DOCS_RS` 対応
