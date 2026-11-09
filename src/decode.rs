use std::collections::HashMap;
use std::sync::{Arc, mpsc};
use std::thread;

use crate::codec_info::{FrameData, FrameFormat, VideoCodecType};
use crate::stats::{Counter, Gauge};
use crate::vpl::{FrameSurface, Session, VplLibrary, frame_data_pitch};
use crate::{AdapterSelector, Error, sys};

/// デバイスビジー時の最大リトライ回数
const DEVICE_BUSY_MAX_RETRIES: u32 = 30;

/// デコーダの設定
///
/// デコードするビットストリームのコーデックと使用するアダプタを指定する。
/// 解像度やフレームレートはビットストリームのヘッダから自動的に検出される。
#[derive(Debug, Clone)]
pub struct DecoderConfig {
    /// 使用する Intel HW アダプタの指定（DRM render node 番号など）
    pub adapter: AdapterSelector,
    /// コーデック識別子
    pub codec: VideoCodecType,
    /// 非同期深度（mfxVideoParam.AsyncDepth）
    ///
    /// 1 = 最小メモリだが性能が低い。4 = 高スループット寄りの推奨値。
    /// None の場合は 4（推奨値）を使用する。
    pub async_depth: Option<u16>,
}

impl DecoderConfig {
    /// 必須パラメータのみ指定して DecoderConfig を作成する
    ///
    /// オプションパラメータはすべて None (デコーダのデフォルト) に設定される。
    pub fn new(adapter: AdapterSelector, codec: VideoCodecType) -> Self {
        Self {
            adapter,
            codec,
            async_depth: None,
        }
    }
}

/// デコードされたフレーム
///
/// 出力形式はストリームのネイティブ形式で決まる。8bit 4:2:0 は NV12、10bit 4:2:0 は
/// P010、8bit 4:4:4 は AYUV、10bit 4:4:4 は Y410 になる。形式は
/// [`DecodedFrame::frame_format`] で取得できる。これら以外の形式 (12bit の P016 など) は
/// デコード時にエラーになる。
///
/// 画素データは [`DecodedFrame::data`] が返す [`FrameData`] で取得する。
/// NV12 / P010 は [`FrameData::SemiPlanar`]、AYUV / Y410 は [`FrameData::Packed`] になる。
/// スライスはピッチ幅を含む生データであり、各行の先頭 `FrameFormat::bytes_per_row(width())`
/// バイトだけが有効データで、残りはパディングとなる。
///
/// # ライフタイム
///
/// `data()` が返すスライスはコールバック呼び出し中のみ有効。
/// コールバックの外に持ち出そうとするとコンパイルエラーになる。
pub struct DecodedFrame<'a, T> {
    data: FrameData<'a>,
    frame_format: FrameFormat,
    width: usize,
    height: usize,
    user_data: T,
}

impl<'a, T> DecodedFrame<'a, T> {
    /// デコード出力の色形式を返す
    ///
    /// 返る値は [FrameFormat::Nv12] / [FrameFormat::P010] / [FrameFormat::Ayuv] /
    /// [FrameFormat::Y410] のいずれかである。
    pub fn frame_format(&self) -> FrameFormat {
        self.frame_format
    }

    /// 画素データを返す
    ///
    /// `SemiPlanar` の Y プレーンは `y_pitch * height()` バイト、UV プレーンは
    /// `uv_pitch * (height() / 2)` バイトである。`Packed` は `pitch * height()` バイトである。
    pub fn data(&self) -> FrameData<'a> {
        self.data
    }

    /// フレームのクロップ幅を返す
    pub fn width(&self) -> usize {
        self.width
    }

    /// フレームのクロップ高さを返す
    pub fn height(&self) -> usize {
        self.height
    }

    /// デコード時に渡した値を取得する
    pub fn user_data(&self) -> &T {
        &self.user_data
    }

    /// デコード時に渡した値を取得する（所有権を移動）
    pub fn into_user_data(self) -> T {
        self.user_data
    }
}

/// フレームレート
///
/// `num / den` でフレームレートを表す。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Framerate {
    /// 分子
    pub num: u32,
    /// 分母
    pub den: u32,
}

/// ストリームのコーデック・解像度・フレームレートを表す情報
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamInfo {
    /// ストリームのコーデック種別
    pub codec: VideoCodecType,
    /// coded 幅 (ピクセル)
    ///
    /// VPL が返すアライン済みのフレーム幅
    pub coded_width: usize,
    /// coded 高さ (ピクセル)
    ///
    /// VPL が返すアライン済みのフレーム高さ
    pub coded_height: usize,
    /// 表示幅 (ピクセル)
    pub width: usize,
    /// 表示高さ (ピクセル)
    pub height: usize,
    /// フレームレート
    ///
    /// ビットストリームにフレームレートの情報が無い場合は `None`。
    /// `Some` である場合、`framerate.num` と `framerate.den` は必ず 0 以外の値になる
    pub framerate: Option<Framerate>,
}

/// デコーダーの統計値
///
/// [`Decoder::stats`] が返す統計値である。値はデコード処理の進行に伴って
/// 更新される。
///
/// `Clone` は各フィールドの現在値を個別にコピーするスナップショットを返すため、
/// フィールド間の一貫性は保証されない。
#[derive(Debug, Clone, Default)]
pub struct DecoderStats {
    /// [`Decoder::decode`] が入力を pending に登録した通算回数
    ///
    /// 入力データの長さ検証、デコーダーの初期化、pending の登録に失敗した
    /// 呼び出しは計上しない。pending の登録後にデコードが失敗した場合と、
    /// ビットストリームが空の場合も計上する。
    pub total_decode_count: Counter,

    /// [`DecodeHandler::on_decoded`] に `Ok` を渡した通算回数
    pub total_output_frame_count: Counter,

    /// [`DecodeHandler::on_decoded`] に `Err` を渡した通算回数
    ///
    /// [`Decoder::finish`] が残留フレームをエラー通知した場合も含む。
    pub total_error_count: Counter,

