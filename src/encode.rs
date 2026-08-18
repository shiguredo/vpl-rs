use std::collections::HashMap;
use std::sync::mpsc;
use std::thread;

use crate::vpl::{FrameSurface, Session, VplLibrary};
use crate::{AdapterSelector, Error, sys};

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
    ///
    /// オーバーフロー時は `None` を返す。
    pub fn frame_size(self, width: usize, height: usize) -> Option<usize> {
        let pixels = width.checked_mul(height)?;
        match self {
            // YUV 4:2:0 8bit: Y + UV/2 = pixels * 3 / 2
            FrameFormat::Nv12 => pixels.checked_mul(3).map(|v| v / 2),
            // YUY2: 2 bytes/pixel (packed YUYV)
            FrameFormat::Yuy2 => pixels.checked_mul(2),
            // BGRA: 4 bytes/pixel
            FrameFormat::Bgra => pixels.checked_mul(4),
        }
    }

    /// フレームデータを内部サーフェスの mfxFrameData にコピーする
    ///
    /// # Safety
    ///
    /// `data` の各プレーンポインタは有効なメモリを指し、Pitch 幅分の書き込みが可能である必要がある。
    /// `src` は `frame_size(coded_width, coded_height)` バイト以上である必要がある。
    unsafe fn copy_to_surface_planes(
        self,
        src: &[u8],
        data: &sys::mfxFrameData,
        coded_width: usize,
        coded_height: usize,
    ) {
        unsafe {
            let pitch = data.__bindgen_anon_2.Pitch as usize;
            match self {
                FrameFormat::Nv12 => {
                    let y_ptr = data.__bindgen_anon_3.Y;
                    let uv_ptr = data.__bindgen_anon_4.UV;
                    let luma_size = coded_width * coded_height;
                    // Y プレーンをコピーする
                    for row in 0..coded_height {
                        std::ptr::copy_nonoverlapping(
                            src.as_ptr().add(row * coded_width),
                            y_ptr.add(row * pitch),
                            coded_width,
                        );
                    }
                    // UV プレーンをコピーする
                    let uv_src = src.as_ptr().add(luma_size);
                    let uv_height = coded_height / 2;
                    for row in 0..uv_height {
                        std::ptr::copy_nonoverlapping(
                            uv_src.add(row * coded_width),
                            uv_ptr.add(row * pitch),
                            coded_width,
                        );
                    }
                }
                FrameFormat::Yuy2 => {
                    let y_ptr = data.__bindgen_anon_3.Y;
                    let row_bytes = coded_width * 2;
                    for row in 0..coded_height {
                        std::ptr::copy_nonoverlapping(
                            src.as_ptr().add(row * row_bytes),
                            y_ptr.add(row * pitch),
                            row_bytes,
                        );
                    }
                }
                FrameFormat::Bgra => {
                    let b_ptr = data.__bindgen_anon_5.B;
                    let row_bytes = coded_width * 4;
                    for row in 0..coded_height {
                        std::ptr::copy_nonoverlapping(
                            src.as_ptr().add(row * row_bytes),
                            b_ptr.add(row * pitch),
                            row_bytes,
                        );
                    }
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
#[non_exhaustive]
pub struct EncoderConfig {
    // --- アダプタ選択 ---
    /// 使用する Intel HW アダプタの指定（DRM render node 番号など）
    pub adapter: AdapterSelector,

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
    WaitIdle(mpsc::Sender<Result<(), Error>>),
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

/// エンコーダ
pub struct Encoder<H: EncodeHandler> {
    session: Session,
    video_param: sys::mfxVideoParam,
    frame_info: sys::mfxFrameInfo,
    frame_format: FrameFormat,
    write_ivf_headers: bool,
    bitstream_buffer_size: usize,
    worker_tx: mpsc::Sender<WorkerCommand<H::UserData>>,
    worker_handle: Option<thread::JoinHandle<()>>,
    frame_count: u64,
    framerate_den: u64,
}

// Safety: エンコード完了通知は専用スレッドで行い、メインスレッドは EncodeFrameAsync のみ実行する。
// VPL 仕様上、セッション操作の同一スレッド制約は明記されておらず、公式サンプルでも
// セッションハンドルにスレッドアフィニティの制約は課されていない。
// Sync は実装しない（生ポインタにより自動的に !Sync）。
unsafe impl<H: EncodeHandler> Send for Encoder<H> {}

impl<H: EncodeHandler> Encoder<H> {
    /// エンコーダを作成する
    pub fn new(config: EncoderConfig, handler: H) -> Result<Self, Error> {
        // 寸法を u16 へ変換する前に範囲を検証する
        if config.width == 0 || config.height == 0 {
            return Err(Error::new_custom(
                "Encoder::new",
                "width and height must be non-zero",
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
        // pitch（行あたりのバイト数）が u16 に収まるか検証する
        // NV12: width, YUY2: width * 2, BGRA: width * 4
        let pitch_bytes: u64 = match config.frame_format {
            FrameFormat::Nv12 => aligned_width as u64,
            FrameFormat::Yuy2 => aligned_width as u64 * 2,
            FrameFormat::Bgra => aligned_width as u64 * 4,
        };
        if pitch_bytes > u16::MAX as u64 {
            return Err(Error::new_custom_owned(
                "Encoder::new",
                format!(
                    "pitch ({pitch_bytes} bytes) for {:?} with width {} exceeds u16::MAX",
                    config.frame_format, aligned_width
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
        // AsyncDepth は設定値またはデフォルト 4 を使用する。
        // libvpl のガイドでは、1 は最小メモリだが性能が低く、4 は高スループット寄りの推奨値とされる。
        // ref:
        // - doc/spec/source/programming_guide/VPL_prg_decoding.rst (AsyncDepth Specific Details)
        // - doc/spec/source/programming_guide/VPL_prg_transcoding.rst (Operation sequence)
        video_param.AsyncDepth = config.async_depth.unwrap_or(4);
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
        let mut ext_vp9: Option<sys::mfxExtVP9Param> = None;

        // VP9 の場合、IVF ヘッダー出力の要求値 (write_ivf_headers) を MFX_CODINGOPTION_* の値へ変換して
        // mfxExtVP9Param を構築する。非 VP9 コーデックでは write_ivf_headers は false 固定とし、
        // ext_vp9 は構築しない。
        //
        // WriteIVFHeaders は CodingOptionValue (MFX_CODINGOPTION_ON/OFF) で指定する。
        // ref:
        // - libvpl api/vpl/mfxstructures.h (mfxExtVP9Param::WriteIVFHeaders)
        // - libvpl api/vpl/mfxstructures.h (CodingOptionValue)
        let write_ivf_headers_value: Option<u16> = match &config.codec {
            CodecConfig::Vp9(vp9) => {
                let value = if vp9.write_ivf_headers {
                    sys::MFX_CODINGOPTION_ON as u16
                } else {
                    sys::MFX_CODINGOPTION_OFF as u16
                };
                let mut vp9_param: sys::mfxExtVP9Param = unsafe { std::mem::zeroed() };
                vp9_param.Header.BufferId = sys::MFX_EXTBUFF_VP9_PARAM;
                vp9_param.Header.BufferSz = std::mem::size_of::<sys::mfxExtVP9Param>() as u32;
                vp9_param.WriteIVFHeaders = value;
                ext_vp9 = Some(vp9_param);
                Some(value)
            }
            _ => None,
        };
        // getter が返す bool は要求値が ON かどうかで決まる
        let write_ivf_headers = write_ivf_headers_value == Some(sys::MFX_CODINGOPTION_ON as u16);

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
        if let Some(ref mut vp9) = ext_vp9 {
            ext_bufs.push(vp9 as *mut sys::mfxExtVP9Param as *mut sys::mfxExtBuffer);
        }
        if !ext_bufs.is_empty() {
            video_param.ExtParam = ext_bufs.as_mut_ptr();
            video_param.NumExtParam = ext_bufs.len() as u16;
        }

        lib.mfx_video_encode_init(session.as_ptr(), &mut video_param)?;

        let lib = session.lib();
        let session_ptr = session.as_ptr();

        // Init 後に ExtParam ポインタをクリアする（ローカルの ext_bufs が drop されるため）
        video_param.ExtParam = std::ptr::null_mut();
        video_param.NumExtParam = 0;

        // 初期化後に実効パラメータを読み戻す。
        // VP9 の場合、write_ivf_headers の要求値が oneVPL に尊重されるかを検証するため、
        // 読み戻し専用の mfxExtVP9Param を attach して GetVideoParam を呼ぶ。
        //
        // GetVideoParam は mfxVideoParam に attach した拡張バッファへ実効値を書き戻す。
        // 読み戻し用バッファの WriteIVFHeaders は zeroed 初期化により UNKNOWN (0) のままにしておき、
        // 書き戻されなかった場合に下の一致検証で必ず検出できるようにする。
        // ref:
        // - libvpl api/vpl/mfxvideo.h (MFXVideoENCODE_GetVideoParam の拡張バッファ説明)
        let mut ext_vp9_readback: Option<sys::mfxExtVP9Param> = None;
        if matches!(&config.codec, CodecConfig::Vp9(_)) {
            let mut vp9_param: sys::mfxExtVP9Param = unsafe { std::mem::zeroed() };
            vp9_param.Header.BufferId = sys::MFX_EXTBUFF_VP9_PARAM;
            vp9_param.Header.BufferSz = std::mem::size_of::<sys::mfxExtVP9Param>() as u32;
            ext_vp9_readback = Some(vp9_param);
        }

        // 読み戻し用の拡張バッファポインタ配列を構築する
        let mut ext_bufs_readback: Vec<*mut sys::mfxExtBuffer> = Vec::new();
        if let Some(ref mut vp9) = ext_vp9_readback {
            ext_bufs_readback.push(vp9 as *mut sys::mfxExtVP9Param as *mut sys::mfxExtBuffer);
        }
        if !ext_bufs_readback.is_empty() {
            video_param.ExtParam = ext_bufs_readback.as_mut_ptr();
            video_param.NumExtParam = ext_bufs_readback.len() as u16;
        }

        lib.mfx_video_encode_get_video_param(session_ptr, &mut video_param)
            .inspect_err(|_| {
                // GetVideoParam 失敗時は MFXVideoENCODE_Close を呼んでから session を Drop させる
                let _ = lib.mfx_video_encode_close(session_ptr);
            })?;
        video_param.ExtParam = std::ptr::null_mut();
        video_param.NumExtParam = 0;
        frame_info = unsafe { video_param.__bindgen_anon_1.mfx.FrameInfo };

        // VP9 の場合、WriteIVFHeaders の実効値が要求値と完全一致することを検証する。
        // 一致しない場合（UNKNOWN を含む）は oneVPL が要求を尊重しなかったことを意味するため、
        // エンコーダをクローズしてエラーを返す。
        if let Some(expected) = write_ivf_headers_value
            && let Some(vp9) = ext_vp9_readback
            && vp9.WriteIVFHeaders != expected
        {
            let _ = lib.mfx_video_encode_close(session_ptr);
            return Err(Error::new_custom_owned(
                "Encoder::new",
                format!(
                    "VP9 WriteIVFHeaders mismatch: requested {} but got {}",
                    coding_option_name(expected),
                    coding_option_name(vp9.WriteIVFHeaders),
                ),
            ));
        }

        let bitstream_buffer_size = (buffer_size_in_kb as usize) * 1024;
        let (worker_tx, worker_rx) = mpsc::channel();
        let session_handle = session_ptr as usize;
        let worker_handle = thread::Builder::new()
            .name("vpl-encoder-sync".to_owned())
            .spawn(move || {
                run_sync_worker(lib, session_handle, worker_rx, handler);
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
            frame_format: config.frame_format,
            write_ivf_headers,
            bitstream_buffer_size,
            worker_tx,
            worker_handle: Some(worker_handle),
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
        let status =
            self.session
                .lib()
                .mfx_video_encode_query(self.session.as_ptr(), input, output);
        Error::check_mfx_allow_warn(status, "MFXVideoENCODE_Query")
    }

    /// エンコーダパラメータを動的に変更する
    ///
    /// MFXVideoENCODE_Reset を呼び出す。ビットレートやフレームレートの変更に使用する。
    pub fn reconfigure(&mut self, params: ReconfigureParams) -> Result<(), Error> {
        if let Some(0) = params.framerate_den {
            return Err(Error::new_custom(
                "Encoder::reconfigure",
                "framerate_den must be non-zero",
            ));
        }

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

        self.session
            .lib()
            .mfx_video_encode_reset(self.session.as_ptr(), &mut self.video_param)
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
        self.session
            .lib()
            .mfx_video_encode_get_video_param(self.session.as_ptr(), &mut param)?;
        Ok(param)
    }

    /// エンコーダが要求する coded フレームサイズ（`FrameInfo::Width/Height`）を取得する
    pub fn coded_size(&self) -> (usize, usize) {
        let fi = unsafe { self.frame_info.__bindgen_anon_1.__bindgen_anon_1 };
        (usize::from(fi.Width), usize::from(fi.Height))
    }

    /// IVF ヘッダーを出力する設定かどうかを返す
    ///
    /// `Vp9EncoderConfig::write_ivf_headers` で指定した値（oneVPL へ要求し、初期化時に実効値との一致を検証済みの値）を返す。
    /// 非 VP9 コーデックでは常に `false` を返す。
    ///
    /// 実効値の一致検証は GetVideoParam が拡張バッファへ実効値を書き戻す挙動に依存している。
    /// この挙動は libvpl の実装由来で、将来のバージョンで変更される可能性がある。
    pub fn write_ivf_headers(&self) -> bool {
        self.write_ivf_headers
    }

    /// エンコード統計情報を取得する
    pub fn get_encode_stat(&self) -> Result<EncoderStats, Error> {
        let mut stat: sys::mfxEncodeStat = unsafe { std::mem::zeroed() };
        self.session
            .lib()
            .mfx_video_encode_get_encode_stat(self.session.as_ptr(), &mut stat)?;
        Ok(EncoderStats {
            num_frame: stat.NumFrame,
            num_bit: stat.NumBit,
            num_cached_frame: stat.NumCachedFrame,
        })
    }

    /// フレームをエンコードする
    pub fn encode(
        &mut self,
        frame_data: &[u8],
        user_data: H::UserData,
        options: &EncodeOptions,
    ) -> Result<(), Error> {
        // フレームサイズを検証する
        let (coded_width, coded_height) = self.coded_size();
        let expected = self
            .frame_format
            .frame_size(coded_width, coded_height)
            .ok_or_else(|| {
                Error::new_custom("Encoder::encode", "frame size calculation overflowed")
            })?;
        if frame_data.len() < expected {
            return Err(Error::new_custom_owned(
                "Encoder::encode",
                format!(
                    "frame data is too small for coded size {}x{} (got {}, need at least {})",
                    coded_width,
                    coded_height,
                    frame_data.len(),
                    expected
                ),
            ));
        }

        let frame_seq = self.frame_count;
        // 公開 API 向けのタイムスタンプは従来どおり frame_rate 由来で計算する。
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

        // フレームデータを内部サーフェスにコピーする
        unsafe {
            (*frame_surface.as_ptr()).Data.TimeStamp = frame_seq;
            self.frame_format.copy_to_surface_planes(
                &frame_data[..expected],
                &(*frame_surface.as_ptr()).Data,
                coded_width,
                coded_height,
            );
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

        // syncp が None の入力でも、後続出力との対応付けのため pending は必ず登録する。
        self.send_worker_command(
            "Encoder::encode",
            WorkerCommand::QueueFrame {
                frame_seq,
                pending_frame: PendingFrame {
                    presentation_timestamp,
                    user_data,
                },
            },
        )?;

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

    /// エンコーダをフラッシュして残りのフレームを取得する
    ///
    /// この関数は全てのエンコードの完了を待ち、内部で保持している
    /// フレームをすべて排出してコールバックを呼び出し終わるまでブロックする。
    pub fn finish(&mut self) -> Result<(), Error> {
        loop {
            // null surface でエンコードを呼び出して、残りのフレームをすべて排出する
            let (mut bitstream, bitstream_buffer) = self.create_bitstream();

            let Some(syncp) =
                self.encode_frame_async(std::ptr::null_mut(), None, bitstream.as_mut())?
            else {
                // すべて排出済み
                break;
            };

            self.send_worker_command(
                "Encoder::finish",
                WorkerCommand::Sync(SyncData {
                    syncp,
                    bitstream,
                    bitstream_buffer,
                }),
            )?;
        }

        // ここまでに送った Sync が worker 側で全て処理されるまで待つ。
        let (tx, rx) = mpsc::channel();
        self.send_worker_command("Encoder::finish", WorkerCommand::WaitIdle(tx))?;
        rx.recv().map_err(|_| {
            Error::new_custom("Encoder::finish", "sync worker thread stopped unexpectedly")
        })?
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
                    handler.on_encoded(Err(Error::new_custom_owned(
                        "Encoder::sync_worker",
                        format!("duplicate frame sequence in pending frames: {frame_seq}"),
                    )
                    .into()));
                }
            }
            WorkerCommand::Sync(sync) => {
                // 通常入力の sync 完了を待ち、bitstream.TimeStamp と frame_seq を完全一致で対応付ける。
                handler.on_encoded(
                    sync_and_build_frame(lib, session_handle, sync, &mut pending_store)
                        .map_err(Into::into),
                );
            }
            WorkerCommand::WaitIdle(reply_tx) => {
                // finish 側のバリア。ここに到達した時点で、それ以前に送信された
                // QueueFrame / Sync はすべて処理済みである。
                if pending_store.is_empty() {
                    let _ = reply_tx.send(Ok(()));
                    continue;
                }

                let pending = pending_store.drain_all();
                let remaining_count = pending.len();
                for (frame_seq, _meta) in pending {
                    handler
                        .on_encoded(Err(finish_pending_error(frame_seq, remaining_count).into()));
                }
                let _ = reply_tx.send(Err(Error::new_custom_owned(
                    "Encoder::finish",
                    format!("finish completed but {remaining_count} pending frames remained"),
                )));
            }
            WorkerCommand::Stop => {
                // drop 時の中断。未完了 frame はすべて MFX_ERR_ABORTED として通知する。
                for (_frame_seq, _pending) in pending_store.drain_all() {
                    handler.on_encoded(Err(canceled_error().into()));
                }
                break;
            }
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
/// 取り出し時は `DataOffset` / `DataLength` の範囲検証を行い、
/// 不正なオフセットでメモリアクセスしないように防御する。
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

    // VPL のドレイン処理では syncp は返るが DataLength == 0 となるケースがあるため、
    // 空ビットストリームをエラーではなく空データとして正常に処理する。
    if length == 0 {
        return Ok(SyncedBitstream {
            data: vec![],
            frame_seq: bitstream.TimeStamp,
            picture_type: picture_type_from_frame_type(bitstream.FrameType),
        });
    }

    // VPL が返したオフセットと長さがバッファ範囲内か検証する
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

fn finish_pending_error(frame_seq: u64, pending_count: usize) -> Error {
    Error::new_custom_owned(
        "Encoder::finish",
        format!(
            "pending frames remained after flush for frame sequence {frame_seq} (pending count: {pending_count})"
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

/// MFX_CODINGOPTION_* の数値を記号名へ変換する
///
/// CodingOptionValue (mfxstructures.h) の値をエラーメッセージで判読可能にするための表示用関数。
/// 想定外の値は unknown (0xNN) で表示する。
fn coding_option_name(value: u16) -> String {
    match value {
        v if v == sys::MFX_CODINGOPTION_UNKNOWN as u16 => "MFX_CODINGOPTION_UNKNOWN".to_owned(),
        v if v == sys::MFX_CODINGOPTION_ON as u16 => "MFX_CODINGOPTION_ON".to_owned(),
        v if v == sys::MFX_CODINGOPTION_OFF as u16 => "MFX_CODINGOPTION_OFF".to_owned(),
        v if v == sys::MFX_CODINGOPTION_ADAPTIVE as u16 => "MFX_CODINGOPTION_ADAPTIVE".to_owned(),
        v => format!("unknown ({v:#x})"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

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
        let (command_tx, command_rx) = mpsc::channel();
        let (callback_tx, callback_rx) = mpsc::channel::<Result<(), Error>>();
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
            );
        });

        command_tx
            .send(WorkerCommand::QueueFrame {
                frame_seq: 77,
                pending_frame: make_pending(7700, 7),
            })
            .expect("failed to send QueueFrame");
        let (reply_tx, reply_rx) = mpsc::channel();
        command_tx
            .send(WorkerCommand::WaitIdle(reply_tx))
            .expect("failed to send WaitIdle");
        let wait_result = reply_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("failed to receive WaitIdle response");
        assert!(
            wait_result.is_err(),
            "WaitIdle should fail when pending frames remain"
        );

        let callback_result = callback_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("failed to receive callback result");
        assert!(
            callback_result.is_err(),
            "pending frames on WaitIdle must be reported as callback error"
        );

        command_tx
            .send(WorkerCommand::Stop)
            .expect("failed to send Stop");
        worker.join().expect("worker thread panicked");
    }

    #[test]
    fn worker_stop_returns_aborted_for_all_pending() {
        let (command_tx, command_rx) = mpsc::channel();
        let (callback_tx, callback_rx) = mpsc::channel::<Result<(), Error>>();
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
            );
        });

        command_tx
            .send(WorkerCommand::QueueFrame {
                frame_seq: 1,
                pending_frame: make_pending(100, 10),
            })
            .expect("failed to send QueueFrame");
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
    }
}
