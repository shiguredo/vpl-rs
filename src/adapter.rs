//! VPL ローダー経由で Intel HW 実装（GPU アダプタ）を列挙・選択する API
//!
//! 複数 Intel GPU 環境で `Encoder` / `Decoder` を作るとき、`AdapterSelector` で
//! どの物理アダプタを使うかを指定する。`AdapterSelector::DrmRenderNode(n)` の `n` は
//! `/dev/dri/renderD<N>` の `N`（通常 128 以上）で、libvpl のディスパッチャに
//! `mfxExtendedDeviceId.DRMRenderNodeNum` プロパティとして渡される。
//!
//! libvpl 側のサポート:
//!
//! - `libvpl/src/mfx_dispatcher_vpl_config.cpp` がプロパティ名 `"DRMRenderNodeNum"` を受理する
//! - `libvpl/src/mfx_dispatcher_vpl_loader.cpp` の `LoaderCtxVPL::CreateSession` がフィルタにマッチ
//!   しないと `MFX_ERR_NOT_FOUND` を返す
//!
//! 参照バージョンは `Cargo.toml` の `package.metadata.external-dependencies.vpl.version`。
//! 将来 libvpl の挙動が変わる可能性があるため、本実装は当該バージョンに依存する。

#[cfg(target_os = "linux")]
use std::ffi::CStr;

use crate::Error;
#[cfg(target_os = "linux")]
use crate::sys;

/// `Encoder` / `Decoder` のセッションを開くときの対象アダプタ
///
/// 現状は DRM render node 番号による指定のみをサポートする。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum AdapterSelector {
    /// `/dev/dri/renderD<N>` の `N`（通常 128 以上）
    DrmRenderNode(u32),
}

impl AdapterSelector {
    /// アダプタ指定値の入力検証
    ///
    /// `DrmRenderNode(0)` は libvpl 上「未設定」を意味する値のため `Err` を返す。
    /// `Encoder::new` / `Decoder::new` / `supported_codecs` から共通で呼ぶ。
    pub(crate) fn validate(self) -> Result<(), Error> {
        match self {
            AdapterSelector::DrmRenderNode(0) => Err(Error::new_custom(
                "AdapterSelector::validate",
                "DrmRenderNode(0) is reserved by libvpl and cannot be used",
            )),
            AdapterSelector::DrmRenderNode(_) => Ok(()),
        }
    }
}

/// integrated GPU か discrete GPU か
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum MediaAdapterType {
    /// integrated GPU（iGPU）
    Integrated,
    /// discrete GPU（dGPU）
    Discrete,
    /// 不明（`MFX_MEDIA_UNKNOWN` または未知の値）
    Unknown,
}

/// PCI アドレス
///
/// 各フィールドは libvpl の `mfxExtendedDeviceId` (`PCIDomain` / `PCIBus` /
/// `PCIDevice` / `PCIFunction` がすべて `mfxU32`) に合わせて `u32` にしている。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct PciAddress {
    /// PCI domain
    pub domain: u32,
    /// PCI bus
    pub bus: u32,
    /// PCI device
    pub device: u32,
    /// PCI function
    pub function: u32,
}

/// VPL ローダーで列挙される Intel HW 実装の情報
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct AdapterInfo {
    /// `/dev/dri/renderD<N>` の `N`（通常 128 以上）。`AdapterSelector::DrmRenderNode` に渡す
    pub drm_render_node: u32,
    /// 実装名（NUL 終端を除去した UTF-8 文字列。例: "mfx-gen"。取得できない場合は空文字列）
    pub impl_name: String,
    /// 人間向けの GPU 名（例: "Intel(R) Arc(TM) A310 Graphics"）。取得できない場合は空文字列
    pub device_name: String,
    /// PCI device ID（例: Arc A310 は 0x56a6）
    pub pci_device_id: u16,
    /// PCI アドレス
    pub pci_address: PciAddress,
    /// integrated か discrete か
    pub media_adapter_type: MediaAdapterType,
}