    /// 受理したが、まだ結果を通知していないフレーム数の現在値
    ///
    /// [`Decoder::decode`] がフレームを受理した時点で増え、対応する結果を
    /// [`DecodeHandler::on_decoded`] に通知する直前に減る。[`Decoder::finish`] の
    /// 処理が完了した時点で 0 になる。処理の途中でエラーになった場合は残留する
    /// ことがある。`Drop` 時に中断したフレームを通知した場合も減る。
    pub in_flight_frames: Gauge,
}

/// デコード用サーフェスの同期情報
///
/// Worker スレッドに渡される。
/// `syncp` で SyncOperation を待機し、`out_surface` から Map でデータを読み取る。
struct DecodeSyncData {
    syncp: sys::mfxSyncPoint,
    frame_surface: FrameSurface,
}

/// Worker スレッドへの命令
///
/// - `QueueFrame`: Main スレッドから Worker へ frame_seq と user_data を転送する。
/// - `Sync`: デコード済みフレームの SyncOperation。Worker 側で frame_seq の一致により対応付ける。
/// - `WaitIdle`: finish() のバリア。残留 pending_map のチェック後に応答を返す。
/// - `Stop`: Drop 時の中断。残りの pending_map を通知して Worker を停止する。
enum WorkerCommand<T> {
    QueueFrame { frame_seq: u64, user_data: T },
    Sync { sync_data: DecodeSyncData },
    WaitIdle(mpsc::Sender<Result<(), Error>>),
    Stop,
}

// Safety: WorkerCommand はスレッド間で所有権を移動するだけで、同時アクセスはしない。
unsafe impl<T: Send> Send for WorkerCommand<T> {}

/// デコード結果を通知するためのハンドラー
///
/// デコード処理が完了するたびに [`DecodeHandler::on_decoded`] が呼ばれる。
pub trait DecodeHandler: Send + 'static {
    /// ユーザーデータ型
    type UserData: Send + 'static;
    /// エラー型
    type Error: From<crate::Error> + Send + 'static;
    /// デコード完了時に呼ばれる
    fn on_decoded(&mut self, result: Result<DecodedFrame<'_, Self::UserData>, Self::Error>);
}

/// `FnMut(Result<DecodedFrame<T>, E>)` を [`DecodeHandler`] にするラッパー
pub struct FnDecodeHandler<T, E = crate::Error> {
    #[expect(clippy::type_complexity)]
    f: Box<dyn for<'a> FnMut(Result<DecodedFrame<'a, T>, E>) + Send + 'static>,
}

impl<T, E> FnDecodeHandler<T, E> {
    /// `FnMut(Result<DecodedFrame<T>, E>)` から [`DecodeHandler`] を構築する
    pub fn new<F>(f: F) -> Self
    where
        F: for<'a> FnMut(Result<DecodedFrame<'a, T>, E>) + Send + 'static,
    {
        Self { f: Box::new(f) }
    }
}

impl<T, E> DecodeHandler for FnDecodeHandler<T, E>
where
    T: Send + 'static,
    E: From<crate::Error> + Send + 'static,
{
    type UserData = T;
    type Error = E;
    fn on_decoded(&mut self, result: Result<DecodedFrame<'_, T>, E>) {
        (self.f)(result);
    }
}

/// Intel VPL ハードウェアデコーダ
///
/// 圧縮されたビットストリームをデコードする。
/// 出力フォーマットはストリームのネイティブ形式で決まり、[`DecodedFrame::frame_format`] で
/// 取得できる。NV12 / P010 / AYUV / Y410 以外の出力には未対応で、デコード時にエラーになる。
/// 詳細は [`DecodedFrame`] を参照。
/// 最初の [`Decoder::decode`] 呼び出し時にビットストリームのヘッダ (SPS/PPS 等) を解析して
/// 自動的に初期化される。
///
/// VPL の内部割り当て (`surface_work=NULL`) を使用するため、アプリケーション側での
/// サーフェスプール管理は不要。
///
/// # 使い方
///
/// 1. [`Decoder::new`] でデコーダを作成する（コールバックを登録）
/// 2. [`Decoder::decode`] でビットストリームデータと値を投入する
/// 3. すべてのデータを投入したら [`Decoder::finish`] を呼んで残留フレームを排出する
/// 4. デコード完了時にコールバックが呼ばれる
///
/// # スレッド安全性
///
/// デコード完了通知は専用スレッド (`"vpl-decoder-sync"`) で行い、
/// メインスレッドは `DecodeFrameAsync` の呼び出しのみを担当する。
/// `Send` のみ実装し、`Sync` は実装しない（生ポインタにより自動的に `!Sync`）。
pub struct Decoder<H: DecodeHandler> {
    session: Session,
    codec: VideoCodecType,
    /// mfxVideoParam.AsyncDepth に設定する値
    async_depth: u16,
    /// DecodeHeader + Init が完了しているか
    initialized: bool,
    /// 入力フレームの連番
    frame_count: u64,
    /// Worker スレッドへの命令チャネル
    worker_tx: mpsc::Sender<WorkerCommand<H::UserData>>,
    /// Worker スレッドの join ハンドル
    worker_handle: Option<thread::JoinHandle<()>>,
    /// ワーカースレッドと共有する統計値
    stats: Arc<DecoderStats>,
}

// Safety: デコード完了通知は専用スレッドで行い、メインスレッドは DecodeFrameAsync のみ実行する。
// VPL 仕様上、セッション操作の同一スレッド制約は明記されておらず、公式サンプルでも
// セッションハンドルにスレッドアフィニティの制約は課されていない。
// Sync は実装しない（生ポインタにより自動的に !Sync）。
unsafe impl<H: DecodeHandler> Send for Decoder<H> {}

