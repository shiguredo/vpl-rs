use std::collections::HashMap;
use std::sync::{Arc, mpsc};
use std::thread;

use crate::codec_info::{
    Av1Profile, FrameData, FrameFormat, H264Profile, HevcProfile, VideoCodecType, Vp9Profile,
};
use crate::stats::{Counter, Gauge};
use crate::vpl::{FrameSurface, Session, VplLibrary, frame_data_pitch};
use crate::{AdapterSelector, Error, sys};

/// H.264 エンコーダ固有の設定
#[derive(Debug, Clone)]
pub struct H264EncoderConfig {
    /// プロファイル（None の場合はデフォルト）
    pub profile: Option<H264Profile>,
}

/// HEVC エンコーダ固有の設定
#[derive(Debug, Clone)]
pub struct HevcEncoderConfig {
    /// プロファイル（None の場合はデフォルト）
    pub profile: Option<HevcProfile>,
}

/// VP9 エンコーダ固有の設定
#[derive(Debug, Clone)]
pub struct Vp9EncoderConfig {
    /// プロファイル（None の場合はデフォルト）
    pub profile: Option<Vp9Profile>,
    /// IVF ヘッダーを出力するかどうか
    ///
    /// true の場合は IVF ヘッダー付き、false の場合は raw VP9 を出力する。
    pub write_ivf_headers: bool,
}

/// AV1 エンコーダ固有の設定
#[derive(Debug, Clone)]
pub struct Av1EncoderConfig {
    /// プロファイル（None の場合はデフォルト）
    pub profile: Option<Av1Profile>,
}

/// コーデック設定
#[derive(Debug, Clone)]
pub enum CodecConfig {
    /// H.264/AVC
    H264(H264EncoderConfig),
    /// H.265/HEVC
    Hevc(HevcEncoderConfig),
    /// VP9
    Vp9(Vp9EncoderConfig),
    /// AV1
    Av1(Av1EncoderConfig),
}

impl FrameFormat {
    /// フレームデータをコピーする
    ///
    /// `frame` の形式と各プレーンの `pitch` / データ長、`data` のピッチが不正な場合はエラーを返す。
    ///
    /// # Safety
    ///
    /// `data` の各プレーンポインタは有効なメモリを指し、Pitch 幅分の書き込みが可能である必要がある。
    unsafe fn copy_to_surface_planes(
        self,
        frame: FrameData<'_>,
        data: &sys::mfxFrameData,
        width: usize,
        height: usize,
    ) -> Result<(), Error> {
        let row_bytes = self.bytes_per_row(width).ok_or_else(|| {
            Error::new_custom("Encoder::encode", "row bytes calculation overflowed")
        })?;
        unsafe {
            let dst_pitch = frame_data_pitch(data);
            if dst_pitch < row_bytes {
                return Err(Error::new_custom_owned(
                    "Encoder::encode",
                    format!(
                        "destination pitch ({dst_pitch}) is less than bytes per row ({row_bytes})"
                    ),
                ));
            }
            match (self, frame) {
                (
                    FrameFormat::Nv12 | FrameFormat::P010,
                    FrameData::SemiPlanar {
                        y,
                        y_pitch,
                        uv,
                        uv_pitch,
                    },
                ) => {
                    validate_plane(y, y_pitch, height, row_bytes, "Y")?;
                    validate_plane(uv, uv_pitch, height / 2, row_bytes, "UV")?;
                    let y_ptr = data.__bindgen_anon_3.Y;
                    let uv_ptr = data.__bindgen_anon_4.UV;
                    // Y プレーンをコピーする
                    copy_rows(y.as_ptr(), y_ptr, row_bytes, height, y_pitch, dst_pitch);
                    // UV プレーンをコピーする
                    copy_rows(
                        uv.as_ptr(),
                        uv_ptr,
                        row_bytes,
                        height / 2,
                        uv_pitch,
                        dst_pitch,
                    );
                }
                (FrameFormat::Yuy2, FrameData::Packed { data: src, pitch }) => {
                    validate_plane(src, pitch, height, row_bytes, "packed frame")?;
                    let y_ptr = data.__bindgen_anon_3.Y;
                    copy_rows(src.as_ptr(), y_ptr, row_bytes, height, pitch, dst_pitch);
                }
                (FrameFormat::Bgra, FrameData::Packed { data: src, pitch }) => {
                    validate_plane(src, pitch, height, row_bytes, "packed frame")?;
                    let b_ptr = data.__bindgen_anon_5.B;
                    copy_rows(src.as_ptr(), b_ptr, row_bytes, height, pitch, dst_pitch);
                }
                (FrameFormat::Ayuv, FrameData::Packed { data: src, pitch }) => {
                    validate_plane(src, pitch, height, row_bytes, "packed frame")?;
                    // AYUV の各チャネルポインタは、パックされた 1 画素の各バイトを指す。
                    // メモリ上のバイト順は V, U, Y, A なので、行の先頭は V になる。
                    let v_ptr = data.__bindgen_anon_5.V;
                    copy_rows(src.as_ptr(), v_ptr, row_bytes, height, pitch, dst_pitch);
                }
                (FrameFormat::Y410, FrameData::Packed { data: src, pitch }) => {
                    validate_plane(src, pitch, height, row_bytes, "packed frame")?;
                    let y410_ptr = data.__bindgen_anon_4.Y410 as *mut u8;
                    copy_rows(src.as_ptr(), y410_ptr, row_bytes, height, pitch, dst_pitch);
                }
                (format, _) => {
                    return Err(Error::new_custom_owned(
                        "Encoder::encode",
                        format!("frame data variant does not match frame format {format:?}"),
                    ));
                }
            }
        }
        Ok(())
    }

    /// 指定幅のフレームの 1 行あたりの最小バイト数を返す
    ///
    /// オーバーフロー時は `None` を返す。
    pub fn bytes_per_row(self, width: usize) -> Option<usize> {
        match self {
            // 8bit は 1 サンプル 1 バイト (Y プレーン、UV プレーンとも行あたりのバイト数は幅と同じ)
            FrameFormat::Nv12 => Some(width),
            // 1 画素あたり 2 バイト (P010 は 10bit サンプルを 16bit ワード、Yuy2 は 8bit サンプルをパック)
            FrameFormat::P010 | FrameFormat::Yuy2 => width.checked_mul(2),
            // 1 画素 4 バイト
            FrameFormat::Bgra | FrameFormat::Ayuv | FrameFormat::Y410 => width.checked_mul(4),
        }
    }

    /// `mfxFrameInfo.Shift` に設定する値を返す
    ///
    /// VPL の仕様では、10bit サンプルを 16bit ワードに格納する形式について、Shift が 0 の場合は
    /// 有効データが下位 10bit に、0 以外の場合は上位 10bit にあることを表す。
    /// 10bit サンプルを 16bit ワードの上位ビットに格納する P010 だけ 1 を返す。
    /// 10bit サンプルが 10bit 幅のフィールドにそのまま入る Y410 と、8bit の各形式は 0 を返す。
    ///
    /// ref: libvpl api/vpl/mfxstructures.h (mfxFrameInfo::Shift)
    pub(crate) fn frame_info_shift(self) -> u16 {
        match self {
            FrameFormat::P010 => 1,
            FrameFormat::Nv12
            | FrameFormat::Yuy2
            | FrameFormat::Bgra
            | FrameFormat::Ayuv
            | FrameFormat::Y410 => 0,
        }
    }
}

/// コピー元の行を 1 行ずつコピー先のピッチに従ってコピーする
///
/// # Safety
///
/// `src` は `rows` 行分（最終行は `row_bytes` バイト）読み出せ、`dst` も同様に
/// 書き込める必要がある。`rows` が 0 の場合はアクセスしない。
unsafe fn copy_rows(
    src: *const u8,
    dst: *mut u8,
    row_bytes: usize,
    rows: usize,
    src_pitch: usize,
    dst_pitch: usize,
) {
    for row in 0..rows {
        unsafe {
            std::ptr::copy_nonoverlapping(
                src.add(row * src_pitch),
                dst.add(row * dst_pitch),
                row_bytes,
            );
        }
    }
}

/// レート制御モード
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RateControlMode {
    /// 固定ビットレート
    Cbr,
    /// 可変ビットレート
    Vbr,
    /// 固定量子化パラメータ
    Cqp,
    /// Intelligent Constant Quality（品質値のみで制御）
    Icq,
    /// Quality VBR（品質ベースの可変ビットレート）
    Qvbr,
    /// Look Ahead（先読みによる品質向上）
    La,
    /// Average VBR
    Avbr,
    /// Video Conferencing Mode
    Vcm,
    /// Look Ahead + ICQ
    LaIcq,
    /// Look Ahead + HRD 準拠
    LaHrd,
}

impl RateControlMode {
    fn to_mfx(self) -> u16 {
        match self {
            RateControlMode::Cbr => sys::MFX_RATECONTROL_CBR as u16,
            RateControlMode::Vbr => sys::MFX_RATECONTROL_VBR as u16,
            RateControlMode::Cqp => sys::MFX_RATECONTROL_CQP as u16,
            RateControlMode::Icq => sys::MFX_RATECONTROL_ICQ as u16,
            RateControlMode::Qvbr => sys::MFX_RATECONTROL_QVBR as u16,
            RateControlMode::La => sys::MFX_RATECONTROL_LA as u16,
            RateControlMode::Avbr => sys::MFX_RATECONTROL_AVBR as u16,
            RateControlMode::Vcm => sys::MFX_RATECONTROL_VCM as u16,
            RateControlMode::LaIcq => sys::MFX_RATECONTROL_LA_ICQ as u16,
            RateControlMode::LaHrd => sys::MFX_RATECONTROL_LA_HRD as u16,
        }
    }
}

/// エンコーダ設定
///
/// ほとんどのフィールドは VPL の mfxVideoParam / mfxInfoMFX / mfxFrameInfo に対応する。
/// フィールド名は VPL API の構造体メンバ名に準拠する。
///
/// ただし `width` / `height` は coded サイズではなく表示サイズ（crop サイズ）を表す。
/// coded サイズは [`Encoder::coded_size`] で取得する。
#[derive(Debug, Clone)]
pub struct EncoderConfig {
    // --- アダプタ選択 ---
    /// 使用する Intel HW アダプタの指定（DRM render node 番号など）
    pub adapter: AdapterSelector,

    // --- コーデック (mfxInfoMFX.CodecId / CodecProfile) ---
    /// コーデック設定
    pub codec: CodecConfig,

    // --- フレーム情報 (mfxFrameInfo) ---
    /// 表示フレーム幅（ピクセル）
    ///
    /// この値は入力フレームフォーマットが NV12 / P010 / YUY2 の場合は偶数であること。
    pub width: u32,
    /// 表示フレーム高さ（ピクセル）
    ///
    /// この値は入力フレームフォーマットが NV12 / P010 の場合は偶数であること。
    pub height: u32,
    /// 入力フレームフォーマット（mfxFrameInfo.FourCC / ChromaFormat / BitDepth / Shift を決定する）
    pub frame_format: FrameFormat,
    /// フレームレートの分子（mfxFrameInfo.FrameRateExtN）
    pub framerate_num: u32,
    /// フレームレートの分母（mfxFrameInfo.FrameRateExtD）
    pub framerate_den: u32,
    /// サンプルアスペクト比の幅（mfxFrameInfo.AspectRatioW）
    ///
    /// None の場合は 0（デフォルト）。
    pub aspect_ratio_w: Option<u16>,
    /// サンプルアスペクト比の高さ（mfxFrameInfo.AspectRatioH）
    ///
    /// None の場合は 0（デフォルト）。
    pub aspect_ratio_h: Option<u16>,

    // --- 非同期深度 (mfxVideoParam) ---
    /// 非同期深度（mfxVideoParam.AsyncDepth）
    ///
    /// 1 = 最小メモリだが性能が低い。4 = 高スループット寄りの推奨値。
    /// None の場合は 4（推奨値）を使用する。
    pub async_depth: Option<u16>,

    // --- エンコード制御 (mfxInfoMFX) ---
    /// LowPower モード（mfxInfoMFX.LowPower）
    ///
    /// true で VDENC 固定機能パイプラインを使用する。None の場合はエンコーダのデフォルト。
    pub low_power: Option<bool>,
    /// BRC パラメータ乗数（mfxInfoMFX.BRCParamMultiplier）
    ///
    /// 0 以外の場合、InitialDelayInKB / BufferSizeInKB / TargetKbps / MaxKbps に
    /// この値を乗じた値が実際の BRC パラメータとなる。None の場合は 0。
    pub brc_param_multiplier: Option<u16>,
    /// ターゲット品質（mfxInfoMFX.TargetUsage、1-7）
    ///
    /// 1 = 最高品質、4 = バランス、7 = 最高速。None の場合は 4（MFX_TARGETUSAGE_BALANCED）。
    pub target_usage: Option<u16>,

    // --- GOP 構造 (mfxInfoMFX) ---
    /// GOP サイズ（mfxInfoMFX.GopPicSize）
    ///
    /// 0 の場合はエンコーダのデフォルト。None の場合は 0。
    pub gop_pic_size: Option<u16>,
    /// GOP 参照距離（mfxInfoMFX.GopRefDist）
    ///
    /// 1 = B フレームなし、2 = 1 B フレーム、3 = 2 B フレーム。
    /// None の場合は 1（B フレームなし）。
    pub gop_ref_dist: Option<u16>,
    /// GOP オプションフラグ（mfxInfoMFX.GopOptFlag）
    ///
    /// MFX_GOP_CLOSED (1) と MFX_GOP_STRICT (2) の OR。None の場合は 0。
    pub gop_opt_flag: Option<u16>,
    /// IDR フレーム間隔（mfxInfoMFX.IdrInterval）
    ///
    /// I フレーム数単位で IDR フレームの挿入間隔を指定する。
    /// AVC: 0 = 毎 I フレームが IDR。HEVC: 0 = 最初の I フレームのみ IDR。
    /// None の場合は 0。
    pub idr_interval: Option<u16>,

