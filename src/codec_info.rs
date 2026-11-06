//! コーデック種別・プロファイル・色形式の定義と、コーデック情報の照会

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

/// コーデックのプロファイル
///
/// VPL の `Profile` の値に対応するコーデックごとの型。
/// VPL の標準プロファイルでも、本クレートが型として持たないものは `Unknown` に生の値を保持する。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoProfile {
    /// H.264 プロファイル
    H264(H264Profile),
    /// HEVC プロファイル
    Hevc(HevcProfile),
    /// VP9 プロファイル
    Vp9(Vp9Profile),
    /// AV1 プロファイル
    Av1(Av1Profile),
    /// 本クレートが型として持たないプロファイル
    ///
    /// VPL の標準プロファイルでも、`H264Profile` / `HevcProfile` / `Vp9Profile` / `Av1Profile`
    /// に定義されていないものはここに入る。
    /// 例えば H.264 の Extended、AV1 の High / Professional が該当する。
    Unknown(u32),
}

impl VideoProfile {
    /// MFX_PROFILE_* の値から変換する
    ///
    /// 既知のプロファイル以外の値の場合は [`VideoProfile::Unknown`] を返す。
    fn from_mfx_profile(codec: VideoCodecType, profile: u32) -> Self {
        match codec {
            VideoCodecType::H264 => H264Profile::from_mfx_profile(profile)
                .map(Self::H264)
                .unwrap_or(Self::Unknown(profile)),
            VideoCodecType::Hevc => HevcProfile::from_mfx_profile(profile)
                .map(Self::Hevc)
                .unwrap_or(Self::Unknown(profile)),
            VideoCodecType::Vp9 => Vp9Profile::from_mfx_profile(profile)
                .map(Self::Vp9)
                .unwrap_or(Self::Unknown(profile)),
            VideoCodecType::Av1 => Av1Profile::from_mfx_profile(profile)
                .map(Self::Av1)
                .unwrap_or(Self::Unknown(profile)),
        }
    }
}

/// メモリタイプ
///
/// VPL の `mfxResourceType` に対応する。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryType {
    /// システムメモリ
    System,
    /// VA サーフェス
    VaSurface,
    /// VA バッファ
    VaBuffer,
    /// Direct3D 9 サーフェス
    Dx9Surface,
    /// Direct3D 11 テクスチャ
    Dx11Texture,
    /// Direct3D 12 リソース
    Dx12Resource,
    /// DMA リソース
    DmaResource,
    /// HDDL Unite のリモートメモリ
    HddlUniteRemoteMemory,
    /// 既知の型で表現できないメモリタイプ
    Unknown(u32),
}

impl MemoryType {
    /// mfxResourceType の値から変換する
    fn from_mfx_resource_type(resource_type: sys::mfxResourceType) -> Self {
        match resource_type {
            sys::mfxResourceType_MFX_RESOURCE_SYSTEM_SURFACE => Self::System,
            sys::mfxResourceType_MFX_RESOURCE_VA_SURFACE => Self::VaSurface,
            sys::mfxResourceType_MFX_RESOURCE_VA_BUFFER => Self::VaBuffer,
            sys::mfxResourceType_MFX_RESOURCE_DX9_SURFACE => Self::Dx9Surface,
            sys::mfxResourceType_MFX_RESOURCE_DX11_TEXTURE => Self::Dx11Texture,
            sys::mfxResourceType_MFX_RESOURCE_DX12_RESOURCE => Self::Dx12Resource,
            sys::mfxResourceType_MFX_RESOURCE_DMA_RESOURCE => Self::DmaResource,
            sys::mfxResourceType_MFX_RESOURCE_HDDLUNITE_REMOTE_MEMORY => {
                Self::HddlUniteRemoteMemory
            }
            other => Self::Unknown(other),
        }
    }
}

/// VPL の値の範囲
///
/// VPL の `mfxRange32U` に対応する。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ValueRange {
    /// 最小値
    pub min: u32,
    /// 最大値
    pub max: u32,
    /// 増分
    pub step: u32,
}

impl ValueRange {
    /// mfxRange32U の値から変換する
    fn from_mfx_range(range: sys::mfxRange32U) -> Self {
        Self {
            min: range.Min,
            max: range.Max,
            step: range.Step,
        }
    }
}

/// フレームの色形式
///
/// エンコーダーの入力と、エンコード・デコードのケーパビリティ照会で使う。
///
/// ref: libvpl api/vpl/mfxstructures.h (MFX_FOURCC_P010 / MFX_FOURCC_AYUV / MFX_FOURCC_Y410 / mfxY410)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameFormat {
    /// Semi-Planar YUV 4:2:0 8bit
    ///
    /// Y プレーン (画素数と同じバイト数) の後ろに、2 画素につき 1 組の UV を
    /// インターリーブしたプレーン (画素数の半分のバイト数) を並べる。
    Nv12,
    /// Semi-Planar YUV 4:2:0 10bit
    ///
    /// 並びは [`FrameFormat::Nv12`] と同じで、1 サンプルを 16bit ワードで格納する。
    /// 10bit の値は 16bit ワードの上位ビットに入れる。
    P010,
    /// Packed YUV 4:2:2 8bit
    ///
    /// 2 画素を 4 バイト (Y0, U, Y1, V) で格納する。
    Yuy2,
    /// Packed BGRA 8bit
    ///
    /// 1 画素を 4 バイト (B, G, R, A) で格納する。
    Bgra,
    /// Packed YUV 4:4:4 8bit
    ///
    /// 1 画素を 4 バイト (V, U, Y, A) で格納する。
    Ayuv,
    /// Packed YUV 4:4:4 10bit
    ///
    /// 1 画素を 4 バイトの 32bit ワードで格納する。
    /// ワードの下位から U (10bit) / Y (10bit) / V (10bit) / A (2bit) の順に入れる。
    Y410,
}

