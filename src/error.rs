use crate::sys;
use std::borrow::Cow;

/// Intel VPL API またはこの crate で発生するエラー
///
/// VPL API 由来のエラーは `mfxStatus` のコード・名前・メッセージを保持する。
/// この crate 固有のエラー（サーフェス不足など）はカスタムメッセージのみ保持する。
///
/// [`std::fmt::Display`] でエラーの詳細を人間が読める形式で表示できる。
/// [`std::error::Error`] を実装しているため、`anyhow` や `thiserror` と組み合わせて使える。
#[derive(Debug, Clone)]
pub struct Error {
    /// エラーが発生した VPL API 関数名またはメソッド名
    function: &'static str,
    /// mfxStatus のステータスコード（VPL API エラーの場合のみ）
    status_code: Option<i32>,
    /// mfxStatus の定数名（例: `MFX_ERR_UNSUPPORTED`）
    status_name: Option<Cow<'static, str>>,
    /// エラーの説明メッセージ
    status_message: Option<Cow<'static, str>>,
}

impl Error {
    /// エラーが発生した関数名を返す
    pub fn function(&self) -> &str {
        self.function
    }

    /// mfxStatus のステータスコードを返す（VPL API エラーの場合のみ）
    pub fn status_code(&self) -> Option<i32> {
        self.status_code
    }

    /// mfxStatus の定数名を返す（例: `MFX_ERR_UNSUPPORTED`）
    pub fn status_name(&self) -> Option<&str> {
        self.status_name.as_deref()
    }

    /// エラーの説明メッセージを返す
    pub fn status_message(&self) -> Option<&str> {
        self.status_message.as_deref()
    }

    /// Intel VPL ではなく、この crate 起因のエラーを構築する
    pub(crate) fn new_custom(function: &'static str, message: &'static str) -> Self {
        Self {
            function,
            status_code: None,
            status_name: None,
            status_message: Some(Cow::Borrowed(message)),
        }
    }

    /// 動的メッセージを持つ crate 起因のエラーを構築する
    pub(crate) fn new_custom_owned(function: &'static str, message: String) -> Self {
        Self {
            function,
            status_code: None,
            status_name: None,
            status_message: Some(Cow::Owned(message)),
        }
    }

    /// mfxStatus エラーを構築する
    pub(crate) fn from_mfx(status: i32, function: &'static str) -> Self {
        let entry = find_status(status);
        Self {
            function,
            status_code: Some(status),
            status_name: entry.map(|(name, _)| Cow::Borrowed(name)),
            status_message: entry.map(|(_, msg)| Cow::Borrowed(msg)),
        }
    }

    /// `status_message` のみを置き換えた新しい Error を返す
    ///
    /// `status_code` / `status_name` / `function` は保持する。
    /// 動的に詳細情報を付加したいケース（DRM render node 番号を含めたい場合など）で使う。
    pub(crate) fn with_message(mut self, message: String) -> Self {
        self.status_message = Some(Cow::Owned(message));
        self
    }

    /// mfxStatus をチェックして、エラーなら Err を返す
    pub(crate) fn check_mfx(status: i32, function: &'static str) -> Result<(), Error> {
        if status == sys::mfxStatus_MFX_ERR_NONE {
            Ok(())
        } else {
            Err(Self::from_mfx(status, function))
        }
    }

    /// mfxStatus をチェックして、エラー (負値) なら Err を返す
    ///
    /// VPL の仕様では正値は警告で、操作自体は成功している。
    /// MFXVideoENCODE_Init や MFXVideoENCODE_Reset など、
    /// 警告を返しつつも正常に完了する関数で使用する。
    pub(crate) fn check_mfx_allow_warn(status: i32, function: &'static str) -> Result<(), Error> {
        if status >= 0 {
            Ok(())
        } else {
            Err(Self::from_mfx(status, function))
        }
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}() failed", self.function)?;

        if let Some(code) = self.status_code {
            write!(f, "[status={code}]")?;
        }
        if self.status_name.is_some() || self.status_message.is_some() {
            write!(f, ": ")?;
        }

        if let Some(message) = &self.status_message {
            write!(f, "{message}")?;
        }