    // --- レート制御 (mfxInfoMFX) ---
    /// レート制御モード（mfxInfoMFX.RateControlMethod）
    pub rate_control_mode: RateControlMode,
    /// VBV バッファ初期サイズ（KB 単位、mfxInfoMFX.InitialDelayInKB）
    ///
    /// CBR/VBR/VCM/QVBR/LA_HRD で使用する。
    /// QPI と union で共用されるため CQP/AVBR では使用不可。
    pub initial_delay_in_kb: Option<u16>,
    /// ビットストリームバッファサイズ（KB 単位、mfxInfoMFX.BufferSizeInKB）
    ///
    /// None の場合はエンコーダーが自動計算する。
    pub buffer_size_in_kb: Option<u16>,
    /// ターゲットビットレート（kbps、mfxInfoMFX.TargetKbps）
    ///
    /// CBR/VBR/LA/VCM/QVBR/LA_HRD/AVBR で使用する。
    /// ICQ/LA_ICQ/CQP では無視される（union の別メンバと共用）。
    pub target_kbps: Option<u16>,
    /// 最大ビットレート（kbps、mfxInfoMFX.MaxKbps）
    ///
    /// VBR/VCM/QVBR/LA_HRD で使用する。
    /// CBR/LA/AVBR/ICQ/CQP では無視される（union の別メンバと共用）。
    pub max_kbps: Option<u16>,
    /// CQP I フレーム QP 値（mfxInfoMFX.QPI）
    ///
    /// CQP モードで使用する。InitialDelayInKB と union で共用。
    pub qpi: Option<u16>,
    /// CQP P フレーム QP 値（mfxInfoMFX.QPP）
    ///
    /// CQP モードで使用する。TargetKbps と union で共用。
    pub qpp: Option<u16>,
    /// CQP B フレーム QP 値（mfxInfoMFX.QPB）
    ///
    /// CQP モードで使用する。MaxKbps と union で共用。
    pub qpb: Option<u16>,
    /// ICQ 品質値（mfxInfoMFX.ICQQuality、1-51）
    ///
    /// ICQ/LA_ICQ モードで使用する。TargetKbps と union で共用。
    pub icq_quality: Option<u16>,
    /// AVBR 精度（mfxInfoMFX.Accuracy、0.1% 単位）
    ///
    /// AVBR モードで使用する。QPI / InitialDelayInKB と union で共用。
    pub accuracy: Option<u16>,
    /// AVBR 収束期間（mfxInfoMFX.Convergence、100 フレーム単位）
    ///
    /// AVBR モードで使用する。QPB / MaxKbps と union で共用。
    pub convergence: Option<u16>,

    // --- スライス・参照フレーム (mfxInfoMFX) ---
    /// スライス数（mfxInfoMFX.NumSlice）
    ///
    /// 0 の場合はエンコーダが自動決定する。None の場合は 0。
    pub num_slice: Option<u16>,
    /// 最大参照フレーム数（mfxInfoMFX.NumRefFrame）
    ///
    /// AVC/HEVC では DPB サイズを定義する。0 の場合はエンコーダが自動決定する。
    pub num_ref_frame: Option<u16>,

    // --- 拡張バッファ (mfxExtCodingOption2 / mfxExtCodingOption3) ---
    /// Look Ahead depth（mfxExtCodingOption2.LookAheadDepth）
    ///
    /// LA/LA_ICQ/LA_HRD モードで使用する。None の場合はエンコーダのデフォルト。
    pub look_ahead_depth: Option<u16>,
    /// QVBR 品質値（mfxExtCodingOption3.QVBRQuality、1-51）
    ///
    /// QVBR モードで使用する。None の場合はエンコーダのデフォルト。
    pub qvbr_quality: Option<u16>,
}

impl EncoderConfig {
    /// 必須パラメータのみ指定して EncoderConfig を作成する
    ///
    /// オプションパラメータはすべて None (エンコーダのデフォルト) に設定される。
    #[expect(clippy::too_many_arguments)]
    pub fn new(
        adapter: AdapterSelector,
        codec: CodecConfig,
        width: u32,
        height: u32,
        frame_format: FrameFormat,
        framerate_num: u32,
        framerate_den: u32,
        rate_control_mode: RateControlMode,
    ) -> Self {
        Self {
            adapter,
            codec,
            width,
            height,
            frame_format,
            framerate_num,
            framerate_den,
            aspect_ratio_w: None,
            aspect_ratio_h: None,
            async_depth: None,
            low_power: None,
            brc_param_multiplier: None,
            target_usage: None,
            gop_pic_size: None,
            gop_ref_dist: None,
            gop_opt_flag: None,
            idr_interval: None,
            rate_control_mode,
            initial_delay_in_kb: None,
            buffer_size_in_kb: None,
            target_kbps: None,
            max_kbps: None,
            qpi: None,
            qpp: None,
            qpb: None,
            icq_quality: None,
            accuracy: None,
            convergence: None,
            num_slice: None,
            num_ref_frame: None,
            look_ahead_depth: None,
            qvbr_quality: None,
        }
    }
}

/// ExtParam として VPL に渡す拡張バッファの本体を所有する
///
/// [`ExtBuffers::ext_param`] が返すポインタは、この構造体が所有する `Box` の
/// ヒープ領域を指す。`Box` のヒープアドレスはこの構造体の move では変わらないため、
/// ポインタはこの構造体の生存期間中ずっと有効である。
struct ExtBuffers {
    /// mfxExtCodingOption2（LookAheadDepth 用）
    ext_co2: Option<Box<sys::mfxExtCodingOption2>>,
    /// mfxExtCodingOption3（QVBRQuality 用）
    ext_co3: Option<Box<sys::mfxExtCodingOption3>>,
    /// mfxExtVP9Param（WriteIVFHeaders 用）
    ext_vp9: Option<Box<sys::mfxExtVP9Param>>,
}

/// mfxExtCodingOption2 を構築する
///
/// `Header` を設定し、`LookAheadDepth` に `look_ahead_depth` を設定する。
///
/// LA / LA_ICQ / LA_HRD モードでは LookAheadDepth で先読みフレーム数を指定する。
/// ref: libvpl api/vpl/mfxstructures.h (mfxExtCodingOption2::LookAheadDepth)
fn coding_option2(look_ahead_depth: u16) -> Box<sys::mfxExtCodingOption2> {
    let mut co2: sys::mfxExtCodingOption2 = unsafe { std::mem::zeroed() };
    co2.Header.BufferId = sys::MFX_EXTBUFF_CODING_OPTION2;
    co2.Header.BufferSz = std::mem::size_of::<sys::mfxExtCodingOption2>() as u32;
    co2.LookAheadDepth = look_ahead_depth;
    Box::new(co2)
}

/// mfxExtCodingOption3 を構築する
///
/// `Header` を設定し、`QVBRQuality` に `qvbr_quality` を設定する。
///
/// QVBR モードでは QVBRQuality で品質値を指定する。
/// ref: libvpl api/vpl/mfxstructures.h (mfxExtCodingOption3::QVBRQuality)
fn coding_option3(qvbr_quality: u16) -> Box<sys::mfxExtCodingOption3> {
    let mut co3: sys::mfxExtCodingOption3 = unsafe { std::mem::zeroed() };
    co3.Header.BufferId = sys::MFX_EXTBUFF_CODING_OPTION3;
    co3.Header.BufferSz = std::mem::size_of::<sys::mfxExtCodingOption3>() as u32;
    co3.QVBRQuality = qvbr_quality;
    Box::new(co3)
}

/// mfxExtVP9Param を構築する
///
/// `Header` を設定し、`WriteIVFHeaders` に `write_ivf_headers` を設定する。
///
/// WriteIVFHeaders は CodingOptionValue (MFX_CODINGOPTION_ON/OFF) で指定する。
/// ref:
/// - libvpl api/vpl/mfxstructures.h (mfxExtVP9Param::WriteIVFHeaders)
/// - libvpl api/vpl/mfxstructures.h (CodingOptionValue)
fn vp9_param(write_ivf_headers: u16) -> Box<sys::mfxExtVP9Param> {
    let mut param: sys::mfxExtVP9Param = unsafe { std::mem::zeroed() };
    param.Header.BufferId = sys::MFX_EXTBUFF_VP9_PARAM;
    param.Header.BufferSz = std::mem::size_of::<sys::mfxExtVP9Param>() as u32;
    param.WriteIVFHeaders = write_ivf_headers;
    Box::new(param)
}

impl ExtBuffers {
    /// ExtParam として VPL に渡す拡張バッファ一式を構築する
    ///
    /// `look_ahead_depth` は mfxExtCodingOption2、`qvbr_quality` は mfxExtCodingOption3、
    /// `write_ivf_headers` は mfxExtVP9Param として構築する。None の項目は構築しない。
    fn new(
        look_ahead_depth: Option<u16>,
        qvbr_quality: Option<u16>,
        write_ivf_headers: Option<bool>,
    ) -> Self {
        Self {
            ext_co2: look_ahead_depth.map(coding_option2),
            ext_co3: qvbr_quality.map(coding_option3),
            ext_vp9: write_ivf_headers.map(|enabled| vp9_param(coding_option_value(enabled))),
        }
    }

    /// 読み戻し用に、要求したものと同じ種類のバッファをゼロ初期化して構築する
    ///
    /// `Header` だけを設定し、値は 0 のままにする。要求値が入ったバッファを読み戻しに
    /// 使うと、VPL が値を書き戻さなかった場合に要求値がそのまま残り、
    /// 読み戻せたように見えてしまう。
    fn zeroed_copy(&self) -> Self {
        Self {
            ext_co2: self.ext_co2.as_ref().map(|_| coding_option2(0)),
            ext_co3: self.ext_co3.as_ref().map(|_| coding_option3(0)),
            ext_vp9: self.ext_vp9.as_ref().map(|_| vp9_param(0)),
        }
    }

    /// 読み戻した拡張バッファを VPL の構造体のまま取り出す
    fn into_params(self) -> ExtParams {
        ExtParams {
            coding_option2: self.ext_co2.map(|buffer| *buffer),
            coding_option3: self.ext_co3.map(|buffer| *buffer),
            vp9_param: self.ext_vp9.map(|buffer| *buffer),
        }
    }

    /// VPL に渡す ExtParam のポインタ配列を組み立てる
    ///
    /// 戻り値の `Vec` は VPL の呼び出しが終わるまで保持すること。
    /// 空の場合は ExtParam を渡さない（拡張バッファを要求しない）ことを意味する。
    fn ext_param(&mut self) -> Vec<*mut sys::mfxExtBuffer> {
        let mut ext_bufs: Vec<*mut sys::mfxExtBuffer> = Vec::new();
        if let Some(co2) = self.ext_co2.as_deref_mut() {
            ext_bufs.push(co2 as *mut sys::mfxExtCodingOption2 as *mut sys::mfxExtBuffer);
        }
        if let Some(co3) = self.ext_co3.as_deref_mut() {
            ext_bufs.push(co3 as *mut sys::mfxExtCodingOption3 as *mut sys::mfxExtBuffer);
        }
        if let Some(vp9) = self.ext_vp9.as_deref_mut() {
            ext_bufs.push(vp9 as *mut sys::mfxExtVP9Param as *mut sys::mfxExtBuffer);
        }
        ext_bufs
    }
}

/// シーケンスパラメータの読み戻し用バッファの容量
///
/// VPL は `mfxExtCodingOptionSPSPPS` / `mfxExtCodingOptionVPS` のサイズフィールドを
/// 書き込み先バッファの容量として扱う。パラメータセットが収まる十分な容量を確保する。
const SEQUENCE_PARAM_BUFFER_SIZE: usize = 1024;

/// シーケンスパラメータの読み戻し用バッファを所有する
///
/// `MFXVideoENCODE_GetVideoParam` に attach した `mfxExtCodingOptionSPSPPS` /
/// `mfxExtCodingOptionVPS` のバッファポインタは、この構造体の生存期間中だけ有効である。
// `Vec` のヒープアドレスは構造体の move では変わらないため、フィールドに保持した
// ポインタは move 後も有効なままである。
struct SequenceParamBuffers {
    /// mfxExtCodingOptionSPSPPS（SPS / PPS の書き込み先を保持する）
    spspps: Box<sys::mfxExtCodingOptionSPSPPS>,
    /// mfxExtCodingOptionVPS（要求した場合のみ）
    vps_ext: Option<Box<sys::mfxExtCodingOptionVPS>>,
    /// SPS の書き込み先
    sps: Vec<u8>,
    /// PPS の書き込み先（要求しない場合は空）
    pps: Vec<u8>,
    /// VPS の書き込み先（要求しない場合は空）
    vps: Vec<u8>,
}

impl SequenceParamBuffers {
    /// 読み戻し用バッファ一式を構築する
    ///
    /// `with_vps` が true の場合は VPS 用バッファを、`with_pps` が true の場合は
    /// PPS 用バッファを構築する。
    fn new(with_vps: bool, with_pps: bool) -> Self {
        let mut sps = vec![0u8; SEQUENCE_PARAM_BUFFER_SIZE];
        // PPS を要求しない場合は読み戻し用の領域を確保しない
        let mut pps = if with_pps {
            vec![0u8; SEQUENCE_PARAM_BUFFER_SIZE]
        } else {
            Vec::new()
        };
        // VPS を要求しない場合は読み戻し用の領域を確保しない
        let mut vps = if with_vps {
            vec![0u8; SEQUENCE_PARAM_BUFFER_SIZE]
        } else {
            Vec::new()
        };

        let mut spspps: sys::mfxExtCodingOptionSPSPPS = unsafe { std::mem::zeroed() };
        spspps.Header.BufferId = sys::MFX_EXTBUFF_CODING_OPTION_SPSPPS;
        spspps.Header.BufferSz = std::mem::size_of::<sys::mfxExtCodingOptionSPSPPS>() as u32;
        spspps.SPSBuffer = sps.as_mut_ptr();
        spspps.SPSBufSize = SEQUENCE_PARAM_BUFFER_SIZE as u16;
        if with_pps {
            spspps.PPSBuffer = pps.as_mut_ptr();
            spspps.PPSBufSize = SEQUENCE_PARAM_BUFFER_SIZE as u16;
        }

        let vps_ext = if with_vps {
            let mut vps_ext: sys::mfxExtCodingOptionVPS = unsafe { std::mem::zeroed() };
            vps_ext.Header.BufferId = sys::MFX_EXTBUFF_CODING_OPTION_VPS;
            vps_ext.Header.BufferSz = std::mem::size_of::<sys::mfxExtCodingOptionVPS>() as u32;
            vps_ext.__bindgen_anon_1.VPSBuffer = vps.as_mut_ptr();
            vps_ext.VPSBufSize = SEQUENCE_PARAM_BUFFER_SIZE as u16;
            Some(Box::new(vps_ext))
        } else {
            None
        };

        Self {
            spspps: Box::new(spspps),
            vps_ext,
            sps,
            pps,
            vps,
        }
    }