impl FrameFormat {
    /// すべての色形式を返す
    #[cfg(test)]
    pub(crate) fn all() -> &'static [Self] {
        const ALL: &[FrameFormat] = &[
            FrameFormat::Nv12,
            FrameFormat::P010,
            FrameFormat::Yuy2,
            FrameFormat::Bgra,
            FrameFormat::Ayuv,
            FrameFormat::Y410,
        ];
        // variant を追加したときに match がコンパイルエラーになり、
        // 一覧の更新漏れに気づけるようにする
        for format in ALL {
            match format {
                FrameFormat::Nv12
                | FrameFormat::P010
                | FrameFormat::Yuy2
                | FrameFormat::Bgra
                | FrameFormat::Ayuv
                | FrameFormat::Y410 => {}
            }
        }
        ALL
    }

    /// MFX_FOURCC_* の値から変換する
    ///
    /// 本クレートが表現できない FourCC の場合は `None` を返す。
    pub(crate) fn from_fourcc(fourcc: u32) -> Option<Self> {
        match fourcc {
            sys::MFX_FOURCC_NV12 => Some(Self::Nv12),
            sys::MFX_FOURCC_P010 => Some(Self::P010),
            sys::MFX_FOURCC_YUY2 => Some(Self::Yuy2),
            sys::MFX_FOURCC_RGB4 => Some(Self::Bgra),
            sys::MFX_FOURCC_AYUV => Some(Self::Ayuv),
            sys::MFX_FOURCC_Y410 => Some(Self::Y410),
            _ => None,
        }
    }

    pub(crate) fn fourcc(self) -> u32 {
        match self {
            FrameFormat::Nv12 => sys::MFX_FOURCC_NV12,
            FrameFormat::P010 => sys::MFX_FOURCC_P010,
            FrameFormat::Yuy2 => sys::MFX_FOURCC_YUY2,
            FrameFormat::Bgra => sys::MFX_FOURCC_RGB4,
            FrameFormat::Ayuv => sys::MFX_FOURCC_AYUV,
            FrameFormat::Y410 => sys::MFX_FOURCC_Y410,
        }
    }

    pub(crate) fn chroma_format(self) -> u16 {
        match self {
            FrameFormat::Nv12 | FrameFormat::P010 => sys::MFX_CHROMAFORMAT_YUV420 as u16,
            FrameFormat::Yuy2 => sys::MFX_CHROMAFORMAT_YUV422 as u16,
            FrameFormat::Bgra | FrameFormat::Ayuv | FrameFormat::Y410 => {
                sys::MFX_CHROMAFORMAT_YUV444 as u16
            }
        }
    }

    /// フレームを構成する各サンプルのビット深度を返す
    pub(crate) fn bit_depth(self) -> u16 {
        match self {
            FrameFormat::P010 | FrameFormat::Y410 => 10,
            FrameFormat::Nv12 | FrameFormat::Yuy2 | FrameFormat::Bgra | FrameFormat::Ayuv => 8,
        }
    }

    /// 指定した幅と高さのフレームのバイトサイズを返す
    ///
    /// NV12 / P010 は `height` が偶数の場合のレイアウトを前提とする。
    /// 計算がオーバーフローした場合は `None` を返す。
    pub fn frame_size(self, width: usize, height: usize) -> Option<usize> {
        let pixels = width.checked_mul(height)?;
        match self {
            // YUV 4:2:0 8bit: Y + UV/2 = 画素 * 3 / 2
            FrameFormat::Nv12 => pixels.checked_mul(3).map(|v| v / 2),
            // YUV 4:2:0 10bit: 1 サンプル 2 バイトで Y + UV/2 = 画素 * 3
            FrameFormat::P010 => pixels.checked_mul(3),
            // YUY2: 2 バイト/画素 (YUYV パック)
            FrameFormat::Yuy2 => pixels.checked_mul(2),
            // BGRA / AYUV / Y410: 4 バイト/画素
            FrameFormat::Bgra | FrameFormat::Ayuv | FrameFormat::Y410 => pixels.checked_mul(4),
        }
    }
}

/// メモリタイプごとのケーパビリティ
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryTypeCapabilities {
    /// メモリタイプ
    pub memory_type: MemoryType,
    /// 幅の範囲
    pub width: ValueRange,
    /// 高さの範囲
    pub height: ValueRange,
    /// 対応する色形式
    ///
    /// [`FrameFormat`] で表現できる FourCC だけを、VPL の `ColorFormats` に
    /// 並んでいる順序で格納する。
    /// VPL が色形式を返さない場合と、表現できる FourCC が無い場合は空。
    pub color_formats: Vec<FrameFormat>,
}

/// プロファイルごとのケーパビリティ
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileCapabilities {
    /// プロファイル
    pub profile: VideoProfile,
    /// メモリタイプごとのケーパビリティ
    ///
    /// VPL の実装記述に並んでいる順序を維持する。
    /// VPL がメモリタイプを返さない場合は空。
    pub memory_types: Vec<MemoryTypeCapabilities>,
}

