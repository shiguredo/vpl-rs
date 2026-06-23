# Container

macOS 上で Apple Container (`container` CLI) を使い、CI と同等の clippy 検証をローカルで実行する方法です。

Apple Silicon Mac のネイティブ arm64 コンテナ内で、x86_64-unknown-linux-gnu ターゲットに対してクロスコンパイルを行う構成です。Rosetta 2 によるエミュレーションは使いません。

## 動作確認環境

- macOS 26.5.1 (Apple Silicon)
- container CLI 1.0.0 (Homebrew)

## 準備

Homebrew で container をインストールし、サービスを起動します。

```bash
brew install container
brew services start container
```

## 検証用イメージのビルド

```bash
container build -t vpl-rs-check -f Dockerfile.check .
```

## clippy の実行

```bash
container run --rm vpl-rs-check
```

デフォルトで `cargo clippy --workspace --target x86_64-unknown-linux-gnu -- -D warnings` が実行されます。

## その他のコマンドを実行する

```bash
container run --rm vpl-rs-check cargo check --workspace --target x86_64-unknown-linux-gnu
container run --rm vpl-rs-check cargo build --workspace --target x86_64-unknown-linux-gnu
container run --rm vpl-rs-check cargo test --workspace --target x86_64-unknown-linux-gnu
```

## 注意事項

- Intel VPL は x86_64 Linux 専用ですが、libvpl は build.rs が CMake で static build する設計のため、ホスト arm64 から x86_64 ターゲットへのクロスビルドだけで clippy 検証が完結します
- `cargo test` は実機 Intel GPU を必要とするテスト (`INTEL_VPL=1` でガード) を除いてビルドは可能ですが、x86_64 バイナリを実行することはコンテナ内ではできません
- Apple Container は将来的に Rosetta 2 のサポートが縮小される見込みのため、本構成はあえて Rosetta に依存しない形にしています
- `brew services start container` を実行しないと `container run` が失敗することがあります