        if let Some(name) = &self.status_name {
            if self.status_message.is_some() {
                write!(f, " ({name})")?;
            } else {
                write!(f, "{name}")?;
            }
        }

        Ok(())
    }
}

impl std::error::Error for Error {}

// mfxStatus の (ステータスコード, 定数名, 説明) のテーブル
//
// Intel VPL Specification で定義されたステータスコードを網羅する。
// 負値はエラー、正値は警告、0 は成功を表す。
const STATUS_TABLE: &[(i32, &str, &str)] = &[
    (sys::mfxStatus_MFX_ERR_NONE, "MFX_ERR_NONE", "No error"),
    (
        sys::mfxStatus_MFX_ERR_UNKNOWN,
        "MFX_ERR_UNKNOWN",
        "Unknown error",
    ),
    (
        sys::mfxStatus_MFX_ERR_NULL_PTR,
        "MFX_ERR_NULL_PTR",
        "Null pointer",
    ),
    (
        sys::mfxStatus_MFX_ERR_UNSUPPORTED,
        "MFX_ERR_UNSUPPORTED",
        "Unsupported feature or implementation",
    ),
    (
        sys::mfxStatus_MFX_ERR_MEMORY_ALLOC,
        "MFX_ERR_MEMORY_ALLOC",
        "Failed to allocate memory",
    ),
    (
        sys::mfxStatus_MFX_ERR_NOT_ENOUGH_BUFFER,
        "MFX_ERR_NOT_ENOUGH_BUFFER",
        "Insufficient buffer",
    ),
    (
        sys::mfxStatus_MFX_ERR_INVALID_HANDLE,
        "MFX_ERR_INVALID_HANDLE",
        "Invalid handle",
    ),
    (
        sys::mfxStatus_MFX_ERR_LOCK_MEMORY,
        "MFX_ERR_LOCK_MEMORY",
        "Failed to lock memory",
    ),
    (
        sys::mfxStatus_MFX_ERR_NOT_INITIALIZED,
        "MFX_ERR_NOT_INITIALIZED",
        "Not initialized",
    ),
    (
        sys::mfxStatus_MFX_ERR_NOT_FOUND,
        "MFX_ERR_NOT_FOUND",
        "Object not found",
    ),
    (
        sys::mfxStatus_MFX_ERR_MORE_DATA,
        "MFX_ERR_MORE_DATA",
        "More data is required to complete the operation",
    ),
    (
        sys::mfxStatus_MFX_ERR_MORE_SURFACE,
        "MFX_ERR_MORE_SURFACE",
        "More surface is required to complete the operation",
    ),
    (
        sys::mfxStatus_MFX_ERR_ABORTED,
        "MFX_ERR_ABORTED",
        "Operation aborted",
    ),
    (
        sys::mfxStatus_MFX_ERR_DEVICE_LOST,
        "MFX_ERR_DEVICE_LOST",
        "Device lost",
    ),
    (
        sys::mfxStatus_MFX_ERR_INCOMPATIBLE_VIDEO_PARAM,
        "MFX_ERR_INCOMPATIBLE_VIDEO_PARAM",
        "Incompatible video parameters",
    ),
    (
        sys::mfxStatus_MFX_ERR_INVALID_VIDEO_PARAM,
        "MFX_ERR_INVALID_VIDEO_PARAM",
        "Invalid video parameters",
    ),
    (
        sys::mfxStatus_MFX_ERR_UNDEFINED_BEHAVIOR,
        "MFX_ERR_UNDEFINED_BEHAVIOR",
        "Undefined behavior",
    ),
    (
        sys::mfxStatus_MFX_ERR_DEVICE_FAILED,
        "MFX_ERR_DEVICE_FAILED",
        "Device failed",
    ),
    (
        sys::mfxStatus_MFX_ERR_MORE_BITSTREAM,
        "MFX_ERR_MORE_BITSTREAM",
        "More bitstream data is required",
    ),
    (
        sys::mfxStatus_MFX_ERR_GPU_HANG,
        "MFX_ERR_GPU_HANG",
        "GPU hang",
    ),
    (
        sys::mfxStatus_MFX_ERR_REALLOC_SURFACE,
        "MFX_ERR_REALLOC_SURFACE",
        "Surface reallocation is required",
    ),
    (
        sys::mfxStatus_MFX_ERR_RESOURCE_MAPPED,
        "MFX_ERR_RESOURCE_MAPPED",
        "Resource already mapped",
    ),
    (
        sys::mfxStatus_MFX_ERR_NOT_IMPLEMENTED,
        "MFX_ERR_NOT_IMPLEMENTED",
        "Not implemented",
    ),
    (
        sys::mfxStatus_MFX_ERR_MORE_EXTBUFFER,
        "MFX_ERR_MORE_EXTBUFFER",
        "More extended buffers required",
    ),
    (
        sys::mfxStatus_MFX_WRN_IN_EXECUTION,
        "MFX_WRN_IN_EXECUTION",
        "Asynchronous operation is in execution",
    ),
    (
        sys::mfxStatus_MFX_WRN_DEVICE_BUSY,
        "MFX_WRN_DEVICE_BUSY",
        "The hardware acceleration device is busy",
    ),
    (
        sys::mfxStatus_MFX_WRN_VIDEO_PARAM_CHANGED,
        "MFX_WRN_VIDEO_PARAM_CHANGED",
        "Video parameters changed",
    ),
    (
        sys::mfxStatus_MFX_WRN_PARTIAL_ACCELERATION,
        "MFX_WRN_PARTIAL_ACCELERATION",
        "Partial acceleration",
    ),
    (
        sys::mfxStatus_MFX_WRN_INCOMPATIBLE_VIDEO_PARAM,
        "MFX_WRN_INCOMPATIBLE_VIDEO_PARAM",
        "Incompatible video parameters (warning)",
    ),
    (
        sys::mfxStatus_MFX_WRN_VALUE_NOT_CHANGED,
        "MFX_WRN_VALUE_NOT_CHANGED",
        "Value not changed",
    ),
    (
        sys::mfxStatus_MFX_WRN_OUT_OF_RANGE,
        "MFX_WRN_OUT_OF_RANGE",
        "Value out of range",
    ),
    (
        sys::mfxStatus_MFX_WRN_FILTER_SKIPPED,
        "MFX_WRN_FILTER_SKIPPED",
        "Filter skipped",
    ),
    (
        sys::mfxStatus_MFX_WRN_ALLOC_TIMEOUT_EXPIRED,
        "MFX_WRN_ALLOC_TIMEOUT_EXPIRED",
        "Allocation timeout expired",
    ),
];