/// コーデックごとの情報
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodecInfo {
    /// コーデック種別
    pub codec: VideoCodecType,
    /// デコード情報
    ///
    /// デコードに対応していない場合は `None`。
    pub decoding: Option<DecodingInfo>,
    /// エンコード情報
    ///
    /// エンコードに対応していない場合は `None`。
    pub encoding: Option<EncodingInfo>,
}

/// デコード情報
///
/// プロファイルとメモリタイプごとのケーパビリティを持つ。
/// プロファイルやメモリタイプをまたいだ集約は行わない。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodingInfo {
    /// 最大コーデックレベル
    ///
    /// 値の意味はコーデックごとに異なる。
    /// レベルが不明な場合は `None`。
    pub max_codec_level: Option<u16>,
    /// プロファイルごとの情報
    ///
    /// VPL の実装記述に並んでいる順序を維持する。
    /// VPL がプロファイルを返さない場合は空。
    pub profiles: Vec<ProfileCapabilities>,
}

/// エンコード情報
///
/// プロファイルとメモリタイプごとのケーパビリティを持つ。
/// プロファイルやメモリタイプをまたいだ集約は行わない。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodingInfo {
    /// フレームリオーダリング（B フレーム）をサポートするか
    pub supports_frame_reordering: bool,
    /// 最大コーデックレベル
    ///
    /// 値の意味はコーデックごとに異なる。
    /// レベルが不明な場合は `None`。
    pub max_codec_level: Option<u16>,
    /// プロファイルごとの情報
    ///
    /// VPL の実装記述に並んでいる順序を維持する。
    /// VPL がプロファイルを返さない場合は空。
    pub profiles: Vec<ProfileCapabilities>,
}

/// 指定アダプタで利用可能なコーデック情報の一覧を返す
///
/// 各コーデックのエンコード・デコード対応状況とケーパビリティを返す。
/// 返る要素は H.264 / HEVC / VP9 / AV1 の順で、各要素の `decoding` / `encoding` は
/// そのコーデックが対応している場合だけ `Some` になり、対応していない場合は `None` になる。
///
/// # 返る値の構造
///
/// ケーパビリティはコーデック、プロファイル、メモリタイプの階層で返す。
/// プロファイルやメモリタイプをまたいだ集約は行わない。
///
/// - コーデックごとに `max_codec_level`
/// - プロファイルごとに `profiles`
/// - プロファイルの中にメモリタイプごとの `memory_types`
/// - メモリタイプごとに幅・高さの範囲 (`min` / `max` / `step`) と色形式
///
/// 色形式は [`FrameFormat`] で表現できる FourCC だけを、VPL の `ColorFormats` に
/// 並んでいる順序で格納する。
/// VPL が返すプロファイル ID とメモリタイプのうち、本クレートが既知の型で表現できないものは
/// [`VideoProfile::Unknown`] / [`MemoryType::Unknown`] として生の値を保持する。
/// `max_codec_level` はレベルが不明な場合に `None` になる。
/// コーデックのエントリが無い場合は `decoding` / `encoding` 自体が `None` になる。
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

/// メモリタイプごとのケーパビリティを返す
///
/// # Safety
///
/// `color_formats` が null でなく `num_color_formats` が 0 より大きい場合、
/// `color_formats` は `num_color_formats` 個の `u32` を指す有効なポインタで、
/// `u32` として整列されていること。
/// それ以外の場合は `color_formats` を参照しない。
unsafe fn memory_type_capabilities(
    memory_type: sys::mfxResourceType,
    width: sys::mfxRange32U,
    height: sys::mfxRange32U,
    num_color_formats: u16,
    color_formats: *const u32,
) -> MemoryTypeCapabilities {
    let color_formats = if num_color_formats == 0 || color_formats.is_null() {
        Vec::new()
    } else {
        let raw = unsafe { std::slice::from_raw_parts(color_formats, num_color_formats as usize) };
        raw.iter()
            .filter_map(|&fourcc| FrameFormat::from_fourcc(fourcc))
            .collect()
    };
    MemoryTypeCapabilities {
        memory_type: MemoryType::from_mfx_resource_type(memory_type),
        width: ValueRange::from_mfx_range(width),
        height: ValueRange::from_mfx_range(height),
        color_formats,
    }
}

/// mfxDecoderDescription からデコード情報を返す
///
/// コーデックに対応するエントリがあれば `Some`、なければ `None` を返す。
unsafe fn probe_decoding(
    dec: &sys::mfxDecoderDescription,
    codec: VideoCodecType,
) -> Option<DecodingInfo> {
    let target_id = codec.to_codec_id();
    let num_codecs = dec.NumCodecs as usize;

    if num_codecs == 0 || dec.Codecs.is_null() {
        return None;
    }

    for i in 0..num_codecs {
        let entry = unsafe { &*dec.Codecs.add(i) };
        if entry.CodecID != target_id {
            continue;
        }

        return Some(DecodingInfo {
            // VPL はレベルが不明な場合に 0 (MFX_LEVEL_UNKNOWN) を返す。
            max_codec_level: (entry.MaxcodecLevel != 0).then_some(entry.MaxcodecLevel),
            profiles: unsafe { query_decoding_profiles(codec, entry) },
        });
    }

    None
}

