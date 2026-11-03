//! コーデック種別・プロファイルの定義と、コーデック情報の照会

use crate::vpl::{VplLibrary, VplLoader};
use crate::{AdapterSelector, Error, sys};

/// コーデック種別
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoCodecType {
    /// H.264 / AVC
    H264,
    /// H.265 / HEVC
    Hevc,
    /// VP9
    Vp9,
    /// AV1
    Av1,
}

impl VideoCodecType {
    /// MFX CodecID に変換する
    pub(crate) fn to_codec_id(self) -> u32 {
        match self {
            Self::H264 => sys::MFX_CODEC_AVC,
            Self::Hevc => sys::MFX_CODEC_HEVC,
            Self::Vp9 => sys::MFX_CODEC_VP9,
            Self::Av1 => sys::MFX_CODEC_AV1,
        }
    }

    /// すべてのコーデック種別を返す
    fn all() -> &'static [Self] {
        &[Self::H264, Self::Hevc, Self::Vp9, Self::Av1]
    }
}

/// H.264 プロファイル
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum H264Profile {
    /// ベースラインプロファイル
    Baseline,
    /// Constrained ベースラインプロファイル
    ConstrainedBaseline,
    /// メインプロファイル
    Main,
    /// ハイプロファイル
    High,
    /// Constrained ハイプロファイル
    ConstrainedHigh,
    /// High 10 プロファイル（10bit）
    High10,
    /// High 4:2:2 プロファイル
    High422,
}

impl H264Profile {
    /// MFX_PROFILE_AVC_* の値に変換する
    pub(crate) fn to_mfx_profile(self) -> u32 {
        match self {
            Self::Baseline => sys::MFX_PROFILE_AVC_BASELINE,
            Self::ConstrainedBaseline => sys::MFX_PROFILE_AVC_CONSTRAINED_BASELINE,
            Self::Main => sys::MFX_PROFILE_AVC_MAIN,
            Self::High => sys::MFX_PROFILE_AVC_HIGH,
            Self::ConstrainedHigh => sys::MFX_PROFILE_AVC_CONSTRAINED_HIGH,
            Self::High10 => sys::MFX_PROFILE_AVC_HIGH10,
            Self::High422 => sys::MFX_PROFILE_AVC_HIGH_422,
        }
    }

    /// MFX_PROFILE_AVC_* の値から変換する
    ///
    /// 既知のプロファイル以外の値の場合は None を返す。
    pub(crate) fn from_mfx_profile(id: u32) -> Option<Self> {
        match id {
            sys::MFX_PROFILE_AVC_BASELINE => Some(Self::Baseline),
            sys::MFX_PROFILE_AVC_CONSTRAINED_BASELINE => Some(Self::ConstrainedBaseline),
            sys::MFX_PROFILE_AVC_MAIN => Some(Self::Main),
            sys::MFX_PROFILE_AVC_HIGH => Some(Self::High),
            sys::MFX_PROFILE_AVC_CONSTRAINED_HIGH => Some(Self::ConstrainedHigh),
            sys::MFX_PROFILE_AVC_HIGH10 => Some(Self::High10),
            sys::MFX_PROFILE_AVC_HIGH_422 => Some(Self::High422),
            _ => None,
        }
    }
}

/// HEVC プロファイル
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HevcProfile {
    /// メインプロファイル
    Main,
    /// Main 10 プロファイル（10bit）
    Main10,
    /// Main Still Picture プロファイル
    MainSp,
    /// Range Extension プロファイル
    Rext,
    /// Screen Content Coding プロファイル
    Scc,
}

impl HevcProfile {
    /// MFX_PROFILE_HEVC_* の値に変換する
    pub(crate) fn to_mfx_profile(self) -> u32 {
        match self {
            Self::Main => sys::MFX_PROFILE_HEVC_MAIN,
            Self::Main10 => sys::MFX_PROFILE_HEVC_MAIN10,
            Self::MainSp => sys::MFX_PROFILE_HEVC_MAINSP,
            Self::Rext => sys::MFX_PROFILE_HEVC_REXT,
            Self::Scc => sys::MFX_PROFILE_HEVC_SCC,
        }
    }

    /// MFX_PROFILE_HEVC_* の値から変換する
    ///
    /// 既知のプロファイル以外の値の場合は None を返す。
    pub(crate) fn from_mfx_profile(id: u32) -> Option<Self> {
        match id {
            sys::MFX_PROFILE_HEVC_MAIN => Some(Self::Main),
            sys::MFX_PROFILE_HEVC_MAIN10 => Some(Self::Main10),
            sys::MFX_PROFILE_HEVC_MAINSP => Some(Self::MainSp),
            sys::MFX_PROFILE_HEVC_REXT => Some(Self::Rext),
            sys::MFX_PROFILE_HEVC_SCC => Some(Self::Scc),
            _ => None,
        }
    }
}