fn find_status(status: i32) -> Option<(&'static str, &'static str)> {
    STATUS_TABLE
        .iter()
        .find(|(s, _, _)| *s == status)
        .map(|(_, name, msg)| (*name, *msg))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_custom_display() {
        let error = Error::new_custom("test_func", "custom error message");
        assert_eq!(
            error.to_string(),
            "test_func() failed: custom error message"
        );
    }

    #[test]
    fn test_check_mfx_success() {
        let result = Error::check_mfx(sys::mfxStatus_MFX_ERR_NONE, "mfx_func");
        assert!(result.is_ok());
    }

    #[test]
    fn test_check_mfx_error() {
        let result = Error::check_mfx(sys::mfxStatus_MFX_ERR_UNSUPPORTED, "mfx_func");
        let error = result.expect_err("not err");
        assert_eq!(
            error.to_string(),
            "mfx_func() failed[status=-3]: Unsupported feature or implementation (MFX_ERR_UNSUPPORTED)"
        );
    }

    #[test]
    fn test_check_mfx_allow_warn_success() {
        let result = Error::check_mfx_allow_warn(sys::mfxStatus_MFX_ERR_NONE, "mfx_func");
        assert!(result.is_ok());
    }

    #[test]
    fn test_check_mfx_allow_warn_warning() {
        let result = Error::check_mfx_allow_warn(
            sys::mfxStatus_MFX_WRN_INCOMPATIBLE_VIDEO_PARAM,
            "mfx_func",
        );
        assert!(result.is_ok());
    }

    #[test]
    fn test_check_mfx_allow_warn_error() {
        let result = Error::check_mfx_allow_warn(sys::mfxStatus_MFX_ERR_UNSUPPORTED, "mfx_func");
        assert!(result.is_err());
    }
}