    /// VPL に渡す ExtParam のポインタ配列を組み立てる
    ///
    /// 戻り値の `Vec` は VPL の呼び出しが終わるまで保持すること。
    fn ext_param(&mut self) -> Vec<*mut sys::mfxExtBuffer> {
        let mut ext_bufs: Vec<*mut sys::mfxExtBuffer> = vec![self.spspps.as_mut()
            as *mut sys::mfxExtCodingOptionSPSPPS
            as *mut sys::mfxExtBuffer];
        if let Some(vps_ext) = self.vps_ext.as_deref_mut() {
            ext_bufs.push(vps_ext as *mut sys::mfxExtCodingOptionVPS as *mut sys::mfxExtBuffer);
        }
        ext_bufs
    }
}

/// `video_param` に ExtParam を設定する
///
/// 空の場合は設定しない（空 `Vec` の `as_mut_ptr()` は dangling ポインタを返すため）。
fn attach_ext_param(video_param: &mut sys::mfxVideoParam, ext_bufs: &mut [*mut sys::mfxExtBuffer]) {
    if ext_bufs.is_empty() {
        return;
    }
    video_param.ExtParam = ext_bufs.as_mut_ptr();
    video_param.NumExtParam = ext_bufs.len() as u16;
}

/// `video_param` の ExtParam をクリアする
///
/// `video_param.ExtParam` は VPL API 呼び出しの直前だけ設定し、呼び出し直後に
/// 必ずこの関数でクリアする。
fn clear_ext_param(video_param: &mut sys::mfxVideoParam) {
    video_param.ExtParam = std::ptr::null_mut();
    video_param.NumExtParam = 0;
}

/// 読み戻したバッファからパラメータセットを 1 つ取り出す
///
/// 先頭のスタートコード (`00 00 00 01` / `00 00 01`) を除去したバイト列を返す。
/// VPL が報告したサイズが 0 の場合、確保したバッファの容量を超える場合、
/// スタートコードを除いた結果が空になる場合は `Err` を返す。`function` は
/// エラーに付ける呼び出し元の関数名である。
fn read_sequence_param(
    buffer: &[u8],
    size: u16,
    name: &str,
    function: &'static str,
) -> Result<Vec<u8>, Error> {
    let size = usize::from(size);
    if size == 0 {
        return Err(Error::new_custom_owned(
            function,
            format!("{name} is not available"),
        ));
    }
    let data = buffer.get(..size).ok_or_else(|| {
        Error::new_custom_owned(
            function,
            format!(
                "{name} size {size} exceeds the readback buffer size {}",
                buffer.len()
            ),
        )
    })?;
    let data = strip_start_code(data);
    if data.is_empty() {
        return Err(Error::new_custom_owned(
            function,
            format!("{name} is empty after removing the start code"),
        ));
    }
    Ok(data.to_vec())
}

/// NAL ユニット先頭のスタートコードを除去する
///
/// 先頭に 4 バイト (`00 00 00 01`) または 3 バイト (`00 00 01`) のスタートコードが
/// 付いている場合は除去し、付いていない場合はそのまま返す。
fn strip_start_code(data: &[u8]) -> &[u8] {
    data.strip_prefix(&[0x00, 0x00, 0x00, 0x01])
        .or_else(|| data.strip_prefix(&[0x00, 0x00, 0x01]))
        .unwrap_or(data)
}

/// エンコーダ再構成パラメータ
///
/// `Encoder::reconfigure` で動的に変更可能なパラメータ。
/// None のフィールドは変更しない。
#[derive(Debug, Clone, Default)]
pub struct ReconfigureParams {
    /// ターゲットビットレート（kbps）
    pub target_kbps: Option<u16>,
    /// 最大ビットレート（kbps）
    pub max_kbps: Option<u16>,
    /// フレームレートの分子
    pub framerate_num: Option<u32>,
    /// フレームレートの分母
    pub framerate_den: Option<u32>,
}

/// エンコードオプション
#[derive(Debug, Clone)]
pub struct EncodeOptions {
    /// フレームタイプ（mfxEncodeCtrl.FrameType に対応）
    ///
    /// 0 の場合はエンコーダが自動決定する。
    /// `frame_type` モジュールの定数をビット OR で組み合わせて指定する。
    pub frame_type: u16,
}

/// プレーンのデータ長が指定行数分あることを検証する
fn validate_plane(
    data: &[u8],
    pitch: usize,
    rows: usize,
    row_bytes: usize,
    name: &str,
) -> Result<(), Error> {
    if pitch < row_bytes {
        return Err(Error::new_custom_owned(
            "Encoder::encode",
            format!("{name} plane pitch ({pitch}) is less than bytes per row ({row_bytes})"),
        ));
    }
    let required = plane_span(rows, pitch, row_bytes).ok_or_else(|| {
        Error::new_custom_owned(
            "Encoder::encode",
            format!("{name} plane size calculation overflowed"),
        )
    })?;
    if data.len() < required {
        return Err(Error::new_custom_owned(
            "Encoder::encode",
            format!(
                "{name} plane data is too small (got {}, need at least {required})",
                data.len()
            ),
        ));
    }
    Ok(())
}

/// プレーンが必要とするバイト数を返す
///
/// 最終行の後ろのパディングは含まない。
fn plane_span(rows: usize, pitch: usize, row_bytes: usize) -> Option<usize> {
    if rows == 0 {
        return Some(0);
    }
    rows.checked_sub(1)?
        .checked_mul(pitch)?
        .checked_add(row_bytes)
}

/// ピクチャータイプ
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PictureType {
    /// IDR フレーム
    Idr,
    /// I フレーム
    I,
    /// P フレーム
    P,
    /// B フレーム
    B,
    /// 不明
    Unknown,
}

/// エンコーダーの統計値
///
/// [`Encoder::stats`] が返す統計値である。値はエンコード処理の進行に伴って
/// 更新される。
///
/// `Clone` は各フィールドの現在値を個別にコピーするスナップショットを返すため、
/// フィールド間の一貫性は保証されない。
#[derive(Debug, Clone, Default)]
pub struct EncoderStats {
    /// [`Encoder::encode`] が VPL に受理させたフレームの通算回数
    ///
    /// フレームサイズの検証など、エンコード API に渡す前に拒否した呼び出しは計上しない。
    pub total_encode_count: Counter,

    /// [`EncodeHandler::on_encoded`] に `Ok` を渡した通算回数
    pub total_output_frame_count: Counter,

    /// [`EncodeHandler::on_encoded`] に `Err` を渡した通算回数
    ///
    /// [`Encoder::finish`] と [`Encoder::reconfigure`] が残留フレームを
    /// エラー通知した場合も含む。
    pub total_error_count: Counter,

    /// [`Encoder::reconfigure`] が成功した通算回数
    pub total_reconfigure_count: Counter,

    /// [`EncodeHandler::on_encoded`] に渡した出力ビットストリームの総ビット数
    pub total_output_bit_count: Counter,

    /// 受理したが、まだ結果を通知していないフレーム数の現在値
    ///
    /// [`Encoder::encode`] がフレームを受理した時点で増え、対応する結果を
    /// [`EncodeHandler::on_encoded`] に通知する直前に減る。[`Encoder::finish`] と
    /// [`Encoder::reconfigure`] の処理が完了した時点で 0 になる。処理の途中で
    /// エラーになった場合は残留することがある。`Drop` 時に中断したフレームを
    /// 通知した場合も減る。
    pub in_flight_frames: Gauge,
}

/// エンコード済みフレーム
pub struct EncodedFrame<T> {
    data: Vec<u8>,
    timestamp: u64,
    picture_type: PictureType,
    user_data: T,
}

impl<T> EncodedFrame<T> {
    /// エンコード済みデータを取得する
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    /// エンコード済みデータを取得する（所有権を移動）
    pub fn into_data(self) -> Vec<u8> {
        self.data
    }

    /// タイムスタンプを取得する
    pub fn timestamp(&self) -> u64 {
        self.timestamp
    }

    /// ピクチャータイプを取得する
    pub fn picture_type(&self) -> PictureType {
        self.picture_type
    }

    /// エンコード時に渡したユーザーデータを取得する
    pub fn user_data(&self) -> &T {
        &self.user_data
    }

    /// エンコード時に渡したユーザーデータを取得する（所有権を移動）
    pub fn into_user_data(self) -> T {
        self.user_data
    }
}

/// エンコーダが現在使用しているシーケンスパラメータ
///
/// スタートコードを含まない NAL / OBU の生バイト列を保持する。
///
/// コーデックごとの各フィールドの内容は次のとおり。
///
/// - H.264: SPS / PPS をそれぞれ `sps` / `pps` に設定する。`vps` は `None`
/// - HEVC: VPS / SPS / PPS をそれぞれ `vps` / `sps` / `pps` に設定する。
/// - AV1: Sequence Header OBU を `sps` に設定する。`vps` と `pps` は `None`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SequenceParams {
    /// VPS (HEVC のみ)
    pub vps: Option<Vec<u8>>,
    /// SPS (AV1 の場合は Sequence Header OBU)
    pub sps: Vec<u8>,
    /// PPS (AV1 では `None`)
    pub pps: Option<Vec<u8>>,
}

/// デバイスビジー時の最大リトライ回数
const DEVICE_BUSY_MAX_RETRIES: u32 = 30;

struct PendingFrame<T> {
    presentation_timestamp: u64,
    user_data: T,
}

// Safety: PendingFrame はスレッド間で所有権を移動するだけで、同時アクセスはしない。
unsafe impl<T: Send> Send for PendingFrame<T> {}

struct SyncData {
    syncp: sys::mfxSyncPoint,
    bitstream: Box<sys::mfxBitstream>,
    bitstream_buffer: Vec<u8>,
}

// Safety: SyncData はスレッド間で所有権を移動するだけで、同時アクセスはしない。
unsafe impl Send for SyncData {}

struct SyncedBitstream {
    data: Vec<u8>,
    frame_seq: u64,
    picture_type: PictureType,
}

/// frame_seq と pending frame の対応情報を管理する構造体
struct PendingFrameStore<T> {
    by_frame_seq: HashMap<u64, PendingFrame<T>>,
}

impl<T> PendingFrameStore<T> {
    fn new() -> Self {
        Self {
            by_frame_seq: HashMap::new(),
        }
    }

    fn insert(&mut self, frame_seq: u64, pending: PendingFrame<T>) -> Option<PendingFrame<T>> {
        self.by_frame_seq.insert(frame_seq, pending)
    }

    fn take_by_frame_seq(&mut self, frame_seq: u64) -> Option<PendingFrame<T>> {
        self.by_frame_seq.remove(&frame_seq)
    }

    fn is_empty(&self) -> bool {
        self.by_frame_seq.is_empty()
    }

    fn len(&self) -> usize {
        self.by_frame_seq.len()
    }

    fn drain_all(&mut self) -> Vec<(u64, PendingFrame<T>)> {
        let mut drained = Vec::with_capacity(self.len());
        for (frame_seq, pending) in self.by_frame_seq.drain() {
            drained.push((frame_seq, pending));
        }
        drained
    }
}

enum WorkerCommand<T> {
    QueueFrame {
        frame_seq: u64,
        pending_frame: PendingFrame<T>,
    },
    Sync(SyncData),
    WaitIdle {
        reply_tx: mpsc::Sender<Result<(), Error>>,
        /// pending が残留したときのエラーに付けるコンテキスト
        function: &'static str,
    },
    Stop,
}

// Safety: WorkerCommand はスレッド間で所有権を移動するだけで、同時アクセスはしない。
unsafe impl<T: Send> Send for WorkerCommand<T> {}

/// エンコード結果を通知するためのハンドラー
///
/// エンコード処理が完了するたびに [`EncodeHandler::on_encoded`] が呼ばれる。
pub trait EncodeHandler: Send + 'static {
    /// ユーザーデータ型
    type UserData: Send + 'static;
    /// エラー型
    type Error: From<crate::Error> + Send + 'static;
    /// エンコード完了時に呼ばれる
    fn on_encoded(&mut self, result: Result<EncodedFrame<Self::UserData>, Self::Error>);
}

/// `FnMut(Result<EncodedFrame<T>, E>)` を [`EncodeHandler`] にするラッパー
pub struct FnEncodeHandler<T, E = crate::Error> {
    f: Box<dyn FnMut(Result<EncodedFrame<T>, E>) + Send + 'static>,
}

impl<T, E> FnEncodeHandler<T, E> {
    /// `FnMut(Result<EncodedFrame<T>, E>)` から [`EncodeHandler`] を構築する
    pub fn new<F>(f: F) -> Self
    where
        F: FnMut(Result<EncodedFrame<T>, E>) + Send + 'static,
    {
        Self { f: Box::new(f) }
    }
}

impl<T, E> EncodeHandler for FnEncodeHandler<T, E>
where
    T: Send + 'static,
    E: From<crate::Error> + Send + 'static,
{
    type UserData = T;
    type Error = E;
    fn on_encoded(&mut self, result: Result<EncodedFrame<T>, E>) {
        (self.f)(result);
    }
}

/// エンコーダが現在使用しているパラメータ
///
/// 基本パラメータは VPL の生の構造体、拡張バッファは VPL が書き戻した実効値を保持する。
/// いずれも要求値ではなく、VPL が報告した値である。
#[derive(Clone, Copy)]
pub struct VideoParam {
    /// 基本パラメータ
    ///
    /// `ExtParam` は常に null、`NumExtParam` は常に 0 である。
    /// VPL の共用体メンバを読む場合は `unsafe` を伴う。
    pub param: sys::mfxVideoParam,
    /// 拡張バッファの実効値
    pub ext: ExtParams,
}

/// エンコーダが現在使用している拡張バッファ
///
/// バッファを要求していない場合は `None` になる。要求した場合でも、VPL が値を
/// 書き戻さなかった場合はゼロ初期化された値のままになる。
#[derive(Clone, Copy)]
pub struct ExtParams {
    /// mfxExtCodingOption2（`LookAheadDepth`）
    pub coding_option2: Option<sys::mfxExtCodingOption2>,
    /// mfxExtCodingOption3（`QVBRQuality`）
    pub coding_option3: Option<sys::mfxExtCodingOption3>,
    /// mfxExtVP9Param（`WriteIVFHeaders`）
    pub vp9_param: Option<sys::mfxExtVP9Param>,
}

/// エンコーダ
pub struct Encoder<H: EncodeHandler> {
    session: Session,
    video_param: sys::mfxVideoParam,
    frame_info: sys::mfxFrameInfo,
    /// エンコードするコーデック種別
    codec: VideoCodecType,
    frame_format: FrameFormat,
    /// 入力バッファの表示幅 (`EncoderConfig::width`)
    width: usize,
    /// 入力バッファの表示高さ (`EncoderConfig::height`)
    height: usize,
    write_ivf_headers: bool,
    bitstream_buffer_size: usize,
    /// Init / Reset で VPL へ渡す拡張バッファの本体
    ext_buffers: ExtBuffers,
    worker_tx: mpsc::Sender<WorkerCommand<H::UserData>>,
    worker_handle: Option<thread::JoinHandle<()>>,
    frame_count: u64,
    framerate_den: u64,
    /// ワーカースレッドと共有する統計値
    stats: Arc<EncoderStats>,
}

