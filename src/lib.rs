//! Intel VPL (Video Processing Library) の Rust バインディング
//!
//! Intel GPU のハードウェアアクセラレーションを利用した動画エンコード・デコードを提供する。
//! libvpl を静的リンクするため、実行時に VPL ライブラリのインストールは不要。
//!
//! # 対応コーデック
//!
//! - H.264/AVC
//! - H.265/HEVC
//! - VP9
//! - AV1
//!
//! # 対応入力フォーマット
//!
//! - NV12 (Semi-Planar YUV 4:2:0 8bit)
//! - YUY2 (Packed YUV 4:2:2 8bit)
//! - BGRA (Packed 8bit)
//!
//! # メモリタイプ
//!
//! `supported_codecs` の `MemoryType` は VPL の実装記述が持つすべてのメモリタイプを返す。
//! このクレートの `Encoder` / `Decoder` が実際に使うのは `MemoryType::System` だけである。
//!
//! # 動作要件
//!
//! - Linux (x86_64)
//! - Intel GPU (第 6 世代 Core 以降)
//! - ビルド時: git, clang
//!
//! # エンコードの例
//!
//! ```no_run
//! use shiguredo_vpl::{
//!     AdapterSelector, CodecConfig, EncodeOptions, EncodedFrame, Encoder, EncoderConfig,
//!     Error, FnEncodeHandler, FrameFormat, H264EncoderConfig, RateControlMode, list_adapters,
//! };
//! use std::sync::mpsc;
//!
//! let adapter = list_adapters().unwrap().into_iter().next().unwrap();
//! let config = EncoderConfig::new(
//!     AdapterSelector::DrmRenderNode(adapter.drm_render_node),
//!     CodecConfig::H264(H264EncoderConfig { profile: None }),
//!     1920,
//!     1080,
//!     FrameFormat::Nv12,
//!     30,
//!     1,
//!     RateControlMode::Cqp,
//! );
//! let (tx, rx) = mpsc::channel();
//! let mut encoder = Encoder::new(config, FnEncodeHandler::new(move |result: Result<EncodedFrame<()>, Error>| {
//!     tx.send(result).unwrap();
//! })).unwrap();
//!
//! // フレームデータをエンコードする
//! let (coded_width, coded_height) = encoder.coded_size();
//! let frame_size = FrameFormat::Nv12
//!     .frame_size(coded_width, coded_height)
//!     .unwrap();
//! let frame_data = vec![0u8; frame_size];
//! let options = EncodeOptions { frame_type: 0 };
//! encoder.encode(&frame_data, (), &options).unwrap();
//!
//! encoder.finish().unwrap();
//! let encoded = rx.recv().unwrap().unwrap();
//! let _bitstream = encoded.data();
//! ```
//!
//! # デコードの例
//!
//! ```no_run
//! use shiguredo_vpl::{AdapterSelector, DecodedFrame, Decoder, DecoderConfig, Error, FnDecodeHandler, VideoCodecType, list_adapters};
//! use std::sync::mpsc;
//!
//! let adapter = list_adapters().unwrap().into_iter().next().unwrap();
//! let config = DecoderConfig::new(AdapterSelector::DrmRenderNode(adapter.drm_render_node), VideoCodecType::H264);
//! let (tx, rx) = mpsc::channel();
//! let mut decoder = Decoder::new(config, FnDecodeHandler::new(move |result: Result<DecodedFrame<'_, ()>, Error>| {
//!     // DecodedFrame は借用データを含むため、コールバック内でコピーする
//!     let info = result.map(|frame| (frame.y().to_vec(), frame.pitch(), frame.width(), frame.height()));
//!     tx.send(info).unwrap();
//! })).unwrap();
//!
//! // ビットストリームデータをデコードする
//! let bitstream = vec![0u8; 1024];
//! decoder.decode(&bitstream, ()).unwrap();
//! decoder.finish().unwrap();
//! let (y, _pitch, _width, _height) = rx.recv().unwrap().unwrap();
//! ```

// Linux x86_64 以外では libvpl をビルド・リンクできないため、対応外のターゲットではコンパイル時点で拒否する。
// 対応ターゲットの判定はこの compile_error! に集約し、モジュールや項目ごとの cfg は付けない。
#[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
compile_error!("shiguredo_vpl requires Linux x86_64. Other targets are not supported.");

mod adapter;
mod codec_info;
mod decode;
mod encode;
mod error;
mod stats;
mod sys;
mod vpl;

/// テスト用に VPL バインディングを公開する
#[doc(hidden)]
pub mod ffi {
    pub use crate::sys::*;
}

pub use adapter::{AdapterInfo, AdapterSelector, MediaAdapterType, PciAddress, list_adapters};
pub use codec_info::*;
pub use decode::{
    DecodeHandler, DecodedFrame, Decoder, DecoderConfig, DecoderStats, FnDecodeHandler,
};
pub use encode::{
    Av1EncoderConfig, CodecConfig, EncodeHandler, EncodeOptions, EncodedFrame, Encoder,
    EncoderConfig, EncoderStats, ExtParams, FnEncodeHandler, H264EncoderConfig, HevcEncoderConfig,
    PictureType, RateControlMode, ReconfigureParams, SequenceParams, VideoParam, Vp9EncoderConfig,
};
pub use error::Error;
pub use stats::{Counter, Gauge};
pub use vpl::{frame_type, gop_opt_flag};

/// ビルド時に参照したバージョン
pub const BUILD_VERSION: &str = sys::BUILD_METADATA_VERSION;
