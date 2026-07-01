# 非 Linux 環境で Encoder / Decoder の cfg ガードが欠落しリンカエラーになる

- Priority: Medium
- Created: 2026-07-01
- Completed: {YYYY-MM-DD}
- Model: Opus 4.7
- Branch: feature/fix-non-linux-cfg-guard-inconsistency
- Polished: {YYYY-MM-DD}

## 目的

このクレートは Linux (x86_64) 前提だが、`Encoder` / `Decoder` / `VplLibrary` の関数群に `#[cfg(target_os = "linux")]` ガードが付いていないため、非 Linux で `cargo build` すると **生のリンカ未解決シンボルエラー** で失敗する。加えて `list_adapters()` は非 Linux で空 `Vec` を返す実装が用意されていて「非 Linux でも使える」ような錯覚を与える。cfg ガードの方針を一貫させ、非 Linux では明確なコンパイルエラーで拒否する。

## 優先度根拠

Medium。以下による。

- **開発者体験の悪さ**: macOS / Windows で `cargo build` を試みるとリンカ・エラーが返るが、原因が「Linux 前提のクレート」と伝わらない。undef sym をひとつずつ調べることになる。
- **API の一貫性の欠如**: `list_adapters` / `supported_codecs` は cfg 分岐しているのに、`Encoder::new` / `Decoder::new` は分岐なし。同じ crate で扱いが揃っていない。
- **本番運用への直接影響はない**: 実運用は Linux 前提で回っているため、Priority は High ではなく Medium とする。開発体験と API の整合性の観点から早期対応が望ましい。

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

`src/lib.rs` に:

```rust
#[cfg(not(target_os = "linux"))]
compile_error!("shiguredo_vpl requires Linux x86_64. Non-Linux targets are not supported.");
```

さらに全 public API を `#[cfg(target_os = "linux")]` でガードするか、モジュール宣言に付ける。

- 長所: 非 Linux では明確なコンパイルエラーで拒否できる。生のリンカエラーに悩まされない。
- 短所: `docs.rs` ビルドを Linux で走らせる必要がある（現在 CI で `docs-rs` ジョブを Ubuntu で回しているため問題なし）。

### 案 B: 非 Linux でも API を空実装で提供する

`Encoder` / `Decoder` の全 API を非 Linux で `Err(Error::new_custom("...", "not supported on this platform"))` を返す実装にする。

- 長所: 非 Linux でも cargo test は通り、ドキュメント生成もどの OS でもできる。
- 短所: 実装コスト大。使えない API を用意する意味が薄い。

推奨は **案 A**。crate の目的が「Intel VPL の Rust バインディング」で、Intel VPL は Linux 専用（正確には Windows でも動くが本 crate では対応していない）である以上、明確に拒否するのが正しい。

### `list_adapters` の非 Linux 実装をどうするか

- **案 A 採用**: `list_adapters` の非 Linux 実装も削除する（crate 全体が拒否されるので不要）。
- 現状の空 `Vec` を返す実装は、`docs.rs` のドキュメント生成に必要な最小限の実装として残してもよい（`DOCS_RS=1` を利用する場合）。ただし `docs.rs` は Linux で回すため実質不要。

`docs.rs` 対応は `build.rs:57-60` の `if std::env::var("DOCS_RS").is_ok()` で処理しており、bindings.rs は生成されるので、案 A 採用時も docs.rs ビルドは通る（Linux 上で）。

## 完了条件

以下すべてを満たす。

1. 非 Linux で `cargo build` すると `compile_error!("shiguredo_vpl requires Linux x86_64. Non-Linux targets are not supported.")` で拒否される。
2. `list_adapters` の非 Linux 空 `Vec` 実装を削除する（案 A 採用時）。
3. `README.md` / `CHANGES.md` に Linux 専用であることを明記する（README 既存の「動作要件」を強調 / CHANGES で `[CHANGE]` として非 Linux ビルドの拒否を通知）。
4. `docs.rs` ビルド（`DOCS_RS=1 cargo doc --no-deps`）は Linux 上で通ることを CI で確認する。
5. `.devcontainer/Dockerfile` は既に Linux コンテナなので影響なし。
6. `CHANGES.md` の `## develop` に `[CHANGE]` として追記する（非 Linux での API 提供停止は破壊的変更）。

## 影響範囲

- `src/lib.rs`（`compile_error!` 追加、モジュール宣言）
- `src/adapter.rs`（非 Linux 版 `list_adapters` 削除）
- `README.md`（Linux 専用の強調）
- `CHANGES.md`

## 参考

- `/review-code` の致命的指摘 F8
- 参考にする pattern: `Encoder` / `Decoder` を非 Linux でエクスポートしていない類似 crate（`shiguredo/nvcodec-rs`, `shiguredo/amf-rs`）