// Safety: エンコード完了通知は専用スレッドで行い、メインスレッドは EncodeFrameAsync のみ実行する。
// VPL 仕様上、セッション操作の同一スレッド制約は明記されておらず、公式サンプルでも
// セッションハンドルにスレッドアフィニティの制約は課されていない。
// また ext_buffers が保持する mfxExtCodingOption2 / mfxExtCodingOption3 / mfxExtVP9Param は
// 生ポインタを含まない全整数フィールドの POD であり、Box<...> は自動で Send になる。
// video_param.ExtParam は VPL API 呼び出しの直前だけ設定し、呼び出し直後に必ず null に
// 戻すため、Encoder をスレッド間で move しても無効なポインタが残ることはない。
// Sync は実装しない（生ポインタにより自動的に !Sync）。
unsafe impl<H: EncodeHandler> Send for Encoder<H> {}

impl<H: EncodeHandler> Encoder<H> {
    /// テスト用に VPL セッションを返す
    ///
    /// `MFXMemory_GetSurfaceForEncode` はエンコーダー初期化後のセッションでのみ
    /// サーフェスを返すため、`FrameSurface` のテストはこのセッションを使う。
    #[cfg(test)]
    pub(crate) fn session_for_test(&self) -> &Session {
        &self.session
    }

    /// エンコーダを作成する
    ///
    /// # 入力フォーマットとプロファイル
    ///
    /// 入力フォーマットとプロファイルの組み合わせの可否は GPU と VPL の実装に依存する。
    /// 初期化に失敗した場合、[`Error::function`] は `MFXVideoENCODE_Init` になる。
    /// また、初期化に成功しても、エンコードに失敗する組み合わせがある。
    ///
    /// 事前に対応を判定する場合は [`crate::supported_codecs`] が返す
    /// [`crate::EncodingInfo::profiles`] の
    /// [`crate::MemoryTypeCapabilities::color_formats`] を参照する。
    /// ただし、ケーパビリティ照会の結果は初期化やエンコードの成功を保証しない。
    ///
    /// # エラー
    ///
    /// 以下の場合はエラーを返す。
    ///
    /// - `width` / `height` が 0
    /// - `framerate_den` が 0
    /// - 16 の倍数に切り上げた幅または高さが `u16::MAX` を超える
    /// - 入力フォーマットが NV12 / P010 であり、`width` または `height` が奇数
    /// - 入力フォーマットが YUY2 であり、`width` が奇数
    /// - VPL の関数がエラーを返す
    pub fn new(config: EncoderConfig, handler: H) -> Result<Self, Error> {
        // 寸法を u16 へ変換する前に範囲を検証する
        if config.width == 0 || config.height == 0 {
            return Err(Error::new_custom(
                "Encoder::new",
                "width and height must be non-zero",
            ));
        }
        // 4:2:0 は幅と高さ、4:2:2 は幅が偶数でなければならない
        if matches!(
            config.frame_format,
            FrameFormat::Nv12 | FrameFormat::P010 | FrameFormat::Yuy2
        ) && !config.width.is_multiple_of(2)
        {
            return Err(Error::new_custom_owned(
                "Encoder::new",
                format!(
                    "{:?} requires an even width (got {})",
                    config.frame_format, config.width
                ),
            ));
        }
        if matches!(config.frame_format, FrameFormat::Nv12 | FrameFormat::P010)
            && !config.height.is_multiple_of(2)
        {
            return Err(Error::new_custom_owned(
                "Encoder::new",
                format!(
                    "{:?} requires an even height (got {})",
                    config.frame_format, config.height
                ),
            ));
        }
        if config.framerate_den == 0 {
            return Err(Error::new_custom(
                "Encoder::new",
                "framerate_den must be non-zero",
            ));
        }
        let aligned_width = align_up(config.width, 16);
        let aligned_height = align_up(config.height, 16);
        if aligned_width > u16::MAX as u32 || aligned_height > u16::MAX as u32 {
            return Err(Error::new_custom_owned(
                "Encoder::new",
                format!(
                    "aligned width ({aligned_width}) and height ({aligned_height}) must not exceed {}",
                    u16::MAX
                ),
            ));
        }

        let lib = VplLibrary::load()?;

        // API 2.x フローで指定アダプタのセッションを作成する
        let session = lib.create_session(config.adapter)?;

        // --- Init 前のエラー → session が Drop されて MFXClose + MFXUnload が自動実行される ---

        // mfxFrameInfo を設定する
        let mut frame_info: sys::mfxFrameInfo = unsafe { std::mem::zeroed() };
        frame_info.FourCC = config.frame_format.fourcc();
        frame_info.ChromaFormat = config.frame_format.chroma_format();
        frame_info.BitDepthLuma = config.frame_format.bit_depth();
        frame_info.BitDepthChroma = config.frame_format.bit_depth();
        frame_info.Shift = config.frame_format.frame_info_shift();
        frame_info.PicStruct = sys::MFX_PICSTRUCT_PROGRESSIVE as u16;
        frame_info.FrameRateExtN = config.framerate_num;
        frame_info.FrameRateExtD = config.framerate_den;
        frame_info.AspectRatioW = config.aspect_ratio_w.unwrap_or(0);
        frame_info.AspectRatioH = config.aspect_ratio_h.unwrap_or(0);
        // 内部サーフェスの Width/Height は 16 の倍数に切り上げ、CropW/CropH は表示サイズにする
        {
            let fi = unsafe { &mut frame_info.__bindgen_anon_1.__bindgen_anon_1 };
            fi.Width = aligned_width as u16;
            fi.Height = aligned_height as u16;
            fi.CropW = config.width as u16;
            fi.CropH = config.height as u16;
        }

        let gop_ref_dist = config.gop_ref_dist.unwrap_or(1);
        let gop_pic_size = config.gop_pic_size.unwrap_or(0);
        let codec = codec_type(&config.codec);

        // 未指定時は 16 の倍数に切り上げた幅 x 高さの画素数に 4 バイトを掛け、
        // 1024 バイト単位に切り上げて KB 単位で設定する
        let pixel_count = aligned_width as u64 * aligned_height as u64;
        let auto_buffer_bytes = pixel_count.saturating_mul(4);
        let auto_buffer_kb = align_up(auto_buffer_bytes.min(u32::MAX as u64) as u32, 1024) / 1024;
        let buffer_size_in_kb: u16 = config
            .buffer_size_in_kb
            .unwrap_or(u16::try_from(auto_buffer_kb).unwrap_or(u16::MAX));

        // mfxVideoParam を設定する
        let mut video_param: sys::mfxVideoParam = unsafe { std::mem::zeroed() };
        video_param.IOPattern = sys::MFX_IOPATTERN_IN_SYSTEM_MEMORY as u16;
        // AsyncDepth は設定値またはデフォルト 4 を使用する。
        // libvpl のガイドでは、1 は最小メモリだが性能が低く、4 は高スループット寄りの推奨値とされる。
        // ref:
        // - doc/spec/source/programming_guide/VPL_prg_decoding.rst (AsyncDepth Specific Details)
        // - doc/spec/source/programming_guide/VPL_prg_transcoding.rst (Operation sequence)
        video_param.AsyncDepth = config.async_depth.unwrap_or(4);
        unsafe {
            let mfx = &mut video_param.__bindgen_anon_1.mfx;
            mfx.FrameInfo = frame_info;
            mfx.CodecId = codec.to_codec_id();
            mfx.CodecProfile = codec_profile(&config.codec);

            // LowPower (VDENC) モード
            if let Some(lp) = config.low_power {
                mfx.LowPower = if lp {
                    sys::MFX_CODINGOPTION_ON as u16
                } else {
                    sys::MFX_CODINGOPTION_OFF as u16
                };
            }

            // BRC パラメータ乗数
            if let Some(multiplier) = config.brc_param_multiplier {
                mfx.BRCParamMultiplier = multiplier;
            }

            let enc = &mut mfx.__bindgen_anon_1.__bindgen_anon_1;
            enc.TargetUsage = config
                .target_usage
                .unwrap_or(sys::MFX_TARGETUSAGE_BALANCED as u16);
            enc.GopPicSize = gop_pic_size;
            enc.GopRefDist = gop_ref_dist;
            if let Some(gop_opt_flag) = config.gop_opt_flag {
                enc.GopOptFlag = gop_opt_flag;
            }
            if let Some(idr_interval) = config.idr_interval {
                enc.IdrInterval = idr_interval;
            }
            enc.RateControlMethod = config.rate_control_mode.to_mfx();
            enc.BufferSizeInKB = buffer_size_in_kb;

            // レート制御モードに応じたパラメータを設定する
            //
            // mfxInfoMFX の union レイアウト:
            //   __bindgen_anon_1: InitialDelayInKB | QPI | Accuracy
            //   __bindgen_anon_2: TargetKbps | QPP | ICQQuality
            //   __bindgen_anon_3: MaxKbps | QPB | Convergence
            match config.rate_control_mode {
                RateControlMode::Cqp => {
                    // CQP: QPI/QPP/QPB を設定する
                    if let Some(qpi) = config.qpi {
                        enc.__bindgen_anon_1.QPI = qpi;
                    }
                    if let Some(qpp) = config.qpp {
                        enc.__bindgen_anon_2.QPP = qpp;
                    }
                    if let Some(qpb) = config.qpb {
                        enc.__bindgen_anon_3.QPB = qpb;
                    }
                }
                RateControlMode::Icq | RateControlMode::LaIcq => {
                    // ICQ/LA_ICQ: ICQQuality のみ使用する
                    // TargetKbps/MaxKbps/InitialDelayInKB は無視される
                    if let Some(quality) = config.icq_quality {
                        enc.__bindgen_anon_2.ICQQuality = quality;
                    }
                }
                RateControlMode::Avbr => {
                    // AVBR: TargetKbps + Accuracy + Convergence を使用する
                    // MaxKbps/InitialDelayInKB は使用しない（union の別メンバ）
                    enc.__bindgen_anon_2.TargetKbps = config.target_kbps.unwrap_or(2000);
                    if let Some(accuracy) = config.accuracy {
                        enc.__bindgen_anon_1.Accuracy = accuracy;
                    }
                    if let Some(convergence) = config.convergence {
                        enc.__bindgen_anon_3.Convergence = convergence;
                    }
                }
                RateControlMode::La => {
                    // LA: TargetKbps のみ使用する
                    // MaxKbps/InitialDelayInKB は無視される
                    enc.__bindgen_anon_2.TargetKbps = config.target_kbps.unwrap_or(2000);
                }
                RateControlMode::Cbr
                | RateControlMode::Vbr
                | RateControlMode::Vcm
                | RateControlMode::Qvbr
                | RateControlMode::LaHrd => {
                    // CBR/VBR/VCM/QVBR/LA_HRD:
                    //   InitialDelayInKB + TargetKbps + MaxKbps を使用する
                    if let Some(initial_delay) = config.initial_delay_in_kb {
                        enc.__bindgen_anon_1.InitialDelayInKB = initial_delay;
                    }
                    enc.__bindgen_anon_2.TargetKbps = config.target_kbps.unwrap_or(2000);
                    if let Some(max_kbps) = config.max_kbps {
                        enc.__bindgen_anon_3.MaxKbps = max_kbps;
                    }
                }
            }

            // スライス数と参照フレーム数を設定する
            if let Some(num_slice) = config.num_slice {
                enc.NumSlice = num_slice;
            }
            if let Some(num_ref_frame) = config.num_ref_frame {
                enc.NumRefFrame = num_ref_frame;
            }
        }

        // 拡張バッファの設定
        //
        // reconfigure (MFXVideoENCODE_Reset) でも Init 時と同じ拡張バッファを渡すため、
        // Box に包んだバッファを構築して Encoder に保持する。
        let write_ivf_headers_setting = vp9_write_ivf_headers(&config.codec);
        let mut ext_buffers = ExtBuffers::new(
            config.look_ahead_depth,
            config.qvbr_quality,
            write_ivf_headers_setting,
        );

        // getter が返す bool は要求値が ON かどうかで決まる
        let write_ivf_headers = write_ivf_headers_setting.unwrap_or(false);

        // Init 用の拡張バッファポインタ配列を構築する
        let mut init_ext_bufs = ext_buffers.ext_param();
        attach_ext_param(&mut video_param, &mut init_ext_bufs);

        lib.mfx_video_encode_init(session.as_ptr(), &mut video_param)?;

        let lib = session.lib();
        let session_ptr = session.as_ptr();

        // Init の呼び出しが終わったので ExtParam をクリアする
        clear_ext_param(&mut video_param);

        // 初期化後の実効 FrameInfo を読み戻す。
        // 内部サーフェスのサイズは VPL が決めるため、その値を coded_size として保持する。
        lib.mfx_video_encode_get_video_param(session_ptr, &mut video_param)
            .inspect_err(|_| {
                // GetVideoParam 失敗時は MFXVideoENCODE_Close を呼んでから session を Drop させる
                let _ = lib.mfx_video_encode_close(session_ptr);
            })?;
        frame_info = unsafe { video_param.__bindgen_anon_1.mfx.FrameInfo };

        let bitstream_buffer_size = (buffer_size_in_kb as usize) * 1024;
        let (worker_tx, worker_rx) = mpsc::channel();
        let session_handle = session_ptr as usize;
        let stats = Arc::new(EncoderStats::default());
        let worker_stats = Arc::clone(&stats);
        let worker_handle = thread::Builder::new()
            .name("vpl-encoder-sync".to_owned())
            .spawn(move || {
                run_sync_worker(lib, session_handle, worker_rx, handler, worker_stats);
            })
            .map_err(|error| {
                // スレッド生成失敗時は MFXVideoENCODE_Close を呼んでから session を Drop させる
                let _ = lib.mfx_video_encode_close(session_ptr);
                Error::new_custom_owned(
                    "Encoder::new",
                    format!("failed to spawn sync worker thread: {error}"),
                )
            })?;

        // すべて成功 → Session の所有権を Encoder に移す
        Ok(Encoder {
            session,
            video_param,
            frame_info,
            codec,
            frame_format: config.frame_format,
            width: config.width as usize,
            height: config.height as usize,
            write_ivf_headers,
            bitstream_buffer_size,
            ext_buffers,
            worker_tx,
            worker_handle: Some(worker_handle),
            frame_count: 0,
            framerate_den: config.framerate_den as u64,
            stats,
        })
    }

