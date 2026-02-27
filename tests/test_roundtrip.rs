use shiguredo_vpl::{
    Av1EncoderConfig, Av1Profile, CodecConfig, Decoder, DecoderCodec, DecoderConfig, EncodeOptions,
    EncodedFrame, Encoder, EncoderConfig, FrameFormat, H264EncoderConfig, H264Profile,
    HevcEncoderConfig, HevcProfile, PictureType, RateControlMode, frame_type,
};

/// ダミー NV12 フレームを生成する
///
/// Y プレーンはフレーム番号に応じたグラデーション、UV プレーンは 128 固定。
fn generate_dummy_nv12(width: usize, height: usize, frame_index: usize) -> Vec<u8> {
    let y_size = width * height;
    let uv_size = y_size / 2;
    let mut data = vec![0u8; y_size + uv_size];

    for y in 0..height {
        for x in 0..width {
            data[y * width + x] = ((x + y + frame_index * 7) % 256) as u8;
        }
    }
    for i in 0..uv_size {
        data[y_size + i] = 128;
    }

    data
}

/// SMPTE カラーバー風の NV12 フレームを生成する
///
/// 7 色の縦ストライプ（白/黄/シアン/緑/マゼンタ/赤/青）を
/// BT.601 で YUV に変換し NV12 形式で返す。
fn generate_colorbar_nv12(width: usize, height: usize) -> Vec<u8> {
    // SMPTE カラーバーの RGB 値（白/黄/シアン/緑/マゼンタ/赤/青）
    let bars: [(u8, u8, u8); 7] = [
        (235, 235, 235), // 白
        (235, 235, 16),  // 黄
        (16, 235, 235),  // シアン
        (16, 235, 16),   // 緑
        (235, 16, 235),  // マゼンタ
        (235, 16, 16),   // 赤
        (16, 16, 235),   // 青
    ];

    let y_size = width * height;
    let uv_size = y_size / 2;
    let mut data = vec![0u8; y_size + uv_size];
    let uv_offset = y_size;

    for y in 0..height {
        for x in 0..width {
            let bar_index = x * 7 / width;
            let (r, g, b) = bars[bar_index];

            // BT.601 RGB -> YCbCr
            let rf = r as f64;
            let gf = g as f64;
            let bf = b as f64;
            let yv = (0.257 * rf + 0.504 * gf + 0.098 * bf + 16.0).clamp(16.0, 235.0) as u8;
            data[y * width + x] = yv;

            // UV は 2x2 ブロック単位（左上ピクセルで代表する）
            if y % 2 == 0 && x % 2 == 0 {
                let u = (-0.148 * rf - 0.291 * gf + 0.439 * bf + 128.0).clamp(16.0, 240.0) as u8;
                let v = (0.439 * rf - 0.368 * gf - 0.071 * bf + 128.0).clamp(16.0, 240.0) as u8;
                let uv_row = y / 2;
                let uv_col = x; // NV12 はインターリーブなので x そのまま
                data[uv_offset + uv_row * width + uv_col] = u;
                data[uv_offset + uv_row * width + uv_col + 1] = v;
            }
        }
    }

    data
}

/// Y プレーン同士の PSNR を計算する（dB）
///
/// 値が大きいほど入力と出力が近い。一般に 30dB 以上あれば視覚的に良好。
fn psnr_y(original: &[u8], decoded: &[u8], width: usize, height: usize) -> f64 {
    assert_eq!(original.len(), decoded.len());
    let y_size = width * height;
    let mut mse_sum: f64 = 0.0;
    for i in 0..y_size {
        let diff = original[i] as f64 - decoded[i] as f64;
        mse_sum += diff * diff;
    }
    let mse = mse_sum / y_size as f64;
    if mse == 0.0 {
        return f64::INFINITY;
    }
    10.0 * (255.0_f64 * 255.0 / mse).log10()
}

/// エンコードしてフレーム一覧とビットストリームを返すヘルパー
fn encode(config: EncoderConfig, frames: &[Vec<u8>]) -> (Vec<EncodedFrame>, Vec<u8>) {
    let mut encoder = Encoder::new(config).expect("failed to create encoder");
    let options = EncodeOptions {
        frame_type: frame_type::UNKNOWN,
    };
    let mut encoded_frames = Vec::new();
    let mut bitstream = Vec::new();

    for frame in frames {
        encoder.encode(frame, &options).expect("failed to encode");
        while let Some(encoded) = encoder.next_frame() {
            bitstream.extend_from_slice(encoded.data());
            encoded_frames.push(encoded);
        }
    }

    encoder.finish().expect("failed to finish");
    while let Some(encoded) = encoder.next_frame() {
        bitstream.extend_from_slice(encoded.data());
        encoded_frames.push(encoded);
    }

    (encoded_frames, bitstream)
}