/// 利用可能な Intel HW 実装を列挙する
///
/// 同一 DRM render node に対する重複エントリは除去し、`drm_render_node` 昇順で返す。
/// HW 実装が見つからないことはエラーではなく、空の `Vec` を返す。
///
/// 呼び出しごとに `MFXLoad` + 列挙 + `MFXUnload` を行う重い処理なので、
/// 利用側はアプリ起動時に 1 回だけ呼んで結果を保持することを想定する。
#[cfg(target_os = "linux")]
pub fn list_adapters() -> Result<Vec<AdapterInfo>, Error> {
    let loader = unsafe { sys::MFXLoad() };
    if loader.is_null() {
        return Err(Error::new_custom("MFXLoad", "returned null"));
    }
    let loader_guard = LoaderGuard { loader };

    // ハードウェア実装のみに絞る
    let cfg = unsafe { sys::MFXCreateConfig(loader) };
    if cfg.is_null() {
        return Err(Error::new_custom("MFXCreateConfig", "returned null"));
    }
    let name = b"mfxImplDescription.Impl\0";
    let mut variant: sys::mfxVariant = unsafe { std::mem::zeroed() };
    variant.Type = sys::mfxVariantType_MFX_VARIANT_TYPE_U32;
    variant.Data.U32 = sys::mfxImplType_MFX_IMPL_TYPE_HARDWARE;
    let status = unsafe { sys::MFXSetConfigFilterProperty(cfg, name.as_ptr(), variant) };
    if status != sys::mfxStatus_MFX_ERR_NONE {
        return Err(Error::from_mfx(status, "MFXSetConfigFilterProperty"));
    }

    let mut adapters: Vec<AdapterInfo> = Vec::new();
    let mut i: sys::mfxU32 = 0;
    loop {
        // mfxImplDescription（実装名 / MediaAdapterType 等）を取る
        let mut hdl: sys::mfxHDL = std::ptr::null_mut();
        let status = unsafe {
            sys::MFXEnumImplementations(
                loader,
                i,
                sys::mfxImplCapsDeliveryFormat_MFX_IMPLCAPS_IMPLDESCSTRUCTURE,
                &mut hdl,
            )
        };
        if status == sys::mfxStatus_MFX_ERR_NOT_FOUND {
            break;
        }
        if status != sys::mfxStatus_MFX_ERR_NONE {
            return Err(Error::from_mfx(status, "MFXEnumImplementations"));
        }
        if hdl.is_null() {
            return Err(Error::new_custom(
                "MFXEnumImplementations",
                "returned null handle for IMPLDESCSTRUCTURE",
            ));
        }
        let desc_guard = HdlGuard { loader, hdl };

        // mfxExtendedDeviceId（DRMRenderNodeNum / PCI 情報 等）を取る
        let mut hdl_ext: sys::mfxHDL = std::ptr::null_mut();
        let ext_status = unsafe {
            sys::MFXEnumImplementations(
                loader,
                i,
                sys::mfxImplCapsDeliveryFormat_MFX_IMPLCAPS_DEVICE_ID_EXTENDED,
                &mut hdl_ext,
            )
        };
        // 古い実装で未対応のときは MFX_ERR_UNSUPPORTED が返るのでエントリ全体を捨てる
        if ext_status == sys::mfxStatus_MFX_ERR_UNSUPPORTED || hdl_ext.is_null() {
            drop(desc_guard);
            i += 1;
            continue;
        }
        if ext_status != sys::mfxStatus_MFX_ERR_NONE {
            return Err(Error::from_mfx(ext_status, "MFXEnumImplementations"));
        }
        let ext_guard = HdlGuard {
            loader,
            hdl: hdl_ext,
        };

        let desc = unsafe { &*(hdl as *const sys::mfxImplDescription) };
        let ext = unsafe { &*(hdl_ext as *const sys::mfxExtendedDeviceId) };

        // DRMRenderNodeNum == 0 は libvpl 上「未設定」扱いなので捨てる
        if ext.DRMRenderNodeNum != 0 {
            // 同一 DRMRenderNodeNum の重複は最初に出てきたものを採用する
            let already_seen = adapters
                .iter()
                .any(|a| a.drm_render_node == ext.DRMRenderNodeNum);
            if !already_seen {
                let impl_name = c_array_to_string(&desc.ImplName);
                let device_name = c_array_to_string(&ext.DeviceName);
                let media_adapter_type = map_media_adapter_type(desc.Dev.MediaAdapterType);
                adapters.push(AdapterInfo {
                    drm_render_node: ext.DRMRenderNodeNum,
                    impl_name,
                    device_name,
                    pci_device_id: ext.DeviceID,
                    pci_address: PciAddress {
                        domain: ext.PCIDomain,
                        bus: ext.PCIBus,
                        device: ext.PCIDevice,
                        function: ext.PCIFunction,
                    },
                    media_adapter_type,
                });
            }
        }

        drop(ext_guard);
        drop(desc_guard);
        i += 1;
    }

    drop(loader_guard);

    adapters.sort_by_key(|a| a.drm_render_node);
    Ok(adapters)
}