/// デコーダのプロファイル一覧を返す
unsafe fn query_decoding_profiles(
    codec: VideoCodecType,
    entry: &sys::mfxDecoderDescription_decoder,
) -> Vec<ProfileCapabilities> {
    let num_profiles = entry.NumProfiles as usize;
    if num_profiles == 0 || entry.Profiles.is_null() {
        return Vec::new();
    }

    let mut profiles = Vec::new();
    for i in 0..num_profiles {
        let profile = unsafe { &*entry.Profiles.add(i) };
        let num_mem_types = profile.NumMemTypes as usize;
        let mut memory_types = Vec::new();
        if num_mem_types > 0 && !profile.MemDesc.is_null() {
            for j in 0..num_mem_types {
                let mem = unsafe { &*profile.MemDesc.add(j) };
                memory_types.push(unsafe {
                    memory_type_capabilities(
                        mem.MemHandleType,
                        mem.Width,
                        mem.Height,
                        mem.NumColorFormats,
                        mem.ColorFormats,
                    )
                });
            }
        }
        profiles.push(ProfileCapabilities {
            profile: VideoProfile::from_mfx_profile(codec, profile.Profile),
            memory_types,
        });
    }
    profiles
}

/// mfxEncoderDescription からエンコード情報を返す
///
/// コーデックに対応するエントリがあれば `Some`、なければ `None` を返す。
unsafe fn probe_encoding(
    enc: &sys::mfxEncoderDescription,
    codec: VideoCodecType,
) -> Option<EncodingInfo> {
    let target_id = codec.to_codec_id();
    let num_codecs = enc.NumCodecs as usize;

    if num_codecs == 0 || enc.Codecs.is_null() {
        return None;
    }

    for i in 0..num_codecs {
        let entry = unsafe { &*enc.Codecs.add(i) };
        if entry.CodecID != target_id {
            continue;
        }

        return Some(EncodingInfo {
            supports_frame_reordering: entry.BiDirectionalPrediction != 0,
            // VPL はレベルが不明な場合に 0 (MFX_LEVEL_UNKNOWN) を返す。
            max_codec_level: (entry.MaxcodecLevel != 0).then_some(entry.MaxcodecLevel),
            profiles: unsafe { query_encoding_profiles(codec, entry) },
        });
    }

    None
}

/// エンコーダのプロファイル一覧を返す
unsafe fn query_encoding_profiles(
    codec: VideoCodecType,
    entry: &sys::mfxEncoderDescription_encoder,
) -> Vec<ProfileCapabilities> {
    let num_profiles = entry.NumProfiles as usize;
    if num_profiles == 0 || entry.Profiles.is_null() {
        return Vec::new();
    }

    let mut profiles = Vec::new();
    for i in 0..num_profiles {
        let profile = unsafe { &*entry.Profiles.add(i) };
        let num_mem_types = profile.NumMemTypes as usize;
        let mut memory_types = Vec::new();
        if num_mem_types > 0 && !profile.MemDesc.is_null() {
            for j in 0..num_mem_types {
                let mem = unsafe { &*profile.MemDesc.add(j) };
                memory_types.push(unsafe {
                    memory_type_capabilities(
                        mem.MemHandleType,
                        mem.Width,
                        mem.Height,
                        mem.NumColorFormats,
                        mem.ColorFormats,
                    )
                });
            }
        }
        profiles.push(ProfileCapabilities {
            profile: VideoProfile::from_mfx_profile(codec, profile.Profile),
            memory_types,
        });
    }
    profiles
}

#[cfg(test)]
mod tests {
    use super::*;

    /// テスト用の mfxRange32U を作る
    fn range(min: u32, max: u32, step: u32) -> sys::mfxRange32U {
        sys::mfxRange32U {
            Min: min,
            Max: max,
            Step: step,
        }
    }

    /// テスト用のデコーダのメモリ記述を作る
    ///
    /// `color_formats` はこの構造体より長く生存させること。
    fn dec_mem_desc(
        mem_handle_type: sys::mfxResourceType,
        width: sys::mfxRange32U,
        height: sys::mfxRange32U,
        color_formats: &mut [u32],
    ) -> sys::mfxDecoderDescription_decoder_decprofile_decmemdesc {
        sys::mfxDecoderDescription_decoder_decprofile_decmemdesc {
            MemHandleType: mem_handle_type,
            Width: width,
            Height: height,
            reserved: [0; 7],
            NumColorFormats: color_formats.len() as u16,
            ColorFormats: color_formats.as_mut_ptr(),
        }
    }

    /// テスト用のデコーダのプロファイル記述を作る
    ///
    /// `mem_descs` はこの構造体より長く生存させること。
    fn dec_profile(
        profile: u32,
        mem_descs: &mut [sys::mfxDecoderDescription_decoder_decprofile_decmemdesc],
    ) -> sys::mfxDecoderDescription_decoder_decprofile {
        sys::mfxDecoderDescription_decoder_decprofile {
            Profile: profile,
            reserved: [0; 7],
            NumMemTypes: mem_descs.len() as u16,
            MemDesc: mem_descs.as_mut_ptr(),
        }
    }

    /// テスト用のデコーダ記述を作る
    ///
    /// `profiles` はこの記述より長く生存させること。
    fn dec_codec(
        codec_id: u32,
        max_codec_level: u16,
        profiles: &mut [sys::mfxDecoderDescription_decoder_decprofile],
    ) -> sys::mfxDecoderDescription_decoder {
        sys::mfxDecoderDescription_decoder {
            CodecID: codec_id,
            reserved: [0; 8],
            MaxcodecLevel: max_codec_level,
            NumProfiles: profiles.len() as u16,
            Profiles: profiles.as_mut_ptr(),
        }
    }