/// デコードしてフレーム一覧を返すヘルパー
fn decode(decoder_codec: DecoderCodec, bitstream: &[u8]) -> Vec<shiguredo_vpl::DecodedFrame> {
    let config = DecoderConfig {
        codec: decoder_codec,
    };
    let mut decoder = Decoder::new(config).expect("failed to create decoder");

    decoder.decode(bitstream).expect("failed to decode");
    decoder.finish().expect("failed to finish");

    let mut decoded_frames = Vec::new();
    while let Some(frame) = decoder.next_frame() {
        decoded_frames.push(frame);
    }

    decoded_frames
}

/// エンコード→デコードのラウンドトリップを検証するヘルパー
fn roundtrip(
    encoder_config: EncoderConfig,
    decoder_codec: DecoderCodec,
    input_frames: &[Vec<u8>],
) -> (Vec<EncodedFrame>, Vec<shiguredo_vpl::DecodedFrame>) {
    let width = encoder_config.width as usize;
    let height = encoder_config.height as usize;
    let num_frames = input_frames.len();

    let (encoded_frames, bitstream) = encode(encoder_config, input_frames);

    assert!(
        !encoded_frames.is_empty(),
        "no encoded frames were produced"
    );
    for (i, frame) in encoded_frames.iter().enumerate() {
        assert!(!frame.data().is_empty(), "encoded frame {i} has empty data");
    }

    let decoded_frames = decode(decoder_codec, &bitstream);

    assert_eq!(
        decoded_frames.len(),
        num_frames,
        "decoded {} frames, expected {num_frames}",
        decoded_frames.len()
    );
    for (i, frame) in decoded_frames.iter().enumerate() {
        assert_eq!(frame.width(), width, "decoded frame {i} width mismatch");
        assert_eq!(frame.height(), height, "decoded frame {i} height mismatch");
        assert!(!frame.data().is_empty(), "decoded frame {i} has empty data");
    }

    (encoded_frames, decoded_frames)
}

/// カラーバーを使ったラウンドトリップで PSNR を検証するヘルパー
///
/// 同一のカラーバーフレームを num_frames 回エンコードし、デコード後に
/// 元の Y プレーンとの PSNR が min_psnr_db 以上であることを確認する。
fn roundtrip_colorbar(
    encoder_config: EncoderConfig,
    decoder_codec: DecoderCodec,
    num_frames: usize,
    min_psnr_db: f64,
) {
    let width = encoder_config.width as usize;
    let height = encoder_config.height as usize;

    let colorbar = generate_colorbar_nv12(width, height);
    let input_frames: Vec<Vec<u8>> = (0..num_frames).map(|_| colorbar.clone()).collect();

    let (_, decoded_frames) = roundtrip(encoder_config, decoder_codec, &input_frames);

    for (i, decoded) in decoded_frames.iter().enumerate() {
        let psnr = psnr_y(&colorbar, decoded.data(), width, height);
        assert!(
            psnr >= min_psnr_db,
            "frame {i}: PSNR {psnr:.1} dB < {min_psnr_db} dB"
        );
    }
}

// --- H.264 ---

/// H.264 CBR カラーバーのラウンドトリップ（PSNR 検証）
#[test]
fn test_roundtrip_h264_cbr() {
    let mut config = EncoderConfig::new(
        CodecConfig::H264(H264EncoderConfig {
            profile: Some(H264Profile::High),
        }),
        320,
        240,
        FrameFormat::Nv12,
        30,
        1,
        RateControlMode::Cbr,
    );
    config.target_kbps = Some(1000);
    config.gop_pic_size = Some(30);

    roundtrip_colorbar(config, DecoderCodec::H264, 30, 25.0);
}

