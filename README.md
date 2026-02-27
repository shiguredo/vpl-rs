# vpl-rs

[![crates.io](https://img.shields.io/crates/v/shiguredo_vpl.svg)](https://crates.io/crates/shiguredo_vpl)
[![docs.rs](https://docs.rs/shiguredo_vpl/badge.svg)](https://docs.rs/shiguredo_vpl)
[![License](https://img.shields.io/badge/License-Apache%202.0-blue.svg)](https://opensource.org/licenses/Apache-2.0)
[![GitHub Actions](https://github.com/shiguredo/vpl-rs/actions/workflows/ci.yml/badge.svg)](https://github.com/shiguredo/vpl-rs/actions/workflows/ci.yml)
[![Discord](https://img.shields.io/badge/Discord-%235865F2.svg?logo=discord&logoColor=white)](https://discord.gg/shiguredo)

## About Shiguredo's open source software

We will not respond to PRs or issues that have not been discussed on Discord. Also, Discord is only available in Japanese.

Please read <https://github.com/shiguredo/oss> before use.

## 時雨堂のオープンソースソフトウェアについて

利用前に <https://github.com/shiguredo/oss> をお読みください。

## 概要

[Intel VPL (Video Processing Library)](https://github.com/intel/libvpl) を利用したハードウェアビデオエンコーダーおよびデコーダーの Rust バインディングです。

libvpl を static link するため、実行時に Intel VPL 共有ライブラリは不要です。

## 特徴

- Intel VPL によるハードウェアエンコード (H.264 / H.265 / VP9 / AV1)
- Intel VPL によるハードウェアデコード (H.264 / H.265 / VP9 / AV1)
- libvpl を static link してビルド (CMake で自動ビルド)
- ビルド時に GitHub から libvpl ヘッダーを自動取得
- エンコード入力フォーマット選択 (NV12 / I420 / YV12 / BGRA / P010)
- デコード出力は NV12 フォーマット
- フレーム単位のエンコードオプション (IDR フレーム強制)
- CBR / VBR / CQP / ICQ / QVBR / LA / AVBR / VCM / LA_ICQ / LA_HRD レート制御モード
- エンコーダーパラメータの動的変更 (MFXVideoENCODE_Reset)
- パラメータのサポート可否検証 (MFXVideoENCODE_Query)

## 動作要件

- Linux (x86_64)
- Intel GPU (第 6 世代 Core 以降)
- ビルド時: git、clang (bindgen 依存)

## ビルド

```bash
cargo build
```

ビルド時に GitHub から libvpl を自動取得し、CMake で static build します。

### docs.rs 向けビルド

libvpl がない環境では、docs.rs 向けのドキュメント生成のみ可能です。

```bash
DOCS_RS=1 cargo doc --no-deps
```

## 使い方

### エンコード

```rust
use shiguredo_vpl::{
    CodecConfig, EncodeOptions, Encoder, EncoderConfig, FrameFormat,
    H264EncoderConfig, H264Profile, RateControlMode, frame_type,
};

let mut config = EncoderConfig::new(
    CodecConfig::H264(H264EncoderConfig {
        profile: Some(H264Profile::High),
    }),
    1920,                    // width
    1080,                    // height
    FrameFormat::Nv12,
    30,                      // framerate_num
    1,                       // framerate_den
    RateControlMode::Cbr,
);
config.target_kbps = Some(5_000);

let mut encoder = Encoder::new(config)?;

// フレームデータをエンコード
let options = EncodeOptions { frame_type: frame_type::UNKNOWN };
encoder.encode(&frame_data, &options)?;

// IDR フレームを強制してエンコード
encoder.encode(&frame_data, &EncodeOptions {
    frame_type: frame_type::IDR | frame_type::I | frame_type::REF,
})?;

// エンコード済みフレームを取得
while let Some(encoded) = encoder.next_frame() {
    println!("encoded bytes: {}", encoded.data().len());
    println!("timestamp: {}", encoded.timestamp());
    println!("picture type: {:?}", encoded.picture_type());
}

// 残りのフレームをすべて取得する
encoder.finish()?;
while let Some(encoded) = encoder.next_frame() {
    println!("flushed: {} bytes", encoded.data().len());
}
```

### デコード

```rust
use shiguredo_vpl::{Decoder, DecoderCodec, DecoderConfig};

let config = DecoderConfig {
    codec: DecoderCodec::H264,
};
let mut decoder = Decoder::new(config)?;

// ビットストリームデータをデコード
decoder.decode(&bitstream_data)?;

// デコード済みフレームを取得 (NV12 フォーマット)
while let Some(frame) = decoder.next_frame() {
    println!("decoded: {}x{}, {} bytes", frame.width(), frame.height(), frame.data().len());
}

// 残りのフレームをすべて取得する
decoder.finish()?;
while let Some(frame) = decoder.next_frame() {
    println!("flushed: {}x{}", frame.width(), frame.height());
}
```

## サポートコーデック

### エンコード

| コーデック | `CodecConfig` |
|-----------|--------------|
| H.264     | `CodecConfig::H264(H264EncoderConfig)` |
| H.265     | `CodecConfig::Hevc(HevcEncoderConfig)` |
| VP9       | `CodecConfig::Vp9(Vp9EncoderConfig)` |
| AV1       | `CodecConfig::Av1(Av1EncoderConfig)` |

### デコード

| コーデック | `DecoderCodec` |
|-----------|---------------|
| H.264     | `DecoderCodec::H264` |
| H.265     | `DecoderCodec::Hevc` |
| VP9       | `DecoderCodec::Vp9` |
| AV1       | `DecoderCodec::Av1` |

## サポートフォーマット

### エンコード入力フォーマット (`FrameFormat`)

| フォーマット | `FrameFormat` | 説明 |
|---|---|---|
| NV12 | `FrameFormat::Nv12` | Semi-Planar YUV 4:2:0 8bit |
| YUY2 | `FrameFormat::Yuy2` | Packed YUV 4:2:2 8bit |
| BGRA | `FrameFormat::Bgra` | Packed BGRA 8bit |

### デコード出力フォーマット

| フォーマット | 説明 |
|---|---|
| NV12 | Semi-Planar YUV 4:2:0 8bit |

## サポートプロファイル

### H.264 (`H264Profile`)

| プロファイル | `H264Profile` |
|------------|--------------|
| Baseline   | `H264Profile::Baseline` |
| Constrained Baseline | `H264Profile::ConstrainedBaseline` |
| Main       | `H264Profile::Main` |
| High       | `H264Profile::High` |
| Constrained High | `H264Profile::ConstrainedHigh` |
| High 10    | `H264Profile::High10` |
| High 4:2:2 | `H264Profile::High422` |

### H.265 (`HevcProfile`)

| プロファイル | `HevcProfile` |
|------------|--------------|
| Main       | `HevcProfile::Main` |
| Main 10    | `HevcProfile::Main10` |
| Main SP    | `HevcProfile::MainSp` |
| RExt       | `HevcProfile::Rext` |
| SCC        | `HevcProfile::Scc` |

### VP9 (`Vp9Profile`)

| プロファイル | `Vp9Profile` | 説明 |
|------------|-------------|------|
| Profile 0  | `Vp9Profile::Profile0` | 8bit YUV 4:2:0 |
| Profile 1  | `Vp9Profile::Profile1` | 8bit YUV 4:2:2 または 4:4:4 |
| Profile 2  | `Vp9Profile::Profile2` | 10/12bit YUV 4:2:0 |
| Profile 3  | `Vp9Profile::Profile3` | 10/12bit YUV 4:2:2 または 4:4:4 |

### AV1 (`Av1Profile`)

| プロファイル | `Av1Profile` |
|------------|-------------|
| Main       | `Av1Profile::Main` |

## ライセンス

Apache License 2.0

```text
Copyright 2026-2026, Shiguredo Inc.

Licensed under the Apache License, Version 2.0 (the "License");
you may not use this file except in compliance with the License.
You may obtain a copy of the License at

    http://www.apache.org/licenses/LICENSE-2.0

Unless required by applicable law or agreed to in writing, software
distributed under the License is distributed on an "AS IS" BASIS,
WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
See the License for the specific language governing permissions and
limitations under the License.
```

## Intel VPL

<https://github.com/intel/libvpl>

<https://www.intel.com/content/www/us/en/developer/tools/oneapi/onevpl.html>