/// DecodeFrameAsync を DEVICE_BUSY / MORE_SURFACE の上限付きリトライ付きで実行する
///
/// 最終ステータスが、MFX_ERR_MORE_DATA（入力不足 / ドレイン完了）または
/// 正の警告（MFX_WRN_VIDEO_PARAM_CHANGED / MFX_WRN_ALLOC_TIMEOUT_EXPIRED 等）の場合は Ok を返す。
/// MFX_ERR_MORE_DATA 以外の負値のエラーとリトライ上限超過の場合は Err を返す。
fn call_decode_frame_async_with_retry(
    session: &Session,
    bs: *mut sys::mfxBitstream,
    out_surface: &mut *mut sys::mfxFrameSurface1,
    syncp: &mut sys::mfxSyncPoint,
) -> Result<i32, Error> {
    let mut last_status = sys::mfxStatus_MFX_ERR_NONE;
    for _ in 0..DEVICE_BUSY_MAX_RETRIES {
        // surface_work=NULL で VPL 内部割り当てを使用する
        last_status = session.lib().mfx_video_decode_frame_async(
            session.as_ptr(),
            bs,
            std::ptr::null_mut(),
            out_surface,
            syncp,
        );
        if last_status == sys::mfxStatus_MFX_WRN_DEVICE_BUSY
            || last_status == sys::mfxStatus_MFX_ERR_MORE_SURFACE
        {
            // デバイスが混雑 / 出力サーフェス不足。1ms 待って再試行する
            std::thread::sleep(std::time::Duration::from_millis(1));
            continue;
        }
        if last_status == sys::mfxStatus_MFX_ERR_MORE_DATA {
            return Ok(last_status);
        }
        if last_status < 0 {
            return Err(Error::from_mfx(
                last_status,
                "MFXVideoDECODE_DecodeFrameAsync",
            ));
        }
        return Ok(last_status);
    }
    // リトライ上限超過。最後に返ったステータスでエラーメッセージを分岐する
    // リトライループは DEVICE_BUSY / MORE_SURFACE 以外のステータスですぐに return しているので、
    // 上限超過時の last_status は必ず DEVICE_BUSY か MORE_SURFACE のどちらかになる。
    if last_status == sys::mfxStatus_MFX_ERR_MORE_SURFACE {
        Err(Error::new_custom(
            "MFXVideoDECODE_DecodeFrameAsync",
            "more surface after max retries",
        ))
    } else {
        Err(Error::new_custom(
            "MFXVideoDECODE_DecodeFrameAsync",
            "device busy after max retries",
        ))
    }
}

/// 入力データを参照する `mfxBitstream` を構築する
///
/// 返る `mfxBitstream` の `Data` は `data` を指すため、`data` より長く保持してはならない。
/// データ長が `u32::MAX` を超える場合は `function` を関数名に持つ `Error` を返す。
fn create_bitstream(data: &[u8], function: &'static str) -> Result<sys::mfxBitstream, Error> {
    let data_len = u32::try_from(data.len()).map_err(|_| {
        Error::new_custom_owned(
            function,
            format!("bitstream length {} exceeds u32::MAX", data.len()),
        )
    })?;
    let mut bs: sys::mfxBitstream = unsafe { std::mem::zeroed() };
    // VPL API は書き込み可能なポインタを要求するが、入力データを書き換えないためキャストする
    bs.Data = data.as_ptr() as *mut u8;
    bs.DataLength = data_len;
    bs.MaxLength = data_len;
    Ok(bs)
}

/// ヘッダ解析済みの `mfxVideoParam` から `StreamInfo` を構築する
fn stream_info_from_video_param(
    codec: VideoCodecType,
    video_param: &sys::mfxVideoParam,
) -> StreamInfo {
    let mfx = unsafe { video_param.__bindgen_anon_1.mfx };
    let frame_info = mfx.FrameInfo;
    let fi = unsafe { frame_info.__bindgen_anon_1.__bindgen_anon_1 };
    // フレームレートの情報が無い場合、VPL は 0/0 を返す
    let framerate = match (frame_info.FrameRateExtN, frame_info.FrameRateExtD) {
        (0, _) | (_, 0) => None,
        (num, den) => Some(Framerate { num, den }),
    };
    StreamInfo {
        codec,
        coded_width: usize::from(fi.Width),
        coded_height: usize::from(fi.Height),
        width: usize::from(fi.CropW),
        height: usize::from(fi.CropH),
        framerate,
    }
}

impl<H: DecodeHandler> Decoder<H> {
    /// デコーダを作成する
    ///
    /// `handler` は Worker スレッドから呼ばれる。
    /// `DecodedFrame` のライフタイムはコールバック呼び出し中のみ有効。
    pub fn new(config: DecoderConfig, handler: H) -> Result<Self, Error> {
        let lib = VplLibrary::load()?;

        // API 2.x フローで指定アダプタのセッションを作成する
        let session = lib.create_session(config.adapter)?;

        let (worker_tx, worker_rx) = mpsc::channel();
        let lib = session.lib();
        let session_handle = session.as_ptr() as usize;
        let stats = Arc::new(DecoderStats::default());
        let worker_stats = Arc::clone(&stats);
        let worker_handle = thread::Builder::new()
            .name("vpl-decoder-sync".to_owned())
            .spawn(move || {
                run_sync_worker(lib, session_handle, worker_rx, handler, worker_stats);
            })
            .map_err(|error| {
                Error::new_custom_owned(
                    "Decoder::new",
                    format!("failed to spawn sync worker thread: {error}"),
                )
            })?;

        Ok(Decoder {
            session,
            codec: config.codec,
            async_depth: config.async_depth.unwrap_or(4),
            initialized: false,
            frame_count: 0,
            worker_tx,
            worker_handle: Some(worker_handle),
            stats,
        })
    }

    /// デコーダーの統計値を取得する
    pub fn stats(&self) -> &DecoderStats {
        &self.stats
    }

