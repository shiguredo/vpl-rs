use std::collections::VecDeque;
use std::sync::mpsc;
use std::thread;

use crate::vpl::{FrameSurface, Session, VplLibrary};
use crate::{AdapterSelector, Error, sys};

/// デバイスビジー時の最大リトライ回数
const DEVICE_BUSY_MAX_RETRIES: u32 = 30;

/// デコーダ用コーデック識別子
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecoderCodec {
    /// H.264/AVC
    H264,
    /// H.265/HEVC
    Hevc,
    /// VP9
    Vp9,
    /// AV1
    Av1,
}

impl DecoderCodec {
    fn codec_id(self) -> u32 {
        match self {
            DecoderCodec::H264 => sys::MFX_CODEC_AVC,
            DecoderCodec::Hevc => sys::MFX_CODEC_HEVC,
            DecoderCodec::Vp9 => sys::MFX_CODEC_VP9,
            DecoderCodec::Av1 => sys::MFX_CODEC_AV1,
        }
    }
}

/// デコーダの設定
///
/// デコードするビットストリームのコーデックと使用するアダプタを指定する。
/// 解像度やフレームレートはビットストリームのヘッダから自動的に検出される。
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct DecoderConfig {
    /// 使用する Intel HW アダプタの指定（DRM render node 番号など）
    pub adapter: AdapterSelector,
    /// コーデック識別子
    pub codec: DecoderCodec,
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
    pub fn new(adapter: AdapterSelector, codec: DecoderCodec) -> Self {
        Self {
            adapter,
            codec,
            async_depth: None,
        }
    }
}

/// デコードされたフレーム
///
/// NV12 フォーマット (Y プレーン + インターリーブ UV プレーン) のフレームデータを
/// コールバック呼び出し中のみ有効な借用として提供する。
/// データは VPL の内部サーフェスからピッチ幅を考慮した生データとして渡される。
///
/// `y()` および `uv()` が返すスライスはピッチ幅を含んだサーフェス生データである。
/// 各行の先頭 `width()` バイトのみが有効データで、残りはパディングとなる。
///
/// # ライフタイム
///
/// 内部の `y`, `uv` スライスはコールバック呼び出し中のみ有効。
/// コールバックの外に持ち出そうとするとコンパイルエラーになる。
pub struct DecodedFrame<'a, T> {
    y: &'a [u8],
    uv: &'a [u8],
    pitch: usize,
    width: usize,
    height: usize,
    user_data: T,
}

impl<'a, T> DecodedFrame<'a, T> {
    /// Y プレーンを取得する（ピッチ含む生データ）
    ///
    /// 長さは `pitch() * height()` バイト。
    /// 各行の先頭 `width()` バイトのみが有効データ。
    pub fn y(&self) -> &[u8] {
        self.y
    }

    /// UV プレーンを取得する（ピッチ含む生データ、NV12 インターリーブ）
    ///
    /// 長さは `pitch() * height() / 2` バイト。
    /// 各行の先頭 `width()` バイトのみが有効データ。
    pub fn uv(&self) -> &[u8] {
        self.uv
    }

