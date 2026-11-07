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
- エンコード入力フォーマット選択 (NV12 / P010 / YUY2 / BGRA / AYUV / Y410)
- デコード出力はストリームのネイティブ形式 (8bit は NV12、10bit は P010)
- フレーム単位のエンコードオプション (IDR フレーム強制)
- CBR / VBR / CQP / ICQ / QVBR / LA / AVBR / VCM / LA_ICQ / LA_HRD レート制御モード
- エンコーダーパラメータの動的変更 (MFXVideoENCODE_Reset)
- パラメータのサポート可否検証 (MFXVideoENCODE_Query)
- エンコーダーからの SPS / PPS / VPS 取得 (MP4 / RTP 用のパラメータセット)
- コーデックケーパビリティ照会 (プロファイル別・メモリタイプ別の解像度範囲 / 最大コーデックレベル / 対応色形式)

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

Linux (x86_64) 上では、libvpl がない環境でも docs.rs 向けのドキュメント生成のみ行えます。

```bash
DOCS_RS=1 cargo doc --no-deps
```

## 使い方

### GPU アダプタの指定

アダプタは Intel VPL が利用する Intel GPU デバイスのことで、Linux 上では `/dev/dri/renderD<N>` の DRM render node 番号で識別します。

エンコーダーおよびデコーダーの初期化には、使用する GPU アダプタを `AdapterSelector` で指定する必要があります。
システムに搭載されている Intel GPU は `list_adapters()` で列挙できます。

```rust
use shiguredo_vpl::{AdapterSelector, list_adapters};

let adapters = list_adapters()?;
for adapter in &adapters {
    println!(
        "DRM render node {}: {} ({})",
        adapter.drm_render_node, adapter.device_name, adapter.impl_name,
    );
}

// 使用する GPU アダプタを DRM render node 番号で指定する
let adapter = AdapterSelector::DrmRenderNode(adapters[0].drm_render_node);
```

### コーデック情報の照会

`supported_codecs()` は、H.264 / HEVC / VP9 / AV1 それぞれの対応状況とケーパビリティを返します。
エンコード・デコードに対応していないコーデックは、`CodecInfo::encoding` / `CodecInfo::decoding` が `None` になります。

ケーパビリティは VPL の実装記述 (`mfxDecoderDescription` / `mfxEncoderDescription`) の階層をそのまま反映します。プロファイルやメモリタイプをまたいだ集約は行いません。

```rust
use shiguredo_vpl::{AdapterSelector, list_adapters, supported_codecs};

let adapter = AdapterSelector::DrmRenderNode(list_adapters()?[0].drm_render_node);
for codec in supported_codecs(adapter)? {
    if let Some(encoding) = &codec.encoding {
        for profile in &encoding.profiles {
            for memory_type in &profile.memory_types {
                println!(
                    "{:?} {:?} {:?}: width={}..{} step={}, height={}..{} step={}, {:?}",
                    codec.codec,
                    profile.profile,
                    memory_type.memory_type,
                    memory_type.width.min,
                    memory_type.width.max,
                    memory_type.width.step,
                    memory_type.height.min,
                    memory_type.height.max,
                    memory_type.height.step,
                    memory_type.color_formats,
                );
            }
        }
    }
}
```

| 型 | 説明 |
|---|---|
| `CodecInfo` | コーデック種別と、エンコード・デコードの情報 |
| `DecodingInfo` / `EncodingInfo` | 最大コーデックレベルとプロファイル一覧。エンコードは B フレーム対応も持つ |
| `ProfileCapabilities` | プロファイルと、メモリタイプごとのケーパビリティ |
| `MemoryTypeCapabilities` | メモリタイプ、幅・高さの範囲 (`ValueRange`)、対応色形式 |
| `ValueRange` | 最小値・最大値・増分 (VPL の `mfxRange32U`) |
| `VideoProfile` | コーデックごとのプロファイル。既知の型で表現できない値は `Unknown(u32)` |
| `MemoryType` | VPL の `mfxResourceType`。既知の型で表現できない値は `Unknown(u32)` |

- 解像度範囲と色形式は、プロファイルとメモリタイプごとの値をそのまま返す
- 色形式は `FrameFormat` で表現できる FourCC だけを、VPL の `ColorFormats` に並んでいる順序で返す
- `max_codec_level` は最大コーデックレベルを返し、レベルが不明な場合は `None`
- すべてのメモリタイプを返すが、このクレートの `Encoder` / `Decoder` が実際に使うのは `MemoryType::System` だけ
- VPL がプロファイルを返さない場合は `profiles` が空、メモリタイプを返さない場合は `memory_types` が空、色形式を返さない場合や表現できる FourCC が無い場合は `color_formats` が空になる