    /// ビットストリームのヘッダからストリーム情報を取得する
    ///
    /// `data` の先頭から走査して SPS / PPS / sequence header を探し、解像度と
    /// フレームレートを返す。
    /// `data` は消費しないため、解析した同じアクセスユニットをそのまま
    /// [`Decoder::decode`] に渡せる。
    ///
    /// デコーダーの初期化前後を問わず呼べる。[`Decoder::new`] の直後でも呼べる。
    ///
    /// # 前提
    ///
    /// `data` は SPS / PPS / sequence header を含む必要がある。これらは通常
    /// キーフレームのアクセスユニットに含まれる。
    ///
    /// # エラー
    ///
    /// ヘッダを解析できなかった場合に `Err` を返す。VPL が返したエラーの
    /// [`Error::function`] は `MFXVideoDECODE_DecodeHeader` になる。
    /// データ長が `u32::MAX` を超える場合も `Err` を返す。
    ///
    /// # 呼び出しスレッド
    ///
    /// [`Decoder::decode`] と同じスレッドで、[`Decoder::decode`] と直列になるように
    /// 呼ぶこと。コールバック ([`DecodeHandler::on_decoded`]) から呼び出してはならない。
    pub fn decode_header(&self, data: &[u8]) -> Result<StreamInfo, Error> {
        let mut bs = create_bitstream(data, "Decoder::decode_header")?;
        let video_param = self.read_video_param_from_header(&mut bs)?;
        Ok(stream_info_from_video_param(self.codec, &video_param))
    }

    /// ビットストリームからヘッダを解析してデコーダを初期化する
    ///
    /// ヘッダを解析できなかった場合とデコーダの初期化に失敗した場合に `Err` を返す。
    /// 成功するとデコードできる状態になる。
    fn initialize(&mut self, bs: &mut sys::mfxBitstream) -> Result<(), Error> {
        let mut video_param = self.read_video_param_from_header(bs)?;

        // デコーダを初期化する。警告を返すことがあるが初期化自体は成功している
        self.session
            .lib()
            .mfx_video_decode_init(self.session.as_ptr(), &mut video_param)?;

        self.initialized = true;

        Ok(())
    }

    /// デコーダのパラメータを構築する
    ///
    /// `CodecId` / `IOPattern` / `AsyncDepth` と、`FrameInfo` の出力形式を設定した
    /// `mfxVideoParam` を返す。
    fn create_decode_video_param(&self) -> sys::mfxVideoParam {
        let mut video_param: sys::mfxVideoParam = unsafe { std::mem::zeroed() };
        video_param.IOPattern = sys::MFX_IOPATTERN_OUT_SYSTEM_MEMORY as u16;
        video_param.AsyncDepth = self.async_depth;
        unsafe {
            let mfx = &mut video_param.__bindgen_anon_1.mfx;
            mfx.CodecId = self.codec.to_codec_id();
            mfx.FrameInfo.FourCC = sys::MFX_FOURCC_NV12;
            mfx.FrameInfo.ChromaFormat = sys::MFX_CHROMAFORMAT_YUV420 as u16;
            mfx.FrameInfo.PicStruct = sys::MFX_PICSTRUCT_PROGRESSIVE as u16;
        }
        video_param
    }

    /// ビットストリームのヘッダを解析し、解析結果が入ったデコーダパラメータを返す
    ///
    /// 解像度・表示サイズ・フレームレートは、解析した値になる。
    fn read_video_param_from_header(
        &self,
        bs: &mut sys::mfxBitstream,
    ) -> Result<sys::mfxVideoParam, Error> {
        let mut video_param = self.create_decode_video_param();
        self.session.lib().mfx_video_decode_decode_header(
            self.session.as_ptr(),
            bs,
            &mut video_param,
        )?;
        Ok(video_param)
    }

    /// 圧縮されたビットストリームデータをデコードする
    ///
    /// 最初の呼び出し時にヘッダ解析とデコーダ初期化を自動的に行う。
    /// デコード完了時にはコンストラクタで登録したコールバックが呼ばれる。
    ///
    /// `user_data` は対応するフレームがデコードされたときに [`DecodedFrame::user_data`] で取得できる。
    /// VPL は内部バッファにビットストリームを蓄積するため、
    /// `decode()` 呼び出しとフレーム出力は 1:1 に対応しない場合がある。
    /// user_data は Worker スレッド内で frame_seq の一致により管理される。
    pub fn decode(&mut self, data: &[u8], user_data: H::UserData) -> Result<(), Error> {
        let mut bs = create_bitstream(data, "Decoder::decode")?;

        // 未初期化の場合は初期化する。初期化エラー時に QueueFrame を送信しない。
        if !self.initialized {
            self.initialize(&mut bs)?;
        }

        // 入力ビットストリームの TimeStamp に連番 frame_seq を設定し、
        // 出力フレームの TimeStamp との完全一致で user_data を対応付ける。
        let frame_seq = self.frame_count;
        bs.TimeStamp = frame_seq;

        // QueueFrame を送信してからデコードを開始する。
        // これにより Worker 内で user_data と出力フレームが frame_seq の一致で対応付けられる。
        // pending の登録と in-flight の加算を同じタイミングで行い、
        // ワーカーが対応する結果を通知する直前に減算する。
        self.stats.in_flight_frames.inc();
        if let Err(error) = self.send_worker_command(
            "Decoder::decode",
            WorkerCommand::QueueFrame {
                frame_seq,
                user_data,
            },
        ) {
            // pending を登録できなかったため、加算した in-flight を戻す
            self.stats.in_flight_frames.dec();
            return Err(error);
        }
        // pending に登録できた時点で通算回数を計上する
        self.stats.total_decode_count.inc();

        // frame_seq を進める。デコード開始前に進めることで、decode_bitstream がエラーを返しても
        // 同じ frame_seq が再利用されることが無いようにする。
        self.frame_count = self
            .frame_count
            .checked_add(1)
            .ok_or_else(|| Error::new_custom("Decoder::decode", "frame sequence overflowed"))?;

        self.decode_bitstream(&mut bs)
    }

