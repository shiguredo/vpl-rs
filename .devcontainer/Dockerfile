# Apple Silicon (arm64) ホストから Rosetta 2 を使わずに
# x86_64-unknown-linux-gnu ターゲットの clippy / build を回せるようにする devcontainer
#
# ベースは multi-arch の ubuntu:24.04 を使い、arm64 ホストでは arm64 ネイティブで起動する
# libvpl は build.rs が GitHub から git clone し shiguredo_cmake で CMake static build する
# 設計のため、CMake のクロスコンパイル設定 (CC/CXX/LINKER) と bindgen の clang target /
# sysroot 指定だけで完結する (重い sysroot 構築は不要)
FROM ubuntu:24.04

# ビルドに必要なパッケージ
# - crossbuild-essential-amd64: x86_64 用のクロスコンパイラと sysroot 一式
#   (arm64 ホストから x86_64-unknown-linux-gnu ターゲットをビルドするため)
# - cmake: libvpl の CMake static build に必要
# - libclang-dev: bindgen が利用する libclang
# - git: build.rs が libvpl リポジトリを git clone するために使う
RUN apt-get update && apt-get install -y --no-install-recommends \
    curl \
    ca-certificates \
    build-essential \
    crossbuild-essential-amd64 \
    cmake \
    pkg-config \
    libclang-dev \
    git \
    && rm -rf /var/lib/apt/lists/*

# Rust ツールチェインのインストール
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable
ENV PATH="/root/.cargo/bin:${PATH}"

# clippy / rustfmt コンポーネント (rust-toolchain.toml と同期)
RUN rustup component add clippy rustfmt

# x86_64 ターゲットのツールチェインを追加 (arm64 ホストからクロスビルドするため)
RUN rustup target add x86_64-unknown-linux-gnu

# cargo のデフォルトターゲットを x86_64-unknown-linux-gnu に固定し
# クロスビルド用の linker / コンパイラを指定する (devcontainer 内でのみ有効)
# cmake クレートは CC_<target> / CXX_<target> を読んで libvpl の CMake build に渡す
ENV CARGO_BUILD_TARGET=x86_64-unknown-linux-gnu
ENV CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER=x86_64-linux-gnu-gcc
ENV CC_x86_64_unknown_linux_gnu=x86_64-linux-gnu-gcc
ENV CXX_x86_64_unknown_linux_gnu=x86_64-linux-gnu-g++

# bindgen に x86_64 用の sysroot とターゲットを渡す
# crossbuild-essential-amd64 が /usr/x86_64-linux-gnu に sysroot 一式を配置する
ENV BINDGEN_EXTRA_CLANG_ARGS_X86_64_UNKNOWN_LINUX_GNU="--sysroot=/usr/x86_64-linux-gnu --target=x86_64-linux-gnu"