/// Linux 以外では実機が存在しないため空 `Vec` を返す
#[cfg(not(target_os = "linux"))]
pub fn list_adapters() -> Result<Vec<AdapterInfo>, Error> {
    Ok(Vec::new())
}

/// `mfxDeviceDescription.MediaAdapterType` (`mfxU16`) を Rust 側の enum にマップする
#[cfg(target_os = "linux")]
fn map_media_adapter_type(value: sys::mfxU16) -> MediaAdapterType {
    // MFX_MEDIA_* は本来 c_uint だが、mfxDeviceDescription.MediaAdapterType フィールドは mfxU16。
    // u16 にキャストして比較する。
    if value as u32 == sys::mfxMediaAdapterType_MFX_MEDIA_INTEGRATED {
        MediaAdapterType::Integrated
    } else if value as u32 == sys::mfxMediaAdapterType_MFX_MEDIA_DISCRETE {
        MediaAdapterType::Discrete
    } else {
        // MFX_MEDIA_UNKNOWN (0xFFFF) と未知値はすべて Unknown
        MediaAdapterType::Unknown
    }
}

/// `mfxChar` (= `c_char`) の固定長配列を UTF-8 文字列として読み出す
///
/// NUL 終端があればそこまでを切り出し、なければ配列全体を UTF-8 として解釈する。
/// 不正 UTF-8 のときは空文字列にフォールバックする。
#[cfg(target_os = "linux")]
fn c_array_to_string(arr: &[sys::mfxChar]) -> String {
    // mfxChar は c_char = i8 / u8 (環境依存) なので u8 にキャストする
    let bytes: &[u8] = unsafe { std::slice::from_raw_parts(arr.as_ptr() as *const u8, arr.len()) };
    match CStr::from_bytes_until_nul(bytes) {
        Ok(cstr) => cstr.to_str().unwrap_or("").to_string(),
        Err(_) => std::str::from_utf8(bytes).unwrap_or("").to_string(),
    }
}

/// `MFXUnload` を Drop で呼ぶガード
#[cfg(target_os = "linux")]
struct LoaderGuard {
    loader: sys::mfxLoader,
}

#[cfg(target_os = "linux")]
impl Drop for LoaderGuard {
    fn drop(&mut self) {
        unsafe { sys::MFXUnload(self.loader) };
    }
}

/// `MFXDispReleaseImplDescription` を Drop で呼ぶガード
#[cfg(target_os = "linux")]
struct HdlGuard {
    loader: sys::mfxLoader,
    hdl: sys::mfxHDL,
}

#[cfg(target_os = "linux")]
impl Drop for HdlGuard {
    fn drop(&mut self) {
        unsafe { sys::MFXDispReleaseImplDescription(self.loader, self.hdl) };
    }
}