    /// ビットストリームをデコードしてフレームを収集する
    ///
    /// `bs.DataLength > 0` の間 `DecodeFrameAsync` を繰り返し呼び出す。
    /// VPL は `MORE_DATA` 時にビットストリームを内部バッファに蓄積し、
    /// 十分なデータが溜まった時点でフレームを出力する。
    fn decode_bitstream(&mut self, bs: &mut sys::mfxBitstream) -> Result<(), Error> {
        while bs.DataLength > 0 {
            let mut syncp: sys::mfxSyncPoint = std::ptr::null_mut();
            let mut out_surface: *mut sys::mfxFrameSurface1 = std::ptr::null_mut();

            let status = call_decode_frame_async_with_retry(
                &self.session,
                bs,
                &mut out_surface,
                &mut syncp,
            )?;

            // MORE_DATA: データ不足。VPL 内部に蓄積済み。呼び出し元に戻る
            if status == sys::mfxStatus_MFX_ERR_MORE_DATA {
                return Ok(());
            }
            // デコード済みフレームが存在する場合は Worker に送信する
            if !syncp.is_null() {
                let frame_surface = FrameSurface::new(self.session.lib(), out_surface)?;
                self.send_worker_command(
                    "Decoder::decode",
                    WorkerCommand::Sync {
                        sync_data: DecodeSyncData {
                            syncp,
                            frame_surface,
                        },
                    },
                )?;
            }
        }
        Ok(())
    }

