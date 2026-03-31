use std::collections::VecDeque;

use crate::{Error, VplLibrary, sys};

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

/// AV1 プロファイル
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Av1Profile {
    /// メインプロファイル
    Main,
}

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

/// 入力フレームフォーマット
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameFormat {
    /// Semi-Planar YUV 4:2:0 8bit [Y plane + interleaved UV plane]
    Nv12,
    /// Packed YUV 4:2:2 8bit [YUYV interleaved]
    Yuy2,
    /// Packed BGRA 8bit
    Bgra,
}

impl FrameFormat {
    fn fourcc(self) -> u32 {
        match self {
            FrameFormat::Nv12 => sys::MFX_FOURCC_NV12,
            FrameFormat::Yuy2 => sys::MFX_FOURCC_YUY2,
            FrameFormat::Bgra => sys::MFX_FOURCC_RGB4,
        }
    }

    fn chroma_format(self) -> u16 {
        match self {
            FrameFormat::Nv12 => sys::MFX_CHROMAFORMAT_YUV420 as u16,
            FrameFormat::Yuy2 => sys::MFX_CHROMAFORMAT_YUV422 as u16,
            FrameFormat::Bgra => sys::MFX_CHROMAFORMAT_YUV444 as u16,
        }
    }

    fn bit_depth(self) -> u16 {
        8
    }

    /// フレームデータのバイトサイズを返す
    pub fn frame_size(self, width: usize, height: usize) -> usize {
        let pixels = width * height;
        match self {
            // YUV 4:2:0 8bit: Y + UV/2 = pixels * 3 / 2
            FrameFormat::Nv12 => pixels * 3 / 2,
            // YUY2: 2 bytes/pixel (packed YUYV)
            FrameFormat::Yuy2 => pixels * 2,
            // BGRA: 4 bytes/pixel
            FrameFormat::Bgra => pixels * 4,
        }
    }

    /// ピッチ（行あたりのバイト数）を返す
    fn pitch(self, width: usize) -> u16 {
        match self {
            FrameFormat::Nv12 => width as u16,
            FrameFormat::Yuy2 => (width * 2) as u16,
            FrameFormat::Bgra => (width * 4) as u16,
        }
    }

