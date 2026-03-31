use std::collections::VecDeque;

use crate::{Error, VplLibrary, sys};

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
/// デコードするビットストリームのコーデックを指定する。
/// 解像度やフレームレートはビットストリームのヘッダから自動的に検出される。
#[derive(Debug, Clone)]
pub struct DecoderConfig {
    /// コーデック識別子
    pub codec: DecoderCodec,
}

/// デコードされたフレーム
///
/// NV12 フォーマット (Y プレーン + インターリーブ UV プレーン) のフレームデータを保持する。
/// データサイズは `width * height * 3 / 2` バイトとなる。
pub struct DecodedFrame {
    width: usize,
    height: usize,
    data: Vec<u8>,
}

impl DecodedFrame {
    /// フレームデータを取得する
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    /// フレームデータを取得する（所有権を移動）
    pub fn into_data(self) -> Vec<u8> {
        self.data
    }

    /// フレームの幅を返す
    pub fn width(&self) -> usize {
        self.width
    }

    /// フレームの高さを返す
    pub fn height(&self) -> usize {
        self.height
    }
}

/// デコード用サーフェスプールのサーフェス数
const DECODE_SURFACE_POOL_SIZE: usize = 8;

/// Intel VPL ハードウェアデコーダ
///
/// 圧縮されたビットストリームを NV12 フレームにデコードする。
/// 最初の [`Decoder::decode`] 呼び出し時にビットストリームのヘッダ (SPS/PPS 等) を解析して
/// 自動的に初期化される。
///
/// # 使い方
///
/// 1. [`Decoder::new`] でデコーダを作成する
/// 2. [`Decoder::decode`] でビットストリームデータを投入する
/// 3. [`Decoder::next_frame`] でデコード済みフレームを取り出す
/// 4. すべてのデータを投入したら [`Decoder::finish`] を呼んで残留フレームを排出する
pub struct Decoder {
    lib: VplLibrary,
    loader: sys::mfxLoader,
    session: sys::mfxSession,
    codec: DecoderCodec,
    surfaces: Vec<sys::mfxFrameSurface1>,
    /// サーフェスごとのフレームバッファ（surfaces と同じインデックスで対応する）
    surface_buffers: Vec<Vec<u8>>,
    /// デコードされたフレームサイズ情報
    surf_width: usize,
    surf_height: usize,
    /// デコード済みフレームのキュー
    decoded_frames: VecDeque<DecodedFrame>,
    /// フラッシュ中かどうか
    flushing: bool,
}

// Safety: Decoder の全公開メソッドは &mut self を要求するため、同時に複数スレッドから
// アクセスされることはない。VPL 仕様上、セッション操作の同一スレッド制約は明記されて
// いないため、スレッド間の移動は許容する。Intel の公式サンプル（hello-decode 等）でも
// セッションハンドルにスレッドアフィニティの制約は課されていない。
// Sync は実装しない（生ポインタにより自動的に !Sync）。
unsafe impl Send for Decoder {}

impl Decoder {
    /// デコーダを作成する
    pub fn new(config: DecoderConfig) -> Result<Self, Error> {
        let lib = VplLibrary::load()?;

        // API 2.x フローでセッションを作成する（ハードウェア実装を使用）
        let (loader, session) = lib.create_session(sys::mfxImplType_MFX_IMPL_TYPE_HARDWARE)?;

        Ok(Decoder {
            lib,
            loader,
            session,
            codec: config.codec,
            surfaces: Vec::new(),
            surface_buffers: Vec::new(),
            surf_width: 0,
            surf_height: 0,
            decoded_frames: VecDeque::new(),
            flushing: false,
        })
    }

