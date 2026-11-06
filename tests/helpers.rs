//! テスト間で共有するヘルパー

use std::sync::OnceLock;

use shiguredo_vpl::{AdapterSelector, list_adapters};

/// テスト用アダプタを返す
///
/// `list_adapters()` の結果をテストバイナリ単位でキャッシュし、`MFXLoad` の
/// 繰り返し呼び出しを避ける。Intel HW アダプタが見つからない環境では panic
/// する（実機テストは Intel GPU 付きランナー上で実行する想定）。
pub fn test_adapter() -> AdapterSelector {
    static CACHED: OnceLock<AdapterSelector> = OnceLock::new();
    *CACHED.get_or_init(|| {
        let adapters = list_adapters().expect("list_adapters に失敗");
        let first = adapters.first().expect("Intel HW アダプタが見つからない");
        AdapterSelector::DrmRenderNode(first.drm_render_node)
    })
}