    /// パラメータが HW でサポートされるか事前検証する
    ///
    /// MFXVideoENCODE_Query を呼び出して、入力パラメータが HW でサポートされるかを検証する。
    /// セッション初期化済みの状態で呼び出す必要がある。
    ///
    /// # 戻り値
    ///
    /// - `Ok(())`: すべてのパラメータがサポートされている（MFX_ERR_NONE）、
    ///   または一部が修正された（MFX_WRN_INCOMPATIBLE_VIDEO_PARAM）。
    ///   修正された場合、`output` に修正後の値が入る。
    /// - `Err(Error)`: パラメータがサポートされていない。
    pub fn query(
        &self,
        input: &mut sys::mfxVideoParam,
        output: &mut sys::mfxVideoParam,
    ) -> Result<(), Error> {
        let status =
            self.session
                .lib()
                .mfx_video_encode_query(self.session.as_ptr(), input, output);
        Error::check_mfx_allow_warn(status, "MFXVideoENCODE_Query")
    }

    /// エンコーダパラメータを動的に変更する
    ///
    /// ビットレートやフレームレートの変更に使用する。
    ///
    /// この関数が `Ok` を返した場合、呼び出し時点で未出力だったフレームは、その時点で
    /// すべて `Ok` として [`EncodeHandler::on_encoded`] に通知されている。`Err` を
    /// 返した場合は、未出力フレームの一部が `Err` として通知されるか、通知されないまま
    /// 残ることがある。
    ///
    /// - frame_seq は reconfigure 後も継続し、リセットされない
    /// - この関数は、呼び出し時点までに送信したフレームのエンコード完了と、
    ///   すべてのコールバックの完了までブロックする
    /// - パラメータの検証に失敗した場合を除き、`Err` を返したときはエンコーダが
    ///   使用できない状態のまま残ることがあるため、呼び出し側は [`Encoder`] を
    ///   作り直すこと
    pub fn reconfigure(&mut self, params: ReconfigureParams) -> Result<(), Error> {
        if let Some(0) = params.framerate_den {
            return Err(Error::new_custom(
                "Encoder::reconfigure",
                "framerate_den must be non-zero",
            ));
        }

        // Reset は VPL 内部の未出力フレームを破棄する場合がある。
        // ref:
        // - doc/spec/source/programming_guide/VPL_prg_encoding.rst (Configuration Change)
        // pending に対応する出力を失わないよう、Reset より前に NULL 入力で
        // cached frame をすべて回収する。
        //
        // worker はコマンドを FIFO で処理するため、この待機は先行する QueueFrame と
        // Sync の処理完了も意味する。
        self.drain_cached_frames("Encoder::reconfigure")?;

        // 現在の video_param をベースに変更を適用する。drain より後に適用するのは、
        // drain が Err を返したときに Rust 側のパラメータだけが更新された状態を
        // 残さないため。
        unsafe {
            let enc = &mut self
                .video_param
                .__bindgen_anon_1
                .mfx
                .__bindgen_anon_1
                .__bindgen_anon_1;
            if let Some(target_kbps) = params.target_kbps {
                enc.__bindgen_anon_2.TargetKbps = target_kbps;
            }
            if let Some(max_kbps) = params.max_kbps {
                enc.__bindgen_anon_3.MaxKbps = max_kbps;
            }

            let mfx = &mut self.video_param.__bindgen_anon_1.mfx;
            if let Some(num) = params.framerate_num {
                mfx.FrameInfo.FrameRateExtN = num;
            }
            if let Some(den) = params.framerate_den {
                mfx.FrameInfo.FrameRateExtD = den;
                self.framerate_den = den as u64;
            }
        }

        // Init 時に設定した拡張バッファを Reset にも渡す。
        //
        // Reset で attach されなかった拡張バッファの扱いはバッファ種別と実装に依存するため、
        // Init 時の設定を確実に再要求するには Init と同じ ExtParam を渡す必要がある。
        //
        // ExtParam の設定は drain の完了後に行う。drain のエラーで早期リターンしたときに、
        // ExtParam が設定されたまま残らないようにするため。
        let mut ext_bufs = self.ext_buffers.ext_param();
        attach_ext_param(&mut self.video_param, &mut ext_bufs);

        let result = self
            .session
            .lib()
            .mfx_video_encode_reset(self.session.as_ptr(), &mut self.video_param);

        // Reset の成否にかかわらず ExtParam をクリアする
        clear_ext_param(&mut self.video_param);

        // 成功した Reset だけを reconfigure の回数として計上する
        if result.is_ok() {
            self.stats.total_reconfigure_count.inc();
        }
        result
    }

    /// 現在の作業パラメータを読み戻す
    ///
    /// `ext_bufs` は VPL に attach する拡張バッファのポインタ配列である。
    /// VPL の呼び出し後に `mfxVideoParam` の ExtParam をクリアして返す。
    fn read_video_param(
        &self,
        ext_bufs: &mut [*mut sys::mfxExtBuffer],
    ) -> Result<sys::mfxVideoParam, Error> {
        let mut param: sys::mfxVideoParam = unsafe { std::mem::zeroed() };
        param.IOPattern = self.video_param.IOPattern;

        attach_ext_param(&mut param, ext_bufs);
        let result = self
            .session
            .lib()
            .mfx_video_encode_get_video_param(self.session.as_ptr(), &mut param);

        // 成功・失敗にかかわらず、拡張バッファのポインタを残さない
        clear_ext_param(&mut param);
        result?;

        Ok(param)
    }

    /// エンコーダが現在使用しているパラメータを取得する
    ///
    /// 基本パラメータと、要求した拡張バッファの実効値を返す。拡張バッファの値は
    /// VPL が報告した値であり、要求値と一致するとは限らない。
    ///
    /// 拡張バッファを要求していない場合、対応する [`ExtParams`] のフィールドは
    /// `None` になる。
    pub fn get_video_param(&self) -> Result<VideoParam, Error> {
        // 読み戻し用のバッファは毎回新しく構築する。
        // 保持している要求値のバッファを読み戻し先に使うと、要求値が実効値で
        // 上書きされ、以降の Reset が要求値ではなくなる。
        let mut readback = self.ext_buffers.zeroed_copy();
        let mut ext_bufs = readback.ext_param();
        let param = self.read_video_param(&mut ext_bufs)?;

        Ok(VideoParam {
            param,
            ext: readback.into_params(),
        })
    }

    /// 現在の作業パラメータセット (SPS / PPS / VPS) を取得する
    ///
    /// H.264 / HEVC / AV1 以外のコーデックはパラメータセットを持たないため `Ok(None)` を返す。
    ///
    /// 返るのは現在の作業パラメータセットであり、次にエンコードするフレームに
    /// 適用される。エンコード済みで出力待ちのフレームに適用されたものではない。
    /// パラメータセットの内容が変わるのは [`Encoder::new`] と
    /// [`Encoder::reconfigure`] だけで、[`Encoder::encode`] では変わらない。
    /// [`EncodeOptions::frame_type`] で IDR を強制しても、その時点の
    /// パラメータセットが挿入されるだけで内容は変わらない。
    ///
    /// # 呼び出しスレッド
    ///
    /// [`Encoder::encode`] と同じスレッドで、[`Encoder::encode`] と直列になるように
    /// 呼ぶこと。コールバック ([`EncodeHandler::on_encoded`]) から呼び出してはならない。
    ///
    /// # エラー
    ///
    /// パラメータセットを取得できなかった場合に `Err` を返す。VPL からの読み戻しに
    /// 失敗した場合のほか、SPS、H.264 / HEVC の PPS、HEVC の VPS のサイズが 0 の
    /// 場合も `Err` になる。
    pub fn get_sequence_params(&self) -> Result<Option<SequenceParams>, Error> {
        // コーデックごとに取得するパラメータセットを明示する
        // タプルは左から (VPS を取得するか, PPS を取得するか) を表す
        // H.264 / AV1 に mfxExtCodingOptionVPS を attach すると
        // MFXVideoENCODE_GetVideoParam が MFX_ERR_UNSUPPORTED を返すため、
        // VPS は HEVC の場合だけ要求する
        let (with_vps, with_pps) = match self.codec {
            VideoCodecType::H264 => (false, true),
            VideoCodecType::Hevc => (true, true),
            // VP9 は SPS / PPS / VPS に相当するパラメータセットを持たない
            VideoCodecType::Vp9 => return Ok(None),
            // AV1 の Sequence Header OBU は PPS を持たない
            VideoCodecType::Av1 => (false, false),
        };

        let mut buffers = SequenceParamBuffers::new(with_vps, with_pps);
        let mut ext_bufs = buffers.ext_param();
        self.read_video_param(&mut ext_bufs)?;

        let function = "Encoder::get_sequence_params";
        let sps = read_sequence_param(&buffers.sps, buffers.spspps.SPSBufSize, "SPS", function)?;

        let pps = if with_pps {
            Some(read_sequence_param(
                &buffers.pps,
                buffers.spspps.PPSBufSize,
                "PPS",
                function,
            )?)
        } else {
            None
        };

        let vps = buffers
            .vps_ext
            .as_ref()
            .map(|vps_ext| read_sequence_param(&buffers.vps, vps_ext.VPSBufSize, "VPS", function))
            .transpose()?;

        Ok(Some(SequenceParams { vps, sps, pps }))
    }

    /// 内部サーフェスのサイズを返す
    pub fn coded_size(&self) -> (usize, usize) {
        let fi = unsafe { self.frame_info.__bindgen_anon_1.__bindgen_anon_1 };
        (usize::from(fi.Width), usize::from(fi.Height))
    }

    /// IVF ヘッダーを出力する設定かどうかを返す
    ///
    /// `Vp9EncoderConfig::write_ivf_headers` で指定した要求値を返す。
    /// 非 VP9 コーデックでは常に `false` を返す。
    pub fn write_ivf_headers(&self) -> bool {
        self.write_ivf_headers
    }

    /// エンコーダーの統計値を取得する
    pub fn stats(&self) -> &EncoderStats {
        &self.stats
    }

    /// フレームをエンコードする
    ///
    /// `frame` は [`EncoderConfig::frame_format`] に対応する [`FrameData`] であること。
    /// 各プレーンの `pitch` とデータ長は [`FrameData`] の契約を満たすこと。
    ///
    /// # エラー
    ///
    /// 入力フォーマットと frame の variant の不一致、`pitch` 不足、データ長不足の場合はエラーになる。
    pub fn encode(
        &mut self,
        frame: FrameData<'_>,
        user_data: H::UserData,
        options: &EncodeOptions,
    ) -> Result<(), Error> {
        let frame_seq = self.frame_count;
        // 公開 API 向けのタイムスタンプは frame_rate 由来で計算する。
        let presentation_timestamp = frame_seq
            .checked_mul(self.framerate_den)
            .ok_or_else(|| Error::new_custom("Encoder::encode", "timestamp overflowed"))?;

        // エンコード用内部サーフェスを取得する
        let mut surface: *mut sys::mfxFrameSurface1 = std::ptr::null_mut();
        let status = self
            .session
            .lib()
            .mfx_memory_get_surface_for_encode(self.session.as_ptr(), &mut surface);
        if status != sys::mfxStatus_MFX_ERR_NONE {
            return Err(Error::from_mfx(status, "MFXMemory_GetSurfaceForEncode"));
        }
        let mut frame_surface = FrameSurface::new(self.session.lib(), surface)?;

        // Map して CPU から書き込めるようにする
        frame_surface.map_write()?;

        // フレームデータを検証して内部サーフェスにコピーする
        unsafe {
            (*frame_surface.as_ptr()).Data.TimeStamp = frame_seq;
            self.frame_format.copy_to_surface_planes(
                frame,
                &(*frame_surface.as_ptr()).Data,
                self.width,
                self.height,
            )?;
        }

        // Unmap して書き込み完了を通知する
        frame_surface.unmap()?;

        // エンコード制御を設定する
        let mut ctrl: sys::mfxEncodeCtrl = unsafe { std::mem::zeroed() };
        let ctrl_ptr = if options.frame_type != 0 {
            ctrl.FrameType = options.frame_type;
            &mut ctrl as *mut sys::mfxEncodeCtrl
        } else {
            std::ptr::null_mut()
        };

        let (mut bitstream, bitstream_buffer) = self.create_bitstream();
        let syncp = self.encode_frame_async(ctrl_ptr, Some(&frame_surface), bitstream.as_mut())?;

        // VPL が受理した時点で通算回数を計上する
        self.stats.total_encode_count.inc();

        // syncp が None の入力でも、後続出力との対応付けのため pending は必ず登録する。
        // pending の登録と in-flight の加算を同じタイミングで行い、
        // ワーカーが対応する結果を通知する直前に減算する。
        self.stats.in_flight_frames.inc();
        if let Err(error) = self.send_worker_command(
            "Encoder::encode",
            WorkerCommand::QueueFrame {
                frame_seq,
                pending_frame: PendingFrame {
                    presentation_timestamp,
                    user_data,
                },
            },
        ) {
            // pending を登録できなかったため、加算した in-flight を戻す
            self.stats.in_flight_frames.dec();
            return Err(error);
        }

        if let Some(syncp) = syncp {
            self.send_worker_command(
                "Encoder::encode",
                WorkerCommand::Sync(SyncData {
                    syncp,
                    bitstream,
                    bitstream_buffer,
                }),
            )?;
        }

        self.frame_count = self
            .frame_count
            .checked_add(1)
            .ok_or_else(|| Error::new_custom("Encoder::encode", "frame sequence overflowed"))?;
        Ok(())
    }

    /// EncodeFrameAsync をデバイスビジー時に再試行する
    ///
    /// None = MORE_DATA（出力なし）、Some(syncp) = エンコード完了
    fn encode_frame_async(
        &mut self,
        ctrl: *mut sys::mfxEncodeCtrl,
        surface: Option<&FrameSurface>,
        bitstream: &mut sys::mfxBitstream,
    ) -> Result<Option<sys::mfxSyncPoint>, Error> {
        let surface_ptr = surface.map(|s| s.as_ptr()).unwrap_or(std::ptr::null_mut());
        for _ in 0..DEVICE_BUSY_MAX_RETRIES {
            let mut syncp: sys::mfxSyncPoint = std::ptr::null_mut();
            let status = self.session.lib().mfx_video_encode_frame_async(
                self.session.as_ptr(),
                ctrl,
                surface_ptr,
                bitstream,
                &mut syncp,
            );
            if status == sys::mfxStatus_MFX_WRN_DEVICE_BUSY {
                std::thread::sleep(std::time::Duration::from_millis(1));
                continue;
            }
            if status == sys::mfxStatus_MFX_ERR_MORE_DATA {
                return Ok(None);
            }
            if status != sys::mfxStatus_MFX_ERR_NONE {
                return Err(Error::from_mfx(status, "MFXVideoENCODE_EncodeFrameAsync"));
            }
            return Ok(Some(syncp));
        }
        Err(Error::new_custom(
            "MFXVideoENCODE_EncodeFrameAsync",
            "device busy after max retries",
        ))
    }