    /// ビットストリームからヘッダを解析してデコーダを初期化する
    ///
    /// 最初のデコード呼び出しの前に、ビットストリームの先頭（SPS/PPS 等を含む部分）で
    /// この関数を呼び出す必要がある。
    fn initialize(&mut self, bs: &mut sys::mfxBitstream) -> Result<(), Error> {
        let codec_id = self.codec.codec_id();

        // デコーダパラメータを設定する
        let mut video_param: sys::mfxVideoParam = unsafe { std::mem::zeroed() };
        video_param.IOPattern = sys::MFX_IOPATTERN_OUT_SYSTEM_MEMORY as u16;
        video_param.AsyncDepth = 1;
        unsafe {
            let mfx = &mut video_param.__bindgen_anon_1.mfx;
            mfx.CodecId = codec_id;
            mfx.FrameInfo.FourCC = sys::MFX_FOURCC_NV12;
            mfx.FrameInfo.ChromaFormat = sys::MFX_CHROMAFORMAT_YUV420 as u16;
            mfx.FrameInfo.PicStruct = sys::MFX_PICSTRUCT_PROGRESSIVE as u16;
        }

        // DecodeHeader でビットストリームからパラメータを読み取る
        self.lib
            .mfx_video_decode_decode_header(self.session, bs, &mut video_param)?;

        // デコーダを初期化する
        self.lib
            .mfx_video_decode_init(self.session, &mut video_param)?;

        // サーフェスプールを確保する
        let (surf_width, surf_height) = unsafe {
            let mfx = &video_param.__bindgen_anon_1.mfx;
            let fi = &mfx.FrameInfo.__bindgen_anon_1.__bindgen_anon_1;
            (fi.Width as usize, fi.Height as usize)
        };
        // NV12: Y平面 + UV平面 (各ピクセル 1.5 バイト)
        let frame_size = surf_width
            .checked_mul(surf_height)
            .and_then(|v| v.checked_mul(3))
            .map(|v| v / 2)
            .ok_or_else(|| {
                Error::new_custom("Decoder::initialize", "surface size calculation overflowed")
            })?;

        let num_surfaces = DECODE_SURFACE_POOL_SIZE;
        let mut surface_buffers: Vec<Vec<u8>> =
            (0..num_surfaces).map(|_| vec![0u8; frame_size]).collect();
        let mut surfaces: Vec<sys::mfxFrameSurface1> = Vec::with_capacity(num_surfaces);

        let dec_mfx_frame_info = unsafe { video_param.__bindgen_anon_1.mfx.FrameInfo };
        for buf in &mut surface_buffers {
            let mut surface: sys::mfxFrameSurface1 = unsafe { std::mem::zeroed() };
            surface.Info = dec_mfx_frame_info;
            unsafe {
                surface.Data.__bindgen_anon_2.Pitch = surf_width as u16;
                surface.Data.__bindgen_anon_3.Y = buf.as_mut_ptr();
                surface.Data.__bindgen_anon_4.UV = buf.as_mut_ptr().add(surf_width * surf_height);
            }
            surfaces.push(surface);
        }

        self.surfaces = surfaces;
        self.surface_buffers = surface_buffers;
        self.surf_width = surf_width;
        self.surf_height = surf_height;

        Ok(())
    }

    /// 圧縮されたビットストリームデータをデコードする
    ///
    /// 最初の呼び出し時にヘッダ解析とデコーダ初期化を自動的に行う。
    /// デコード済みフレームは [`Decoder::next_frame`] で取り出す。
    pub fn decode(&mut self, data: &[u8]) -> Result<(), Error> {
        let mut bs: sys::mfxBitstream = unsafe { std::mem::zeroed() };
        // VPL API は *mut を要求するが入力データを書き換えないためキャストする
        let data_len = u32::try_from(data.len()).map_err(|_| {
            Error::new_custom_owned(
                "Decoder::decode",
                format!("bitstream length {} exceeds u32::MAX", data.len()),
            )
        })?;
        bs.Data = data.as_ptr() as *mut u8;
        bs.DataLength = data_len;
        bs.MaxLength = data_len;

        // サーフェスが空の場合は初期化が必要
        if self.surfaces.is_empty() {
            self.initialize(&mut bs)?;
        }

        self.decode_bitstream(&mut bs)?;
        Ok(())
    }

    /// ビットストリームをデコードしてフレームを収集する
    fn decode_bitstream(&mut self, bs: &mut sys::mfxBitstream) -> Result<(), Error> {
        while bs.DataLength > 0 {
            let work_surface = self
                .surfaces
                .iter_mut()
                .find(|s| s.Data.Locked == 0)
                .ok_or_else(|| Error::new_custom("Decoder::decode", "no free surface available"))?;

            let mut syncp: sys::mfxSyncPoint = std::ptr::null_mut();
            let mut out_surface: *mut sys::mfxFrameSurface1 = std::ptr::null_mut();

            let status = self.lib.mfx_video_decode_frame_async(
                self.session,
                bs,
                work_surface,
                &mut out_surface,
                &mut syncp,
            );

            if status == sys::mfxStatus_MFX_ERR_MORE_DATA {
                // データ不足、呼び出し元に戻る
                break;
            }
            if status == sys::mfxStatus_MFX_ERR_MORE_SURFACE {
                // サーフェス不足、再試行する
                continue;
            }
            if status == sys::mfxStatus_MFX_WRN_DEVICE_BUSY {
                std::thread::sleep(std::time::Duration::from_millis(1));
                continue;
            }
            if status < 0 {
                return Err(Error::from_mfx(status, "MFXVideoDECODE_DecodeFrameAsync"));
            }

            if !syncp.is_null() {
                self.sync_and_collect(syncp, out_surface)?;
            }
        }
        Ok(())
    }

