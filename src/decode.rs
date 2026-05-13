use std::collections::VecDeque;
use std::sync::mpsc;
use std::thread;

use crate::{AdapterSelector, Error, VplLibrary, sys};

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
    value: T,
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
    pub fn value(&self) -> &T {
        &self.value
    }

    /// デコード時に渡した値を取得する（所有権を移動）
    pub fn into_value(self) -> T {
        self.value
    }
}

/// デコード用サーフェスの同期情報
///
/// Worker スレッドに渡される。
/// `syncp` で SyncOperation を待機し、`out_surface` から Map でデータを読み取る。
struct DecodeSyncData {
    syncp: sys::mfxSyncPoint,
    out_surface: *mut sys::mfxFrameSurface1,
}

// Safety: DecodeSyncData はスレッド間で所有権を移動するだけで、同時アクセスはしない。
unsafe impl Send for DecodeSyncData {}

/// デコード予約情報
///
/// `decode()` で渡された value を保持し、Worker がコールバックを呼ぶ際に
/// `DecodedFrame` に含めて返す。
struct PendingDecode<T> {
    value: T,
}

// Safety: PendingDecode はスレッド間で所有権を移動するだけで、同時アクセスはしない。
unsafe impl<T: Send> Send for PendingDecode<T> {}

/// Worker スレッドへの命令
///
/// - `DecodeFrame`: 通常のデコードフレーム。value を含み、コールバックで通知する。
/// - `DrainFrame`: ドレインで排出されたフレーム、または value 枯渇時のフレーム。
///   Sync + Map + Unmap + Release のみ行い、コールバックは呼ばない。
/// - `WaitIdle`: finish() のバリア。全コマンド処理後に応答を返す。
/// - `Stop`: Drop 時の中断。Worker を停止する。
enum WorkerCommand<T> {
    DecodeFrame {
        sync_data: DecodeSyncData,
        pending: PendingDecode<T>,
    },
    DrainFrame {
        sync_data: DecodeSyncData,
    },
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
    #[allow(clippy::type_complexity)]
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
    lib: VplLibrary,
    loader: sys::mfxLoader,
    session: sys::mfxSession,
    codec: DecoderCodec,
    /// mfxVideoParam.AsyncDepth に設定する値
    async_depth: u16,
    /// DecodeHeader + Init が完了しているか
    initialized: bool,
    /// Worker スレッドへの命令チャネル
    worker_tx: mpsc::Sender<WorkerCommand<H::UserData>>,
    /// Worker スレッドの join ハンドル
    worker_handle: Option<thread::JoinHandle<()>>,
    /// decode() で渡された value の FIFO キュー
    ///
    /// VPL は内部バッファにビットストリームを蓄積するため、
    /// `decode()` 呼び出しとフレーム出力は 1:1 に対応しない。
    /// このキューにより value と出力フレームの対応を管理する。
    pending_values: VecDeque<H::UserData>,
}

// Safety: デコード完了通知は専用スレッドで行い、メインスレッドは DecodeFrameAsync のみ実行する。
// VPL 仕様上、セッション操作の同一スレッド制約は明記されておらず、公式サンプルでも
// セッションハンドルにスレッドアフィニティの制約は課されていない。
// Sync は実装しない（生ポインタにより自動的に !Sync）。
unsafe impl<H: DecodeHandler> Send for Decoder<H> {}

