use std::ptr::NonNull;

use crate::{AdapterSelector, Error, sys};

/// mfxEncodeCtrl.FrameType に対応するフレームタイプ定数
///
/// `EncodeOptions.frame_type` にビット OR で組み合わせて指定する。
pub mod frame_type {
    use crate::sys;

    /// フレームタイプ未指定（エンコーダが自動決定）
    pub const UNKNOWN: u16 = 0;
    /// I フレーム
    pub const I: u16 = sys::MFX_FRAMETYPE_I as u16;
    /// P フレーム
    pub const P: u16 = sys::MFX_FRAMETYPE_P as u16;
    /// B フレーム
    pub const B: u16 = sys::MFX_FRAMETYPE_B as u16;
    /// S フレーム（SkipFrame）
    pub const S: u16 = sys::MFX_FRAMETYPE_S as u16;
    /// 参照フレーム
    pub const REF: u16 = sys::MFX_FRAMETYPE_REF as u16;
    /// IDR フレーム
    pub const IDR: u16 = sys::MFX_FRAMETYPE_IDR as u16;
}

/// mfxInfoMFX.GopOptFlag に対応する GOP オプション定数
///
/// `EncoderConfig.gop_opt_flag` にビット OR で組み合わせて指定する。
pub mod gop_opt_flag {
    use crate::sys;

    /// Closed GOP（前の GOP のフレームを参照しない）
    pub const CLOSED: u16 = sys::MFX_GOP_CLOSED as u16;
    /// Strict GOP（フレームタイプの変更を禁止する）
    pub const STRICT: u16 = sys::MFX_GOP_STRICT as u16;
}

/// Intel VPL ライブラリのラッパー構造体
///
/// 静的リンクされた libvpl の関数を直接呼び出す。
#[derive(Debug, Clone, Copy)]
pub(crate) struct VplLibrary;

impl VplLibrary {
    /// VPL ライブラリをロードする
    pub(crate) fn load() -> Result<Self, Error> {
        Ok(Self)
    }

    /// 指定アダプタのセッションを生成する
    ///
    /// 成功時は `Session` を返す。
    /// 指定の DRM render node に対応する Intel HW 実装が見つからない場合は `Error` を返す。
    pub(crate) fn create_session(&self, adapter: AdapterSelector) -> Result<Session, Error> {
        adapter.validate()?;
        let AdapterSelector::DrmRenderNode(render_node) = adapter;

        let mut loader = VplLoader::new(*self)?;
        loader.require_hardware()?;
        loader.require_drm_render_node(render_node)?;
        loader.create_session()
    }

    /// MFXUnload を呼び出してローダーを破棄する
    pub(crate) fn mfx_unload(&self, loader: sys::mfxLoader) {
        unsafe { sys::MFXUnload(loader) };
    }

    /// MFXClose を呼び出してセッションを破棄する
    pub(crate) fn mfx_close(&self, session: sys::mfxSession) -> Result<(), Error> {
        let status = unsafe { sys::MFXClose(session) };
        Error::check_mfx(status, "MFXClose")
    }

    /// MFXVideoENCODE_Init を呼び出してエンコーダを初期化する
    ///
    /// VPL の仕様では Init は MFX_WRN_INCOMPATIBLE_VIDEO_PARAM 等の警告を
    /// 返すことがあるが、初期化自体は成功している。警告は許容する。
    pub(crate) fn mfx_video_encode_init(
        &self,
        session: sys::mfxSession,
        par: *mut sys::mfxVideoParam,
    ) -> Result<(), Error> {
        let status = unsafe { sys::MFXVideoENCODE_Init(session, par) };
        Error::check_mfx_allow_warn(status, "MFXVideoENCODE_Init")
    }

    /// MFXVideoENCODE_Close を呼び出してエンコーダを破棄する
    pub(crate) fn mfx_video_encode_close(&self, session: sys::mfxSession) -> Result<(), Error> {
        let status = unsafe { sys::MFXVideoENCODE_Close(session) };
        Error::check_mfx(status, "MFXVideoENCODE_Close")
    }

