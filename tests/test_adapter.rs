#![cfg(target_os = "linux")]

use shiguredo_vpl::{
    AdapterSelector, CodecConfig, Decoder, DecoderCodec, DecoderConfig, Encoder, EncoderConfig,
    Error, FrameFormat, H264EncoderConfig, RateControlMode, list_adapters, supported_codecs,
};

/// `Encoder` / `Decoder` は `Debug` を実装しないため、`expect_err` の代わりにこのヘルパーで
/// `Result` をエラーに変換する。
fn into_err<T>(result: Result<T, Error>, message: &str) -> Error {
    match result {
        Ok(_) => panic!("{message}"),
        Err(e) => e,
    }
}

/// 列挙結果が DRM render node 番号で昇順に並び、重複がないこと
#[test]
fn test_list_adapters_sorted_and_deduped() {
    let adapters = list_adapters().expect("list_adapters に失敗");

    let mut nodes: Vec<u32> = adapters.iter().map(|a| a.drm_render_node).collect();
    let original = nodes.clone();
    nodes.sort();
    assert_eq!(nodes, original, "drm_render_node が昇順になっていない");

    let mut deduped = nodes.clone();
    deduped.dedup();
    assert_eq!(deduped, nodes, "drm_render_node が重複している");
}

/// `AdapterSelector::DrmRenderNode(0)` を Encoder::new に渡すと Err になる
#[test]
fn test_encoder_rejects_render_node_zero() {
    let config = EncoderConfig::new(
        AdapterSelector::DrmRenderNode(0),
        CodecConfig::H264(H264EncoderConfig { profile: None }),
        1920,
        1080,
        FrameFormat::Nv12,
        30,
        1,
        RateControlMode::Cqp,
    );
    let err = into_err(Encoder::new(config), "Encoder::new が成功してはいけない");
    assert_eq!(err.function(), "AdapterSelector::validate");
}

/// `AdapterSelector::DrmRenderNode(0)` を Decoder::new に渡すと Err になる
#[test]
fn test_decoder_rejects_render_node_zero() {
    let config = DecoderConfig::new(AdapterSelector::DrmRenderNode(0), DecoderCodec::H264);
    let err = into_err(Decoder::new(config), "Decoder::new が成功してはいけない");
    assert_eq!(err.function(), "AdapterSelector::validate");
}

/// `AdapterSelector::DrmRenderNode(0)` を supported_codecs に渡すと Err になる
#[test]
fn test_supported_codecs_rejects_render_node_zero() {
    let err = supported_codecs(AdapterSelector::DrmRenderNode(0))
        .expect_err("supported_codecs が成功してはいけない");
    assert_eq!(err.function(), "AdapterSelector::validate");
}

/// 存在しない DRM render node 番号で Encoder を作ると `MFX_ERR_NOT_FOUND` を含むエラーが返る
#[test]
fn test_encoder_not_found_for_invalid_render_node() {
    let bogus_node = u32::MAX;
    let config = EncoderConfig::new(
        AdapterSelector::DrmRenderNode(bogus_node),
        CodecConfig::H264(H264EncoderConfig { profile: None }),
        1920,
        1080,
        FrameFormat::Nv12,
        30,
        1,
        RateControlMode::Cqp,
    );
    let err = into_err(Encoder::new(config), "Encoder::new が成功してはいけない");
    assert_eq!(
        err.status_name(),
        Some("MFX_ERR_NOT_FOUND"),
        "status_name が MFX_ERR_NOT_FOUND ではない: {err}",
    );
    let message = err.status_message().expect("status_message が空");
    assert!(
        message.contains(&bogus_node.to_string()),
        "メッセージに render node 番号 {bogus_node} が含まれていない: {message}",
    );
}
