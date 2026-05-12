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
//!     AdapterSelector, CodecConfig, EncodeOptions, Encoder, EncoderConfig,
//!     FrameFormat, H264EncoderConfig, RateControlMode, list_adapters,
//! };
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
//! let mut encoder = Encoder::new(config).unwrap();
//!
//! // フレームデータをエンコードする
//! let (coded_width, coded_height) = encoder.coded_size();
//! let frame_size = FrameFormat::Nv12
//!     .frame_size(coded_width, coded_height)
//!     .unwrap();
//! let frame_data = vec![0u8; frame_size];
//! let options = EncodeOptions { frame_type: 0 };
//! encoder.encode(&frame_data, &options).unwrap();
//!
//! // エンコード済みフレームを取り出す
//! while let Some(encoded) = encoder.next_frame() {
//!     let _bitstream = encoded.data();
//! }
//! ```

mod adapter;
mod codec_info;
mod decode;
mod encode;
mod error;
mod sys;

/// テスト用に VPL バインディングを公開する
#[doc(hidden)]
pub mod ffi {
    pub use crate::sys::*;
}

pub use adapter::{AdapterInfo, AdapterSelector, MediaAdapterType, PciAddress, list_adapters};
pub use codec_info::*;
pub use decode::{DecodedFrame, Decoder, DecoderCodec, DecoderConfig};
pub use encode::{
    Av1EncoderConfig, Av1Profile, CodecConfig, EncodeOptions, EncodedFrame, Encoder, EncoderConfig,
    EncoderStats, FrameFormat, H264EncoderConfig, H264Profile, HevcEncoderConfig, HevcProfile,
    PictureType, RateControlMode, ReconfigureParams, Vp9EncoderConfig, Vp9Profile,
};
pub use error::Error;

/// ビルド時に参照したバージョン
pub const BUILD_VERSION: &str = sys::BUILD_METADATA_VERSION;

/// mfxEncodeCtrl.FrameType に対応するフレームタイプ定数
///
/// `EncodeOptions.frame_type` にビット OR で組み合わせて指定する。
pub mod frame_type {
    use crate::sys;

    /// フレームタイプ未指定（エンコーダが自動決定）
    pub const UNKNOWN: u16 = 0;
    /// I フレーム
    pub const I: u16 = sys::MFX_FRAMETYPE_I as u16;
    /// P フレーム
    pub const P: u16 = sys::MFX_FRAMETYPE_P as u16;
    /// B フレーム
    pub const B: u16 = sys::MFX_FRAMETYPE_B as u16;
    /// S フレーム（SkipFrame）
    pub const S: u16 = sys::MFX_FRAMETYPE_S as u16;
    /// 参照フレーム
    pub const REF: u16 = sys::MFX_FRAMETYPE_REF as u16;
    /// IDR フレーム
    pub const IDR: u16 = sys::MFX_FRAMETYPE_IDR as u16;
}

/// mfxInfoMFX.GopOptFlag に対応する GOP オプション定数
///
/// `EncoderConfig.gop_opt_flag` にビット OR で組み合わせて指定する。
pub mod gop_opt_flag {
    use crate::sys;

    /// Closed GOP（前の GOP のフレームを参照しない）
    pub const CLOSED: u16 = sys::MFX_GOP_CLOSED as u16;
    /// Strict GOP（フレームタイプの変更を禁止する）
    pub const STRICT: u16 = sys::MFX_GOP_STRICT as u16;
}

/// Intel VPL ライブラリのラッパー構造体
///
/// 静的リンクされた libvpl の関数を直接呼び出す。
#[derive(Debug, Clone, Copy)]
pub(crate) struct VplLibrary;

impl VplLibrary {
    /// VPL ライブラリをロードする
    pub(crate) fn load() -> Result<Self, Error> {
        Ok(Self)
    }