    /// MFXVideoENCODE_EncodeFrameAsync を呼び出してフレームを非同期エンコードする
    ///
    /// mfxStatus をそのまま返す（MFX_ERR_MORE_DATA や MFX_WRN_DEVICE_BUSY の処理は呼び出し元が行う）
    pub(crate) fn mfx_video_encode_frame_async(
        &self,
        session: sys::mfxSession,
        ctrl: *mut sys::mfxEncodeCtrl,
        surface: *mut sys::mfxFrameSurface1,
        bs: *mut sys::mfxBitstream,
        syncp: *mut sys::mfxSyncPoint,
    ) -> i32 {
        unsafe { sys::MFXVideoENCODE_EncodeFrameAsync(session, ctrl, surface, bs, syncp) }
    }

    /// MFXVideoCORE_SyncOperation を呼び出して非同期操作の完了を待機する
    pub(crate) fn mfx_video_core_sync_operation(
        &self,
        session: sys::mfxSession,
        syncp: sys::mfxSyncPoint,
        wait: u32,
    ) -> i32 {
        unsafe { sys::MFXVideoCORE_SyncOperation(session, syncp, wait) }
    }

    /// MFXVideoENCODE_Query を呼び出してパラメータのサポート可否を検証する
    pub(crate) fn mfx_video_encode_query(
        &self,
        session: sys::mfxSession,
        input: *mut sys::mfxVideoParam,
        output: *mut sys::mfxVideoParam,
    ) -> i32 {
        unsafe { sys::MFXVideoENCODE_Query(session, input, output) }
    }

    /// MFXVideoENCODE_Reset を呼び出してエンコーダパラメータを動的に変更する
    ///
    /// Init と同様に警告を返すことがあるが、リセット自体は成功している。
    pub(crate) fn mfx_video_encode_reset(
        &self,
        session: sys::mfxSession,
        par: *mut sys::mfxVideoParam,
    ) -> Result<(), Error> {
        let status = unsafe { sys::MFXVideoENCODE_Reset(session, par) };
        Error::check_mfx_allow_warn(status, "MFXVideoENCODE_Reset")
    }

    /// MFXVideoENCODE_GetVideoParam を呼び出して実効パラメータを取得する
    pub(crate) fn mfx_video_encode_get_video_param(
        &self,
        session: sys::mfxSession,
        par: *mut sys::mfxVideoParam,
    ) -> Result<(), Error> {
        let status = unsafe { sys::MFXVideoENCODE_GetVideoParam(session, par) };
        Error::check_mfx(status, "MFXVideoENCODE_GetVideoParam")
    }

    /// MFXVideoENCODE_GetEncodeStat を呼び出してエンコード統計情報を取得する
    pub(crate) fn mfx_video_encode_get_encode_stat(
        &self,
        session: sys::mfxSession,
        stat: *mut sys::mfxEncodeStat,
    ) -> Result<(), Error> {
        let status = unsafe { sys::MFXVideoENCODE_GetEncodeStat(session, stat) };
        Error::check_mfx(status, "MFXVideoENCODE_GetEncodeStat")
    }

    /// MFXVideoDECODE_DecodeHeader を呼び出してビットストリームからパラメータを読み取る
    pub(crate) fn mfx_video_decode_decode_header(
        &self,
        session: sys::mfxSession,
        bs: *mut sys::mfxBitstream,
        par: *mut sys::mfxVideoParam,
    ) -> Result<(), Error> {
        let status = unsafe { sys::MFXVideoDECODE_DecodeHeader(session, bs, par) };
        Error::check_mfx(status, "MFXVideoDECODE_DecodeHeader")
    }

    /// MFXVideoDECODE_Init を呼び出してデコーダを初期化する
    ///
    /// エンコーダと同様に警告を返すことがあるが、初期化自体は成功している。
    pub(crate) fn mfx_video_decode_init(
        &self,
        session: sys::mfxSession,
        par: *mut sys::mfxVideoParam,
    ) -> Result<(), Error> {
        let status = unsafe { sys::MFXVideoDECODE_Init(session, par) };
        Error::check_mfx_allow_warn(status, "MFXVideoDECODE_Init")
    }

    /// MFXVideoDECODE_Close を呼び出してデコーダを破棄する
    pub(crate) fn mfx_video_decode_close(&self, session: sys::mfxSession) -> Result<(), Error> {
        let status = unsafe { sys::MFXVideoDECODE_Close(session) };
        Error::check_mfx(status, "MFXVideoDECODE_Close")
    }