    /// テスト用のデコーダ記述の一覧を作る
    ///
    /// `codecs` はこの記述より長く生存させること。
    fn dec_description(
        codecs: &mut [sys::mfxDecoderDescription_decoder],
    ) -> sys::mfxDecoderDescription {
        sys::mfxDecoderDescription {
            Version: sys::mfxStructVersion { Version: 0 },
            reserved: [0; 7],
            NumCodecs: codecs.len() as u16,
            Codecs: codecs.as_mut_ptr(),
        }
    }

    /// テスト用のエンコーダのメモリ記述を作る
    ///
    /// `color_formats` はこの構造体より長く生存させること。
    fn enc_mem_desc(
        mem_handle_type: sys::mfxResourceType,
        width: sys::mfxRange32U,
        height: sys::mfxRange32U,
        color_formats: &mut [u32],
    ) -> sys::mfxEncoderDescription_encoder_encprofile_encmemdesc {
        sys::mfxEncoderDescription_encoder_encprofile_encmemdesc {
            MemHandleType: mem_handle_type,
            Width: width,
            Height: height,
            reserved: [0; 7],
            NumColorFormats: color_formats.len() as u16,
            ColorFormats: color_formats.as_mut_ptr(),
        }
    }

    /// テスト用のエンコーダのプロファイル記述を作る
    ///
    /// `mem_descs` はこの構造体より長く生存させること。
    fn enc_profile(
        profile: u32,
        mem_descs: &mut [sys::mfxEncoderDescription_encoder_encprofile_encmemdesc],
    ) -> sys::mfxEncoderDescription_encoder_encprofile {
        sys::mfxEncoderDescription_encoder_encprofile {
            Profile: profile,
            reserved: [0; 7],
            NumMemTypes: mem_descs.len() as u16,
            MemDesc: mem_descs.as_mut_ptr(),
        }
    }

    /// テスト用のエンコーダ記述を作る
    ///
    /// `profiles` はこの記述より長く生存させること。
    fn enc_codec(
        codec_id: u32,
        max_codec_level: u16,
        bi_directional_prediction: u16,
        profiles: &mut [sys::mfxEncoderDescription_encoder_encprofile],
    ) -> sys::mfxEncoderDescription_encoder {
        sys::mfxEncoderDescription_encoder {
            CodecID: codec_id,
            MaxcodecLevel: max_codec_level,
            BiDirectionalPrediction: bi_directional_prediction,
            reserved: [0; 7],
            NumProfiles: profiles.len() as u16,
            Profiles: profiles.as_mut_ptr(),
        }
    }

    /// テスト用のエンコーダ記述の一覧を作る
    ///
    /// `codecs` はこの記述より長く生存させること。
    fn enc_description(
        codecs: &mut [sys::mfxEncoderDescription_encoder],
    ) -> sys::mfxEncoderDescription {
        sys::mfxEncoderDescription {
            Version: sys::mfxStructVersion { Version: 0 },
            reserved: [0; 7],
            NumCodecs: codecs.len() as u16,
            Codecs: codecs.as_mut_ptr(),
        }
    }