    /// API 2.x フローでローダーを作成しセッションを生成する
    ///
    /// MFXLoad → MFXCreateConfig × 2 → MFXSetConfigFilterProperty × 2 → MFXCreateSession の順に
    /// 呼び出す。HW 実装フィルタと DRM render node フィルタを別々の `mfxConfig` ハンドルに設定
    /// するのは libvpl の慣用（ヘッダ `mfxdispatcher.h` の `MFX_ADD_PROPERTY_U32` マクロが
    /// 1 プロパティ 1 cfg で組み立てるスタイル）に合わせるため。
    ///
    /// 成功時はローダーとセッションのペアを返す。ローダーはセッションが有効な間は保持する必要が
    /// ある。指定の DRM render node に対応する Intel HW 実装が見つからない場合は libvpl の
    /// `MFXCreateSession` が `MFX_ERR_NOT_FOUND` を返すため、エラーメッセージにその render
    /// node 番号を含めて返す。
    pub(crate) fn create_session(
        &self,
        adapter: AdapterSelector,
    ) -> Result<(sys::mfxLoader, sys::mfxSession), Error> {
        adapter.validate()?;
        let AdapterSelector::DrmRenderNode(render_node) = adapter;

        // ローダーを作成する
        let loader = unsafe { sys::MFXLoad() };
        if loader.is_null() {
            return Err(Error::new_custom("MFXLoad", "returned null"));
        }

        // 1 つ目の cfg: HW 実装フィルタ
        let cfg_impl = unsafe { sys::MFXCreateConfig(loader) };
        if cfg_impl.is_null() {
            unsafe { sys::MFXUnload(loader) };
            return Err(Error::new_custom("MFXCreateConfig", "returned null"));
        }
        let name = b"mfxImplDescription.Impl\0";
        let mut variant: sys::mfxVariant = unsafe { std::mem::zeroed() };
        variant.Type = sys::mfxVariantType_MFX_VARIANT_TYPE_U32;
        variant.Data.U32 = sys::mfxImplType_MFX_IMPL_TYPE_HARDWARE;
        let status = unsafe { sys::MFXSetConfigFilterProperty(cfg_impl, name.as_ptr(), variant) };
        if status != sys::mfxStatus_MFX_ERR_NONE {
            unsafe { sys::MFXUnload(loader) };
            return Err(Error::from_mfx(status, "MFXSetConfigFilterProperty"));
        }

        // 2 つ目の cfg: DRM render node フィルタ
        let cfg_drm = unsafe { sys::MFXCreateConfig(loader) };
        if cfg_drm.is_null() {
            unsafe { sys::MFXUnload(loader) };
            return Err(Error::new_custom("MFXCreateConfig", "returned null"));
        }
        let drm_name = b"mfxExtendedDeviceId.DRMRenderNodeNum\0";
        let mut drm_variant: sys::mfxVariant = unsafe { std::mem::zeroed() };
        drm_variant.Type = sys::mfxVariantType_MFX_VARIANT_TYPE_U32;
        drm_variant.Data.U32 = render_node;
        let status =
            unsafe { sys::MFXSetConfigFilterProperty(cfg_drm, drm_name.as_ptr(), drm_variant) };
        if status != sys::mfxStatus_MFX_ERR_NONE {
            unsafe { sys::MFXUnload(loader) };
            return Err(Error::from_mfx(status, "MFXSetConfigFilterProperty"));
        }

        // フィルタを通過した実装（インデックス 0）でセッションを作成する
        let mut session: sys::mfxSession = std::ptr::null_mut();
        let status = unsafe { sys::MFXCreateSession(loader, 0, &mut session) };
        if status != sys::mfxStatus_MFX_ERR_NONE {
            unsafe { sys::MFXUnload(loader) };
            let err = Error::from_mfx(status, "MFXCreateSession");
            if status == sys::mfxStatus_MFX_ERR_NOT_FOUND {
                return Err(err.with_message(format!(
                    "no Intel HW implementation found for DRM render node {render_node}"
                )));
            }
            return Err(err);
        }

        Ok((loader, session))
    }

    /// MFXUnload を呼び出してローダーを破棄する
    pub(crate) fn mfx_unload(&self, loader: sys::mfxLoader) {
        unsafe { sys::MFXUnload(loader) };
    }

    /// MFXClose を呼び出してセッションを破棄する
    pub(crate) fn mfx_close(&self, session: sys::mfxSession) -> Result<(), Error> {
        let status = unsafe { sys::MFXClose(session) };
        Error::check_mfx(status, "MFXClose")
    }

    /// MFXVideoENCODE_Init を呼び出してエンコーダを初期化する
    ///
    /// VPL の仕様では Init は MFX_WRN_INCOMPATIBLE_VIDEO_PARAM 等の警告を
    /// 返すことがあるが、初期化自体は成功している。警告は許容する。
    pub(crate) fn mfx_video_encode_init(
        &self,
        session: sys::mfxSession,
        par: *mut sys::mfxVideoParam,
    ) -> Result<(), Error> {
        let status = unsafe { sys::MFXVideoENCODE_Init(session, par) };
        Error::check_mfx_allow_warn(status, "MFXVideoENCODE_Init")
    }

    /// MFXVideoENCODE_Close を呼び出してエンコーダを破棄する
    pub(crate) fn mfx_video_encode_close(&self, session: sys::mfxSession) -> Result<(), Error> {
        let status = unsafe { sys::MFXVideoENCODE_Close(session) };
        Error::check_mfx(status, "MFXVideoENCODE_Close")
    }