/// VP9 プロファイル
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Vp9Profile {
    /// Profile 0（8bit YUV 4:2:0）
    Profile0,
    /// Profile 1（8bit YUV 4:2:2 または 4:4:4）
    Profile1,
    /// Profile 2（10/12bit YUV 4:2:0）
    Profile2,
    /// Profile 3（10/12bit YUV 4:2:2 または 4:4:4）
    Profile3,
}

impl Vp9Profile {
    /// MFX_PROFILE_VP9_* の値に変換する
    pub(crate) fn to_mfx_profile(self) -> u32 {
        match self {
            Self::Profile0 => sys::MFX_PROFILE_VP9_0,
            Self::Profile1 => sys::MFX_PROFILE_VP9_1,
            Self::Profile2 => sys::MFX_PROFILE_VP9_2,
            Self::Profile3 => sys::MFX_PROFILE_VP9_3,
        }
    }

    /// MFX_PROFILE_VP9_* の値から変換する
    ///
    /// 既知のプロファイル以外の値の場合は None を返す。
    pub(crate) fn from_mfx_profile(id: u32) -> Option<Self> {
        match id {
            sys::MFX_PROFILE_VP9_0 => Some(Self::Profile0),
            sys::MFX_PROFILE_VP9_1 => Some(Self::Profile1),
            sys::MFX_PROFILE_VP9_2 => Some(Self::Profile2),
            sys::MFX_PROFILE_VP9_3 => Some(Self::Profile3),
            _ => None,
        }
    }
}

/// AV1 プロファイル
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Av1Profile {
    /// メインプロファイル
    Main,
}

impl Av1Profile {
    /// MFX_PROFILE_AV1_* の値に変換する
    pub(crate) fn to_mfx_profile(self) -> u32 {
        match self {
            Self::Main => sys::MFX_PROFILE_AV1_MAIN,
        }
    }

    /// MFX_PROFILE_AV1_* の値から変換する
    ///
    /// 既知のプロファイル以外の値の場合は None を返す。
    pub(crate) fn from_mfx_profile(id: u32) -> Option<Self> {
        match id {
            sys::MFX_PROFILE_AV1_MAIN => Some(Self::Main),
            _ => None,
        }
    }
}

/// コーデックごとの情報
#[derive(Debug, Clone, PartialEq)]
pub struct CodecInfo {
    /// コーデック種別
    pub codec: VideoCodecType,
    /// デコード情報
    pub decoding: DecodingInfo,
    /// エンコード情報
    pub encoding: EncodingInfo,
}

/// デコード情報
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodingInfo {
    /// デコードが可能か
    pub supported: bool,
    /// ハードウェアアクセラレーションが利用可能か
    pub hardware_accelerated: bool,
}

/// エンコード情報
#[derive(Debug, Clone, PartialEq)]
pub struct EncodingInfo {
    /// エンコードが可能か
    pub supported: bool,
    /// ハードウェアアクセラレーションが利用可能か
    pub hardware_accelerated: bool,
    /// フレームリオーダリング（B フレーム）をサポートするか
    pub supports_frame_reordering: bool,
    /// マルチパスエンコードをサポートするか
    pub supports_multi_pass: bool,
    /// コーデック固有のプロファイル情報
    pub profiles: EncodingProfiles,
}

/// コーデック固有のエンコードプロファイル情報
#[derive(Debug, Clone, PartialEq)]
pub enum EncodingProfiles {
    /// H.264 プロファイル一覧
    H264(Vec<H264Profile>),
    /// HEVC プロファイル一覧
    Hevc(Vec<HevcProfile>),
    /// VP9 プロファイル一覧
    Vp9(Vec<Vp9Profile>),
    /// AV1 プロファイル一覧
    Av1(Vec<Av1Profile>),
    /// プロファイル情報なし（エンコード非対応）
    None,
}

/// 指定アダプタで利用可能なコーデック情報の一覧を返す
///
/// 指定アダプタの Intel HW 実装を照会し、各コーデックのエンコード・デコード対応状況を返す。
///
/// 指定アダプタに対応する Intel HW 実装が見つからない場合は、render node 番号を含む
/// メッセージと `MFX_ERR_NOT_FOUND` のステータスを持つ `Error` を返す。
pub fn supported_codecs(adapter: AdapterSelector) -> Result<Vec<CodecInfo>, Error> {
    adapter.validate()?;
    let AdapterSelector::DrmRenderNode(render_node) = adapter;

    let mut loader = VplLoader::new(VplLibrary::load()?)?;
    loader.require_hardware()?;
    loader.require_drm_render_node(render_node)?;
    let raw_loader = unsafe { loader.as_ptr() };

    // 実装の詳細情報を取得する
    let mut hdl: sys::mfxHDL = std::ptr::null_mut();
    let status = unsafe {
        sys::MFXEnumImplementations(
            raw_loader,
            0,
            sys::mfxImplCapsDeliveryFormat_MFX_IMPLCAPS_IMPLDESCSTRUCTURE,
            &mut hdl,
        )
    };
    if status != sys::mfxStatus_MFX_ERR_NONE {
        let err = Error::from_mfx(status, "MFXEnumImplementations");
        if status == sys::mfxStatus_MFX_ERR_NOT_FOUND {
            return Err(err.with_message(VplLoader::not_found_message(render_node)));
        }
        return Err(err);
    }

    let desc = hdl as *const sys::mfxImplDescription;
    if desc.is_null() {
        return Err(Error::new_custom(
            "MFXEnumImplementations",
            "returned null handle",
        ));
    }
    let result = unsafe { build_codec_info_list(&*desc) };

    // MFXDispReleaseImplDescription はローダーが有効なうちに呼ぶ必要がある
    unsafe {
        sys::MFXDispReleaseImplDescription(raw_loader, hdl);
    }

    Ok(result)
}