    /// これ以上データが来ないことをデコーダに伝え、残留フレームを排出する
    ///
    /// デコーダの内部バッファに残っているフレームをすべて処理する。
    /// 完了後は [`Decoder::next_frame`] で残りのフレームを取り出せる。
    pub fn finish(&mut self) -> Result<(), Error> {
        // 初期化前（decode 未呼び出し）なら排出するフレームがないので即座に返す
        if self.surfaces.is_empty() {
            return Ok(());
        }

        self.flushing = true;

        loop {
            let work_surface = self
                .surfaces
                .iter_mut()
                .find(|s| s.Data.Locked == 0)
                .ok_or_else(|| Error::new_custom("Decoder::finish", "no free surface available"))?;

            let mut syncp: sys::mfxSyncPoint = std::ptr::null_mut();
            let mut out_surface: *mut sys::mfxFrameSurface1 = std::ptr::null_mut();

            let status = self.lib.mfx_video_decode_frame_async(
                self.session,
                std::ptr::null_mut(),
                work_surface,
                &mut out_surface,
                &mut syncp,
            );

            if status == sys::mfxStatus_MFX_ERR_MORE_DATA {
                // すべて排出済み
                break;
            }
            if status == sys::mfxStatus_MFX_ERR_MORE_SURFACE {
                continue;
            }
            if status == sys::mfxStatus_MFX_WRN_DEVICE_BUSY {
                std::thread::sleep(std::time::Duration::from_millis(1));
                continue;
            }
            if status < 0 {
                return Err(Error::from_mfx(status, "MFXVideoDECODE_DecodeFrameAsync"));
            }

            if !syncp.is_null() {
                self.sync_and_collect(syncp, out_surface)?;
            }
        }

        self.flushing = false;
        Ok(())
    }

    /// デコード済みのフレームを取り出す
    ///
    /// フレームがない場合は `None` を返す。
    /// [`Decoder::decode`] または [`Decoder::finish`] の後に呼び出す。
    pub fn next_frame(&mut self) -> Option<DecodedFrame> {
        self.decoded_frames.pop_front()
    }

    /// SyncOperation を実行してデコード済みフレームを収集する
    fn sync_and_collect(
        &mut self,
        syncp: sys::mfxSyncPoint,
        out_surface: *mut sys::mfxFrameSurface1,
    ) -> Result<(), Error> {
        Error::check_mfx(
            self.lib
                .mfx_video_core_sync_operation(self.session, syncp, sys::MFX_INFINITE),
            "MFXVideoCORE_SyncOperation",
        )?;

        if out_surface.is_null() {
            return Ok(());
        }

        // デコードされたフレームデータをコピーする
        let surface = unsafe { &*out_surface };
        let (crop_w, crop_h) = unsafe {
            let fi = &surface.Info.__bindgen_anon_1.__bindgen_anon_1;
            (fi.CropW as usize, fi.CropH as usize)
        };
        let pitch = unsafe { surface.Data.__bindgen_anon_2.Pitch as usize };
        let y_ptr = unsafe { surface.Data.__bindgen_anon_3.Y };
        let uv_ptr = unsafe { surface.Data.__bindgen_anon_4.UV };

        if y_ptr.is_null() || uv_ptr.is_null() {
            return Err(Error::new_custom(
                "Decoder::sync_and_collect",
                "decoded surface has null plane pointers",
            ));
        }

        // VPL が返したサーフェス寸法の整合性を検証する
        if crop_w == 0 || crop_h == 0 {
            return Err(Error::new_custom(
                "Decoder::sync_and_collect",
                "decoded surface has zero crop dimensions",
            ));
        }
        if pitch < crop_w {
            return Err(Error::new_custom_owned(
                "Decoder::sync_and_collect",
                format!("pitch ({pitch}) is less than crop width ({crop_w})"),
            ));
        }
        if crop_w > self.surf_width || crop_h > self.surf_height {
            return Err(Error::new_custom_owned(
                "Decoder::sync_and_collect",
                format!(
                    "crop dimensions ({crop_w}x{crop_h}) exceed surface dimensions ({}x{})",
                    self.surf_width, self.surf_height
                ),
            ));
        }

        // NV12: Y プレーン (crop_w * crop_h) + UV プレーン (crop_w * crop_h / 2)
        let y_size = crop_w * crop_h;
        let uv_size = crop_w * (crop_h / 2);
        let mut data = vec![0u8; y_size + uv_size];

        // ピッチを考慮してコピーする（ピッチ == crop_w なら一括コピー可能）
        if pitch == crop_w {
            unsafe {
                std::ptr::copy_nonoverlapping(y_ptr, data.as_mut_ptr(), y_size);
                std::ptr::copy_nonoverlapping(uv_ptr, data[y_size..].as_mut_ptr(), uv_size);
            }
        } else {
            // 行ごとにコピーする
            for row in 0..crop_h {
                unsafe {
                    std::ptr::copy_nonoverlapping(
                        y_ptr.add(row * pitch),
                        data[row * crop_w..].as_mut_ptr(),
                        crop_w,
                    );
                }
            }
            for row in 0..(crop_h / 2) {
                unsafe {
                    std::ptr::copy_nonoverlapping(
                        uv_ptr.add(row * pitch),
                        data[y_size + row * crop_w..].as_mut_ptr(),
                        crop_w,
                    );
                }
            }
        }

        self.decoded_frames.push_back(DecodedFrame {
            width: crop_w,
            height: crop_h,
            data,
        });

        Ok(())
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        if !self.surfaces.is_empty() {
            let _ = self.lib.mfx_video_decode_close(self.session);
        }
        let _ = self.lib.mfx_close(self.session);
        self.lib.mfx_unload(self.loader);
    }
}