    /// MFXVideoENCODE_EncodeFrameAsync を呼び出してフレームを非同期エンコードする
    ///
    /// mfxStatus をそのまま返す（MFX_ERR_MORE_DATA や MFX_WRN_DEVICE_BUSY の処理は呼び出し元が行う）
    pub(crate) fn mfx_video_encode_frame_async(
        &self,
        session: sys::mfxSession,
        ctrl: *mut sys::mfxEncodeCtrl,
        surface: *mut sys::mfxFrameSurface1,
        bs: *mut sys::mfxBitstream,
        syncp: *mut sys::mfxSyncPoint,
    ) -> i32 {
        unsafe { sys::MFXVideoENCODE_EncodeFrameAsync(session, ctrl, surface, bs, syncp) }
    }

    /// MFXVideoCORE_SyncOperation を呼び出して非同期操作の完了を待機する
    pub(crate) fn mfx_video_core_sync_operation(
        &self,
        session: sys::mfxSession,
        syncp: sys::mfxSyncPoint,
        wait: u32,
    ) -> i32 {
        unsafe { sys::MFXVideoCORE_SyncOperation(session, syncp, wait) }
    }

    /// MFXVideoENCODE_Query を呼び出してパラメータのサポート可否を検証する
    pub(crate) fn mfx_video_encode_query(
        &self,
        session: sys::mfxSession,
        input: *mut sys::mfxVideoParam,
        output: *mut sys::mfxVideoParam,
    ) -> i32 {
        unsafe { sys::MFXVideoENCODE_Query(session, input, output) }
    }

    /// MFXVideoENCODE_Reset を呼び出してエンコーダパラメータを動的に変更する
    ///
    /// Init と同様に警告を返すことがあるが、リセット自体は成功している。
    pub(crate) fn mfx_video_encode_reset(
        &self,
        session: sys::mfxSession,
        par: *mut sys::mfxVideoParam,
    ) -> Result<(), Error> {
        let status = unsafe { sys::MFXVideoENCODE_Reset(session, par) };
        Error::check_mfx_allow_warn(status, "MFXVideoENCODE_Reset")
    }

    /// MFXVideoENCODE_GetVideoParam を呼び出して実効パラメータを取得する
    pub(crate) fn mfx_video_encode_get_video_param(
        &self,
        session: sys::mfxSession,
        par: *mut sys::mfxVideoParam,
    ) -> Result<(), Error> {
        let status = unsafe { sys::MFXVideoENCODE_GetVideoParam(session, par) };
        Error::check_mfx(status, "MFXVideoENCODE_GetVideoParam")
    }

    /// MFXVideoENCODE_GetEncodeStat を呼び出してエンコード統計情報を取得する
    pub(crate) fn mfx_video_encode_get_encode_stat(
        &self,
        session: sys::mfxSession,
        stat: *mut sys::mfxEncodeStat,
    ) -> Result<(), Error> {
        let status = unsafe { sys::MFXVideoENCODE_GetEncodeStat(session, stat) };
        Error::check_mfx(status, "MFXVideoENCODE_GetEncodeStat")
    }

    /// MFXVideoDECODE_DecodeHeader を呼び出してビットストリームからパラメータを読み取る
    pub(crate) fn mfx_video_decode_decode_header(
        &self,
        session: sys::mfxSession,
        bs: *mut sys::mfxBitstream,
        par: *mut sys::mfxVideoParam,
    ) -> Result<(), Error> {
        let status = unsafe { sys::MFXVideoDECODE_DecodeHeader(session, bs, par) };
        Error::check_mfx(status, "MFXVideoDECODE_DecodeHeader")
    }

    /// MFXVideoDECODE_Init を呼び出してデコーダを初期化する
    ///
    /// エンコーダと同様に警告を返すことがあるが、初期化自体は成功している。
    pub(crate) fn mfx_video_decode_init(
        &self,
        session: sys::mfxSession,
        par: *mut sys::mfxVideoParam,
    ) -> Result<(), Error> {
        let status = unsafe { sys::MFXVideoDECODE_Init(session, par) };
        Error::check_mfx_allow_warn(status, "MFXVideoDECODE_Init")
    }

    /// MFXVideoDECODE_Close を呼び出してデコーダを破棄する
    pub(crate) fn mfx_video_decode_close(&self, session: sys::mfxSession) -> Result<(), Error> {
        let status = unsafe { sys::MFXVideoDECODE_Close(session) };
        Error::check_mfx(status, "MFXVideoDECODE_Close")
    }

    /// MFXVideoDECODE_DecodeFrameAsync を呼び出してフレームを非同期デコードする
    ///
    /// mfxStatus をそのまま返す（MFX_ERR_MORE_DATA 等の処理は呼び出し元が行う）
    pub(crate) fn mfx_video_decode_frame_async(
        &self,
        session: sys::mfxSession,
        bs: *mut sys::mfxBitstream,
        work_surface: *mut sys::mfxFrameSurface1,
        out_surface: *mut *mut sys::mfxFrameSurface1,
        syncp: *mut sys::mfxSyncPoint,
    ) -> i32 {
        unsafe {
            sys::MFXVideoDECODE_DecodeFrameAsync(session, bs, work_surface, out_surface, syncp)
        }
    }
}