/// mfxImplDescription からコーデック情報の一覧を構築する
unsafe fn build_codec_info_list(desc: &sys::mfxImplDescription) -> Vec<CodecInfo> {
    VideoCodecType::all()
        .iter()
        .map(|&codec| CodecInfo {
            codec,
            decoding: unsafe { probe_decoding(&desc.Dec, codec) },
            encoding: unsafe { probe_encoding(&desc.Enc, codec) },
        })
        .collect()
}

/// mfxDecoderDescription からデコード情報を判定する
///
/// VPL はハードウェアセッション前提の API であるため、
/// supported と hardware_accelerated は同じ値になる。
unsafe fn probe_decoding(dec: &sys::mfxDecoderDescription, codec: VideoCodecType) -> DecodingInfo {
    let target_id = codec.to_codec_id();
    let num_codecs = dec.NumCodecs as usize;

    if num_codecs > 0 && dec.Codecs.is_null() {
        return DecodingInfo {
            supported: false,
            hardware_accelerated: false,
        };
    }

    for i in 0..num_codecs {
        let entry = unsafe { &*dec.Codecs.add(i) };
        if entry.CodecID == target_id {
            return DecodingInfo {
                supported: true,
                hardware_accelerated: true,
            };
        }
    }

    DecodingInfo {
        supported: false,
        hardware_accelerated: false,
    }
}

/// mfxEncoderDescription からエンコード情報を判定する
unsafe fn probe_encoding(enc: &sys::mfxEncoderDescription, codec: VideoCodecType) -> EncodingInfo {
    let target_id = codec.to_codec_id();
    let num_codecs = enc.NumCodecs as usize;

    if num_codecs > 0 && enc.Codecs.is_null() {
        return EncodingInfo {
            supported: false,
            hardware_accelerated: false,
            supports_frame_reordering: false,
            supports_multi_pass: false,
            profiles: EncodingProfiles::None,
        };
    }

    for i in 0..num_codecs {
        let entry = unsafe { &*enc.Codecs.add(i) };
        if entry.CodecID != target_id {
            continue;
        }

        let supports_frame_reordering = entry.BiDirectionalPrediction != 0;
        let profiles = unsafe { query_encoding_profiles(codec, entry) };

        return EncodingInfo {
            supported: true,
            hardware_accelerated: true,
            supports_frame_reordering,
            // VPL にマルチパスエンコードの照会フィールドはない
            supports_multi_pass: false,
            profiles,
        };
    }

    EncodingInfo {
        supported: false,
        hardware_accelerated: false,
        supports_frame_reordering: false,
        supports_multi_pass: false,
        profiles: EncodingProfiles::None,
    }
}

/// エンコーダのプロファイル一覧を照会する
///
/// 既知のプロファイル ID だけを、`Profiles` に並んでいる順序で返す。
unsafe fn query_encoding_profiles(
    codec: VideoCodecType,
    entry: &sys::mfxEncoderDescription_encoder,
) -> EncodingProfiles {
    let num_profiles = entry.NumProfiles as usize;
    let mut profile_ids = Vec::new();

    if num_profiles > 0 && !entry.Profiles.is_null() {
        for i in 0..num_profiles {
            let profile = unsafe { &*entry.Profiles.add(i) };
            profile_ids.push(profile.Profile);
        }
    }

    match codec {
        VideoCodecType::H264 => EncodingProfiles::H264(
            profile_ids
                .into_iter()
                .filter_map(H264Profile::from_mfx_profile)
                .collect(),
        ),
        VideoCodecType::Hevc => EncodingProfiles::Hevc(
            profile_ids
                .into_iter()
                .filter_map(HevcProfile::from_mfx_profile)
                .collect(),
        ),
        VideoCodecType::Vp9 => EncodingProfiles::Vp9(
            profile_ids
                .into_iter()
                .filter_map(Vp9Profile::from_mfx_profile)
                .collect(),
        ),
        VideoCodecType::Av1 => EncodingProfiles::Av1(
            profile_ids
                .into_iter()
                .filter_map(Av1Profile::from_mfx_profile)
                .collect(),
        ),
    }
}
