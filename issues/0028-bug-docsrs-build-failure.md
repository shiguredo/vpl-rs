# 実 docs.rs サービスのビルドがネットワーク遮断で失敗する

- Priority: Medium
- Created: 2026-08-02
- Completed: {YYYY-MM-DD}
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

## 解決方法

- `build.rs` の処理順序を変更し、`DOCS_RS` 時はネットワークアクセス（`git clone`）を伴わない経路にする。
- バインディング生成をスキップする場合は、バインディングに依存するコードが `cargo doc` で通るようにする（例: 生成済みバインディングのコミット、または doc ビルド専用のスタブ）。

## 参考

- 関連 issue: 0015（非 Linux の cfg ガード不整合。docs.rs 対応の現状と CI での検証方法を調査した）