    /// MFXVideoDECODE_DecodeFrameAsync を呼び出してフレームを非同期デコードする
    ///
    /// mfxStatus をそのまま返す（MFX_ERR_MORE_DATA 等の処理は呼び出し元が行う）
    pub(crate) fn mfx_video_decode_frame_async(
        &self,
        session: sys::mfxSession,
        bs: *mut sys::mfxBitstream,
        work_surface: *mut sys::mfxFrameSurface1,
        out_surface: *mut *mut sys::mfxFrameSurface1,
        syncp: *mut sys::mfxSyncPoint,
    ) -> i32 {
        unsafe {
            sys::MFXVideoDECODE_DecodeFrameAsync(session, bs, work_surface, out_surface, syncp)
        }
    }

    /// MFXMemory_GetSurfaceForEncode を呼び出してエンコード用内部サーフェスを取得する
    pub(crate) fn mfx_memory_get_surface_for_encode(
        &self,
        session: sys::mfxSession,
        surface: *mut *mut sys::mfxFrameSurface1,
    ) -> i32 {
        unsafe { sys::MFXMemory_GetSurfaceForEncode(session, surface) }
    }

    /// mfxFrameSurfaceInterface::Map を呼び出してサーフェスをマップする
    pub(crate) fn mfx_frame_surface_map(
        &self,
        surface: *mut sys::mfxFrameSurface1,
        flags: u32,
    ) -> i32 {
        unsafe {
            let iface = (*surface).__bindgen_anon_1.FrameInterface;
            if iface.is_null() {
                return sys::mfxStatus_MFX_ERR_NULL_PTR;
            }
            let Some(map_fn) = (*iface).Map else {
                return sys::mfxStatus_MFX_ERR_NULL_PTR;
            };
            map_fn(surface, flags)
        }
    }

    /// mfxFrameSurfaceInterface::Unmap を呼び出してサーフェスをアンマップする
    pub(crate) fn mfx_frame_surface_unmap(&self, surface: *mut sys::mfxFrameSurface1) -> i32 {
        unsafe {
            let iface = (*surface).__bindgen_anon_1.FrameInterface;
            if iface.is_null() {
                return sys::mfxStatus_MFX_ERR_NULL_PTR;
            }
            let Some(unmap_fn) = (*iface).Unmap else {
                return sys::mfxStatus_MFX_ERR_NULL_PTR;
            };
            unmap_fn(surface)
        }
    }

    /// mfxFrameSurfaceInterface::Release を呼び出してサーフェスの参照を解除する
    pub(crate) fn mfx_frame_surface_release(&self, surface: *mut sys::mfxFrameSurface1) -> i32 {
        unsafe {
            let iface = (*surface).__bindgen_anon_1.FrameInterface;
            if iface.is_null() {
                return sys::mfxStatus_MFX_ERR_NULL_PTR;
            }
            let Some(release_fn) = (*iface).Release else {
                return sys::mfxStatus_MFX_ERR_NULL_PTR;
            };
            release_fn(surface)
        }
    }
}

/// mfxFrameSurface1 の RAII ガード
///
/// Map/Unmap/Release を自動管理する。
pub(crate) struct FrameSurface {
    lib: VplLibrary,
    surface: NonNull<sys::mfxFrameSurface1>,
    mapped: bool,
}

impl FrameSurface {
    /// 新しい FrameSurface を作成する
    ///
    /// `surface` が null の場合は `Error` を返す。
    pub fn new(lib: VplLibrary, surface: *mut sys::mfxFrameSurface1) -> Result<Self, Error> {
        let surface = NonNull::new(surface)
            .ok_or_else(|| Error::new_custom("FrameSurface::new", "surface pointer is null"))?;
        Ok(Self {
            lib,
            surface,
            mapped: false,
        })
    }

    /// 生ポインタを返す
    pub fn as_ptr(&self) -> *mut sys::mfxFrameSurface1 {
        self.surface.as_ptr()
    }

    /// サーフェスを書き込みモードでマップする
    ///
    /// 既にマップ済みの場合は `Error` を返す。
    pub fn map_write(&mut self) -> Result<(), Error> {
        self.map_inner("FrameSurface::map_write", sys::mfxMemoryFlags_MFX_MAP_WRITE)
    }

