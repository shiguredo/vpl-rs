// Rust の統合テストは tests/test_*.rs ごとに別バイナリのため、
// 各ファイルが独立クレートとして扱われる。テスト間で共有したいヘルパーは
// 本ファイルに置き、各テストから `mod common;` で取り込む。
//
// 一部のヘルパーが特定ファイルからしか使われないケースでも `-D warnings` で
// 未使用警告を踏まないように、ファイル冒頭で dead_code を許可する。
#![allow(dead_code)]

use std::sync::OnceLock;

use shiguredo_vpl::{AdapterSelector, list_adapters};

static CACHED_ADAPTER: OnceLock<AdapterSelector> = OnceLock::new();

/// テスト用アダプタを返す
///
/// `list_adapters()` の結果をテストバイナリ単位でキャッシュし、`MFXLoad` の
/// 繰り返し呼び出しを避ける。Intel HW アダプタが見つからない環境では panic
/// する（CI 環境はテスト前提として最低 1 つのアダプタが見つかる構成にする）。
pub fn test_adapter() -> AdapterSelector {
    *CACHED_ADAPTER.get_or_init(|| {
        let adapters = list_adapters().expect("list_adapters に失敗");
        let first = adapters.first().expect("Intel HW アダプタが見つからない");
        AdapterSelector::DrmRenderNode(first.drm_render_node)
    })
}