    /// 保留しているフレームをすべて回収する
    ///
    /// 送信済みのフレームの出力がすべて揃い、それらのコールバックの処理が終わるまで待つ。
    fn drain_cached_frames(&mut self, function: &'static str) -> Result<(), Error> {
        loop {
            // null surface でエンコードを呼び出して、残りのフレームをすべて排出する
            let (mut bitstream, bitstream_buffer) = self.create_bitstream();

            let Some(syncp) = self
                .encode_frame_async(std::ptr::null_mut(), None, bitstream.as_mut())
                .map_err(|error| {
                    // drain の失敗を呼び出し元の操作のコンテキスト付きで返す。
                    // 元のメッセージは保持したまま前置きする。
                    let message = match error.status_message() {
                        Some(message) => format!("{function}: {message}"),
                        None => format!("{function}: failed to retrieve a cached frame"),
                    };
                    error.with_message(message)
                })?
            else {
                // すべて排出済み
                break;
            };

            self.send_worker_command(
                function,
                WorkerCommand::Sync(SyncData {
                    syncp,
                    bitstream,
                    bitstream_buffer,
                }),
            )?;
        }

        // ここまでに送った Sync が worker 側で全て処理されるまで待つ。
        let (tx, rx) = mpsc::channel();
        self.send_worker_command(
            function,
            WorkerCommand::WaitIdle {
                reply_tx: tx,
                function,
            },
        )?;
        rx.recv()
            .map_err(|_| Error::new_custom(function, "sync worker thread stopped unexpectedly"))?
    }

    /// エンコーダをフラッシュして残りのフレームを取得する
    ///
    /// この関数は全てのエンコードの完了を待ち、内部で保持している
    /// フレームをすべて排出してコールバックを呼び出し終わるまでブロックする。
    pub fn finish(&mut self) -> Result<(), Error> {
        self.drain_cached_frames("Encoder::finish")
    }

    fn create_bitstream(&self) -> (Box<sys::mfxBitstream>, Vec<u8>) {
        let mut bitstream_buffer = vec![0u8; self.bitstream_buffer_size];
        let mut bitstream: Box<sys::mfxBitstream> = Box::new(unsafe { std::mem::zeroed() });
        bitstream.Data = bitstream_buffer.as_mut_ptr();
        bitstream.MaxLength = bitstream_buffer.len() as u32;
        (bitstream, bitstream_buffer)
    }

    fn send_worker_command(
        &mut self,
        function: &'static str,
        command: WorkerCommand<H::UserData>,
    ) -> Result<(), Error> {
        self.worker_tx
            .send(command)
            .map_err(|_| Error::new_custom(function, "sync worker thread is not running"))
    }

    fn stop_worker(&mut self) {
        if let Some(handle) = self.worker_handle.take() {
            // Stop を送って join した時点で worker は確実に終了するため、
            // 以降に worker へデータが届くケースは考慮しない。
            let _ = self.worker_tx.send(WorkerCommand::Stop);
            let _ = handle.join();
        }
    }
}

impl<H: EncodeHandler> Drop for Encoder<H> {
    fn drop(&mut self) {
        self.stop_worker();
        let _ = self
            .session
            .lib()
            .mfx_video_encode_close(self.session.as_ptr());
        // self.session が続けて Drop され、MFXClose + MFXUnload が実行される
    }
}

fn run_sync_worker<H: EncodeHandler>(
    lib: VplLibrary,
    session_handle: usize,
    worker_rx: mpsc::Receiver<WorkerCommand<H::UserData>>,
    mut handler: H,
    stats: Arc<EncoderStats>,
) {
    let mut pending_store = PendingFrameStore::new();
    while let Ok(command) = worker_rx.recv() {
        match command {
            WorkerCommand::QueueFrame {
                frame_seq,
                pending_frame,
            } => {
                // encode 呼び出し単位の pending frame を frame_seq で登録する。
                // syncp == None の入力も必ずここで保持し、後続出力との一致で回収する。
                if pending_store.insert(frame_seq, pending_frame).is_some() {
                    // 同じ frame_seq で置き換えた pending は結果を通知できないため、
                    // in-flight から取り除く
                    stats.in_flight_frames.dec();
                    notify_encoded(
                        &mut handler,
                        &stats,
                        Err(Error::new_custom_owned(
                            "Encoder::sync_worker",
                            format!("duplicate frame sequence in pending frames: {frame_seq}"),
                        )),
                    );
                }
            }
            WorkerCommand::Sync(sync) => {
                // 通常入力の sync 完了を待ち、bitstream.TimeStamp と frame_seq を完全一致で対応付ける。
                let result = sync_and_build_frame(lib, session_handle, sync, &mut pending_store);
                if result.is_ok() {
                    // pending を消費して結果を通知できる状態になった
                    stats.in_flight_frames.dec();
                }
                notify_encoded(&mut handler, &stats, result);
            }
            WorkerCommand::WaitIdle { reply_tx, function } => {
                // finish / reconfigure 側のバリア。ここに到達した時点で、それ以前に
                // 送信された QueueFrame / Sync はすべて処理済みである。
                if pending_store.is_empty() {
                    let _ = reply_tx.send(Ok(()));
                    continue;
                }

                // バリア時点で回収できなかったフレームは、対応する出力が得られなかった
                // ものとしてエラー通知する。
                let pending = pending_store.drain_all();
                let remaining_count = pending.len();
                for (frame_seq, _meta) in pending {
                    stats.in_flight_frames.dec();
                    notify_encoded(
                        &mut handler,
                        &stats,
                        Err(pending_frame_error(frame_seq, remaining_count, function)),
                    );
                }
                let _ = reply_tx.send(Err(Error::new_custom_owned(
                    function,
                    format!("{function} completed but {remaining_count} pending frames remained"),
                )));
            }
            WorkerCommand::Stop => {
                // drop 時の中断。未完了 frame はすべて MFX_ERR_ABORTED として通知する。
                for (_frame_seq, _pending) in pending_store.drain_all() {
                    stats.in_flight_frames.dec();
                    notify_encoded(&mut handler, &stats, Err(canceled_error()));
                }
                break;
            }
        }
    }
}

/// エンコード結果をハンドラーへ通知し、通知内容に応じて統計を更新する
///
/// `Ok` は出力フレーム数と出力ビット数に、`Err` はエラー数に計上する。
fn notify_encoded<H: EncodeHandler>(
    handler: &mut H,
    stats: &EncoderStats,
    result: Result<EncodedFrame<H::UserData>, Error>,
) {
    match result {
        Ok(frame) => {
            stats.total_output_frame_count.inc();
            stats
                .total_output_bit_count
                .add(frame.data().len() as u64 * 8);
            handler.on_encoded(Ok(frame));
        }
        Err(error) => {
            stats.total_error_count.inc();
            handler.on_encoded(Err(error.into()));
        }
    }
}

/// `SyncOperation` 完了後の bitstream を取り出し、
/// `bitstream.TimeStamp == frame_seq` の完全一致で pending frame を引き当てて `EncodedFrame<T>` を構築する。
///
/// この関数は以下をまとめて行う。
/// 1. `SyncOperation` 完了待ちと bitstream データ抽出
/// 2. `frame_seq` 完全一致での pending frame 取り出し
/// 3. callback へ渡す `EncodedFrame<T>` の生成
fn sync_and_build_frame<T>(
    lib: VplLibrary,
    session_handle: usize,
    sync_data: SyncData,
    pending_store: &mut PendingFrameStore<T>,
) -> Result<EncodedFrame<T>, Error> {
    let synced = sync_and_collect(lib, session_handle, sync_data)?;
    let pending = pending_store
        .take_by_frame_seq(synced.frame_seq)
        .ok_or_else(|| mismatched_timestamp_error(synced.frame_seq, pending_store.len()))?;
    Ok(EncodedFrame {
        data: synced.data,
        timestamp: pending.presentation_timestamp,
        picture_type: synced.picture_type,
        user_data: pending.user_data,
    })
}

/// `SyncOperation` を実行し、VPL が返した `mfxBitstream` から
/// callback に必要な情報を取り出して `SyncedBitstream` に整形する。
///
/// `DataOffset` / `DataLength` がバッファ範囲外の場合は `Err` を返す。
fn sync_and_collect(
    lib: VplLibrary,
    session_handle: usize,
    sync_data: SyncData,
) -> Result<SyncedBitstream, Error> {
    let SyncData {
        syncp,
        bitstream,
        bitstream_buffer,
    } = sync_data;
    if syncp.is_null() {
        return Err(Error::new_custom(
            "Encoder::sync_worker",
            "sync point is null",
        ));
    }

    let status = lib.mfx_video_core_sync_operation(
        session_handle as sys::mfxSession,
        syncp,
        sys::MFX_INFINITE,
    );
    Error::check_mfx(status, "MFXVideoCORE_SyncOperation")?;

    let offset = bitstream.DataOffset as usize;
    let length = bitstream.DataLength as usize;

    // VPL が返したオフセットと長さをバッファ範囲内のスライスに変換する。
    // これは VPL の値の正しさを検証するためではなく、範囲外のメモリに
    // アクセスしないための Rust の安全境界としての確認である。
    let end = offset.checked_add(length).ok_or_else(|| {
        Error::new_custom_owned(
            "Encoder::sync_worker",
            format!("bitstream offset ({offset}) + length ({length}) overflows usize"),
        )
    })?;
    if end > bitstream_buffer.len() {
        return Err(Error::new_custom_owned(
            "Encoder::sync_worker",
            format!(
                "bitstream range {}..{} exceeds buffer size {}",
                offset,
                end,
                bitstream_buffer.len()
            ),
        ));
    }

    let data = bitstream_buffer[offset..end].to_vec();
    let picture_type = picture_type_from_frame_type(bitstream.FrameType);
    Ok(SyncedBitstream {
        data,
        frame_seq: bitstream.TimeStamp,
        picture_type,
    })
}

fn mismatched_timestamp_error(frame_seq: u64, pending_len: usize) -> Error {
    Error::new_custom_owned(
        "Encoder::sync_worker",
        format!(
            "no pending frame for bitstream timestamp {frame_seq} (pending count: {pending_len})"
        ),
    )
}

/// バリア時点で回収できなかった pending frame のエラー
fn pending_frame_error(frame_seq: u64, pending_count: usize, function: &'static str) -> Error {
    Error::new_custom_owned(
        function,
        format!(
            "pending frame remained for frame sequence {frame_seq} (pending count: {pending_count})"
        ),
    )
}

fn canceled_error() -> Error {
    Error::from_mfx(sys::mfxStatus_MFX_ERR_ABORTED, "Encoder::drop")
}

fn picture_type_from_frame_type(frame_type: u16) -> PictureType {
    if frame_type & (sys::MFX_FRAMETYPE_IDR as u16) != 0 {
        PictureType::Idr
    } else if frame_type & (sys::MFX_FRAMETYPE_I as u16) != 0 {
        PictureType::I
    } else if frame_type & (sys::MFX_FRAMETYPE_P as u16) != 0 {
        PictureType::P
    } else if frame_type & (sys::MFX_FRAMETYPE_B as u16) != 0 {
        PictureType::B
    } else {
        PictureType::Unknown
    }
}

/// VP9 の IVF ヘッダー出力設定を返す
///
/// 非 VP9 コーデックは IVF ヘッダー出力の設定を持たないため None を返す。
fn vp9_write_ivf_headers(codec: &CodecConfig) -> Option<bool> {
    match codec {
        CodecConfig::Vp9(vp9) => Some(vp9.write_ivf_headers),
        _ => None,
    }
}

/// コーデック種別を返す
fn codec_type(codec: &CodecConfig) -> VideoCodecType {
    match codec {
        CodecConfig::H264(_) => VideoCodecType::H264,
        CodecConfig::Hevc(_) => VideoCodecType::Hevc,
        CodecConfig::Vp9(_) => VideoCodecType::Vp9,
        CodecConfig::Av1(_) => VideoCodecType::Av1,
    }
}

/// コーデックプロファイルを返す
///
/// プロファイル未指定の場合は MFX_PROFILE_UNKNOWN を返す。
fn codec_profile(codec: &CodecConfig) -> u16 {
    let profile = match codec {
        CodecConfig::H264(config) => config.profile.map(H264Profile::to_mfx_profile),
        CodecConfig::Hevc(config) => config.profile.map(HevcProfile::to_mfx_profile),
        CodecConfig::Vp9(config) => config.profile.map(Vp9Profile::to_mfx_profile),
        CodecConfig::Av1(config) => config.profile.map(Av1Profile::to_mfx_profile),
    };
    profile.map_or(sys::MFX_PROFILE_UNKNOWN as u16, |profile| profile as u16)
}

/// alignment の倍数に切り上げる
///
/// オーバーフロー時は alignment 境界に丸めた最大値を返す。
fn align_up(value: u32, alignment: u32) -> u32 {
    match value.checked_add(alignment - 1) {
        Some(v) => v & !(alignment - 1),
        None => !(alignment - 1),
    }
}

