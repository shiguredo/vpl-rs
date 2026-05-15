#![cfg(target_os = "linux")]

use shiguredo_vpl::{
    AdapterSelector, CodecConfig, Decoder, DecoderCodec, DecoderConfig, EncodedFrame, Encoder,
    EncoderConfig, Error, FnDecodeHandler, FnEncodeHandler, FrameFormat, H264EncoderConfig,
    RateControlMode, list_adapters, supported_codecs,
};

/// `Encoder` / `Decoder` は `Debug` を実装しないため、`expect_err` の代わりにこのヘルパーで
/// `Result` をエラーに変換する。
fn into_err<T>(result: Result<T, Error>, message: &str) -> Error {
    match result {
        Ok(_) => panic!("{message}"),
        Err(e) => e,
    }
}

/// AdapterInfo の Debug 出力を 1 行ずつ整形した文字列で返す（assert メッセージ用）
#[cfg(intel_vpl)]
fn format_adapters(adapters: &[shiguredo_vpl::AdapterInfo]) -> String {
    let mut s = String::from("[");
    for (i, a) in adapters.iter().enumerate() {
        if i > 0 {
            s.push_str(", ");
        }
        s.push_str(&format!(
            "DRMRenderNodeNum={} device_name={:?} impl_name={:?} pci_device_id=0x{:04x} type={:?}",
            a.drm_render_node, a.device_name, a.impl_name, a.pci_device_id, a.media_adapter_type,
        ));
    }
    s.push(']');
    s
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
    let err = into_err(
        Encoder::new(
            config,
            FnEncodeHandler::new(|_: Result<EncodedFrame<()>, Error>| {}),
        ),
        "Encoder::new が成功してはいけない",
    );
    assert_eq!(err.function(), "AdapterSelector::validate");
}

/// `AdapterSelector::DrmRenderNode(0)` を Decoder::new に渡すと Err になる
#[test]
fn test_decoder_rejects_render_node_zero() {
    let config = DecoderConfig::new(AdapterSelector::DrmRenderNode(0), DecoderCodec::H264);
    let err = into_err(
        Decoder::new(
            config,
            FnDecodeHandler::new(|_: Result<shiguredo_vpl::DecodedFrame<()>, Error>| {}),
        ),
        "Decoder::new が成功してはいけない",
    );
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
    let err = into_err(
        Encoder::new(
            config,
            FnEncodeHandler::new(|_: Result<EncodedFrame<()>, Error>| {}),
        ),
        "Encoder::new が成功してはいけない",
    );
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

/// 実機アダプタの列挙とセッション作成を確認する
///
/// `INTEL_VPL=1` を設定したビルドでのみ cfg ガードによってコンパイルされる
/// （build.rs で `cargo:rustc-cfg=intel_vpl` を出力）。実機ランナーでアダプタが
/// 1 つ以上列挙され、その先頭の render node で `Encoder` / `Decoder` /
/// `supported_codecs` が動作することを保証する。
#[cfg(intel_vpl)]
#[test]
fn test_real_adapter_session() {
    let adapters = list_adapters().expect("list_adapters に失敗");
    let listing = format_adapters(&adapters);
    // CI ログでアダプタ一覧を確認できるよう stderr に出力する
    // (cargo test の `--nocapture` 指定でログに残る)
    eprintln!("検出されたアダプタ: {listing}");
    assert!(
        !adapters.is_empty(),
        "Intel HW アダプタが列挙されない: {listing}"
    );

    let node = adapters[0].drm_render_node;
    let adapter = AdapterSelector::DrmRenderNode(node);

    // Encoder を作って即破棄する
    let encoder_config = EncoderConfig::new(
        adapter,
        CodecConfig::H264(H264EncoderConfig { profile: None }),
        320,
        240,
        FrameFormat::Nv12,
        30,
        1,
        RateControlMode::Cqp,
    );
    let _encoder = match Encoder::new(
        encoder_config,
        FnEncodeHandler::new(|_: Result<EncodedFrame<()>, Error>| {}),
    ) {
        Ok(e) => e,
        Err(err) => panic!("Encoder::new に失敗 ({err}) for render node {node}: {listing}"),
    };

    // Decoder を作って即破棄する
    let decoder_config = DecoderConfig::new(adapter, DecoderCodec::H264);
    let _decoder = match Decoder::new(
        decoder_config,
        FnDecodeHandler::new(|_: Result<shiguredo_vpl::DecodedFrame<()>, Error>| {}),
    ) {
        Ok(d) => d,
        Err(err) => panic!("Decoder::new に失敗 ({err}) for render node {node}: {listing}"),
    };

    // supported_codecs が空でないこと
    let codecs = supported_codecs(adapter).expect("supported_codecs に失敗");
    assert!(
        !codecs.is_empty(),
        "supported_codecs が空 (render node {node}): {listing} codecs={codecs:?}",
    );
}