    /// サーフェスを読み取りモードでマップする
    ///
    /// 既にマップ済みの場合は `Error` を返す。
    pub fn map_read(&mut self) -> Result<(), Error> {
        self.map_inner("FrameSurface::map_read", sys::mfxMemoryFlags_MFX_MAP_READ)
    }

    /// サーフェスを指定フラグでマップする内部ヘルパー
    fn map_inner(&mut self, function: &'static str, flags: u32) -> Result<(), Error> {
        if self.mapped {
            return Err(Error::new_custom(function, "surface is already mapped"));
        }
        let status = self.lib.mfx_frame_surface_map(self.as_ptr(), flags);
        Error::check_mfx(status, "mfxFrameSurfaceInterface::Map")?;
        self.mapped = true;
        Ok(())
    }

    /// サーフェスをアンマップする
    ///
    /// マップされていない場合は `Error` を返す。
    pub fn unmap(&mut self) -> Result<(), Error> {
        if !self.mapped {
            return Err(Error::new_custom(
                "FrameSurface::unmap",
                "surface is not mapped",
            ));
        }
        let status = self.lib.mfx_frame_surface_unmap(self.as_ptr());
        Error::check_mfx(status, "mfxFrameSurfaceInterface::Unmap")?;
        self.mapped = false;
        Ok(())
    }
}

impl Drop for FrameSurface {
    fn drop(&mut self) {
        if self.mapped {
            let _ = Error::check_mfx(
                self.lib.mfx_frame_surface_unmap(self.as_ptr()),
                "mfxFrameSurfaceInterface::Unmap",
            );
        }
        let _ = Error::check_mfx(
            self.lib.mfx_frame_surface_release(self.as_ptr()),
            "mfxFrameSurfaceInterface::Release",
        );
    }
}

/// VPL ローダーの RAII ラッパー
///
/// ローダーを作成し、HW 実装フィルタと DRM render node フィルタを設定できる。
/// `create_session` でセッションを生成するか、`as_ptr` で生のローダーハンドルを取得する。
/// この型は Drop で `MFXUnload` によりローダーを解放する（`create_session` で `Session` に
/// 所有権を渡した場合はそちらが解放する）。
pub(crate) struct VplLoader {
    lib: VplLibrary,
    loader: sys::mfxLoader,
    /// `require_drm_render_node` で設定した render node
    ///
    /// `create_session` が `MFX_ERR_NOT_FOUND` を返したときのメッセージに使う。
    render_node: Option<u32>,
    /// `create_session` でローダーの所有権を `Session` に渡したか
    ///
    /// true のときは Drop で `MFXUnload` を呼ばない。
    consumed: bool,
}

impl VplLoader {
    /// ローダーを作成する
    ///
    /// 作成に失敗した場合は `MFXLoad` を function に持つ `Error` を返す。
    pub(crate) fn new(lib: VplLibrary) -> Result<Self, Error> {
        let loader = unsafe { sys::MFXLoad() };
        if loader.is_null() {
            return Err(Error::new_custom("MFXLoad", "returned null"));
        }
        Ok(Self {
            lib,
            loader,
            render_node: None,
            consumed: false,
        })
    }

    /// HW 実装のみを選択するフィルタを設定する
    ///
    /// 1 つのローダーに対して 1 回だけ呼び出すこと。複数回呼ぶとフィルタが重複する。
    /// 設定に失敗した場合は `MFXCreateConfig` または `MFXSetConfigFilterProperty` を
    /// function に持つ `Error` を返す。
    pub(crate) fn require_hardware(&self) -> Result<(), Error> {
        let cfg = unsafe { sys::MFXCreateConfig(self.loader) };
        if cfg.is_null() {
            return Err(Error::new_custom("MFXCreateConfig", "returned null"));
        }
        // libvpl の慣用に合わせ、プロパティごとに別の mfxConfig に設定する
        // （ヘッダ mfxdispatcher.h の MFX_ADD_PROPERTY_U32 マクロが 1 プロパティ 1 cfg で
        // 組み立てるスタイル）。
        let name = b"mfxImplDescription.Impl\0";
        let mut variant: sys::mfxVariant = unsafe { std::mem::zeroed() };
        variant.Type = sys::mfxVariantType_MFX_VARIANT_TYPE_U32;
        variant.Data.U32 = sys::mfxImplType_MFX_IMPL_TYPE_HARDWARE;
        let status = unsafe { sys::MFXSetConfigFilterProperty(cfg, name.as_ptr(), variant) };
        if status != sys::mfxStatus_MFX_ERR_NONE {
            return Err(Error::from_mfx(status, "MFXSetConfigFilterProperty"));
        }
        Ok(())
    }