    /// デコーダのプロファイル・メモリタイプごとの値が VPL の記述どおりに返ること
    #[test]
    fn test_probe_decoding_returns_per_profile_capabilities() {
        // 未知の FourCC だけが FrameFormat で表現できないため無視される
        let mut formats0 = [
            sys::MFX_FOURCC_NV12,
            sys::MFX_FOURCC_P010,
            sys::MFX_FOURCC_AYUV,
            0xDEAD_BEEF,
        ];
        let mut formats1 = [sys::MFX_FOURCC_NV12, sys::MFX_FOURCC_YUY2];
        // VPL の ColorFormats の順序を維持することを確認する
        let mut formats2 = [
            sys::MFX_FOURCC_RGB4,
            sys::MFX_FOURCC_YUY2,
            sys::MFX_FOURCC_NV12,
        ];

        let mut mem_descs0 = [
            dec_mem_desc(
                sys::mfxResourceType_MFX_RESOURCE_SYSTEM_SURFACE,
                range(64, 1920, 16),
                range(64, 1080, 16),
                &mut formats0,
            ),
            dec_mem_desc(
                sys::mfxResourceType_MFX_RESOURCE_VA_SURFACE,
                range(1, 8192, 1),
                range(1, 8192, 1),
                &mut formats1,
            ),
        ];
        let mut mem_descs1 = [dec_mem_desc(
            sys::mfxResourceType_MFX_RESOURCE_SYSTEM_SURFACE,
            range(128, 4096, 8),
            range(96, 2160, 8),
            &mut formats2,
        )];

        let mut profiles = [
            dec_profile(sys::MFX_PROFILE_AVC_HIGH, &mut mem_descs0),
            dec_profile(sys::MFX_PROFILE_AVC_MAIN, &mut mem_descs1),
        ];
        let mut codecs = [dec_codec(sys::MFX_CODEC_AVC, 52, &mut profiles)];
        let dec = dec_description(&mut codecs);

        let info = unsafe { probe_decoding(&dec, VideoCodecType::H264) }
            .expect("H.264 のデコード情報が取得できない");

        assert_eq!(
            info.max_codec_level,
            Some(52),
            "最大コーデックレベルが正しくない: {info:?}"
        );
        assert_eq!(
            info.profiles.len(),
            2,
            "プロファイル数が正しくない: {info:?}"
        );
        assert_eq!(
            info.profiles[0].profile,
            VideoProfile::H264(H264Profile::High),
            "プロファイルが正しくない: {info:?}"
        );
        assert_eq!(
            info.profiles[0].memory_types.len(),
            2,
            "メモリタイプ数が正しくない: {info:?}"
        );
        assert_eq!(
            info.profiles[0].memory_types[0].memory_type,
            MemoryType::System,
            "メモリタイプが正しくない: {info:?}"
        );
        assert_eq!(
            info.profiles[0].memory_types[0].width,
            ValueRange {
                min: 64,
                max: 1920,
                step: 16
            },
            "幅の範囲が正しくない: {info:?}"
        );
        assert_eq!(
            info.profiles[0].memory_types[0].height,
            ValueRange {
                min: 64,
                max: 1080,
                step: 16
            },
            "高さの範囲が正しくない: {info:?}"
        );
        assert_eq!(
            info.profiles[0].memory_types[0].color_formats,
            vec![FrameFormat::Nv12, FrameFormat::P010, FrameFormat::Ayuv],
            "色形式が正しくない: {info:?}"
        );
        assert_eq!(
            info.profiles[0].memory_types[1].memory_type,
            MemoryType::VaSurface,
            "メモリタイプが正しくない: {info:?}"
        );
        assert_eq!(
            info.profiles[0].memory_types[1].width,
            ValueRange {
                min: 1,
                max: 8192,
                step: 1
            },
            "幅の範囲が正しくない: {info:?}"
        );
        assert_eq!(
            info.profiles[0].memory_types[1].color_formats,
            vec![FrameFormat::Nv12, FrameFormat::Yuy2],
            "色形式が正しくない: {info:?}"
        );
        assert_eq!(
            info.profiles[1].profile,
            VideoProfile::H264(H264Profile::Main),
            "プロファイルが正しくない: {info:?}"
        );
        assert_eq!(
            info.profiles[1].memory_types[0].width,
            ValueRange {
                min: 128,
                max: 4096,
                step: 8
            },
            "幅の範囲が正しくない: {info:?}"
        );
        assert_eq!(
            info.profiles[1].memory_types[0].height,
            ValueRange {
                min: 96,
                max: 2160,
                step: 8
            },
            "高さの範囲が正しくない: {info:?}"
        );
        assert_eq!(
            info.profiles[1].memory_types[0].color_formats,
            vec![FrameFormat::Bgra, FrameFormat::Yuy2, FrameFormat::Nv12],
            "色形式の順序が維持されていない: {info:?}"
        );
    }

    /// 未知のプロファイルとメモリタイプが生の値のまま返ること
    #[test]
    fn test_probe_decoding_preserves_unknown_profile_and_memory_type() {
        let mut formats = [sys::MFX_FOURCC_NV12];
        let mut mem_descs = [dec_mem_desc(
            99,
            range(1, 2, 1),
            range(3, 4, 1),
            &mut formats,
        )];
        let mut profiles = [dec_profile(999, &mut mem_descs)];
        let mut codecs = [dec_codec(sys::MFX_CODEC_AVC, 52, &mut profiles)];
        let dec = dec_description(&mut codecs);

        let info = unsafe { probe_decoding(&dec, VideoCodecType::H264) }
            .expect("H.264 のデコード情報が取得できない");

        assert_eq!(
            info.profiles.len(),
            1,
            "プロファイル数が正しくない: {info:?}"
        );
        assert_eq!(
            info.profiles[0].memory_types.len(),
            1,
            "メモリタイプ数が正しくない: {info:?}"
        );
        assert_eq!(
            info.profiles[0].profile,
            VideoProfile::Unknown(999),
            "未知のプロファイルが保持されていない: {info:?}"
        );
        assert_eq!(
            info.profiles[0].memory_types[0].memory_type,
            MemoryType::Unknown(99),
            "未知のメモリタイプが保持されていない: {info:?}"
        );
        assert_eq!(
            info.profiles[0].memory_types[0].width,
            ValueRange {
                min: 1,
                max: 2,
                step: 1
            },
            "幅の範囲が正しくない: {info:?}"
        );
        assert_eq!(
            info.profiles[0].memory_types[0].height,
            ValueRange {
                min: 3,
                max: 4,
                step: 1
            },
            "高さの範囲が正しくない: {info:?}"
        );
        assert_eq!(
            info.profiles[0].memory_types[0].color_formats,
            vec![FrameFormat::Nv12],
            "色形式が正しくない: {info:?}"
        );
    }

    /// デコーダのプロファイルが空の場合にプロファイル一覧が空になること
    #[test]
    fn test_probe_decoding_without_profiles_returns_empty_profiles() {
        let mut profiles = [];
        let mut codecs = [dec_codec(sys::MFX_CODEC_AVC, 52, &mut profiles)];
        let dec = dec_description(&mut codecs);

        let info = unsafe { probe_decoding(&dec, VideoCodecType::H264) }
            .expect("H.264 のデコード情報が取得できない");

        assert_eq!(
            info.max_codec_level,
            Some(52),
            "コーデックエントリの最大レベルが使われていない: {info:?}"
        );
        assert!(
            info.profiles.is_empty(),
            "プロファイルが空ではない: {info:?}"
        );
    }