/// bool を MFX_CODINGOPTION_* の値へ変換する
///
/// CodingOptionValue (mfxstructures.h) の ON / OFF に対応する値だけを扱う。
fn coding_option_value(enabled: bool) -> u16 {
    if enabled {
        sys::MFX_CODINGOPTION_ON as u16
    } else {
        sys::MFX_CODINGOPTION_OFF as u16
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    /// すべての FrameFormat が FourCC との相互変換で往復すること
    #[test]
    fn frame_format_fourcc_roundtrip() {
        for format in FrameFormat::all() {
            assert_eq!(
                FrameFormat::from_fourcc(format.fourcc()),
                Some(*format),
                "{format:?} の FourCC 変換が往復しない"
            );
        }
    }

    /// 未知の FourCC が `None` になること
    #[test]
    fn frame_format_from_unknown_fourcc_returns_none() {
        assert_eq!(FrameFormat::from_fourcc(0xDEAD_BEEF), None);
    }

    /// 1 行あたりのバイト数が形式ごとの 1 画素あたりのバイト数と一致すること
    #[test]
    fn frame_format_bytes_per_row_matches_bytes_per_pixel() {
        let cases = [
            (FrameFormat::Nv12, 320),
            (FrameFormat::P010, 640),
            (FrameFormat::Yuy2, 640),
            (FrameFormat::Bgra, 1280),
            (FrameFormat::Ayuv, 1280),
            (FrameFormat::Y410, 1280),
        ];
        for (format, expected) in cases {
            assert_eq!(
                format.bytes_per_row(320),
                Some(expected),
                "{format:?} の行あたりのバイト数が不一致"
            );
        }
        // 1 画素 1 バイトの Nv12 は乗算がないためオーバーフローしない
        assert_eq!(
            FrameFormat::Nv12.bytes_per_row(usize::MAX),
            Some(usize::MAX)
        );
        for format in [
            FrameFormat::P010,
            FrameFormat::Yuy2,
            FrameFormat::Bgra,
            FrameFormat::Ayuv,
            FrameFormat::Y410,
        ] {
            assert_eq!(
                format.bytes_per_row(usize::MAX),
                None,
                "{format:?} の行あたりのバイト数がオーバーフロー時に None にならない"
            );
        }
    }

    /// ビット深度と `mfxFrameInfo.Shift` が形式ごとに正しいこと
    #[test]
    fn frame_format_bit_depth_and_shift() {
        let cases = [
            // 8bit の形式は Shift を 0 にする
            (FrameFormat::Nv12, 8, 0),
            (FrameFormat::Yuy2, 8, 0),
            (FrameFormat::Bgra, 8, 0),
            (FrameFormat::Ayuv, 8, 0),
            // P010 は 10bit の値を 16bit ワードの上位ビットに格納するため Shift を 1 にする
            (FrameFormat::P010, 10, 1),
            // Y410 は 10bit の値が 10bit 幅のフィールドにそのまま入るため Shift を 0 にする
            (FrameFormat::Y410, 10, 0),
        ];
        for (format, bit_depth, shift) in cases {
            assert_eq!(
                format.bit_depth(),
                bit_depth,
                "{format:?} のビット深度が不一致"
            );
            assert_eq!(
                format.frame_info_shift(),
                shift,
                "{format:?} の mfxFrameInfo.Shift が不一致"
            );
        }
    }

    /// コピーが UV の行数と UV ピッチを使い、表示範囲外を変更しないこと
    #[test]
    fn copy_to_surface_planes_uses_uv_pitch_and_rows() {
        let width = 4;
        let height = 4;
        let row_bytes = width;
        let y_pitch = 6;
        let uv_pitch = 8;
        let dst_pitch = 5;

        let mut y_src = vec![0u8; y_pitch * height];
        for row in 0..height {
            for col in 0..row_bytes {
                y_src[row * y_pitch + col] = (row * 10 + col) as u8;
            }
        }
        let mut uv_src = vec![0u8; uv_pitch * (height / 2)];
        for row in 0..height / 2 {
            for col in 0..row_bytes {
                uv_src[row * uv_pitch + col] = (0x40 + row * 10 + col) as u8;
            }
        }

        let mut y_dst = vec![0xAAu8; dst_pitch * height];
        let mut uv_dst = vec![0xBBu8; dst_pitch * (height / 2)];
        let mut data: sys::mfxFrameData = unsafe { std::mem::zeroed() };
        data.PitchHigh = 0;
        data.__bindgen_anon_2.PitchLow = dst_pitch as u16;
        data.__bindgen_anon_3.Y = y_dst.as_mut_ptr();
        data.__bindgen_anon_4.UV = uv_dst.as_mut_ptr();

        let frame = FrameData::SemiPlanar {
            y: &y_src,
            y_pitch,
            uv: &uv_src,
            uv_pitch,
        };
        unsafe {
            FrameFormat::Nv12
                .copy_to_surface_planes(frame, &data, width, height)
                .expect("コピーに失敗した");
        }

        for row in 0..height {
            assert_eq!(
                &y_dst[row * dst_pitch..row * dst_pitch + row_bytes],
                &y_src[row * y_pitch..row * y_pitch + row_bytes],
                "Y の {row} 行目が一致しない"
            );
            assert!(
                y_dst[row * dst_pitch + row_bytes..(row + 1) * dst_pitch]
                    .iter()
                    .all(|&byte| byte == 0xAA),
                "Y の {row} 行目のパディングが変更されている"
            );
        }
        for row in 0..height / 2 {
            assert_eq!(
                &uv_dst[row * dst_pitch..row * dst_pitch + row_bytes],
                &uv_src[row * uv_pitch..row * uv_pitch + row_bytes],
                "UV の {row} 行目が一致しない"
            );
            assert!(
                uv_dst[row * dst_pitch + row_bytes..(row + 1) * dst_pitch]
                    .iter()
                    .all(|&byte| byte == 0xBB),
                "UV の {row} 行目のパディングが変更されている"
            );
        }
    }

    /// コピー先のピッチが 1 行のバイト数未満ならエラーになること
    #[test]
    fn copy_to_surface_planes_rejects_short_destination_pitch() {
        let width = 4;
        let height = 4;
        let row_bytes = width;
        let dst_pitch = row_bytes - 1;
        let y_src = vec![0u8; row_bytes * height];
        let uv_src = vec![0u8; row_bytes * (height / 2)];
        let mut y_dst = vec![0u8; dst_pitch * height];
        let mut uv_dst = vec![0u8; dst_pitch * (height / 2)];
        let mut data: sys::mfxFrameData = unsafe { std::mem::zeroed() };
        data.PitchHigh = 0;
        data.__bindgen_anon_2.PitchLow = dst_pitch as u16;
        data.__bindgen_anon_3.Y = y_dst.as_mut_ptr();
        data.__bindgen_anon_4.UV = uv_dst.as_mut_ptr();
        let frame = FrameData::SemiPlanar {
            y: &y_src,
            y_pitch: row_bytes,
            uv: &uv_src,
            uv_pitch: row_bytes,
        };
        let error = unsafe {
            FrameFormat::Nv12
                .copy_to_surface_planes(frame, &data, width, height)
                .expect_err("コピー先のピッチ不足は拒否されること")
        };
        assert!(
            error.to_string().contains("destination pitch"),
            "エラーメッセージに destination pitch が含まれること: {error}"
        );
    }

    /// パック形式のコピーが形式ごとのポインタとピッチを使うこと
    #[test]
    fn copy_to_surface_planes_uses_packed_plane_pointer() {
        let width = 4;
        let height = 2;
        for format in [FrameFormat::Yuy2, FrameFormat::Bgra, FrameFormat::Ayuv] {
            let row_bytes = format
                .bytes_per_row(width)
                .expect("行バイト数が取得できない");
            let src_pitch = row_bytes + 2;
            let dst_pitch = row_bytes + 1;
            let mut src = vec![0u8; src_pitch * height];
            for row in 0..height {
                for col in 0..row_bytes {
                    src[row * src_pitch + col] = (row * 10 + col) as u8;
                }
            }
            let mut dst = vec![0xAAu8; dst_pitch * height];
            let mut data: sys::mfxFrameData = unsafe { std::mem::zeroed() };
            data.PitchHigh = 0;
            data.__bindgen_anon_2.PitchLow = dst_pitch as u16;
            match format {
                FrameFormat::Yuy2 => data.__bindgen_anon_3.Y = dst.as_mut_ptr(),
                FrameFormat::Bgra => data.__bindgen_anon_5.B = dst.as_mut_ptr(),
                FrameFormat::Ayuv => data.__bindgen_anon_5.V = dst.as_mut_ptr(),
                _ => unreachable!(),
            }
            let frame = FrameData::Packed {
                data: &src,
                pitch: src_pitch,
            };
            unsafe {
                format
                    .copy_to_surface_planes(frame, &data, width, height)
                    .expect("コピーに失敗した");
            }
            for row in 0..height {
                assert_eq!(
                    &dst[row * dst_pitch..row * dst_pitch + row_bytes],
                    &src[row * src_pitch..row * src_pitch + row_bytes],
                    "{format:?} の {row} 行目が一致しない"
                );
                assert!(
                    dst[row * dst_pitch + row_bytes..(row + 1) * dst_pitch]
                        .iter()
                        .all(|&byte| byte == 0xAA),
                    "{format:?} の {row} 行目のパディングが変更されている"
                );
            }
        }
    }

    /// ピッチが PitchHigh と PitchLow を合わせた 32bit の値になること
    #[test]
    fn frame_data_pitch_combines_pitch_high_and_pitch_low() {
        let mut data: sys::mfxFrameData = unsafe { std::mem::zeroed() };
        data.PitchHigh = 1;
        data.__bindgen_anon_2.PitchLow = 2;
        // finalPitch = (1 << 16) + 2
        assert_eq!(unsafe { frame_data_pitch(&data) }, 65538);
    }

    fn make_pending(presentation_timestamp: u64, user_data: u32) -> PendingFrame<u32> {
        PendingFrame {
            presentation_timestamp,
            user_data,
        }
    }

    #[test]
    fn pending_frame_store_takes_by_frame_seq() {
        let mut store = PendingFrameStore::new();
        store.insert(10, make_pending(1000, 1));
        store.insert(20, make_pending(2000, 2));

        let second = store
            .take_by_frame_seq(20)
            .expect("pending frame for frame sequence 20 should exist");
        assert_eq!(second.presentation_timestamp, 2000);
        assert_eq!(second.user_data, 2);
        let first = store
            .take_by_frame_seq(10)
            .expect("pending frame for frame sequence 10 should exist");
        assert_eq!(first.presentation_timestamp, 1000);
        assert_eq!(first.user_data, 1);
        assert_eq!(store.len(), 0);
    }

    #[test]
    fn worker_wait_idle_returns_error_when_pending_remains() {
        // WaitIdle は残留 pending をエラー通知し、呼び出し元に渡した
        // コンテキストをエラーに付けることを確認する。
        let (command_tx, command_rx) = mpsc::channel();
        let (callback_tx, callback_rx) = mpsc::channel::<Result<(), Error>>();
        let stats = Arc::new(EncoderStats::default());
        let worker_stats = Arc::clone(&stats);
        let worker = thread::spawn(move || {
            run_sync_worker(
                VplLibrary,
                0,
                command_rx,
                FnEncodeHandler::new(move |result: Result<EncodedFrame<u32>, Error>| {
                    callback_tx
                        .send(result.map(|_| ()))
                        .expect("コールバック結果の送信に失敗した");
                }),
                worker_stats,
            );
        });

        // encode() と同じく、pending を登録する時点で in-flight を加算する
        stats.in_flight_frames.inc();
        command_tx
            .send(WorkerCommand::QueueFrame {
                frame_seq: 77,
                pending_frame: make_pending(7700, 7),
            })
            .expect("QueueFrame の送信に失敗した");
        let (reply_tx, reply_rx) = mpsc::channel();
        command_tx
            .send(WorkerCommand::WaitIdle {
                reply_tx,
                function: "Encoder::reconfigure",
            })
            .expect("WaitIdle の送信に失敗した");
        let wait_error = reply_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("WaitIdle の応答の受信に失敗した")
            .expect_err("残留 pending があるのに WaitIdle が成功した");
        assert_eq!(
            wait_error.function(),
            "Encoder::reconfigure",
            "WaitIdle のエラーは渡したコンテキストを持つこと"
        );
        assert_eq!(
            wait_error.status_message(),
            Some("Encoder::reconfigure completed but 1 pending frames remained"),
            "WaitIdle のエラーメッセージはコンテキストで始まること"
        );

        let callback_error = callback_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("コールバック結果の受信に失敗した")
            .expect_err("残留 pending はコールバックのエラーとして通知されること");
        assert_eq!(
            callback_error.function(),
            "Encoder::reconfigure",
            "残留 pending のエラーは渡したコンテキストを持つこと"
        );

        // 残留 pending はエラー通知のたびにエラー数へ計上し、
        // 通知を終えた時点で in-flight から取り除かれること
        assert_eq!(
            stats.total_error_count.get(),
            1,
            "残留 pending のエラー通知がエラー数に計上されること"
        );
        assert_eq!(
            stats.total_output_frame_count.get(),
            0,
            "エラー通知は出力フレーム数に計上されないこと"
        );
        assert_eq!(
            stats.in_flight_frames.get(),
            0,
            "残留 pending の通知後に in-flight が 0 であること"
        );

        command_tx
            .send(WorkerCommand::Stop)
            .expect("Stop の送信に失敗した");
        worker.join().expect("worker スレッドがパニックした");
    }

    #[test]
    fn worker_stop_returns_aborted_for_all_pending() {
        let (command_tx, command_rx) = mpsc::channel();
        let (callback_tx, callback_rx) = mpsc::channel::<Result<(), Error>>();
        let stats = Arc::new(EncoderStats::default());
        let worker_stats = Arc::clone(&stats);
        let worker = thread::spawn(move || {
            run_sync_worker(
                VplLibrary,
                0,
                command_rx,
                FnEncodeHandler::new(move |result: Result<EncodedFrame<u32>, Error>| {
                    callback_tx
                        .send(result.map(|_| ()))
                        .expect("failed to forward callback result");
                }),
                worker_stats,
            );
        });

        // encode() と同じく、pending を登録する時点で in-flight を加算する
        stats.in_flight_frames.inc();
        command_tx
            .send(WorkerCommand::QueueFrame {
                frame_seq: 1,
                pending_frame: make_pending(100, 10),
            })
            .expect("failed to send QueueFrame");
        stats.in_flight_frames.inc();
        command_tx
            .send(WorkerCommand::QueueFrame {
                frame_seq: 2,
                pending_frame: make_pending(200, 20),
            })
            .expect("failed to send QueueFrame");
        command_tx
            .send(WorkerCommand::Stop)
            .expect("failed to send Stop");
        worker.join().expect("worker thread panicked");

        let mut callback_count = 0;
        while let Ok(result) = callback_rx.recv_timeout(Duration::from_millis(200)) {
            callback_count += 1;
            let error = result.expect_err("stop callback must be an error");
            assert_eq!(
                error.status_code(),
                Some(sys::mfxStatus_MFX_ERR_ABORTED),
                "stop callback must return MFX_ERR_ABORTED",
            );
        }
        assert_eq!(
            callback_count, 2,
            "expected callbacks for all pending frames"
        );

        // 中断通知でもエラー数を増やし、in-flight を 0 に戻すこと
        assert_eq!(
            stats.total_error_count.get(),
            2,
            "中断した全フレームがエラー数に計上されること"
        );
        assert_eq!(
            stats.in_flight_frames.get(),
            0,
            "中断通知の後に in-flight が 0 であること"
        );
    }

    #[test]
    fn worker_reports_duplicate_frame_sequence() {
        // decode 側と対称に、同一 frame_seq の QueueFrame を 2 回送信する。
        // 置き換えられた pending と残留 pending の両方が in-flight から
        // 取り除かれ、2 件のエラー通知がエラー数に計上されることを確認する。
        let (command_tx, command_rx) = mpsc::channel();
        let (callback_tx, callback_rx) = mpsc::channel::<Result<u32, Error>>();
        let stats = Arc::new(EncoderStats::default());
        let worker_stats = Arc::clone(&stats);
        let worker = thread::spawn(move || {
            run_sync_worker(
                VplLibrary,
                0,
                command_rx,
                FnEncodeHandler::new(move |result: Result<EncodedFrame<u32>, Error>| {
                    callback_tx
                        .send(result.map(|frame| *frame.user_data()))
                        .expect("コールバック結果の送信に失敗した");
                }),
                worker_stats,
            );
        });

        // encode() と同じく、pending を登録する時点で in-flight を加算する
        stats.in_flight_frames.inc();
        command_tx
            .send(WorkerCommand::QueueFrame {
                frame_seq: 42,
                pending_frame: make_pending(100, 1),
            })
            .expect("QueueFrame の送信に失敗した");
        stats.in_flight_frames.inc();
        command_tx
            .send(WorkerCommand::QueueFrame {
                frame_seq: 42,
                pending_frame: make_pending(200, 2),
            })
            .expect("QueueFrame の送信に失敗した");
        command_tx
            .send(WorkerCommand::Stop)
            .expect("Stop の送信に失敗した");
        worker.join().expect("worker スレッドがパニックした");

        // 1 つ目のコールバックが重複 frame_seq のエラー通知であること
        let callback_result = callback_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("重複通知コールバックの受信に失敗した");
        let error = callback_result.expect_err("重複 frame_seq はエラー通知されること");
        assert_eq!(error.function(), "Encoder::sync_worker");
        assert_eq!(
            error.status_message(),
            Some("duplicate frame sequence in pending frames: 42"),
            "重複した frame_seq を含むエラーメッセージであること"
        );

        // 置き換えられた pending は通知済みで、Stop では新しく登録された
        // pending が残留分として ABORTED 通知されること
        let stop_result = callback_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("残留 pending の ABORTED 通知の受信に失敗した");
        let stop_error = stop_result.expect_err("Stop 時のコールバックはエラーであること");
        assert_eq!(
            stop_error.status_code(),
            Some(sys::mfxStatus_MFX_ERR_ABORTED),
            "残留 pending は MFX_ERR_ABORTED で通知されること"
        );

        // 通知は重複エラーと残留 ABORTED の 2 回だけであること
        assert!(
            callback_rx
                .recv_timeout(Duration::from_millis(200))
                .is_err(),
            "通知は 2 回だけであること"
        );

        assert_eq!(
            stats.total_error_count.get(),
            2,
            "重複エラーと中断エラーがエラー数に計上されること"
        );
        assert_eq!(
            stats.total_output_frame_count.get(),
            0,
            "出力フレーム数は増えないこと"
        );
        assert_eq!(
            stats.in_flight_frames.get(),
            0,
            "重複と中断の通知後に in-flight が 0 であること"
        );
    }

    #[test]
    fn ext_buffers_zeroed_copy_keeps_header_and_clears_values() {
        // 読み戻し用のバッファは Header を維持したまま値だけを 0 にする。
        // 値が残っていると、VPL が書き戻さなかった場合に要求値がそのまま見えてしまう。
        let requested = ExtBuffers::new(Some(16), Some(25), Some(true));
        let readback = requested.zeroed_copy();

        let co2 = readback
            .ext_co2
            .as_ref()
            .expect("mfxExtCodingOption2 が構築されていない");
        assert_eq!(co2.Header.BufferId, sys::MFX_EXTBUFF_CODING_OPTION2);
        assert_eq!(
            co2.Header.BufferSz,
            std::mem::size_of::<sys::mfxExtCodingOption2>() as u32
        );
        assert_eq!(
            co2.LookAheadDepth, 0,
            "読み戻し用バッファの LookAheadDepth は 0 であること"
        );

        let co3 = readback
            .ext_co3
            .as_ref()
            .expect("mfxExtCodingOption3 が構築されていない");
        assert_eq!(co3.Header.BufferId, sys::MFX_EXTBUFF_CODING_OPTION3);
        assert_eq!(
            co3.QVBRQuality, 0,
            "読み戻し用バッファの QVBRQuality は 0 であること"
        );

        let vp9 = readback
            .ext_vp9
            .as_ref()
            .expect("mfxExtVP9Param が構築されていない");
        assert_eq!(vp9.Header.BufferId, sys::MFX_EXTBUFF_VP9_PARAM);
        assert_eq!(
            vp9.WriteIVFHeaders, 0,
            "読み戻し用バッファの WriteIVFHeaders は 0 であること"
        );
    }

    #[test]
    fn ext_buffers_zeroed_copy_skips_unrequested_buffers() {
        let requested = ExtBuffers::new(None, None, None);
        let readback = requested.zeroed_copy();

        assert!(
            readback.ext_co2.is_none(),
            "要求していない mfxExtCodingOption2 は構築しないこと"
        );
        assert!(
            readback.ext_co3.is_none(),
            "要求していない mfxExtCodingOption3 は構築しないこと"
        );
        assert!(
            readback.ext_vp9.is_none(),
            "要求していない mfxExtVP9Param は構築しないこと"
        );
    }

    #[test]
    fn ext_buffers_into_params_keeps_requested_values() {
        let buffers = ExtBuffers::new(Some(16), Some(25), Some(true));
        let params = buffers.into_params();

        assert_eq!(
            params
                .coding_option2
                .expect("mfxExtCodingOption2 が取り出せない")
                .LookAheadDepth,
            16
        );
        assert_eq!(
            params
                .coding_option3
                .expect("mfxExtCodingOption3 が取り出せない")
                .QVBRQuality,
            25
        );
        assert_eq!(
            params
                .vp9_param
                .expect("mfxExtVP9Param が取り出せない")
                .WriteIVFHeaders,
            sys::MFX_CODINGOPTION_ON as u16
        );
    }

    /// エンコーダが実際に使っている LookAheadDepth と QVBRQuality を読み戻す
    ///
    /// 実機 (Intel GPU) 上でのみ動作する。
    #[cfg(intel_vpl)]
    fn read_coding_option_ext_params<H: EncodeHandler>(encoder: &Encoder<H>) -> (u16, u16) {
        let param = encoder.get_video_param().expect("GetVideoParam に失敗した");
        let co2 = param
            .ext
            .coding_option2
            .expect("mfxExtCodingOption2 が読み戻されていない");
        let co3 = param
            .ext
            .coding_option3
            .expect("mfxExtCodingOption3 が読み戻されていない");
        (co2.LookAheadDepth, co3.QVBRQuality)
    }

    /// reconfigure 後も Init 時に設定した LookAheadDepth / QVBRQuality が維持されることを確認する
    ///
    /// 実機 (Intel GPU) 上でのみ動作するため `INTEL_VPL=1` ビルドでのみコンパイルする。
    #[cfg(intel_vpl)]
    #[test]
    fn reconfigure_preserves_coding_option_ext_params() {
        let adapters = crate::list_adapters().expect("list_adapters に失敗した");
        let first = adapters.first().expect("Intel アダプタが見つからない");

        // HEVC の QVBR は LookAheadDepth と QVBRQuality の両方を ExtParam として
        // 受け付けるため、両方の維持を 1 つのエンコーダで確認できる。
        let mut config = EncoderConfig::new(
            AdapterSelector::DrmRenderNode(first.drm_render_node),
            CodecConfig::Hevc(HevcEncoderConfig { profile: None }),
            320,
            240,
            FrameFormat::Nv12,
            30,
            1,
            RateControlMode::Qvbr,
        );
        config.target_kbps = Some(2000);
        config.max_kbps = Some(4000);
        config.look_ahead_depth = Some(16);
        config.qvbr_quality = Some(25);

        let mut encoder = Encoder::new(
            config,
            FnEncodeHandler::new(|_: Result<EncodedFrame<u32>, Error>| {}),
        )
        .expect("Encoder::new に失敗した");

        // Init 直後の実効値を取得する。
        // 拡張バッファが実効値として反映されない GPU ではこのテストの前提が
        // 崩れるため、0 でないことも確認する。
        let (look_ahead_before, qvbr_before) = read_coding_option_ext_params(&encoder);
        assert!(
            look_ahead_before > 0,
            "Init 後の LookAheadDepth が 0: この GPU は HEVC QVBR の LookAheadDepth をサポートしていない"
        );
        assert!(
            qvbr_before > 0,
            "Init 後の QVBRQuality が 0: この GPU は HEVC QVBR の QVBRQuality をサポートしていない"
        );

        encoder
            .reconfigure(ReconfigureParams {
                target_kbps: Some(3000),
                ..Default::default()
            })
            .expect("reconfigure に失敗した");

        // reconfigure (MFXVideoENCODE_Reset) 後も Init 時の拡張バッファが
        // 維持されていることを確認する。
        let (look_ahead_after, qvbr_after) = read_coding_option_ext_params(&encoder);
        assert_eq!(
            look_ahead_after, look_ahead_before,
            "reconfigure で LookAheadDepth が変わった (before: {look_ahead_before}, after: {look_ahead_after})"
        );
        assert_eq!(
            qvbr_after, qvbr_before,
            "reconfigure で QVBRQuality が変わった (before: {qvbr_before}, after: {qvbr_after})"
        );
    }

    #[test]
    fn strip_start_code_removes_four_byte_prefix() {
        // 4 バイトのスタートコードが先頭から除去され、NAL ヘッダが先頭に来る
        let data = [0x00, 0x00, 0x00, 0x01, 0x67, 0x42];
        assert_eq!(strip_start_code(&data), &[0x67, 0x42]);
    }

    #[test]
    fn strip_start_code_removes_three_byte_prefix() {
        // 3 バイトのスタートコードが先頭から除去され、NAL ヘッダが先頭に来る
        let data = [0x00, 0x00, 0x01, 0x68, 0xCE];
        assert_eq!(strip_start_code(&data), &[0x68, 0xCE]);
    }

    #[test]
    fn strip_start_code_keeps_data_without_prefix() {
        // スタートコードを持たないデータはそのまま返る
        let data = [0x0A, 0x0B, 0x0C];
        assert_eq!(strip_start_code(&data), &data);
    }

    #[test]
    fn read_sequence_param_rejects_zero_size() {
        // サイズ 0 は「パラメータセットが存在しない」ではなく「取得できなかった」を表す
        let error = read_sequence_param(&[0u8; 8], 0, "SPS", "Encoder::get_sequence_params")
            .expect_err("サイズ 0 のパラメータセットはエラーになること");
        assert_eq!(error.function(), "Encoder::get_sequence_params");
        assert_eq!(
            error.status_message(),
            Some("SPS is not available"),
            "サイズ 0 のエラーメッセージにパラメータ名が含まれること"
        );
    }

    #[test]
    fn read_sequence_param_rejects_size_larger_than_buffer() {
        // VPL が確保した容量を超えるサイズを報告した場合は、範囲外アクセスせずエラーにする
        let error = read_sequence_param(&[0u8; 8], 9, "SPS", "Encoder::get_sequence_params")
            .expect_err("バッファ容量を超えるサイズはエラーになること");
        assert_eq!(error.function(), "Encoder::get_sequence_params");
        assert_eq!(
            error.status_message(),
            Some("SPS size 9 exceeds the readback buffer size 8"),
            "容量超過のエラーメッセージにサイズが含まれること"
        );
    }

    #[test]
    fn read_sequence_param_returns_bytes_without_start_code() {
        // サイズで切り詰めたうえでスタートコードを除去したバイト列を返す
        let buffer = [0x00, 0x00, 0x00, 0x01, 0x67, 0x42, 0xFF, 0xFF];
        let data = read_sequence_param(&buffer, 6, "SPS", "Encoder::get_sequence_params")
            .expect("スタートコード付きのパラメータセットを取得できること");
        assert_eq!(data, vec![0x67, 0x42]);
    }

    #[test]
    fn sequence_param_buffers_attach_requested_params() {
        // H.264 は SPS / PPS を要求し、VPS は要求しない
        let mut h264 = SequenceParamBuffers::new(false, true);
        assert_eq!(h264.ext_param().len(), 1);
        assert!(
            h264.vps_ext.is_none(),
            "VPS を要求していない場合は mfxExtCodingOptionVPS を構築しないこと"
        );
        assert!(
            h264.vps.is_empty(),
            "VPS を要求していない場合は VPS の読み戻し領域を確保しないこと"
        );
        assert!(
            !h264.pps.is_empty(),
            "PPS を要求した場合は PPS の読み戻し領域を確保すること"
        );
        assert!(
            !h264.spspps.PPSBuffer.is_null(),
            "PPS を要求した場合は PPSBuffer を設定すること"
        );
        assert_eq!(
            h264.spspps.PPSBufSize, SEQUENCE_PARAM_BUFFER_SIZE as u16,
            "PPS の書き込み先容量が設定されること"
        );

        // HEVC は VPS / SPS / PPS を要求する
        let mut hevc = SequenceParamBuffers::new(true, true);
        assert_eq!(hevc.ext_param().len(), 2);
        let vps_ext = hevc
            .vps_ext
            .as_ref()
            .expect("VPS を要求した場合は mfxExtCodingOptionVPS を構築すること");
        assert_eq!(
            vps_ext.Header.BufferId,
            sys::MFX_EXTBUFF_CODING_OPTION_VPS,
            "mfxExtCodingOptionVPS の BufferId が設定されること"
        );
        assert_eq!(
            vps_ext.VPSBufSize, SEQUENCE_PARAM_BUFFER_SIZE as u16,
            "VPS の書き込み先容量が設定されること"
        );
        assert!(
            !hevc.pps.is_empty(),
            "PPS を要求した場合は PPS の読み戻し領域を確保すること"
        );

        // AV1 は Sequence Header OBU だけを要求し、VPS / PPS は要求しない
        let mut av1 = SequenceParamBuffers::new(false, false);
        assert_eq!(av1.ext_param().len(), 1);
        assert!(
            av1.vps_ext.is_none(),
            "VPS を要求していない場合は mfxExtCodingOptionVPS を構築しないこと"
        );
        assert!(
            av1.vps.is_empty(),
            "VPS を要求していない場合は VPS の読み戻し領域を確保しないこと"
        );
        assert!(
            av1.pps.is_empty(),
            "PPS を要求していない場合は PPS の読み戻し領域を確保しないこと"
        );
        assert!(
            av1.spspps.PPSBuffer.is_null(),
            "PPS を要求していない場合は PPSBuffer を設定しないこと"
        );
        assert_eq!(
            av1.spspps.PPSBufSize, 0,
            "PPS を要求していない場合は PPSBufSize を 0 のままにすること"
        );
    }
}