    /// DRM render node を選択するフィルタを設定する
    ///
    /// 1 つのローダーに対して 1 回だけ呼び出すこと。複数回呼ぶとフィルタが重複する。
    /// 設定に失敗した場合は `MFXCreateConfig` または `MFXSetConfigFilterProperty` を
    /// function に持つ `Error` を返す。
    pub(crate) fn require_drm_render_node(&mut self, render_node: u32) -> Result<(), Error> {
        let cfg = unsafe { sys::MFXCreateConfig(self.loader) };
        if cfg.is_null() {
            return Err(Error::new_custom("MFXCreateConfig", "returned null"));
        }
        let name = b"mfxExtendedDeviceId.DRMRenderNodeNum\0";
        let mut variant: sys::mfxVariant = unsafe { std::mem::zeroed() };
        variant.Type = sys::mfxVariantType_MFX_VARIANT_TYPE_U32;
        variant.Data.U32 = render_node;
        let status = unsafe { sys::MFXSetConfigFilterProperty(cfg, name.as_ptr(), variant) };
        if status != sys::mfxStatus_MFX_ERR_NONE {
            return Err(Error::from_mfx(status, "MFXSetConfigFilterProperty"));
        }
        self.render_node = Some(render_node);
        Ok(())
    }

    /// 設定済みのフィルタに合致する先頭の実装のセッションを生成する
    ///
    /// 成功した場合はローダーの所有権を `Session` に移す。失敗した場合はローダーを
    /// 解放して `Err` を返す。
    ///
    /// セッションの作成に失敗した場合は `MFXCreateSession` を function に持つ `Error` を返す。
    /// `MFX_ERR_NOT_FOUND` の場合は、`require_drm_render_node` で設定した render node 番号を
    /// 含むメッセージにする。render node が未設定の場合は、その旨のメッセージにする。
    pub(crate) fn create_session(mut self) -> Result<Session, Error> {
        let mut session: sys::mfxSession = std::ptr::null_mut();
        let status = unsafe { sys::MFXCreateSession(self.loader, 0, &mut session) };
        if status != sys::mfxStatus_MFX_ERR_NONE {
            let err = Error::from_mfx(status, "MFXCreateSession");
            if status == sys::mfxStatus_MFX_ERR_NOT_FOUND {
                let message = match self.render_node {
                    Some(render_node) => Self::not_found_message(render_node),
                    None => {
                        "no Intel HW implementation found (DRM render node not set)".to_string()
                    }
                };
                return Err(err.with_message(message));
            }
            return Err(err);
        }

        // ローダーの所有権を Session に移す。これを true にしないと Drop で
        // 二重に MFXUnload してしまう。
        self.consumed = true;
        Ok(Session {
            lib: self.lib,
            loader: self.loader,
            session,
        })
    }

    /// render node 番号を含む「Intel HW 実装が見つからない」エラーのメッセージを返す
    pub(crate) fn not_found_message(render_node: u32) -> String {
        format!("no Intel HW implementation found for DRM render node {render_node}")
    }

    /// 生のローダーハンドルを返す
    ///
    /// # Safety
    ///
    /// 戻り値はこの `VplLoader` の生存期間内でのみ使用すること。
    /// `VplLoader` の破棄後はハンドルが無効になるため、使用すると未定義動作になる。
    pub(crate) unsafe fn as_ptr(&self) -> sys::mfxLoader {
        self.loader
    }
}

impl Drop for VplLoader {
    fn drop(&mut self) {
        if !self.consumed {
            self.lib.mfx_unload(self.loader);
        }
    }
}

/// VPL セッションの RAII ガード
///
/// lib, loader, session の 3 つをまとめて管理し、Drop で MFXClose + MFXUnload を自動実行する。
pub(crate) struct Session {
    lib: VplLibrary,
    loader: sys::mfxLoader,
    session: sys::mfxSession,
}