    /// デコーダのメモリタイプが空の場合にメモリタイプ一覧が空になること
    #[test]
    fn test_probe_decoding_without_memory_types_returns_empty_memory_types() {
        let mut mem_descs = [];
        let mut profiles = [dec_profile(sys::MFX_PROFILE_AVC_HIGH, &mut mem_descs)];
        let mut codecs = [dec_codec(sys::MFX_CODEC_AVC, 52, &mut profiles)];
        let dec = dec_description(&mut codecs);

        let info = unsafe { probe_decoding(&dec, VideoCodecType::H264) }
            .expect("H.264 のデコード情報が取得できない");

        assert_eq!(
            info.profiles.len(),
            1,
            "プロファイル数が正しくない: {info:?}"
        );
        assert!(
            info.profiles[0].memory_types.is_empty(),
            "メモリタイプが空ではない: {info:?}"
        );
    }

    /// コーデック一覧が空の場合に `None` が返ること
    #[test]
    fn test_probe_decoding_empty_codecs_returns_none() {
        let mut codecs = [];
        let dec = dec_description(&mut codecs);

        let info = unsafe { probe_decoding(&dec, VideoCodecType::H264) };

        assert!(info.is_none(), "非対応になっていない: {info:?}");
    }

    /// 対象コーデックが一覧に無い場合に `None` が返ること
    #[test]
    fn test_probe_decoding_mismatched_codec_returns_none() {
        let mut profiles = [];
        let mut codecs = [dec_codec(sys::MFX_CODEC_HEVC, 52, &mut profiles)];
        let dec = dec_description(&mut codecs);

        let info = unsafe { probe_decoding(&dec, VideoCodecType::H264) };

        assert!(info.is_none(), "非対応になっていない: {info:?}");
    }

    /// 色形式の個数が 0、またはポインタが null の場合に空になること
    #[test]
    fn test_probe_decoding_without_color_formats_returns_empty() {
        let mut mem_descs = [
            sys::mfxDecoderDescription_decoder_decprofile_decmemdesc {
                MemHandleType: sys::mfxResourceType_MFX_RESOURCE_SYSTEM_SURFACE,
                Width: range(1, 2, 1),
                Height: range(1, 2, 1),
                reserved: [0; 7],
                NumColorFormats: 0,
                ColorFormats: std::ptr::null_mut(),
            },
            sys::mfxDecoderDescription_decoder_decprofile_decmemdesc {
                MemHandleType: sys::mfxResourceType_MFX_RESOURCE_VA_SURFACE,
                Width: range(1, 2, 1),
                Height: range(1, 2, 1),
                reserved: [0; 7],
                NumColorFormats: 1,
                ColorFormats: std::ptr::null_mut(),
            },
        ];
        let mut profiles = [dec_profile(sys::MFX_PROFILE_AVC_HIGH, &mut mem_descs)];
        let mut codecs = [dec_codec(sys::MFX_CODEC_AVC, 52, &mut profiles)];
        let dec = dec_description(&mut codecs);

        let info = unsafe { probe_decoding(&dec, VideoCodecType::H264) }
            .expect("H.264 のデコード情報が取得できない");

        assert_eq!(
            info.profiles.len(),
            1,
            "プロファイル数が正しくない: {info:?}"
        );
        assert_eq!(
            info.profiles[0].memory_types.len(),
            2,
            "メモリタイプ数が正しくない: {info:?}"
        );
        assert!(
            info.profiles[0].memory_types[0].color_formats.is_empty(),
            "色形式が空ではない: {info:?}"
        );
        assert!(
            info.profiles[0].memory_types[1].color_formats.is_empty(),
            "色形式が空ではない: {info:?}"
        );
    }