### エンコード

```rust
use shiguredo_vpl::{
    AdapterSelector, CodecConfig, EncodeOptions, EncodedFrame, Encoder, EncoderConfig,
    Error, FnEncodeHandler, FrameData, FrameFormat, H264EncoderConfig, H264Profile,
    RateControlMode, frame_type, list_adapters,
};
use std::sync::mpsc;

let width = 1920usize;
let height = 1080usize;
let adapter = AdapterSelector::DrmRenderNode(list_adapters()?[0].drm_render_node);
let mut config = EncoderConfig::new(
    adapter,
    CodecConfig::H264(H264EncoderConfig {
        profile: Some(H264Profile::High),
    }),
    width as u32,
    height as u32,
    FrameFormat::Nv12,
    30,                      // framerate_num
    1,                       // framerate_den
    RateControlMode::Cbr,
);
config.target_kbps = Some(5_000);

let (tx, rx) = mpsc::channel();
let mut encoder = Encoder::new(config, FnEncodeHandler::new(move |result| {
    tx.send(result).expect("failed to send callback result");
}))?;

// width x height の FrameData を構築する
let pitch = FrameFormat::Nv12
    .bytes_per_row(width)
    .ok_or("row bytes overflowed")?;
let y_size = pitch * height;
let uv_size = pitch * (height / 2);
let frame_data = vec![0u8; y_size + uv_size];
let frame = FrameData::SemiPlanar {
    y: &frame_data[..y_size],
    y_pitch: pitch,
    uv: &frame_data[y_size..],
    uv_pitch: pitch,
};
// 構築したフレームデータをエンコードする
let options = EncodeOptions { frame_type: frame_type::UNKNOWN };
encoder.encode(frame, "normal", &options)?;

// IDR フレームを強制してエンコード
encoder.encode(frame, "force-idr", &EncodeOptions {
    frame_type: frame_type::IDR | frame_type::I | frame_type::REF,
})?;

// 残りのフレームをすべて取得する
encoder.finish()?;
for _ in 0..2 {
    let encoded = rx.recv().expect("failed to receive callback result")?;
    println!("encoded bytes: {}", encoded.data().len());
    println!("timestamp: {}", encoded.timestamp());
    println!("picture type: {:?}", encoded.picture_type());
    println!("user_data: {:?}", encoded.user_data());
}
```

`Encoder::encode` に渡すフレームは `FrameData` で表現します。`EncoderConfig::width` / `height` は表示サイズ（crop サイズ）で、16 の倍数に揃える必要はありませんが、NV12 / P010 は幅と高さ、YUY2 は幅を偶数にする必要があります。各行のバイト数が表示幅と異なる場合は、`y_pitch` / `uv_pitch` / `pitch` に実際の行間のバイト数を指定します。`Encoder::coded_size()` は内部サーフェスのサイズであり、入力バッファのサイズではありません。

### シーケンスパラメータの取得

エンコーダーが現在使用している SPS / PPS / VPS は `Encoder::get_sequence_params()` で取得できます。
MP4 の SampleEntry (`avcC` / `hvcC` / `av1C`) や RTP の `fmtp` の組み立てに利用できます。

```rust
let params = encoder.get_sequence_params()?;
if let Some(params) = params {
    println!("SPS: {} bytes", params.sps.len());
    println!("PPS: {:?}", params.pps.as_ref().map(|pps| pps.len()));
    println!("VPS: {:?}", params.vps.as_ref().map(|vps| vps.len()));
}
```

- 返るバイト列はスタートコード (`00 00 00 01` / `00 00 01`) を含まない
- H.264 は `sps` に SPS、`pps` に PPS を返す
- HEVC は `vps` に VPS、`sps` に SPS、`pps` に PPS を返す
- AV1 は `sps` に Sequence Header OBU を返し、`vps` と `pps` は `None`
- VP9 はパラメータセットを持たないため `Ok(None)` を返す
- `Encoder::encode` と同じスレッドで、`Encoder::encode` と直列になるように呼ぶ
- `Encoder::reconfigure` でフレームレートを変えるとパラメータセットの内容が変わるため、必要に応じて `get_sequence_params()` で取得し直す
- MP4 の `avc1` / `hvc1` は SampleEntry を途中で差し替えられないため、SPS が変わる変更を行う場合は `avc3` / `hev1` を使う