// Safety: VPL 仕様上、mfxSession にスレッドアフィニティの制約は課されていない。
// mfxSession / mfxLoader は単一スレッドからの逐次アクセスであれば安全に扱える。
unsafe impl Send for Session {}

impl Session {
    /// VplLibrary のコピーを返す
    pub(crate) fn lib(&self) -> VplLibrary {
        self.lib
    }

    /// セッションハンドルを返す
    pub(crate) fn as_ptr(&self) -> sys::mfxSession {
        self.session
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.lib.mfx_close(self.session);
        self.lib.mfx_unload(self.loader);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_surface_new_rejects_null() {
        let result = FrameSurface::new(VplLibrary, std::ptr::null_mut());
        assert!(result.is_err());
    }

    #[test]
    fn frame_surface_gpu_required() {
        let adapters = match crate::list_adapters() {
            Ok(a) => a,
            Err(_) => return,
        };
        if adapters.is_empty() {
            return;
        }

        let lib = VplLibrary;
        let adapter = AdapterSelector::DrmRenderNode(adapters[0].drm_render_node);
        let session = match lib.create_session(adapter) {
            Ok(v) => v,
            Err(_) => return,
        };

        // 正常系: new → map_write → unmap → drop
        {
            let mut surface: *mut sys::mfxFrameSurface1 = std::ptr::null_mut();
            let status = lib.mfx_memory_get_surface_for_encode(session.as_ptr(), &mut surface);
            if status != sys::mfxStatus_MFX_ERR_NONE {
                return;
            }
            let mut fs = FrameSurface::new(lib, surface).expect("FrameSurface::new should succeed");
            fs.map_write()
                .expect("map_write should succeed on a valid surface");
            fs.unmap().expect("unmap should succeed");
            // drop 時に Release が呼ばれる
        }

        // map_write の二重呼び出し
        {
            let mut surface: *mut sys::mfxFrameSurface1 = std::ptr::null_mut();
            let status = lib.mfx_memory_get_surface_for_encode(session.as_ptr(), &mut surface);
            if status != sys::mfxStatus_MFX_ERR_NONE {
                return;
            }
            let mut fs = FrameSurface::new(lib, surface).expect("FrameSurface::new should succeed");
            fs.map_write()
                .expect("first map_write should succeed on a valid surface");
            let result = fs.map_write();
            assert!(result.is_err(), "second map_write should fail");
            fs.unmap().expect("unmap should succeed");
        }

        // map_read の二重呼び出し
        {
            let mut surface: *mut sys::mfxFrameSurface1 = std::ptr::null_mut();
            let status = lib.mfx_memory_get_surface_for_encode(session.as_ptr(), &mut surface);
            if status != sys::mfxStatus_MFX_ERR_NONE {
                return;
            }
            let mut fs = FrameSurface::new(lib, surface).expect("FrameSurface::new should succeed");
            fs.map_read()
                .expect("first map_read should succeed on a valid surface");
            let result = fs.map_read();
            assert!(result.is_err(), "second map_read should fail");
            fs.unmap().expect("unmap should succeed");
        }

        // unmap の二重呼び出し
        {
            let mut surface: *mut sys::mfxFrameSurface1 = std::ptr::null_mut();
            let status = lib.mfx_memory_get_surface_for_encode(session.as_ptr(), &mut surface);
            if status != sys::mfxStatus_MFX_ERR_NONE {
                return;
            }
            let mut fs = FrameSurface::new(lib, surface).expect("FrameSurface::new should succeed");
            fs.map_write()
                .expect("map_write should succeed on a valid surface");
            fs.unmap().expect("first unmap should succeed");
            let result = fs.unmap();
            assert!(result.is_err(), "second unmap should fail");
        }

        // unmapped 状態での unmap
        {
            let mut surface: *mut sys::mfxFrameSurface1 = std::ptr::null_mut();
            let status = lib.mfx_memory_get_surface_for_encode(session.as_ptr(), &mut surface);
            if status != sys::mfxStatus_MFX_ERR_NONE {
                return;
            }
            let mut fs = FrameSurface::new(lib, surface).expect("FrameSurface::new should succeed");
            let result = fs.unmap();
            assert!(result.is_err(), "unmap on a non-mapped surface should fail");
        }
    }
}
