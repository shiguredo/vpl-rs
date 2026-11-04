//! コーデックケーパビリティ照会の実機テスト

#![cfg(intel_vpl)]

use shiguredo_vpl::{
    AdapterSelector, FrameFormat, MemoryType, MemoryTypeCapabilities, ProfileCapabilities, Range,
    VideoCodecType, list_adapters, supported_codecs,
};

/// テスト用アダプタを返す
fn test_adapter() -> AdapterSelector {
    let adapters = list_adapters().expect("list_adapters に失敗");
    let first = adapters.first().expect("Intel HW アダプタが見つからない");
    AdapterSelector::DrmRenderNode(first.drm_render_node)
}

/// 幅・高さの範囲を検証する
fn assert_range(range: Range, context: &str) {
    assert!(range.min > 0, "{context}: min が 0 になっている: {range:?}");
    assert!(
        range.max >= range.min,
        "{context}: max ({}) が min ({}) より小さい: {range:?}",
        range.max,
        range.min
    );
    assert!(
        range.step > 0,
        "{context}: step が 0 になっている: {range:?}"
    );
}

/// メモリタイプごとのケーパビリティを検証する
fn assert_memory_type_capabilities(capabilities: &MemoryTypeCapabilities, context: &str) {
    assert_range(capabilities.width, &format!("{context} 幅"));
    assert_range(capabilities.height, &format!("{context} 高さ"));
}

/// プロファイルごとのケーパビリティを検証する
fn assert_profiles(profiles: &[ProfileCapabilities], context: &str) {
    assert!(
        !profiles.is_empty(),
        "{context}: プロファイルが空になっている"
    );

    for profile in profiles {
        assert!(
            !profile.memory_types.is_empty(),
            "{context}: {:?} のメモリタイプが空になっている",
            profile.profile
        );
        for capabilities in &profile.memory_types {
            assert_memory_type_capabilities(
                capabilities,
                &format!(
                    "{context} {:?} {:?}",
                    profile.profile, capabilities.memory_type
                ),
            );
        }
    }
}

/// システムメモリのエントリに NV12 が含まれていることを検証する
fn assert_system_memory_supports_nv12(profiles: &[ProfileCapabilities], context: &str) {
    let found = profiles
        .iter()
        .flat_map(|profile| &profile.memory_types)
        .filter(|capabilities| capabilities.memory_type == MemoryType::System)
        .any(|capabilities| capabilities.color_formats.contains(&FrameFormat::Nv12));
    assert!(
        found,
        "{context}: システムメモリの color_formats に NV12 が含まれていない: {profiles:?}"
    );
}

/// 実機のエンコーダー・デコーダーがプロファイル別・メモリタイプ別のケーパビリティを返すこと
#[test]
fn test_supported_codecs_return_capabilities() {
    let codecs = supported_codecs(test_adapter()).expect("supported_codecs に失敗");
    let listing = format!("{codecs:#?}");
    assert_eq!(codecs.len(), 4, "コーデック数が 4 ではない: {listing}");

    let mut num_supported_decoding = 0usize;
    let mut num_supported_encoding = 0usize;

    for codec in &codecs {
        let context = format!("{:?}", codec.codec);

        // デコードのケーパビリティ
        if let Some(decoding) = &codec.decoding {
            num_supported_decoding += 1;
            assert_profiles(&decoding.profiles, &format!("{context} デコード"));
            assert_system_memory_supports_nv12(&decoding.profiles, &format!("{context} デコード"));
            // VPL が 0 を返した場合は None に変換される
            assert_ne!(
                decoding.max_codec_level,
                Some(0),
                "{context}: デコードの max_codec_level が Some(0) になっている: {listing}"
            );
            // VP9 は VPL が最大コーデックレベルを返さない
            if codec.codec != VideoCodecType::Vp9 {
                assert!(
                    matches!(decoding.max_codec_level, Some(level) if level > 0),
                    "{context}: デコードの max_codec_level が無効: {:?}",
                    decoding.max_codec_level
                );
            }
        }

        // エンコードのケーパビリティ
        if let Some(encoding) = &codec.encoding {
            num_supported_encoding += 1;
            assert_profiles(&encoding.profiles, &format!("{context} エンコード"));
            assert_system_memory_supports_nv12(
                &encoding.profiles,
                &format!("{context} エンコード"),
            );
            // VPL が 0 を返した場合は None に変換される
            assert_ne!(
                encoding.max_codec_level,
                Some(0),
                "{context}: エンコードの max_codec_level が Some(0) になっている: {listing}"
            );
            // VP9 は VPL が最大コーデックレベルを返さない
            if codec.codec != VideoCodecType::Vp9 {
                assert!(
                    matches!(encoding.max_codec_level, Some(level) if level > 0),
                    "{context}: エンコードの max_codec_level が無効: {:?}",
                    encoding.max_codec_level
                );
            }
        }
    }

    assert!(
        num_supported_decoding > 0,
        "デコード対応コーデックが 1 つも無い: {listing}"
    );
    assert!(
        num_supported_encoding > 0,
        "エンコード対応コーデックが 1 つも無い: {listing}"
    );
}