### デコード

```rust
use shiguredo_vpl::{
    AdapterSelector, Decoder, DecoderConfig, DecodedFrame, Error, FnDecodeHandler,
    VideoCodecType, list_adapters,
};
use std::sync::mpsc;

let adapter = AdapterSelector::DrmRenderNode(list_adapters()?[0].drm_render_node);
let config = DecoderConfig::new(adapter, VideoCodecType::H264);

let (tx, rx) = mpsc::channel();
let mut decoder = Decoder::new(config, FnDecodeHandler::new(move |result| {
    tx.send(result).expect("failed to send callback result");
}))?;

// ビットストリームデータをデコード (value で任意のユーザーデータを紐付け可能)
let bitstream_data = vec![0u8; 1024];
decoder.decode(&bitstream_data, "frame-0")?;
decoder.decode(&bitstream_data, "frame-1")?;

// 残りのフレームをすべて取得する
decoder.finish()?;

// コールバック経由でデコード結果を受け取る
while let Ok(result) = rx.try_recv() {
    let frame = result?;
    println!("decoded: {}x{}, Y plane: {} bytes, UV plane: {} bytes, user_data: {:?}",
        frame.width(), frame.height(), frame.y().len(), frame.uv().len(), frame.user_data());
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

VP9 の場合、`Vp9EncoderConfig::write_ivf_headers` で出力形式を選択します。
`true` で IVF ヘッダー付き、`false` で raw VP9 を出力します。
Intel GPU の oneVPL は IVF ヘッダー出力が既定で ON のため、従来の IVF 付き出力を維持するには `true` を指定してください。

### デコード

| コーデック | `VideoCodecType` |
|-----------|-----------------|
| H.264     | `VideoCodecType::H264` |
| H.265     | `VideoCodecType::Hevc` |
| VP9       | `VideoCodecType::Vp9` |
| AV1       | `VideoCodecType::Av1` |

## サポートフォーマット

### エンコード入力フォーマット (`FrameFormat`)

| フォーマット | `FrameFormat` | 説明 |
|---|---|---|
| NV12 | `FrameFormat::Nv12` | Semi-Planar YUV 4:2:0 8bit |
| P010 | `FrameFormat::P010` | Semi-Planar YUV 4:2:0 10bit (16bit ワードの上位 10bit) |
| YUY2 | `FrameFormat::Yuy2` | Packed YUV 4:2:2 8bit |
| BGRA | `FrameFormat::Bgra` | Packed BGRA 8bit |
| AYUV | `FrameFormat::Ayuv` | Packed YUV 4:4:4 8bit (バイト順は V, U, Y, A) |
| Y410 | `FrameFormat::Y410` | Packed YUV 4:4:4 10bit (下位から U 10bit / Y 10bit / V 10bit / A 2bit) |

入力フォーマットとプロファイルの組み合わせの可否は GPU と VPL の実装に依存し、初期化およびエンコード時に判定されます。
初期化に失敗した場合、`Encoder::new` はそのエラーを返します。初期化に成功しても、エンコードに失敗する組み合わせがあります。
事前に対応を確認する場合は `supported_codecs()` が返す `EncodingInfo::profiles` の `MemoryTypeCapabilities::color_formats` を参照してください。ただし、ケーパビリティ照会の結果は初期化やエンコードの成功を保証しません。
詳細は `Encoder::new` のドキュメントを参照してください。

### デコード出力フォーマット

デコード出力の形式はストリームのネイティブ形式で決まります。出力形式を選択する API はありません。

| フォーマット | 説明 |
|---|---|
| NV12 | 8bit ストリームの出力 (Semi-Planar YUV 4:2:0 8bit) |
| P010 | 10bit ストリームの出力 (Semi-Planar YUV 4:2:0 10bit、16bit ワードの上位 10bit) |

`supported_codecs()` が返す `CodecInfo::decoding` → `DecodingInfo::profiles` → `ProfileCapabilities::memory_types` → `MemoryTypeCapabilities::color_formats` に、プロファイル・メモリタイプごとのデコード出力色形式が入ります。

12bit (P016) や 4:4:4 (AYUV / Y410) など、NV12 / P010 以外の出力は未対応で、デコード時にエラーになります。
また、`Decoder` から出力フォーマットを取得する API はありません。

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

## Intel VPL ライセンス

<https://github.com/intel/libvpl/blob/main/LICENSE>

```text
MIT License

Copyright (c) 2020 Intel Corporation

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

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