impl<H: DecodeHandler> Decoder<H> {
    /// デコーダを作成する
    ///
    /// `handler` は Worker スレッドから呼ばれる。
    /// `DecodedFrame` のライフタイムはコールバック呼び出し中のみ有効。
    pub fn new(config: DecoderConfig, handler: H) -> Result<Self, Error> {
        let lib = VplLibrary::load()?;

        // API 2.x フローで指定アダプタのセッションを作成する
        let (loader, session) = lib.create_session(config.adapter)?;

        // 初期化失敗時に MFXClose を呼ぶガード
        let session_guard = CloseGuard::session(lib, loader, session);

        let (worker_tx, worker_rx) = mpsc::channel();
        let session_handle = session as usize;
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

        // ガードをキャンセルして所有権を Decoder に移す
        session_guard.cancel();

        Ok(Decoder {
            lib,
            loader,
            session,
            codec: config.codec,
            async_depth: config.async_depth.unwrap_or(4),
            initialized: false,
            worker_tx,
            worker_handle: Some(worker_handle),
            pending_values: VecDeque::new(),
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
        self.lib
            .mfx_video_decode_decode_header(self.session, bs, &mut video_param)?;

        // デコーダを初期化する。警告を返すことがあるが初期化自体は成功している
        self.lib
            .mfx_video_decode_init(self.session, &mut video_param)?;

        self.initialized = true;

        Ok(())
    }

    /// 圧縮されたビットストリームデータをデコードする
    ///
    /// 最初の呼び出し時にヘッダ解析とデコーダ初期化を自動的に行う。
    /// デコード完了時にはコンストラクタで登録したコールバックが呼ばれる。
    ///
    /// `value` は対応するフレームがデコードされたときに [`DecodedFrame::value`] で取得できる。
    /// VPL は内部バッファにビットストリームを蓄積するため、
    /// `decode()` 呼び出しとフレーム出力は 1:1 に対応しない場合がある。
    /// value は `pending_values` キューで管理され、FIFO で出力フレームに割り当てられる。
    pub fn decode(&mut self, data: &[u8], value: H::UserData) -> Result<(), Error> {
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

        // 未初期化の場合は初期化する。value は初期化成功後に push する。
        // これにより初期化エラー時に value が pending_values に残留するのを防ぐ。
        if !self.initialized {
            self.initialize(&mut bs)?;
        }

        self.pending_values.push_back(value);

        match self.decode_bitstream(&mut bs) {
            Ok(()) => Ok(()),
            Err(e) => {
                // エラー時は残留 value をすべて破棄して状態をリセットする
                self.pending_values.clear();
                Err(e)
            }
        }
    }

    /// ビットストリームをデコードしてフレームを収集する
    ///
    /// `bs.DataLength > 0` の間 `DecodeFrameAsync` を繰り返し呼び出す。
    /// VPL は `MORE_DATA` 時にビットストリームを内部バッファに蓄積し、
    /// 十分なデータが溜まった時点でフレームを出力する。
    ///
    /// 出力フレームには `pending_values` から値を取り出して割り当てる。
    /// 値が枯渇した場合は `DrainFrame` として処理する（コールバックなし）。
    fn decode_bitstream(&mut self, bs: &mut sys::mfxBitstream) -> Result<(), Error> {
        while bs.DataLength > 0 {
            let mut syncp: sys::mfxSyncPoint = std::ptr::null_mut();
            let mut out_surface: *mut sys::mfxFrameSurface1 = std::ptr::null_mut();

            // surface_work=NULL で VPL 内部割り当てを使用する
            let status = self.lib.mfx_video_decode_frame_async(
                self.session,
                bs,
                std::ptr::null_mut(),
                &mut out_surface,
                &mut syncp,
            );

            // MORE_DATA: データ不足。VPL 内部に蓄積済み。呼び出し元に戻る
            if status == sys::mfxStatus_MFX_ERR_MORE_DATA {
                return Ok(());
            }
            // MORE_SURFACE: 内部割り当てでは通常発生しないが、安全のため再試行
            if status == sys::mfxStatus_MFX_ERR_MORE_SURFACE {
                continue;
            }
            // DEVICE_BUSY: デバイスが混雑している。1ms 待って再試行
            if status == sys::mfxStatus_MFX_WRN_DEVICE_BUSY {
                std::thread::sleep(std::time::Duration::from_millis(1));
                continue;
            }
            if status < 0 {
                return Err(Error::from_mfx(status, "MFXVideoDECODE_DecodeFrameAsync"));
            }

            // MFX_ERR_NONE または MFX_WRN_*（DEVICE_BUSY 以外）。
            // syncp が非 null ならデコード済みフレームが存在する
            if !syncp.is_null() {
                if let Some(pending_value) = self.pending_values.pop_front() {
                    self.send_worker_command(
                        "Decoder::decode",
                        WorkerCommand::DecodeFrame {
                            sync_data: DecodeSyncData { syncp, out_surface },
                            pending: PendingDecode {
                                value: pending_value,
                            },
                        },
                    )?;
                } else {
                    // pending_values が枯渇しているので drain 扱いで破棄する
                    self.send_worker_command(
                        "Decoder::decode",
                        WorkerCommand::DrainFrame {
                            sync_data: DecodeSyncData { syncp, out_surface },
                        },
                    )?;
                }
            }
        }
        Ok(())
    }

    /// これ以上データが来ないことをデコーダに伝え、残留フレームを排出する
    ///
    /// null bitstream で `DecodeFrameAsync` を `MORE_DATA` が返るまで繰り返す。
    /// ドレイン時に `pending_values` に値が残っていれば `DecodeFrame`（コールバックあり）、
    /// 枯渇していれば `DrainFrame`（コールバックなし）で処理する。
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
            let status = self.lib.mfx_video_decode_frame_async(
                self.session,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut out_surface,
                &mut syncp,
            );

            // MORE_DATA: すべての残留フレームを排出済み
            if status == sys::mfxStatus_MFX_ERR_MORE_DATA {
                break;
            }
            if status == sys::mfxStatus_MFX_WRN_DEVICE_BUSY {
                std::thread::sleep(std::time::Duration::from_millis(1));
                continue;
            }
            if status < 0 {
                return Err(Error::from_mfx(status, "MFXVideoDECODE_DecodeFrameAsync"));
            }

            if !syncp.is_null() {
                // 残留 value があればコールバック付きで送信する。
                // これにより pending_values の全エントリが消費される。
                if let Some(pending_value) = self.pending_values.pop_front() {
                    self.send_worker_command(
                        "Decoder::finish",
                        WorkerCommand::DecodeFrame {
                            sync_data: DecodeSyncData { syncp, out_surface },
                            pending: PendingDecode {
                                value: pending_value,
                            },
                        },
                    )?;
                } else {
                    self.send_worker_command(
                        "Decoder::finish",
                        WorkerCommand::DrainFrame {
                            sync_data: DecodeSyncData { syncp, out_surface },
                        },
                    )?;
                }
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
            // Stop を送って join した時点で worker は確実に終了するため、
            // 以降に worker へデータが届くケースは考慮しない。
            let _ = self.worker_tx.send(WorkerCommand::Stop);
            let _ = handle.join();
        }
    }
}

impl<H: DecodeHandler> Drop for Decoder<H> {
    fn drop(&mut self) {
        self.stop_worker();
        if self.initialized {
            let _ = self.lib.mfx_video_decode_close(self.session);
        }
        let _ = self.lib.mfx_close(self.session);
        self.lib.mfx_unload(self.loader);
    }
}

/// Worker スレッドのメインループ
///
/// mpsc チャネルからコマンドを受信し、VPL API を呼び出す。
/// セッション操作は全てこのスレッドで行うため、VPL のスレッド安全性に関する
/// 暗黙の制約に抵触しない。
fn run_sync_worker<H: DecodeHandler>(
    lib: VplLibrary,
    session_handle: usize,
    worker_rx: mpsc::Receiver<WorkerCommand<H::UserData>>,
    mut handler: H,
) {
    while let Ok(command) = worker_rx.recv() {
        match command {
            WorkerCommand::DecodeFrame { sync_data, pending } => {
                // SyncOperation + Map + 読み取り + callback + Unmap + Release
                if let Err(error) =
                    sync_and_callback(lib, session_handle, sync_data, pending, &mut handler)
                {
                    handler.on_decoded(Err(error.into()));
                }
            }
            WorkerCommand::DrainFrame { sync_data } => {
                // SyncOperation + Map + Unmap + Release のみ。コールバックなし
                sync_and_drain(lib, session_handle, sync_data);
            }
            WorkerCommand::WaitIdle(reply_tx) => {
                // finish 側のバリア。ここに到達した時点で、それ以前に送信された
                // コマンドはすべて処理済みである。
                let _ = reply_tx.send(Ok(()));
            }
            WorkerCommand::Stop => {
                // drop 時の中断。未完了処理は破棄する。
                break;
            }
        }
    }
}

/// デコード済みサーフェスの `Map` / `Unmap` / `Release` を保証するガード
///
/// `Drop` で `Unmap` と `Release` を自動実行する。
struct DecodedSurfaceGuard {
    lib: VplLibrary,
    surface: *mut sys::mfxFrameSurface1,
    mapped: bool,
}

impl DecodedSurfaceGuard {
    fn new(lib: VplLibrary, surface: *mut sys::mfxFrameSurface1) -> Self {
        Self {
            lib,
            surface,
            mapped: false,
        }
    }