    /// mfxFrameData の各プレーンポインタを設定する
    ///
    /// # Safety
    ///
    /// `ptr` は `frame_size(width, height)` バイト以上の有効なメモリを指す必要がある
    unsafe fn set_planes(
        self,
        data: &mut sys::mfxFrameData,
        ptr: *mut u8,
        width: usize,
        height: usize,
    ) {
        let luma_size = width * height;
        unsafe {
            data.__bindgen_anon_2.Pitch = self.pitch(width);
            match self {
                FrameFormat::Nv12 => {
                    data.__bindgen_anon_3.Y = ptr;
                    data.__bindgen_anon_4.UV = ptr.add(luma_size);
                }
                FrameFormat::Yuy2 => {
                    // YUY2 はパック済み。Y と UV は同じベースアドレスを指す
                    data.__bindgen_anon_3.Y = ptr;
                    data.__bindgen_anon_4.UV = ptr;
                }
                FrameFormat::Bgra => {
                    // BGRA はパック済み。R/G/B/A はすべて同じベースアドレスを指す
                    data.__bindgen_anon_3.R = ptr;
                    data.__bindgen_anon_4.G = ptr;
                    data.__bindgen_anon_5.B = ptr;
                    data.A = ptr;
                }
            }
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
/// VPL の mfxVideoParam / mfxInfoMFX / mfxFrameInfo に対応する。
/// フィールド名は VPL API の構造体メンバ名に準拠する。
#[derive(Debug, Clone)]
pub struct EncoderConfig {
    // --- コーデック (mfxInfoMFX.CodecId / CodecProfile) ---
    /// コーデック設定
    pub codec: CodecConfig,

    // --- フレーム情報 (mfxFrameInfo) ---
    /// フレーム幅（ピクセル、16 の倍数にアライメントされる）
    pub width: u32,
    /// フレーム高さ（ピクセル、16 の倍数にアライメントされる）
    pub height: u32,
    /// 入力フレームフォーマット（mfxFrameInfo.FourCC / ChromaFormat / BitDepth を決定する）
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
    /// None の場合は width * height * 4 / 1024 に切り上げて自動計算する。
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
    pub fn new(
        codec: CodecConfig,
        width: u32,
        height: u32,
        frame_format: FrameFormat,
        framerate_num: u32,
        framerate_den: u32,
        rate_control_mode: RateControlMode,
    ) -> Self {
        Self {
            codec,
            width,
            height,
            frame_format,
            framerate_num,
            framerate_den,
            aspect_ratio_w: None,
            aspect_ratio_h: None,
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

/// エンコード統計情報
#[derive(Debug, Clone)]
pub struct EncoderStats {
    /// エンコード済みフレーム数
    pub num_frame: u32,
    /// エンコード済みビット数
    pub num_bit: u64,
    /// キャッシュ済みフレーム数
    pub num_cached_frame: u32,
}

/// エンコード済みフレーム
pub struct EncodedFrame {
    data: Vec<u8>,
    timestamp: u64,
    picture_type: PictureType,
}

impl EncodedFrame {
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
}

/// デバイスビジー時の最大リトライ回数
const DEVICE_BUSY_MAX_RETRIES: u32 = 10;

/// エンコーダ
pub struct Encoder {
    lib: VplLibrary,
    loader: sys::mfxLoader,
    session: sys::mfxSession,
    video_param: sys::mfxVideoParam,
    frame_info: sys::mfxFrameInfo,
    frame_format: FrameFormat,
    crop_w: usize,
    crop_h: usize,
    bitstream_buffer: Vec<u8>,
    encoded_frames: VecDeque<EncodedFrame>,
    frame_count: u64,
    framerate_den: u64,
}

// Safety: Encoder の全公開メソッドは &mut self を要求するため、同時に複数スレッドから
// アクセスされることはない。VPL 仕様上、セッション操作の同一スレッド制約は明記されて
// いないため、スレッド間の移動は許容する。Intel の公式サンプル（hello-encode 等）でも
// セッションハンドルにスレッドアフィニティの制約は課されていない。
// Sync は実装しない（生ポインタにより自動的に !Sync）。
unsafe impl Send for Encoder {}

impl Encoder {
    /// エンコーダを作成する
    pub fn new(config: EncoderConfig) -> Result<Self, Error> {
        // 寸法を u16 へ変換する前に範囲を検証する
        if config.width == 0 || config.height == 0 {
            return Err(Error::new_custom(
                "Encoder::new",
                "width and height must be non-zero",
            ));
        }
        if config.width > u16::MAX as u32 || config.height > u16::MAX as u32 {
            return Err(Error::new_custom_owned(
                "Encoder::new",
                format!(
                    "width ({}) and height ({}) must not exceed {}",
                    config.width,
                    config.height,
                    u16::MAX
                ),
            ));
        }
        // pitch（行あたりのバイト数）が u16 に収まるか検証する
        // NV12: width, YUY2: width * 2, BGRA: width * 4
        let pitch_bytes: u64 = match config.frame_format {
            FrameFormat::Nv12 => config.width as u64,
            FrameFormat::Yuy2 => config.width as u64 * 2,
            FrameFormat::Bgra => config.width as u64 * 4,
        };
        if pitch_bytes > u16::MAX as u64 {
            return Err(Error::new_custom_owned(
                "Encoder::new",
                format!(
                    "pitch ({pitch_bytes} bytes) for {:?} with width {} exceeds u16::MAX",
                    config.frame_format, config.width
                ),
            ));
        }

        let lib = VplLibrary::load()?;

        // API 2.x フローでセッションを作成する（ハードウェア実装を使用）
        let (loader, session) = lib.create_session(sys::mfxImplType_MFX_IMPL_TYPE_HARDWARE)?;

        // 初期化失敗時に MFXClose を呼ぶガード
        let session_guard = CloseGuard::session(lib, loader, session);

        let aligned_width = align_up(config.width, 16);
        let aligned_height = align_up(config.height, 16);

        // mfxFrameInfo を設定する
        let mut frame_info: sys::mfxFrameInfo = unsafe { std::mem::zeroed() };
        frame_info.FourCC = config.frame_format.fourcc();
        frame_info.ChromaFormat = config.frame_format.chroma_format();
        frame_info.BitDepthLuma = config.frame_format.bit_depth();
        frame_info.BitDepthChroma = config.frame_format.bit_depth();
        frame_info.PicStruct = sys::MFX_PICSTRUCT_PROGRESSIVE as u16;
        frame_info.FrameRateExtN = config.framerate_num;
        frame_info.FrameRateExtD = config.framerate_den;
        frame_info.AspectRatioW = config.aspect_ratio_w.unwrap_or(0);
        frame_info.AspectRatioH = config.aspect_ratio_h.unwrap_or(0);
        {
            let fi = unsafe { &mut frame_info.__bindgen_anon_1.__bindgen_anon_1 };
            fi.Width = aligned_width as u16;
            fi.Height = aligned_height as u16;
            fi.CropW = config.width as u16;
            fi.CropH = config.height as u16;
        }

        let gop_ref_dist = config.gop_ref_dist.unwrap_or(1);
        let gop_pic_size = config.gop_pic_size.unwrap_or(0);

        // ビットストリームバッファサイズ（指定がなければ 4 バイト/ピクセルで自動計算する）
        let pixel_count = aligned_width as u64 * aligned_height as u64;
        let auto_buffer_bytes = pixel_count.saturating_mul(4);
        let auto_buffer_kb = align_up(auto_buffer_bytes.min(u32::MAX as u64) as u32, 1024) / 1024;
        let buffer_size_in_kb: u16 = config
            .buffer_size_in_kb
            .unwrap_or(u16::try_from(auto_buffer_kb).unwrap_or(u16::MAX));

        // mfxVideoParam を設定する
        let mut video_param: sys::mfxVideoParam = unsafe { std::mem::zeroed() };
        video_param.IOPattern = sys::MFX_IOPATTERN_IN_SYSTEM_MEMORY as u16;
        video_param.AsyncDepth = 1;
        unsafe {
            let mfx = &mut video_param.__bindgen_anon_1.mfx;
            mfx.FrameInfo = frame_info;
            mfx.CodecId = codec_id(&config.codec);
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
        let mut ext_co2: Option<sys::mfxExtCodingOption2> = None;
        let mut ext_co3: Option<sys::mfxExtCodingOption3> = None;

        // Look Ahead depth の設定 (mfxExtCodingOption2)
        let needs_co2 = config.look_ahead_depth.is_some();
        if needs_co2 {
            let mut co2: sys::mfxExtCodingOption2 = unsafe { std::mem::zeroed() };
            co2.Header.BufferId = sys::MFX_EXTBUFF_CODING_OPTION2;
            co2.Header.BufferSz = std::mem::size_of::<sys::mfxExtCodingOption2>() as u32;
            if let Some(depth) = config.look_ahead_depth {
                co2.LookAheadDepth = depth;
            }
            ext_co2 = Some(co2);
        }

        // QVBR Quality の設定 (mfxExtCodingOption3)
        let needs_co3 = config.qvbr_quality.is_some();
        if needs_co3 {
            let mut co3: sys::mfxExtCodingOption3 = unsafe { std::mem::zeroed() };
            co3.Header.BufferId = sys::MFX_EXTBUFF_CODING_OPTION3;
            co3.Header.BufferSz = std::mem::size_of::<sys::mfxExtCodingOption3>() as u32;
            if let Some(quality) = config.qvbr_quality {
                co3.QVBRQuality = quality;
            }
            ext_co3 = Some(co3);
        }

        // 拡張バッファポインタ配列を構築する
        let mut ext_bufs: Vec<*mut sys::mfxExtBuffer> = Vec::new();
        if let Some(ref mut co2) = ext_co2 {
            ext_bufs.push(co2 as *mut sys::mfxExtCodingOption2 as *mut sys::mfxExtBuffer);
        }
        if let Some(ref mut co3) = ext_co3 {
            ext_bufs.push(co3 as *mut sys::mfxExtCodingOption3 as *mut sys::mfxExtBuffer);
        }
        if !ext_bufs.is_empty() {
            video_param.ExtParam = ext_bufs.as_mut_ptr();
            video_param.NumExtParam = ext_bufs.len() as u16;
        }

        lib.mfx_video_encode_init(session, &mut video_param)?;

        // Init 後に ExtParam ポインタをクリアする（ローカルの ext_bufs が drop されるため）
        video_param.ExtParam = std::ptr::null_mut();
        video_param.NumExtParam = 0;

        // エンコーダ初期化後のガード（エラー時に MFXVideoENCODE_Close を呼ぶ）
        let encoder_guard = CloseGuard::encoder(lib, loader, session);

        let bitstream_buffer = vec![0u8; (buffer_size_in_kb as usize) * 1024];

        // ガードをキャンセルして所有権を Encoder に移す
        encoder_guard.cancel();
        session_guard.cancel();

        Ok(Encoder {
            lib,
            loader,
            session,
            video_param,
            frame_info,
            frame_format: config.frame_format,
            crop_w: config.width as usize,
            crop_h: config.height as usize,
            bitstream_buffer,
            encoded_frames: VecDeque::new(),
            frame_count: 0,
            framerate_den: config.framerate_den as u64,
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
        let status = self.lib.mfx_video_encode_query(self.session, input, output);
        Error::check_mfx_allow_warn(status, "MFXVideoENCODE_Query")
    }

    /// エンコーダパラメータを動的に変更する
    ///
    /// MFXVideoENCODE_Reset を呼び出す。ビットレートやフレームレートの変更に使用する。
    pub fn reconfigure(&mut self, params: ReconfigureParams) -> Result<(), Error> {
        // 現在の video_param をベースに変更を適用する
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

        self.lib
            .mfx_video_encode_reset(self.session, &mut self.video_param)
    }

    /// Init 後の実効パラメータを取得する
    ///
    /// MFXVideoENCODE_GetVideoParam を呼び出す。
    /// エンコーダが実際に使用しているパラメータを確認するために使用する。
    ///
    /// 戻り値の [`ffi::mfxVideoParam`](crate::ffi::mfxVideoParam) は bindgen 生成の型で、
    /// VPL API の構造体に直接対応する。
    pub fn get_video_param(&self) -> Result<sys::mfxVideoParam, Error> {
        let mut param: sys::mfxVideoParam = unsafe { std::mem::zeroed() };
        // 現在のパラメータをコピーしてから GetVideoParam で上書きする
        param.IOPattern = self.video_param.IOPattern;
        self.lib
            .mfx_video_encode_get_video_param(self.session, &mut param)?;
        Ok(param)
    }

    /// エンコード統計情報を取得する
    pub fn get_encode_stat(&self) -> Result<EncoderStats, Error> {
        let mut stat: sys::mfxEncodeStat = unsafe { std::mem::zeroed() };
        self.lib
            .mfx_video_encode_get_encode_stat(self.session, &mut stat)?;
        Ok(EncoderStats {
            num_frame: stat.NumFrame,
            num_bit: stat.NumBit,
            num_cached_frame: stat.NumCachedFrame,
        })
    }

    /// フレームをエンコードする
    pub fn encode(&mut self, frame_data: &[u8], options: &EncodeOptions) -> Result<(), Error> {
        // フレームサイズを検証する
        let expected = self.frame_format.frame_size(self.crop_w, self.crop_h);
        if frame_data.len() < expected {
            return Err(Error::new_custom(
                "Encoder::encode",
                "frame data is too small for the specified frame format",
            ));
        }

        // mfxFrameSurface1 を設定する
        let mut surface: sys::mfxFrameSurface1 = unsafe { std::mem::zeroed() };
        surface.Info = self.frame_info;
        surface.Data.TimeStamp = self.frame_count * self.framerate_den;
        // VPL API は *mut を要求するが入力データを書き換えないためキャストする
        unsafe {
            self.frame_format.set_planes(
                &mut surface.Data,
                frame_data.as_ptr() as *mut u8,
                self.crop_w,
                self.crop_h,
            );
        }

        // エンコード制御を設定する
        let mut ctrl: sys::mfxEncodeCtrl = unsafe { std::mem::zeroed() };
        let ctrl_ptr = if options.frame_type != 0 {
            ctrl.FrameType = options.frame_type;
            &mut ctrl as *mut sys::mfxEncodeCtrl
        } else {
            std::ptr::null_mut()
        };

        let mut bitstream: sys::mfxBitstream = unsafe { std::mem::zeroed() };
        bitstream.Data = self.bitstream_buffer.as_mut_ptr();
        bitstream.MaxLength = self.bitstream_buffer.len() as u32;

        let Some(syncp) = self.encode_frame_async(ctrl_ptr, &mut surface, &mut bitstream)? else {
            // 出力なし（通常の動作）
            self.frame_count += 1;
            return Ok(());
        };

        self.sync_and_collect(&bitstream, syncp, surface.Data.TimeStamp)?;
        self.frame_count += 1;
        Ok(())
    }

    /// バッファに蓄積されたエンコード済みフレームを取り出す
    pub fn next_frame(&mut self) -> Option<EncodedFrame> {
        self.encoded_frames.pop_front()
    }

    /// エンコーダをフラッシュして残りのフレームを取得する
    pub fn finish(&mut self) -> Result<(), Error> {
        loop {
            let mut bitstream: sys::mfxBitstream = unsafe { std::mem::zeroed() };
            bitstream.Data = self.bitstream_buffer.as_mut_ptr();
            bitstream.MaxLength = self.bitstream_buffer.len() as u32;

            let Some(syncp) = self.encode_frame_async(
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut bitstream,
            )?
            else {
                // すべて排出済み
                break;
            };

            self.sync_and_collect(&bitstream, syncp, 0)?;
        }
        Ok(())
    }

    /// EncodeFrameAsync をデバイスビジー時に再試行する
    ///
    /// None = MORE_DATA（出力なし）、Some(syncp) = エンコード完了
    fn encode_frame_async(
        &mut self,
        ctrl: *mut sys::mfxEncodeCtrl,
        surface: *mut sys::mfxFrameSurface1,
        bitstream: &mut sys::mfxBitstream,
    ) -> Result<Option<sys::mfxSyncPoint>, Error> {
        for _ in 0..DEVICE_BUSY_MAX_RETRIES {
            let mut syncp: sys::mfxSyncPoint = std::ptr::null_mut();
            let status = self.lib.mfx_video_encode_frame_async(
                self.session,
                ctrl,
                surface,
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

    /// SyncOperation を実行してエンコード済みフレームを収集する
    fn sync_and_collect(
        &mut self,
        bitstream: &sys::mfxBitstream,
        syncp: sys::mfxSyncPoint,
        timestamp: u64,
    ) -> Result<(), Error> {
        if syncp.is_null() {
            return Ok(());
        }

        Error::check_mfx(
            self.lib
                .mfx_video_core_sync_operation(self.session, syncp, sys::MFX_INFINITE),
            "MFXVideoCORE_SyncOperation",
        )?;

        let offset = bitstream.DataOffset as usize;
        let length = bitstream.DataLength as usize;
        if length == 0 {
            return Ok(());
        }

        // VPL が返したオフセットと長さがバッファ範囲内か検証する
        let end = offset.checked_add(length).ok_or_else(|| {
            Error::new_custom_owned(
                "Encoder::encode",
                format!("bitstream offset ({offset}) + length ({length}) overflows usize",),
            )
        })?;
        if end > self.bitstream_buffer.len() {
            return Err(Error::new_custom_owned(
                "Encoder::encode",
                format!(
                    "bitstream range {}..{} exceeds buffer size {}",
                    offset,
                    end,
                    self.bitstream_buffer.len()
                ),
            ));
        }

        let data = self.bitstream_buffer[offset..end].to_vec();
        let frame_type = bitstream.FrameType;
        let picture_type = if frame_type & (sys::MFX_FRAMETYPE_IDR as u16) != 0 {
            PictureType::Idr
        } else if frame_type & (sys::MFX_FRAMETYPE_I as u16) != 0 {
            PictureType::I
        } else if frame_type & (sys::MFX_FRAMETYPE_P as u16) != 0 {
            PictureType::P
        } else if frame_type & (sys::MFX_FRAMETYPE_B as u16) != 0 {
            PictureType::B
        } else {
            PictureType::Unknown
        };

        self.encoded_frames.push_back(EncodedFrame {
            data,
            timestamp,
            picture_type,
        });
        Ok(())
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        let _ = self.lib.mfx_video_encode_close(self.session);
        let _ = self.lib.mfx_close(self.session);
        self.lib.mfx_unload(self.loader);
    }
}

/// セッションまたはエンコーダの解放ガード（エラー時に MFXClose / MFXVideoENCODE_Close / MFXUnload を呼ぶ）
struct CloseGuard {
    lib: VplLibrary,
    loader: sys::mfxLoader,
    session: sys::mfxSession,
    active: bool,
    close_encoder: bool,
}

impl CloseGuard {
    fn session(lib: VplLibrary, loader: sys::mfxLoader, session: sys::mfxSession) -> Self {
        Self {
            lib,
            loader,
            session,
            active: true,
            close_encoder: false,
        }
    }

    fn encoder(lib: VplLibrary, loader: sys::mfxLoader, session: sys::mfxSession) -> Self {
        Self {
            lib,
            loader,
            session,
            active: true,
            close_encoder: true,
        }
    }

    fn cancel(mut self) {
        self.active = false;
    }
}

impl Drop for CloseGuard {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        if self.close_encoder {
            let _ = self.lib.mfx_video_encode_close(self.session);
        }
        let _ = self.lib.mfx_close(self.session);
        self.lib.mfx_unload(self.loader);
    }
}

/// コーデック ID を返す
fn codec_id(codec: &CodecConfig) -> u32 {
    match codec {
        CodecConfig::H264(_) => sys::MFX_CODEC_AVC,
        CodecConfig::Hevc(_) => sys::MFX_CODEC_HEVC,
        CodecConfig::Vp9(_) => sys::MFX_CODEC_VP9,
        CodecConfig::Av1(_) => sys::MFX_CODEC_AV1,
    }
}

/// コーデックプロファイルを返す
fn codec_profile(codec: &CodecConfig) -> u16 {
    match codec {
        CodecConfig::H264(c) => match c.profile {
            Some(H264Profile::Baseline) => sys::MFX_PROFILE_AVC_BASELINE as u16,
            Some(H264Profile::ConstrainedBaseline) => {
                sys::MFX_PROFILE_AVC_CONSTRAINED_BASELINE as u16
            }
            Some(H264Profile::Main) => sys::MFX_PROFILE_AVC_MAIN as u16,
            Some(H264Profile::High) => sys::MFX_PROFILE_AVC_HIGH as u16,
            Some(H264Profile::ConstrainedHigh) => sys::MFX_PROFILE_AVC_CONSTRAINED_HIGH as u16,
            Some(H264Profile::High10) => sys::MFX_PROFILE_AVC_HIGH10 as u16,
            Some(H264Profile::High422) => sys::MFX_PROFILE_AVC_HIGH_422 as u16,
            None => sys::MFX_PROFILE_UNKNOWN as u16,
        },
        CodecConfig::Hevc(c) => match c.profile {
            Some(HevcProfile::Main) => sys::MFX_PROFILE_HEVC_MAIN as u16,
            Some(HevcProfile::Main10) => sys::MFX_PROFILE_HEVC_MAIN10 as u16,
            Some(HevcProfile::MainSp) => sys::MFX_PROFILE_HEVC_MAINSP as u16,
            Some(HevcProfile::Rext) => sys::MFX_PROFILE_HEVC_REXT as u16,
            Some(HevcProfile::Scc) => sys::MFX_PROFILE_HEVC_SCC as u16,
            None => sys::MFX_PROFILE_UNKNOWN as u16,
        },
        CodecConfig::Vp9(c) => match c.profile {
            Some(Vp9Profile::Profile0) => sys::MFX_PROFILE_VP9_0 as u16,
            Some(Vp9Profile::Profile1) => sys::MFX_PROFILE_VP9_1 as u16,
            Some(Vp9Profile::Profile2) => sys::MFX_PROFILE_VP9_2 as u16,
            Some(Vp9Profile::Profile3) => sys::MFX_PROFILE_VP9_3 as u16,
            None => sys::MFX_PROFILE_UNKNOWN as u16,
        },
        CodecConfig::Av1(c) => match c.profile {
            Some(Av1Profile::Main) => sys::MFX_PROFILE_AV1_MAIN as u16,
            None => sys::MFX_PROFILE_UNKNOWN as u16,
        },
    }
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
