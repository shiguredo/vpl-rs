# 実 docs.rs サービスのビルドがネットワーク遮断で失敗する

- Priority: Medium
- Created: 2026-08-02
- Completed: 2026-11-04
- Model: DeepSeek V4 Flash
- Branch: feature/fix-docsrs-build-failure
- Polished: {YYYY-MM-DD}

## 目的

実 docs.rs サービス（docs.rs のビルド環境）で本クレートのドキュメント生成が失敗する問題を解消する。issue 0015 で「本 issue のスコープ外とする（別 issue で対応予定）」と明記されて切り出されたものである。

## 現状

`build.rs` は以下の順で処理を実行する。

1. `clone_vpl_headers()` で GitHub から libvpl ヘッダを `git clone` する
2. bindgen でバインディングを生成する
3. `DOCS_RS` 環境変数が設定されていれば libvpl のビルド・リンクをスキップする

実 docs.rs サービスはネットワークが遮断されており、ステップ 1 の `git clone` が失敗するため、`DOCS_RS` チェック（ステップ 3）に到達する前にビルドが失敗する。

一方、CI の `docs-rs` ジョブ（ubuntu-24.04、ネットワークあり）は `DOCS_RS=1 cargo doc --no-deps` を実行しており、Linux 上で通ることは確認できている（issue 0015 で検証済み）。問題は実 docs.rs サービスのみである。

## 設計方針

- `build.rs` の `DOCS_RS` チェックを `git clone` より前に移動し、`DOCS_RS` 設定時はヘッダのクローンと bindgen によるバインディング生成をスキップする。
- ただし、`cargo doc` には `src/bindings.rs` 等のバインディングが必要なため、`DOCS_RS` 時は生成済みバインディングを利用するか、バインディングに依存しないドキュメント生成経路を用意する必要がある。
- 具体的な対応方針（バインディングの扱い）は、docs.rs のビルド制約（ネットワーク遮断・外部コマンド制限）を考慮して決定する。

## 完了条件

以下すべてを満たす。

1. 実 docs.rs サービスで本クレートのビルドとドキュメント生成が成功する。
2. 既存の CI `docs-rs` ジョブ（`DOCS_RS=1 cargo doc --no-deps`）が引き続き pass する。
3. 非 `DOCS_RS` 環境（通常ビルド）では libvpl のクローン・ビルド・リンクが従来どおり行われる。
4. `CHANGES.md` の `## develop` の `### misc` サブセクションに `[FIX]` として追記する（docs.rs 対応はライブラリ機能に直接影響しないため）。

## 実装案

- `build.rs` の処理順序を変更し、`DOCS_RS` 時はネットワークアクセス（`git clone`）を伴わない経路にする。
- バインディング生成をスキップする場合は、バインディングに依存するコードが `cargo doc` で通るようにする（例: 生成済みバインディングのコミット、または doc ビルド専用のスタブ）。

## 参考

- 関連 issue: 0015（非 Linux の cfg ガード不整合。docs.rs 対応の現状と CI での検証方法を調査した）

## 解決方法

`DOCS_RS` を設定したビルドでは libvpl の取得と bindgen をスキップし、ドキュメント生成に必要な最小限の定義を持つダミーのバインディングを出力するようにした。同じ時雨堂の video-toolbox-rs と同じ方式である。

docs.rs のビルド環境はネットワークに接続できないため、`git clone` で libvpl のヘッダを取得できない。一方、`cargo doc` は関数本体を型チェックしないため、ドキュメント生成にはアイテムのシグネチャに現れる型と、const 初期化子で参照する定数だけがあれば足りる。ダミーは 85 行で済む。

### 変更内容

#### `build.rs`

- `DOCS_RS` の判定を `git clone` より前に移した
- `DOCS_RS` を設定したビルドでは、libvpl の取得、bindgen によるバインディング生成、CMake による static build、リンクを行わず、`DOCS_RS_DUMMY_BINDINGS` を `OUT_DIR/bindings.rs` に書き出して終了するようにした
- ダミーには、シグネチャに現れる型（`mfxVideoParam` / `mfxBitstream` / `mfxFrameSurface1` など）、型エイリアス、`STATUS_TABLE` などで参照する定数だけを定義した
- `DOCS_RS` を再ビルドのトリガーに追加した

#### `CHANGES.md`

- `## develop` の `### misc` に `[FIX]` として追記した

### 確認

Intel GPU を搭載した実機で確認した。

| 確認内容 | 結果 |
| --- | --- |
| `cargo fmt --all --check` | pass |
| `cargo build --workspace` | pass（通常ビルドで libvpl の取得・bindgen・CMake static build が行われることを含む） |
| `cargo clippy --workspace --all-targets -- -D warnings` | pass |
| `INTEL_VPL=1 cargo test --workspace -- --test-threads=1` | pass（単体テスト 18 件、`tests/test_adapter.rs` 6 件、`tests/test_roundtrip.rs` 21 件、doc テスト 2 件） |
| `DOCS_RS=1 cargo doc --no-deps` | pass |

docs.rs のビルド環境を模擬するため、`cargo package` で作成した `.crate` を読み取り専用のディレクトリに展開し、ネットワークに接続しない状態でドキュメントを生成した。

- `PATH` の先頭に `git` を必ず失敗させるスクリプトを置いて `DOCS_RS=1 cargo doc --no-deps` を実行し、`git` が呼ばれないことを確認した
- `strace -f -e trace=network` で `AF_INET` / `AF_INET6` のネットワーク syscall が発生しないことを確認した
- 安定版 Rust 1.93.1 と nightly 1.101.0-nightly の両方でドキュメント生成に成功した

実 docs.rs サービスでのビルド確認は、次に crates.io へ公開したタイミングで行う。