    /// エンコーダのプロファイル・メモリタイプごとの値が VPL の記述どおりに返ること
    #[test]
    fn test_probe_encoding_returns_per_profile_capabilities() {
        // 未知の FourCC だけが FrameFormat で表現できないため無視される
        let mut formats0 = [
            sys::MFX_FOURCC_NV12,
            sys::MFX_FOURCC_P010,
            sys::MFX_FOURCC_AYUV,
            0xDEAD_BEEF,
        ];
        let mut formats1 = [sys::MFX_FOURCC_AYUV];
        let mut formats2 = [
            sys::MFX_FOURCC_RGB4,
            sys::MFX_FOURCC_YUY2,
            sys::MFX_FOURCC_NV12,
        ];

        let mut mem_descs0 = [
            enc_mem_desc(
                sys::mfxResourceType_MFX_RESOURCE_SYSTEM_SURFACE,
                range(16, 4096, 16),
                range(16, 4096, 16),
                &mut formats0,
            ),
            enc_mem_desc(
                sys::mfxResourceType_MFX_RESOURCE_VA_SURFACE,
                range(1, 16384, 1),
                range(1, 16384, 1),
                &mut formats1,
            ),
        ];
        let mut mem_descs1 = [enc_mem_desc(
            sys::mfxResourceType_MFX_RESOURCE_SYSTEM_SURFACE,
            range(128, 8192, 8),
            range(128, 8192, 8),
            &mut formats2,
        )];

        let mut profiles = [
            enc_profile(sys::MFX_PROFILE_AVC_HIGH, &mut mem_descs0),
            enc_profile(sys::MFX_PROFILE_AVC_MAIN, &mut mem_descs1),
        ];
        let mut codecs = [enc_codec(sys::MFX_CODEC_AVC, 52, 1, &mut profiles)];
        let enc = enc_description(&mut codecs);

        let info = unsafe { probe_encoding(&enc, VideoCodecType::H264) }
            .expect("H.264 のエンコード情報が取得できない");

        assert!(
            info.supports_frame_reordering,
            "B フレーム対応が true になっていない: {info:?}"
        );
        assert_eq!(
            info.max_codec_level,
            Some(52),
            "最大コーデックレベルが正しくない: {info:?}"
        );
        assert_eq!(
            info.profiles.len(),
            2,
            "プロファイル数が正しくない: {info:?}"
        );
        assert_eq!(
            info.profiles[0].profile,
            VideoProfile::H264(H264Profile::High),
            "プロファイルが正しくない: {info:?}"
        );
        assert_eq!(
            info.profiles[0].memory_types[0].memory_type,
            MemoryType::System,
            "メモリタイプが正しくない: {info:?}"
        );
        assert_eq!(
            info.profiles[0].memory_types[0].width,
            ValueRange {
                min: 16,
                max: 4096,
                step: 16
            },
            "幅の範囲が正しくない: {info:?}"
        );
        assert_eq!(
            info.profiles[0].memory_types[0].height,
            ValueRange {
                min: 16,
                max: 4096,
                step: 16
            },
            "高さの範囲が正しくない: {info:?}"
        );
        assert_eq!(
            info.profiles[0].memory_types[0].color_formats,
            vec![FrameFormat::Nv12, FrameFormat::P010, FrameFormat::Ayuv],
            "色形式が正しくない: {info:?}"
        );
        assert_eq!(
            info.profiles[0].memory_types[1].memory_type,
            MemoryType::VaSurface,
            "メモリタイプが正しくない: {info:?}"
        );
        assert_eq!(
            info.profiles[0].memory_types[1].color_formats,
            vec![FrameFormat::Ayuv],
            "色形式が正しくない: {info:?}"
        );
        assert_eq!(
            info.profiles[1].profile,
            VideoProfile::H264(H264Profile::Main),
            "プロファイルが正しくない: {info:?}"
        );
        assert_eq!(
            info.profiles[1].memory_types[0].width,
            ValueRange {
                min: 128,
                max: 8192,
                step: 8
            },
            "幅の範囲が正しくない: {info:?}"
        );
        assert_eq!(
            info.profiles[1].memory_types[0].color_formats,
            vec![FrameFormat::Bgra, FrameFormat::Yuy2, FrameFormat::Nv12],
            "色形式の順序が維持されていない: {info:?}"
        );
    }

    /// エンコーダのプロファイルが空の場合にプロファイル一覧が空になること
    #[test]
    fn test_probe_encoding_without_profiles_returns_empty_profiles() {
        let mut profiles = [];
        let mut codecs = [enc_codec(sys::MFX_CODEC_AVC, 52, 0, &mut profiles)];
        let enc = enc_description(&mut codecs);

        let info = unsafe { probe_encoding(&enc, VideoCodecType::H264) }
            .expect("H.264 のエンコード情報が取得できない");

        assert!(
            !info.supports_frame_reordering,
            "B フレーム対応が true になっている: {info:?}"
        );
        assert_eq!(
            info.max_codec_level,
            Some(52),
            "コーデックエントリの最大レベルが使われていない: {info:?}"
        );
        assert!(
            info.profiles.is_empty(),
            "プロファイルが空ではない: {info:?}"
        );
    }

    /// コーデック一覧が空の場合に `None` が返ること
    #[test]
    fn test_probe_encoding_empty_codecs_returns_none() {
        let mut codecs = [];
        let enc = enc_description(&mut codecs);

        let info = unsafe { probe_encoding(&enc, VideoCodecType::H264) };

        assert!(info.is_none(), "非対応になっていない: {info:?}");
    }

    /// 対象コーデックが一覧に無い場合に `None` が返ること
    #[test]
    fn test_probe_encoding_mismatched_codec_returns_none() {
        let mut profiles = [];
        let mut codecs = [enc_codec(sys::MFX_CODEC_HEVC, 52, 0, &mut profiles)];
        let enc = enc_description(&mut codecs);

        let info = unsafe { probe_encoding(&enc, VideoCodecType::H264) };

        assert!(info.is_none(), "非対応になっていない: {info:?}");
    }

    /// 最大コーデックレベルが 0 の場合に `None` になること
    #[test]
    fn test_probe_zero_max_codec_level_returns_none() {
        let mut profiles = [];
        let mut codecs = [dec_codec(sys::MFX_CODEC_AVC, 0, &mut profiles)];
        let dec = dec_description(&mut codecs);

        let decoding = unsafe { probe_decoding(&dec, VideoCodecType::H264) }
            .expect("H.264 のデコード情報が取得できない");
        assert_eq!(
            decoding.max_codec_level, None,
            "デコードの max_codec_level が None ではない: {decoding:?}"
        );

        let mut enc_profiles = [];
        let mut enc_codecs = [enc_codec(sys::MFX_CODEC_AVC, 0, 0, &mut enc_profiles)];
        let enc = enc_description(&mut enc_codecs);

        let encoding = unsafe { probe_encoding(&enc, VideoCodecType::H264) }
            .expect("H.264 のエンコード情報が取得できない");
        assert_eq!(
            encoding.max_codec_level, None,
            "エンコードの max_codec_level が None ではない: {encoding:?}"
        );
    }
}