    /// ピッチ（行あたりのバイト数）を返す
    pub fn pitch(&self) -> usize {
        self.pitch
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
/// - `QueueFrame`: Main スレッドから Worker へ user_data を転送する。
/// - `Sync`: デコード済みフレームの SyncOperation。Worker 側で QueueFrame と対応付ける。
/// - `WaitIdle`: finish() のバリア。全コマンド処理後に応答を返す。
/// - `Stop`: Drop 時の中断。残りの pending_values を通知して Worker を停止する。
enum WorkerCommand<T> {
    QueueFrame(T),
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
/// 圧縮されたビットストリームを NV12 フレームにデコードする。
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
    codec: DecoderCodec,
    /// mfxVideoParam.AsyncDepth に設定する値
    async_depth: u16,
    /// DecodeHeader + Init が完了しているか
    initialized: bool,
    /// Worker スレッドへの命令チャネル
    worker_tx: mpsc::Sender<WorkerCommand<H::UserData>>,
    /// Worker スレッドの join ハンドル
    worker_handle: Option<thread::JoinHandle<()>>,
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
        let worker_handle = thread::Builder::new()
            .name("vpl-decoder-sync".to_owned())
            .spawn(move || {
                run_sync_worker(lib, session_handle, worker_rx, handler);
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
            worker_tx,
            worker_handle: Some(worker_handle),
        })
    }

    /// ビットストリームからヘッダを解析してデコーダを初期化する
    ///
    /// `MFXVideoDECODE_DecodeHeader` でビットストリームの SPS/PPS 等を解析し、
    /// `MFXVideoDECODE_Init` でデコーダを初期化する。
    /// `IOPattern = OUT_SYSTEM_MEMORY` を使用し、Map/Unmap でデータにアクセスする。
    ///
    /// AsyncDepth は設定値またはデフォルト 4 を使用する。
    /// libvpl のガイドでは、1 は最小メモリだが性能が低く、4 は高スループット寄りの推奨値とされる。
    fn initialize(&mut self, bs: &mut sys::mfxBitstream) -> Result<(), Error> {
        let codec_id = self.codec.codec_id();

        // デコーダパラメータを設定する
        let mut video_param: sys::mfxVideoParam = unsafe { std::mem::zeroed() };
        video_param.IOPattern = sys::MFX_IOPATTERN_OUT_SYSTEM_MEMORY as u16;
        video_param.AsyncDepth = self.async_depth;
        unsafe {
            let mfx = &mut video_param.__bindgen_anon_1.mfx;
            mfx.CodecId = codec_id;
            mfx.FrameInfo.FourCC = sys::MFX_FOURCC_NV12;
            mfx.FrameInfo.ChromaFormat = sys::MFX_CHROMAFORMAT_YUV420 as u16;
            mfx.FrameInfo.PicStruct = sys::MFX_PICSTRUCT_PROGRESSIVE as u16;
        }

        // DecodeHeader でビットストリームから解像度などのパラメータを読み取る
        self.session.lib().mfx_video_decode_decode_header(
            self.session.as_ptr(),
            bs,
            &mut video_param,
        )?;

        // デコーダを初期化する。警告を返すことがあるが初期化自体は成功している
        self.session
            .lib()
            .mfx_video_decode_init(self.session.as_ptr(), &mut video_param)?;

        self.initialized = true;

        Ok(())
    }

    /// 圧縮されたビットストリームデータをデコードする
    ///
    /// 最初の呼び出し時にヘッダ解析とデコーダ初期化を自動的に行う。
    /// デコード完了時にはコンストラクタで登録したコールバックが呼ばれる。
    ///
    /// `user_data` は対応するフレームがデコードされたときに [`DecodedFrame::user_data`] で取得できる。
    /// VPL は内部バッファにビットストリームを蓄積するため、
    /// `decode()` 呼び出しとフレーム出力は 1:1 に対応しない場合がある。
    /// user_data は Worker スレッド内で FIFO キューにより管理される。
    pub fn decode(&mut self, data: &[u8], user_data: H::UserData) -> Result<(), Error> {
        let mut bs: sys::mfxBitstream = unsafe { std::mem::zeroed() };
        let data_len = u32::try_from(data.len()).map_err(|_| {
            Error::new_custom_owned(
                "Decoder::decode",
                format!("bitstream length {} exceeds u32::MAX", data.len()),
            )
        })?;
        // VPL API は *mut を要求するが入力データを書き換えないためキャストする
        bs.Data = data.as_ptr() as *mut u8;
        bs.DataLength = data_len;
        bs.MaxLength = data_len;

        // 未初期化の場合は初期化する。初期化エラー時に QueueFrame を送信しない。
        if !self.initialized {
            self.initialize(&mut bs)?;
        }

        // QueueFrame を送信してからデコードを開始する。
        // これにより Worker 内で user_data と出力フレームが FIFO 対応付けられる。
        self.send_worker_command("Decoder::decode", WorkerCommand::QueueFrame(user_data))?;

        self.decode_bitstream(&mut bs)
    }

    /// ビットストリームをデコードしてフレームを収集する
    ///
    /// `bs.DataLength > 0` の間 `DecodeFrameAsync` を繰り返し呼び出す。
    /// VPL は `MORE_DATA` 時にビットストリームを内部バッファに蓄積し、
    /// 十分なデータが溜まった時点でフレームを出力する。
    ///
    /// 出力フレームはすべて Worker に送信され、Worker 内の FIFO キューで
    /// user_data と対応付けられる。user_data が枯渇した場合は drain 扱いで破棄する。
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
    /// null bitstream で `DecodeFrameAsync` を `MORE_DATA` が返るまで繰り返す。
    /// 出力フレームはすべて Worker に送信され、Worker 内の FIFO キューで
    /// user_data と対応付けられる。
    ///
    /// この関数は全ての Worker コマンドの完了を待ち、
    /// コールバックが呼び出され終わるまでブロックする。
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
            }
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
            // Worker 内の pending_values は Stop ハンドラで処理される。
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
/// `pending_values` は Main スレッドからの `QueueFrame` で蓄積され、
/// `Sync` で消費される FIFO キュー。
fn run_sync_worker<H: DecodeHandler>(
    lib: VplLibrary,
    session_handle: usize,
    worker_rx: mpsc::Receiver<WorkerCommand<H::UserData>>,
    mut handler: H,
) {
    let mut pending_values: VecDeque<H::UserData> = VecDeque::new();
    while let Ok(command) = worker_rx.recv() {
        match command {
            WorkerCommand::QueueFrame(user_data) => {
                pending_values.push_back(user_data);
            }
            WorkerCommand::Sync { sync_data } => {
                if let Some(user_data) = pending_values.pop_front() {
                    // SyncOperation + Map + 読み取り + callback + Unmap + Release
                    if let Err(error) =
                        sync_and_callback(lib, session_handle, sync_data, user_data, &mut handler)
                    {
                        handler.on_decoded(Err(error.into()));
                    }
                } else {
                    // user_data が枯渇しているので drain 扱いで破棄する
                    sync_and_drain(lib, session_handle, sync_data);
                }
            }
            WorkerCommand::WaitIdle(reply_tx) => {
                // finish 側のバリア。ここに到達した時点で、それ以前に送信された
                // コマンドはすべて処理済みである。
                let _ = reply_tx.send(Ok(()));
            }
            WorkerCommand::Stop => {
                // drop 時の中断。未完了 frame はすべて MFX_ERR_ABORTED として通知する。
                for _user_data in pending_values.drain(..) {
                    handler.on_decoded(Err(Error::from_mfx(
                        sys::mfxStatus_MFX_ERR_ABORTED,
                        "Decoder::drop",
                    )
                    .into()));
                }
                break;
            }
        }
    }
}

/// `SyncOperation` 完了後の `out_surface` から Map でデータを読み取り、
/// handler の `on_decoded` を呼び出す。
///
/// `DecodedFrame` 内の y/uv スライスは `on_decoded` 呼び出し中のみ有効。
fn sync_and_callback<H: DecodeHandler>(
    lib: VplLibrary,
    session_handle: usize,
    sync_data: DecodeSyncData,
    user_data: H::UserData,
    handler: &mut H,
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

    // デコードされたフレームデータを読み取る
    let frame = read_decoded_surface(&frame_surface, user_data)?;
    handler.on_decoded(Ok(frame));
    Ok(())
}

/// ドレインフレームの Sync + Map/Unmap + Release を行う
///
/// ドレインフレームには user_data がないため、データは読み取らずに解放のみ行う。
/// エラーはすべて無視する（データ破棄が目的のため）。
fn sync_and_drain(lib: VplLibrary, session_handle: usize, sync_data: DecodeSyncData) {
    let DecodeSyncData {
        syncp,
        mut frame_surface,
    } = sync_data;

    if syncp.is_null() {
        return;
    }

    let _ = lib.mfx_video_core_sync_operation(
        session_handle as sys::mfxSession,
        syncp,
        sys::MFX_INFINITE,
    );

    // Drop での自動解放を使ってクリーンアップする
    let _ = frame_surface.map_read();
}

/// デコード済みサーフェスから Y/UV スライスを読み取り `DecodedFrame` を構築する
///
/// Map 済みであることと、戻り値の `DecodedFrame` が生存している間
/// サーフェスデータが有効であることの保証は呼び出し元の責任。
fn read_decoded_surface<'a, T>(
    frame_surface: &'a FrameSurface,
    user_data: T,
) -> Result<DecodedFrame<'a, T>, Error> {
    let surface = unsafe { &*frame_surface.as_ptr() };
    let (crop_w, crop_h) = unsafe {
        let fi = &surface.Info.__bindgen_anon_1.__bindgen_anon_1;
        (fi.CropW as usize, fi.CropH as usize)
    };
    let pitch = unsafe { surface.Data.__bindgen_anon_2.Pitch as usize };
    let y_ptr = unsafe { surface.Data.__bindgen_anon_3.Y };
    let uv_ptr = unsafe { surface.Data.__bindgen_anon_4.UV };

    // VPL が返したサーフェスデータの整合性を検証する
    if y_ptr.is_null() || uv_ptr.is_null() {
        return Err(Error::new_custom(
            "Decoder::sync_worker",
            "decoded surface has null plane pointers",
        ));
    }

    if crop_w == 0 || crop_h == 0 {
        return Err(Error::new_custom(
            "Decoder::sync_worker",
            "decoded surface has zero crop dimensions",
        ));
    }
    if pitch < crop_w {
        return Err(Error::new_custom_owned(
            "Decoder::sync_worker",
            format!("pitch ({pitch}) is less than crop width ({crop_w})"),
        ));
    }

    // ピッチを考慮したプレーンサイズを計算する
    let y_size = pitch
        .checked_mul(crop_h)
        .ok_or_else(|| Error::new_custom("Decoder::sync_worker", "Y plane size overflowed"))?;
    let uv_height = crop_h / 2;
    let uv_size = pitch
        .checked_mul(uv_height)
        .ok_or_else(|| Error::new_custom("Decoder::sync_worker", "UV plane size overflowed"))?;

    let y = unsafe { std::slice::from_raw_parts(y_ptr, y_size) };
    let uv = unsafe { std::slice::from_raw_parts(uv_ptr, uv_size) };

    Ok(DecodedFrame {
        y,
        uv,
        pitch,
        width: crop_w,
        height: crop_h,
        user_data,
    })
}