/// H.264 CQP カラーバーのラウンドトリップ（PSNR 検証）
#[test]
fn test_roundtrip_h264_cqp() {
    let mut config = EncoderConfig::new(
        CodecConfig::H264(H264EncoderConfig {
            profile: Some(H264Profile::Main),
        }),
        320,
        240,
        FrameFormat::Nv12,
        30,
        1,
        RateControlMode::Cqp,
    );
    config.qpi = Some(26);
    config.qpp = Some(28);
    config.qpb = Some(30);
    config.gop_pic_size = Some(10);

    roundtrip_colorbar(config, DecoderCodec::H264, 10, 25.0);
}

/// H.264 で IDR フレームを強制してラウンドトリップする
#[test]
fn test_roundtrip_h264_force_idr() {
    let mut config = EncoderConfig::new(
        CodecConfig::H264(H264EncoderConfig {
            profile: Some(H264Profile::High),
        }),
        320,
        240,
        FrameFormat::Nv12,
        30,
        1,
        RateControlMode::Cbr,
    );
    config.target_kbps = Some(1000);
    config.gop_pic_size = Some(300);

    let width = config.width as usize;
    let height = config.height as usize;
    let mut encoder = Encoder::new(config).expect("failed to create encoder");
    let mut encoded_frames = Vec::new();
    let mut bitstream = Vec::new();

    for i in 0..15 {
        let frame = generate_dummy_nv12(width, height, i);
        let options = if i == 10 {
            EncodeOptions {
                frame_type: frame_type::IDR | frame_type::I | frame_type::REF,
            }
        } else {
            EncodeOptions {
                frame_type: frame_type::UNKNOWN,
            }
        };
        encoder.encode(&frame, &options).expect("failed to encode");
        while let Some(encoded) = encoder.next_frame() {
            bitstream.extend_from_slice(encoded.data());
            encoded_frames.push(encoded);
        }
    }
    encoder.finish().expect("failed to finish");
    while let Some(encoded) = encoder.next_frame() {
        bitstream.extend_from_slice(encoded.data());
        encoded_frames.push(encoded);
    }

    let idr_count = encoded_frames
        .iter()
        .filter(|f| f.picture_type() == PictureType::Idr)
        .count();
    assert!(
        idr_count >= 2,
        "expected at least 2 IDR frames, got {idr_count}"
    );

    // デコードで復号できることを確認する
    let decoded_frames = decode(DecoderCodec::H264, &bitstream);
    assert_eq!(decoded_frames.len(), 15);
}

// --- H.265 ---

/// H.265 CBR カラーバーのラウンドトリップ（PSNR 検証）
#[test]
fn test_roundtrip_hevc_cbr() {
    let mut config = EncoderConfig::new(
        CodecConfig::Hevc(HevcEncoderConfig {
            profile: Some(HevcProfile::Main),
        }),
        320,
        240,
        FrameFormat::Nv12,
        30,
        1,
        RateControlMode::Cbr,
    );
    config.target_kbps = Some(1000);
    config.gop_pic_size = Some(30);

    roundtrip_colorbar(config, DecoderCodec::Hevc, 30, 25.0);
}

/// H.265 CQP カラーバーのラウンドトリップ（PSNR 検証）
#[test]
fn test_roundtrip_hevc_cqp() {
    let mut config = EncoderConfig::new(
        CodecConfig::Hevc(HevcEncoderConfig {
            profile: Some(HevcProfile::Main),
        }),
        320,
        240,
        FrameFormat::Nv12,
        30,
        1,
        RateControlMode::Cqp,
    );
    config.qpi = Some(26);
    config.qpp = Some(28);
    config.qpb = Some(30);
    config.gop_pic_size = Some(10);

    roundtrip_colorbar(config, DecoderCodec::Hevc, 10, 25.0);
}

// --- AV1 ---

/// AV1 CBR カラーバーのラウンドトリップ（PSNR 検証）
#[test]
fn test_roundtrip_av1_cbr() {
    let mut config = EncoderConfig::new(
        CodecConfig::Av1(Av1EncoderConfig {
            profile: Some(Av1Profile::Main),
        }),
        320,
        240,
        FrameFormat::Nv12,
        30,
        1,
        RateControlMode::Cbr,
    );
    config.target_kbps = Some(1000);
    config.gop_pic_size = Some(30);

    roundtrip_colorbar(config, DecoderCodec::Av1, 30, 25.0);
}