    fn surface(&self) -> *mut sys::mfxFrameSurface1 {
        self.surface
    }

    fn map_read(&mut self) -> Result<(), Error> {
        let status = self
            .lib
            .mfx_frame_surface_map(self.surface, sys::mfxMemoryFlags_MFX_MAP_READ);
        Error::check_mfx(status, "mfxFrameSurfaceInterface::Map")?;
        self.mapped = true;
        Ok(())
    }
}

impl Drop for DecodedSurfaceGuard {
    fn drop(&mut self) {
        if self.surface.is_null() {
            return;
        }
        if self.mapped {
            let _ = Error::check_mfx(
                self.lib.mfx_frame_surface_unmap(self.surface),
                "mfxFrameSurfaceInterface::Unmap",
            );
        }
        let _ = Error::check_mfx(
            self.lib.mfx_frame_surface_release(self.surface),
            "mfxFrameSurfaceInterface::Release",
        );
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
    pending: PendingDecode<H::UserData>,
    handler: &mut H,
) -> Result<(), crate::Error> {
    let DecodeSyncData { syncp, out_surface } = sync_data;
    let mut surface_guard = DecodedSurfaceGuard::new(lib, out_surface);

    if syncp.is_null() {
        return Err(Error::new_custom(
            "Decoder::sync_worker",
            "sync point is null",
        ));
    }

    if out_surface.is_null() {
        return Err(Error::new_custom(
            "Decoder::sync_worker",
            "output surface is null",
        ));
    }

    // SyncOperation でデコード完了を待機する
    let status = lib.mfx_video_core_sync_operation(
        session_handle as sys::mfxSession,
        syncp,
        sys::MFX_INFINITE,
    );
    Error::check_mfx(status, "MFXVideoCORE_SyncOperation")?;

    surface_guard.map_read()?;

    // デコードされたフレームデータを読み取る
    let frame = read_decoded_surface(&surface_guard, pending)?;
    handler.on_decoded(Ok(frame));
    Ok(())
}

/// ドレインフレームの Sync + Map/Unmap + Release を行う
///
/// ドレインフレームには value がないため、データは読み取らずに解放のみ行う。
/// エラーはすべて無視する（データ破棄が目的のため）。
fn sync_and_drain(lib: VplLibrary, session_handle: usize, sync_data: DecodeSyncData) {
    let DecodeSyncData { syncp, out_surface } = sync_data;
    let mut surface_guard = DecodedSurfaceGuard::new(lib, out_surface);

    if syncp.is_null() || out_surface.is_null() {
        return;
    }

    let _ = lib.mfx_video_core_sync_operation(
        session_handle as sys::mfxSession,
        syncp,
        sys::MFX_INFINITE,
    );

    // Drop での自動解放を使ってクリーンアップする
    let _ = surface_guard.map_read();
}

/// デコード済みサーフェスから Y/UV スライスを読み取り `DecodedFrame` を構築する
///
/// # Safety
///
/// 呼び出し元は `out_surface` が Map 済みで有効なデータを持つことを保証すること。
/// 戻り値の `DecodedFrame` が生存している間、`out_surface` のデータは有効でなければならない。
unsafe fn read_decoded_surface_inner<'a, T>(
    out_surface: *mut sys::mfxFrameSurface1,
    pending: PendingDecode<T>,
) -> Result<DecodedFrame<'a, T>, Error> {
    let surface = unsafe { &*out_surface };
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
        value: pending.value,
    })
}

/// デコード済みサーフェスを読み取る安全性ラッパー
///
/// `DecodedFrame` のライフタイムを `DecodedSurfaceGuard` に束縛し、
/// callback 呼び出し中のみデータ参照が有効になるようにする。
fn read_decoded_surface<'a, T>(
    surface_guard: &'a DecodedSurfaceGuard,
    pending: PendingDecode<T>,
) -> Result<DecodedFrame<'a, T>, Error> {
    unsafe { read_decoded_surface_inner(surface_guard.surface(), pending) }
}

/// セッションの解放ガード
///
/// エラー時に `MFXClose` / `MFXUnload` を呼ぶ。
/// `cancel()` でガードを無効化し、正常系ではリソースを `Decoder` に移管する。
struct CloseGuard {
    lib: VplLibrary,
    loader: sys::mfxLoader,
    session: sys::mfxSession,
    active: bool,
}

impl CloseGuard {
    fn session(lib: VplLibrary, loader: sys::mfxLoader, session: sys::mfxSession) -> Self {
        Self {
            lib,
            loader,
            session,
            active: true,
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
        let _ = self.lib.mfx_close(self.session);
        self.lib.mfx_unload(self.loader);
    }
}