    /// これ以上データが来ないことをデコーダに伝え、残留フレームを排出する
    ///
    /// この関数は全ての Worker コマンドの完了を待ち、
    /// 全てのコールバックが呼び出され終わるまでブロックする。
    pub fn finish(&mut self) -> Result<(), Error> {
        // 初期化前（decode 未呼び出し）なら排出するフレームがないので即座に返す
        if !self.initialized {
            return Ok(());
        }

        loop {
            let mut syncp: sys::mfxSyncPoint = std::ptr::null_mut();
            let mut out_surface: *mut sys::mfxFrameSurface1 = std::ptr::null_mut();

            // null bitstream で残留フレームを排出する
            let status = call_decode_frame_async_with_retry(
                &self.session,
                std::ptr::null_mut(),
                &mut out_surface,
                &mut syncp,
            )?;

            // MORE_DATA: すべての残留フレームを排出済み
            if status == sys::mfxStatus_MFX_ERR_MORE_DATA {
                break;
            }
            // デコード済みフレームが存在する場合は Worker に送信する
            if !syncp.is_null() {
                let frame_surface = FrameSurface::new(self.session.lib(), out_surface)?;
                self.send_worker_command(
                    "Decoder::finish",
                    WorkerCommand::Sync {
                        sync_data: DecodeSyncData {
                            syncp,
                            frame_surface,
                        },
                    },
                )?;
                continue;
            }
            // 正の警告（MFX_WRN_ALLOC_TIMEOUT_EXPIRED 等）で出力フレームが生成されない場合、1ms 待って再試行する。
            // libvpl 仕様（mfxvideo.h の MFXVideoDECODE_DecodeFrameAsync の
            // MFX_WRN_ALLOC_TIMEOUT_EXPIRED）では「数 ms 後に再呼び出しする」ように指示されているため。
            std::thread::sleep(std::time::Duration::from_millis(1));
        }

        // ここまでに送ったコマンドが Worker 側で全て処理されるまで待つ
        let (tx, rx) = mpsc::channel();
        self.send_worker_command("Decoder::finish", WorkerCommand::WaitIdle(tx))?;
        rx.recv().map_err(|_| {
            Error::new_custom("Decoder::finish", "sync worker thread stopped unexpectedly")
        })??;

        Ok(())
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
}

impl<H: DecodeHandler> Decoder<H> {
    fn stop_worker(&mut self) {
        if let Some(handle) = self.worker_handle.take() {
            // Worker 内の pending_map は Stop ハンドラで処理される。
            let _ = self.worker_tx.send(WorkerCommand::Stop);
            let _ = handle.join();
        }
    }
}

impl<H: DecodeHandler> Drop for Decoder<H> {
    fn drop(&mut self) {
        self.stop_worker();
        if self.initialized {
            let _ = self
                .session
                .lib()
                .mfx_video_decode_close(self.session.as_ptr());
        }
        // self.session が続けて Drop され、MFXClose + MFXUnload が実行される
    }
}

/// Worker スレッドのメインループ
///
/// mpsc チャネルからコマンドを受信し、VPL API を呼び出す。
/// セッション操作は全てこのスレッドで行うため、VPL のスレッド安全性に関する
/// 暗黙の制約に抵触しない。
///
/// `pending_map` は Main スレッドからの `QueueFrame` で frame_seq ごとに蓄積され、
/// `Sync` の出力フレームの TimeStamp との完全一致で消費される。
fn run_sync_worker<H: DecodeHandler>(
    lib: VplLibrary,
    session_handle: usize,
    worker_rx: mpsc::Receiver<WorkerCommand<H::UserData>>,
    mut handler: H,
    stats: Arc<DecoderStats>,
) {
    let mut pending_map: HashMap<u64, H::UserData> = HashMap::new();
    while let Ok(command) = worker_rx.recv() {
        match command {
            WorkerCommand::QueueFrame {
                frame_seq,
                user_data,
            } => {
                // 入力フレームの frame_seq で user_data を登録する。
                // 同一 frame_seq が二度使われると誤対応付けになるため、重複を検出する。
                if pending_map.insert(frame_seq, user_data).is_some() {
                    // 同じ frame_seq で置き換えた user_data は結果を通知できないため、
                    // in-flight から取り除く
                    stats.in_flight_frames.dec();
                    notify_decoded(
                        &mut handler,
                        &stats,
                        Err(Error::new_custom_owned(
                            "Decoder::sync_worker",
                            format!("duplicate frame sequence in pending frames: {frame_seq}"),
                        )),
                    );
                }
            }
            WorkerCommand::Sync { sync_data } => {
                if let Err(error) = sync_and_callback(
                    lib,
                    session_handle,
                    sync_data,
                    &mut pending_map,
                    &mut handler,
                    &stats,
                ) {
                    notify_decoded(&mut handler, &stats, Err(error));
                }
            }
            WorkerCommand::WaitIdle(reply_tx) => {
                // finish 側のバリア。ここに到達した時点で、それ以前に送信された
                // コマンドはすべて処理済みになっているはず。
                if pending_map.is_empty() {
                    let _ = reply_tx.send(Ok(()));
                    continue;
                }

                // もし出力されなかった入力フレームが残留しているなら、異常系のためエラーとして通知する。
                let remaining_count = pending_map.len();
                for (frame_seq, _user_data) in pending_map.drain() {
                    stats.in_flight_frames.dec();
                    notify_decoded(
                        &mut handler,
                        &stats,
                        Err(Error::new_custom_owned(
                            "Decoder::finish",
                            format!(
                                "decode pending frame seq={frame_seq} (pending count: {remaining_count})"
                            ),
                        )),
                    );
                }
                let _ = reply_tx.send(Err(Error::new_custom_owned(
                    "Decoder::finish",
                    format!("finish completed but {remaining_count} pending frames remained"),
                )));
            }
            WorkerCommand::Stop => {
                // drop 時の中断。未完了 frame はすべて MFX_ERR_ABORTED として通知する。
                for (_frame_seq, _user_data) in pending_map.drain() {
                    stats.in_flight_frames.dec();
                    notify_decoded(
                        &mut handler,
                        &stats,
                        Err(Error::from_mfx(
                            sys::mfxStatus_MFX_ERR_ABORTED,
                            "Decoder::drop",
                        )),
                    );
                }
                break;
            }
        }
    }
}

/// デコード結果をハンドラーへ通知し、通知内容に応じて統計を更新する
///
/// `Ok` は出力フレーム数に、`Err` はエラー数に計上する。
fn notify_decoded<H: DecodeHandler>(
    handler: &mut H,
    stats: &DecoderStats,
    result: Result<DecodedFrame<'_, H::UserData>, Error>,
) {
    match &result {
        Ok(_) => stats.total_output_frame_count.inc(),
        Err(_) => stats.total_error_count.inc(),
    }
    handler.on_decoded(result.map_err(Into::into));
}

/// `SyncOperation` 完了後の `out_surface` から Map でデータを読み取り、
/// 出力フレームの TimeStamp (=frame_seq) と完全一致する user_data を消費し、
/// handler の `on_decoded` を呼び出す。
///
/// `DecodedFrame` の `data()` が返すスライスは `on_decoded` 呼び出し中のみ有効。
///
/// `Err` はデータ破損系（`SyncOperation` 失敗・`map_read` 失敗・`read_decoded_surface` の検証エラー）
/// のみを返す。frame_seq 引き当て失敗はドレインの空フレームや異常出力が原因の正常な破棄として扱い、
/// `Err` にせず `Ok(())` で返す（対応する user_data が存在しないだけで、出力自体は解放すればよいため）。
fn sync_and_callback<H: DecodeHandler>(
    lib: VplLibrary,
    session_handle: usize,
    sync_data: DecodeSyncData,
    pending_map: &mut HashMap<u64, H::UserData>,
    handler: &mut H,
    stats: &DecoderStats,
) -> Result<(), crate::Error> {
    let DecodeSyncData {
        syncp,
        mut frame_surface,
    } = sync_data;

    if syncp.is_null() {
        return Err(Error::new_custom(
            "Decoder::sync_worker",
            "sync point is null",
        ));
    }

    // SyncOperation でデコード完了を待機する
    let status = lib.mfx_video_core_sync_operation(
        session_handle as sys::mfxSession,
        syncp,
        sys::MFX_INFINITE,
    );
    Error::check_mfx(status, "MFXVideoCORE_SyncOperation")?;

    frame_surface.map_read()?;

    // 出力フレームの TimeStamp から user_data を探す
    let frame_seq = unsafe { (*frame_surface.as_ptr()).Data.TimeStamp };
    let Some(user_data) = pending_map.remove(&frame_seq) else {
        return Ok(());
    };

    // pending を消費して結果を通知できる状態になった
    stats.in_flight_frames.dec();

    // デコードされたフレームデータを読み取る
    let frame = read_decoded_surface(&frame_surface, user_data)?;
    notify_decoded(handler, stats, Ok(frame));
    Ok(())
}

/// デコード出力として扱える色形式を返す
///
/// FourCC と ChromaFormat の組が、対応する [`FrameFormat`] の組と一致しない場合は `None` を返す。
/// [`FrameFormat`] で表現できても、YUY2 と BGRA はデコード出力としては扱わないため `None` を返す。
fn decoded_frame_format(fourcc: u32, chroma_format: u16) -> Option<FrameFormat> {
    let format = FrameFormat::from_fourcc(fourcc)?;
    if format.chroma_format() != chroma_format {
        return None;
    }
    match format {
        FrameFormat::Nv12 | FrameFormat::P010 | FrameFormat::Ayuv | FrameFormat::Y410 => {
            Some(format)
        }
        FrameFormat::Yuy2 | FrameFormat::Bgra => None,
    }
}

/// デコード済みサーフェスから画素データを読み取り `DecodedFrame` を構築する
///
/// Map 済みであることと、戻り値の `DecodedFrame` が生存している間
/// サーフェスデータが有効であることの保証は呼び出し元の責任。
fn read_decoded_surface<'a, T>(
    frame_surface: &'a FrameSurface,
    user_data: T,
) -> Result<DecodedFrame<'a, T>, Error> {
    let surface = unsafe { &*frame_surface.as_ptr() };
    let fourcc = surface.Info.FourCC;
    let chroma_format = surface.Info.ChromaFormat;
    let format = decoded_frame_format(fourcc, chroma_format).ok_or_else(|| {
        Error::new_custom_owned(
            "Decoder::sync_worker",
            format!(
                "unsupported decoded surface format: FourCC={fourcc:#x}, ChromaFormat={chroma_format}"
            ),
        )
    })?;

    let (crop_w, crop_h) = unsafe {
        let fi = &surface.Info.__bindgen_anon_1.__bindgen_anon_1;
        (fi.CropW as usize, fi.CropH as usize)
    };
    if crop_w == 0 || crop_h == 0 {
        return Err(Error::new_custom(
            "Decoder::sync_worker",
            "decoded surface has zero crop dimensions",
        ));
    }

    let row_bytes = format
        .bytes_per_row(crop_w)
        .ok_or_else(|| Error::new_custom("Decoder::sync_worker", "row bytes overflowed"))?;
    let pitch = unsafe { frame_data_pitch(&surface.Data) };
    if pitch < row_bytes {
        return Err(Error::new_custom_owned(
            "Decoder::sync_worker",
            format!("pitch ({pitch}) is less than row bytes ({row_bytes})"),
        ));
    }

    // P010 は 10bit サンプルを 16bit ワードの上位ビットに格納する
    if format == FrameFormat::P010 && surface.Info.Shift == 0 {
        return Err(Error::new_custom(
            "Decoder::sync_worker",
            "P010 decoded surface has Shift 0",
        ));
    }

    // Y プレーンの長さ (パック形式ではフレーム全体の長さ) を計算する
    let plane_size = pitch
        .checked_mul(crop_h)
        .ok_or_else(|| Error::new_custom("Decoder::sync_worker", "plane size overflowed"))?;

    let data = unsafe {
        match format {
            FrameFormat::Nv12 | FrameFormat::P010 => {
                if crop_w % 2 != 0 || crop_h % 2 != 0 {
                    return Err(Error::new_custom(
                        "Decoder::sync_worker",
                        "4:2:0 decoded surface has odd crop dimensions",
                    ));
                }
                let y_ptr = surface.Data.__bindgen_anon_3.Y;
                let uv_ptr = surface.Data.__bindgen_anon_4.UV;
                if y_ptr.is_null() || uv_ptr.is_null() {
                    return Err(Error::new_custom(
                        "Decoder::sync_worker",
                        "decoded surface has null plane pointers",
                    ));
                }
                let uv_size = pitch.checked_mul(crop_h / 2).ok_or_else(|| {
                    Error::new_custom("Decoder::sync_worker", "UV plane size overflowed")
                })?;
                FrameData::SemiPlanar {
                    y: std::slice::from_raw_parts(y_ptr, plane_size),
                    y_pitch: pitch,
                    uv: std::slice::from_raw_parts(uv_ptr, uv_size),
                    uv_pitch: pitch,
                }
            }
            FrameFormat::Ayuv => {
                // AYUV のメモリ上の並びは V, U, Y, A であり、V ポインタがフレーム先頭を指す
                let v_ptr = surface.Data.__bindgen_anon_5.V;
                if v_ptr.is_null() {
                    return Err(Error::new_custom(
                        "Decoder::sync_worker",
                        "decoded surface has null AYUV pointer",
                    ));
                }
                FrameData::Packed {
                    data: std::slice::from_raw_parts(v_ptr, plane_size),
                    pitch,
                }
            }
            FrameFormat::Y410 => {
                // Y410 は `mfxFrameData.Y` ではなく `mfxFrameData.Y410` にフレーム先頭が入る
                let y410_ptr = surface.Data.__bindgen_anon_4.Y410 as *const u8;
                if y410_ptr.is_null() {
                    return Err(Error::new_custom(
                        "Decoder::sync_worker",
                        "decoded surface has null Y410 pointer",
                    ));
                }
                FrameData::Packed {
                    data: std::slice::from_raw_parts(y410_ptr, plane_size),
                    pitch,
                }
            }
            // `decoded_frame_format` はデコード出力として扱える形式だけを返す
            FrameFormat::Yuy2 | FrameFormat::Bgra => {
                unreachable!("decoded_frame_format が除外した形式: {format:?}")
            }
        }
    };

    Ok(DecodedFrame {
        data,
        frame_format: format,
        width: crop_w,
        height: crop_h,
        user_data,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    /// [run_sync_worker] を VPL セッションなしで起動するヘルパー
    ///
    /// [WorkerCommand::Sync] 節の [sync_and_callback] は `MFXVideoCORE_SyncOperation` を呼ぶため
    /// 実セッションが必要で単体テストできない。
    /// [WorkerCommand::QueueFrame] / [WorkerCommand::WaitIdle] / [WorkerCommand::Stop] の節は
    /// VPL を一切呼ばないため、ダミーの [VplLibrary] と `session_handle = 0` でテストできる。
    /// TimeStamp 引き当て失敗ドレイン（[sync_and_callback] 内）は roundtrip 統合テストで
    /// 間接的にカバーされる。
    fn spawn_worker<F>(
        callback: F,
    ) -> (
        mpsc::Sender<WorkerCommand<u32>>,
        thread::JoinHandle<()>,
        Arc<DecoderStats>,
    )
    where
        F: FnMut(Result<DecodedFrame<'_, u32>, Error>) + Send + 'static,
    {
        let (command_tx, command_rx) = mpsc::channel();
        let stats = Arc::new(DecoderStats::default());
        let worker_stats = Arc::clone(&stats);
        let handle = thread::Builder::new()
            .name("vpl-decoder-sync".to_owned())
            .spawn(move || {
                run_sync_worker(
                    VplLibrary,
                    0,
                    command_rx,
                    FnDecodeHandler::new(callback),
                    worker_stats,
                );
            })
            .expect("sync worker thread の起動に失敗した");
        (command_tx, handle, stats)
    }

    #[test]
    fn worker_wait_idle_returns_error_when_pending_remains() {
        let (callback_tx, callback_rx) = mpsc::channel::<Result<(), Error>>();
        let (command_tx, handle, stats) = spawn_worker(move |result| {
            callback_tx
                .send(result.map(|_| ()))
                .expect("デコード結果の送信に失敗した");
        });

        // decode() と同じく、pending を登録する時点で in-flight を加算する
        stats.in_flight_frames.inc();
        command_tx
            .send(WorkerCommand::QueueFrame {
                frame_seq: 77,
                user_data: 7,
            })
            .expect("QueueFrame の送信に失敗した");
        let (reply_tx, reply_rx) = mpsc::channel();
        command_tx
            .send(WorkerCommand::WaitIdle(reply_tx))
            .expect("WaitIdle の送信に失敗した");

        let wait_result = reply_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("WaitIdle 応答の受信に失敗した");
        let wait_error = wait_result.expect_err("残留 pending があるのに WaitIdle が成功した");
        assert_eq!(wait_error.function(), "Decoder::finish");
        assert_eq!(
            wait_error.status_message(),
            Some("finish completed but 1 pending frames remained"),
            "残留 pending の件数を示すエラーメッセージであること"
        );

        let callback_result = callback_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("残留通知コールバックの受信に失敗した");
        let callback_error = callback_result.expect_err("残留 pending はエラー通知されること");
        assert_eq!(callback_error.function(), "Decoder::finish");
        assert_eq!(
            callback_error.status_message(),
            Some("decode pending frame seq=77 (pending count: 1)"),
            "frame_seq と残留件数を含むエラーメッセージであること"
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
        handle.join().expect("worker thread がパニックした");
    }

    #[test]
    fn worker_stop_returns_aborted_for_all_pending() {
        let (callback_tx, callback_rx) = mpsc::channel::<Result<(), Error>>();
        let (command_tx, handle, stats) = spawn_worker(move |result| {
            callback_tx
                .send(result.map(|_| ()))
                .expect("デコード結果の送信に失敗した");
        });

        // decode() と同じく、pending を登録する時点で in-flight を加算する
        stats.in_flight_frames.inc();
        command_tx
            .send(WorkerCommand::QueueFrame {
                frame_seq: 1,
                user_data: 10,
            })
            .expect("QueueFrame の送信に失敗した");
        stats.in_flight_frames.inc();
        command_tx
            .send(WorkerCommand::QueueFrame {
                frame_seq: 2,
                user_data: 20,
            })
            .expect("QueueFrame の送信に失敗した");
        command_tx
            .send(WorkerCommand::Stop)
            .expect("Stop の送信に失敗した");
        handle.join().expect("worker thread がパニックした");

        let mut callback_count = 0;
        while let Ok(result) = callback_rx.recv_timeout(Duration::from_millis(200)) {
            callback_count += 1;
            let error = result.expect_err("Stop 時のコールバックはエラーであること");
            assert_eq!(
                error.status_code(),
                Some(sys::mfxStatus_MFX_ERR_ABORTED),
                "Stop 時のコールバックは MFX_ERR_ABORTED を返すこと"
            );
        }
        assert_eq!(
            callback_count, 2,
            "全ての残留 pending 分のコールバックが通知されること"
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
        let (callback_tx, callback_rx) = mpsc::channel::<Result<(), Error>>();
        let (command_tx, handle, stats) = spawn_worker(move |result| {
            callback_tx
                .send(result.map(|_| ()))
                .expect("デコード結果の送信に失敗した");
        });

        // decode() と同じく、pending を登録する時点で in-flight を加算する
        stats.in_flight_frames.inc();
        // 同一 frame_seq の QueueFrame を 2 回送信する
        command_tx
            .send(WorkerCommand::QueueFrame {
                frame_seq: 42,
                user_data: 1,
            })
            .expect("QueueFrame の送信に失敗した");
        stats.in_flight_frames.inc();
        command_tx
            .send(WorkerCommand::QueueFrame {
                frame_seq: 42,
                user_data: 2,
            })
            .expect("QueueFrame の送信に失敗した");
        command_tx
            .send(WorkerCommand::Stop)
            .expect("Stop の送信に失敗した");
        handle.join().expect("worker thread がパニックした");

        // 1 つ目のコールバックが重複 frame_seq のエラー通知であること
        let callback_result = callback_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("重複通知コールバックの受信に失敗した");
        let error = callback_result.expect_err("重複 frame_seq はエラー通知されること");
        assert_eq!(error.function(), "Decoder::sync_worker");
        assert_eq!(
            error.status_message(),
            Some("duplicate frame sequence in pending frames: 42"),
            "重複した frame_seq を含むエラーメッセージであること"
        );

        // 重複検出後も pending エントリは残るため、Stop で残留分が ABORTED 通知されること
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

        // 置き換えられた pending と残留 pending の両方が in-flight から取り除かれ、
        // 2 件のエラー通知がエラー数に計上されること
        assert_eq!(
            stats.total_error_count.get(),
            2,
            "重複エラーと中断エラーがエラー数に計上されること"
        );
        assert_eq!(
            stats.in_flight_frames.get(),
            0,
            "重複と中断の通知後に in-flight が 0 であること"
        );
    }

    /// デコード出力として扱える色形式を FourCC から引けること
    #[test]
    fn decoded_frame_format_accepts_supported_formats() {
        for (fourcc, chroma_format, expected) in [
            (
                sys::MFX_FOURCC_NV12,
                sys::MFX_CHROMAFORMAT_YUV420 as u16,
                FrameFormat::Nv12,
            ),
            (
                sys::MFX_FOURCC_P010,
                sys::MFX_CHROMAFORMAT_YUV420 as u16,
                FrameFormat::P010,
            ),
            (
                sys::MFX_FOURCC_AYUV,
                sys::MFX_CHROMAFORMAT_YUV444 as u16,
                FrameFormat::Ayuv,
            ),
            (
                sys::MFX_FOURCC_Y410,
                sys::MFX_CHROMAFORMAT_YUV444 as u16,
                FrameFormat::Y410,
            ),
        ] {
            assert_eq!(
                decoded_frame_format(fourcc, chroma_format),
                Some(expected),
                "FourCC {fourcc:#x} が {expected:?} として受け付けられない"
            );
        }
    }

    /// デコード出力として扱えない形式と、色形式が一致しない組を拒否すること
    #[test]
    fn decoded_frame_format_rejects_unsupported_formats() {
        for (fourcc, chroma_format) in [
            (sys::MFX_FOURCC_P016, sys::MFX_CHROMAFORMAT_YUV420 as u16),
            (sys::MFX_FOURCC_YUY2, sys::MFX_CHROMAFORMAT_YUV422 as u16),
            (sys::MFX_FOURCC_RGB4, sys::MFX_CHROMAFORMAT_YUV444 as u16),
            (sys::MFX_FOURCC_NV12, sys::MFX_CHROMAFORMAT_YUV444 as u16),
            (sys::MFX_FOURCC_AYUV, sys::MFX_CHROMAFORMAT_YUV420 as u16),
        ] {
            assert_eq!(
                decoded_frame_format(fourcc, chroma_format),
                None,
                "FourCC {fourcc:#x} が受け付けられている"
            );
        }
    }
}