/// AV1 CQP カラーバーのラウンドトリップ（PSNR 検証）
#[test]
fn test_roundtrip_av1_cqp() {
    let mut config = EncoderConfig::new(
        CodecConfig::Av1(Av1EncoderConfig {
            profile: Some(Av1Profile::Main),
        }),
        320,
        240,
        FrameFormat::Nv12,
        30,
        1,
        RateControlMode::Cqp,
    );
    config.qpi = Some(26);
    config.qpp = Some(28);
    config.qpb = Some(30);
    config.gop_pic_size = Some(10);

    roundtrip_colorbar(config, DecoderCodec::Av1, 10, 25.0);
}

// ---------------------------------------------------------------------------
// 各フレームフォーマットのダミーデータ生成ヘルパー
// ---------------------------------------------------------------------------

/// ダミー YUY2 フレームを生成する
///
/// Packed YUV 4:2:2: YUYV の繰り返し（2 ピクセルで 4 バイト）
fn generate_dummy_yuy2(width: usize, height: usize, frame_index: usize) -> Vec<u8> {
    let mut data = vec![0u8; width * height * 2];
    for y in 0..height {
        for x in (0..width).step_by(2) {
            let offset = (y * width + x) * 2;
            let y0 = ((x + y + frame_index * 7) % 256) as u8;
            let y1 = ((x + 1 + y + frame_index * 7) % 256) as u8;
            data[offset] = y0; // Y0
            data[offset + 1] = 128; // U
            data[offset + 2] = y1; // Y1
            data[offset + 3] = 128; // V
        }
    }
    data
}

/// ダミー BGRA フレームを生成する
fn generate_dummy_bgra(width: usize, height: usize, frame_index: usize) -> Vec<u8> {
    let mut data = vec![0u8; width * height * 4];
    for y in 0..height {
        for x in 0..width {
            let offset = (y * width + x) * 4;
            let v = ((x + y + frame_index * 7) % 256) as u8;
            data[offset] = v; // B
            data[offset + 1] = v; // G
            data[offset + 2] = v; // R
            data[offset + 3] = 255; // A
        }
    }
    data
}

/// 指定フォーマットのダミーフレームを生成する
fn generate_dummy_frame(
    format: FrameFormat,
    width: usize,
    height: usize,
    frame_index: usize,
) -> Vec<u8> {
    match format {
        FrameFormat::Nv12 => generate_dummy_nv12(width, height, frame_index),
        FrameFormat::Yuy2 => generate_dummy_yuy2(width, height, frame_index),
        FrameFormat::Bgra => generate_dummy_bgra(width, height, frame_index),
    }
}

/// フォーマット指定のラウンドトリップテストを実行するヘルパー
///
/// 指定フォーマットでダミーフレームを生成し、エンコード→デコードして
/// フレーム数が一致することを検証する。
fn roundtrip_format(
    codec_config: CodecConfig,
    decoder_codec: DecoderCodec,
    format: FrameFormat,
    num_frames: usize,
) {
    let width: u32 = 320;
    let height: u32 = 240;

    let mut config = EncoderConfig::new(
        codec_config,
        width,
        height,
        format,
        30,
        1,
        RateControlMode::Cbr,
    );
    config.target_kbps = Some(1000);
    config.gop_pic_size = Some(30);

    let input_frames: Vec<Vec<u8>> = (0..num_frames)
        .map(|i| generate_dummy_frame(format, width as usize, height as usize, i))
        .collect();

    // フレームサイズが FrameFormat::frame_size() と一致することを確認する
    let expected_size = format.frame_size(width as usize, height as usize);
    for (i, frame) in input_frames.iter().enumerate() {
        assert_eq!(
            frame.len(),
            expected_size,
            "frame {i}: size {} != expected {expected_size} for {format:?}",
            frame.len()
        );
    }

    let (_, decoded_frames) = roundtrip(config, decoder_codec, &input_frames);

    assert_eq!(
        decoded_frames.len(),
        num_frames,
        "{format:?}: decoded {} frames, expected {num_frames}",
        decoded_frames.len()
    );
}

// ---------------------------------------------------------------------------
// フレームフォーマット別ラウンドトリップテスト
// ---------------------------------------------------------------------------

/// BGRA 入力の H.264 ラウンドトリップ
#[test]
fn test_roundtrip_h264_bgra() {
    roundtrip_format(
        CodecConfig::H264(H264EncoderConfig {
            profile: Some(H264Profile::High),
        }),
        DecoderCodec::H264,
        FrameFormat::Bgra,
        10,
    );
}
