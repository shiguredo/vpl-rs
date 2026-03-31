//! コーデック情報の照会

#[cfg(target_os = "linux")]
use crate::{Error, VplLibrary, sys};

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

#[cfg(target_os = "linux")]
impl VideoCodecType {
    /// MFX CodecID に変換する
    fn to_codec_id(self) -> u32 {
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
    H264(Vec<H264EncodingProfile>),
    /// HEVC プロファイル一覧
    Hevc(Vec<HevcEncodingProfile>),
    /// VP9 プロファイル一覧
    Vp9(Vec<Vp9EncodingProfile>),
    /// AV1 プロファイル一覧
    Av1(Vec<Av1EncodingProfile>),
    /// プロファイル情報なし（エンコード非対応）
    None,
}

/// H.264 エンコードプロファイル
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum H264EncodingProfile {
    /// Baseline
    Baseline,
    /// Constrained Baseline
    ConstrainedBaseline,
    /// Main
    Main,
    /// High
    High,
    /// Constrained High
    ConstrainedHigh,
    /// High 10 (10bit)
    High10,
    /// High 4:2:2
    High422,
}

/// HEVC エンコードプロファイル
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HevcEncodingProfile {
    /// Main
    Main,
    /// Main10
    Main10,
    /// Main Still Picture
    MainSp,
    /// Range Extension
    Rext,
    /// Screen Content Coding
    Scc,
}

/// VP9 エンコードプロファイル
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Vp9EncodingProfile {
    /// Profile 0 (8bit YUV 4:2:0)
    Profile0,
    /// Profile 1 (8bit YUV 4:2:2 / 4:4:4)
    Profile1,
    /// Profile 2 (10/12bit YUV 4:2:0)
    Profile2,
    /// Profile 3 (10/12bit YUV 4:2:2 / 4:4:4)
    Profile3,
}

/// AV1 エンコードプロファイル
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Av1EncodingProfile {
    /// Main
    Main,
}

/// このバックエンドで利用可能なコーデック情報の一覧を返す
///
/// VPL ローダーを使って Intel GPU のハードウェア実装を照会し、
/// 各コーデックのエンコード・デコード対応状況を返す。
#[cfg(target_os = "linux")]
pub fn supported_codecs() -> Result<Vec<CodecInfo>, Error> {
    let lib = VplLibrary::load()?;

    // ローダーを作成する
    let loader = unsafe { sys::MFXLoad() };
    if loader.is_null() {
        return Err(Error::new_custom("MFXLoad", "returned null"));
    }

    // ハードウェア実装のフィルタを設定する
    let cfg = unsafe { sys::MFXCreateConfig(loader) };
    if cfg.is_null() {
        unsafe { sys::MFXUnload(loader) };
        return Err(Error::new_custom("MFXCreateConfig", "returned null"));
    }

    let name = b"mfxImplDescription.Impl\0";
    let mut variant: sys::mfxVariant = unsafe { std::mem::zeroed() };
    variant.Type = sys::mfxVariantType_MFX_VARIANT_TYPE_U32;
    variant.Data.U32 = sys::mfxImplType_MFX_IMPL_TYPE_HARDWARE;
    let status = unsafe { sys::MFXSetConfigFilterProperty(cfg, name.as_ptr(), variant) };
    if status != sys::mfxStatus_MFX_ERR_NONE {
        unsafe { sys::MFXUnload(loader) };
        return Err(Error::from_mfx(status, "MFXSetConfigFilterProperty"));
    }

    // 実装の詳細情報を取得する
    let mut hdl: sys::mfxHDL = std::ptr::null_mut();
    let status = unsafe {
        sys::MFXEnumImplementations(
            loader,
            0,
            sys::mfxImplCapsDeliveryFormat_MFX_IMPLCAPS_IMPLDESCSTRUCTURE,
            &mut hdl,
        )
    };
    if status != sys::mfxStatus_MFX_ERR_NONE {
        unsafe { sys::MFXUnload(loader) };
        return Err(Error::from_mfx(status, "MFXEnumImplementations"));
    }

    let desc = hdl as *const sys::mfxImplDescription;
    if desc.is_null() {
        unsafe { sys::MFXUnload(loader) };
        return Err(Error::new_custom(
            "MFXEnumImplementations",
            "returned null handle",
        ));
    }
    let result = unsafe { build_codec_info_list(&*desc) };

    // リソースを解放する
    unsafe {
        sys::MFXDispReleaseImplDescription(loader, hdl);
        sys::MFXUnload(loader);
    }
    let _ = lib;

    Ok(result)
}

/// mfxImplDescription からコーデック情報の一覧を構築する
#[cfg(target_os = "linux")]
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
#[cfg(target_os = "linux")]
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
#[cfg(target_os = "linux")]
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
#[cfg(target_os = "linux")]
unsafe fn query_encoding_profiles(
    codec: VideoCodecType,
    entry: &sys::mfxEncoderDescription_encoder,
) -> EncodingProfiles {
    let num_profiles = entry.NumProfiles as usize;
    let mut profile_ids = Vec::with_capacity(num_profiles);

    if num_profiles > 0 && !entry.Profiles.is_null() {
        for i in 0..num_profiles {
            let profile = unsafe { &*entry.Profiles.add(i) };
            profile_ids.push(profile.Profile);
        }
    }

    match codec {
        VideoCodecType::H264 => {
            let map: &[(u32, H264EncodingProfile)] = &[
                (sys::MFX_PROFILE_AVC_BASELINE, H264EncodingProfile::Baseline),
                (
                    sys::MFX_PROFILE_AVC_CONSTRAINED_BASELINE,
                    H264EncodingProfile::ConstrainedBaseline,
                ),
                (sys::MFX_PROFILE_AVC_MAIN, H264EncodingProfile::Main),
                (sys::MFX_PROFILE_AVC_HIGH, H264EncodingProfile::High),
                (
                    sys::MFX_PROFILE_AVC_CONSTRAINED_HIGH,
                    H264EncodingProfile::ConstrainedHigh,
                ),
                (sys::MFX_PROFILE_AVC_HIGH10, H264EncodingProfile::High10),
                (sys::MFX_PROFILE_AVC_HIGH_422, H264EncodingProfile::High422),
            ];
            EncodingProfiles::H264(match_profiles(&profile_ids, map))
        }
        VideoCodecType::Hevc => {
            let map: &[(u32, HevcEncodingProfile)] = &[
                (sys::MFX_PROFILE_HEVC_MAIN, HevcEncodingProfile::Main),
                (sys::MFX_PROFILE_HEVC_MAIN10, HevcEncodingProfile::Main10),
                (sys::MFX_PROFILE_HEVC_MAINSP, HevcEncodingProfile::MainSp),
                (sys::MFX_PROFILE_HEVC_REXT, HevcEncodingProfile::Rext),
                (sys::MFX_PROFILE_HEVC_SCC, HevcEncodingProfile::Scc),
            ];
            EncodingProfiles::Hevc(match_profiles(&profile_ids, map))
        }
        VideoCodecType::Vp9 => {
            let map: &[(u32, Vp9EncodingProfile)] = &[
                (sys::MFX_PROFILE_VP9_0, Vp9EncodingProfile::Profile0),
                (sys::MFX_PROFILE_VP9_1, Vp9EncodingProfile::Profile1),
                (sys::MFX_PROFILE_VP9_2, Vp9EncodingProfile::Profile2),
                (sys::MFX_PROFILE_VP9_3, Vp9EncodingProfile::Profile3),
            ];
            EncodingProfiles::Vp9(match_profiles(&profile_ids, map))
        }
        VideoCodecType::Av1 => {
            let map: &[(u32, Av1EncodingProfile)] =
                &[(sys::MFX_PROFILE_AV1_MAIN, Av1EncodingProfile::Main)];
            EncodingProfiles::Av1(match_profiles(&profile_ids, map))
        }
    }
}

/// プロファイル ID の一覧から既知のプロファイルをマッチさせる
#[cfg(target_os = "linux")]
fn match_profiles<T: Copy>(profile_ids: &[u32], map: &[(u32, T)]) -> Vec<T> {
    let mut profiles = Vec::new();
    for &id in profile_ids {
        for &(ref_id, profile) in map {
            if id == ref_id {
                profiles.push(profile);
            }
        }
    }
    profiles
}
