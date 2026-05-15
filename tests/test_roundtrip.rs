#![cfg(target_os = "linux")]

use std::sync::OnceLock;
use std::sync::mpsc;
use std::time::Duration;

use shiguredo_vpl::{
    AdapterSelector, Av1EncoderConfig, Av1Profile, CodecConfig, Decoder, DecoderCodec,
    DecoderConfig, EncodeOptions, EncodedFrame, Encoder, EncoderConfig, Error, FnDecodeHandler,
    FnEncodeHandler, FrameFormat, H264EncoderConfig, H264Profile, HevcEncoderConfig, HevcProfile,
    PictureType, RateControlMode, frame_type, list_adapters,
};

/// テスト用アダプタを返す
///
/// `list_adapters()` の結果をテストバイナリ単位でキャッシュし、`MFXLoad` の
/// 繰り返し呼び出しを避ける。Intel HW アダプタが見つからない環境では panic
/// する（実機テストは Intel GPU 付きランナー上で実行する想定）。
fn test_adapter() -> AdapterSelector {
    static CACHED: OnceLock<AdapterSelector> = OnceLock::new();
    *CACHED.get_or_init(|| {
        let adapters = list_adapters().expect("list_adapters に失敗");
        let first = adapters.first().expect("Intel HW アダプタが見つからない");
        AdapterSelector::DrmRenderNode(first.drm_render_node)
    })
}

/// コールバックから取り出したデコード済みフレーム情報（データをコピーして保持する）
struct DecodedFrameInfo {
    y_data: Vec<u8>,
    pitch: usize,
    width: usize,
    height: usize,
}

/// ダミー NV12 フレームを生成する
///
/// Y プレーンはフレーム番号に応じたグラデーション、UV プレーンは 128 固定。
fn generate_dummy_nv12(
    width: usize,
    height: usize,
    coded_width: usize,
    coded_height: usize,
    frame_index: usize,
) -> Vec<u8> {
    assert!(
        width <= coded_width && height <= coded_height,
        "source size {}x{} exceeds coded size {}x{}",
        width,
        height,
        coded_width,
        coded_height,
    );
    let y_size = coded_width * coded_height;
    let uv_height = coded_height.div_ceil(2);
    let uv_size = coded_width * uv_height;
    let mut data = vec![0u8; y_size + uv_size];
    for i in 0..uv_size {
        data[y_size + i] = 128;
    }

    for y in 0..height {
        for x in 0..width {
            data[y * coded_width + x] = ((x + y + frame_index * 7) % 256) as u8;
        }
    }

    data
}

/// SMPTE カラーバー風の NV12 フレームを生成する
///
/// 7 色の縦ストライプ（白/黄/シアン/緑/マゼンタ/赤/青）を
/// BT.601 で YUV に変換し NV12 形式で返す。
fn generate_colorbar_nv12(
    width: usize,
    height: usize,
    coded_width: usize,
    coded_height: usize,
) -> Vec<u8> {
    assert!(
        width <= coded_width && height <= coded_height,
        "source size {}x{} exceeds coded size {}x{}",
        width,
        height,
        coded_width,
        coded_height,
    );
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

    let y_size = coded_width * coded_height;
    let uv_height = coded_height.div_ceil(2);
    let uv_size = coded_width * uv_height;
    let mut data = vec![0u8; y_size + uv_size];
    let uv_offset = y_size;
    for i in 0..uv_size {
        data[uv_offset + i] = 128;
    }

    for y in 0..height {
        for x in 0..width {
            let bar_index = x * 7 / width;
            let (r, g, b) = bars[bar_index];

            // BT.601 RGB -> YCbCr
            let rf = r as f64;
            let gf = g as f64;
            let bf = b as f64;
            let yv = (0.257 * rf + 0.504 * gf + 0.098 * bf + 16.0).clamp(16.0, 235.0) as u8;
            data[y * coded_width + x] = yv;

            // UV は 2x2 ブロック単位（左上ピクセルで代表する）
            if y % 2 == 0 && x % 2 == 0 {
                let u = (-0.148 * rf - 0.291 * gf + 0.439 * bf + 128.0).clamp(16.0, 240.0) as u8;
                let v = (0.439 * rf - 0.368 * gf - 0.071 * bf + 128.0).clamp(16.0, 240.0) as u8;
                let uv_row = y / 2;
                let uv_col = x; // NV12 はインターリーブなので x そのまま
                data[uv_offset + uv_row * coded_width + uv_col] = u;
                data[uv_offset + uv_row * coded_width + uv_col + 1] = v;
            }
        }
    }

    data
}

/// Y プレーン同士の PSNR を計算する（dB）
///
/// 値が大きいほど入力と出力が近い。一般に 30dB 以上あれば視覚的に良好。
/// decoded_y はピッチを含む生データ。
fn psnr_y(original: &[u8], decoded_y: &[u8], pitch: usize, width: usize, height: usize) -> f64 {
    let mut mse_sum: f64 = 0.0;
    let pixels = width * height;
    for row in 0..height {
        for col in 0..width {
            let orig = original[row * width + col] as f64;
            let dec = decoded_y[row * pitch + col] as f64;
            let diff = orig - dec;
            mse_sum += diff * diff;
        }
    }
    let mse = mse_sum / pixels as f64;
    if mse == 0.0 {
        return f64::INFINITY;
    }
    10.0 * (255.0_f64 * 255.0 / mse).log10()
}

/// エンコードしてフレーム一覧とフレームごとのビットストリームを返すヘルパー
fn encode(config: EncoderConfig, frames: &[Vec<u8>]) -> (Vec<EncodedFrame<usize>>, Vec<Vec<u8>>) {
    let (tx, rx) = mpsc::channel::<Result<EncodedFrame<usize>, Error>>();
    let mut encoder = Encoder::new(
        config,
        FnEncodeHandler::new(move |result| {
            tx.send(result)
                .expect("failed to send encoded frame callback result");
        }),
    )
    .expect("failed to create encoder");
    let options = EncodeOptions {
        frame_type: frame_type::UNKNOWN,
    };

    for (index, frame) in frames.iter().enumerate() {
        encoder
            .encode(frame, index, &options)
            .expect("failed to encode");
    }
    encoder.finish().expect("failed to finish");

    let mut encoded_frames = Vec::new();
    let mut bitstreams: Vec<Vec<u8>> = Vec::new();
    for _ in 0..frames.len() {
        let result = rx
            .recv_timeout(Duration::from_secs(10))
            .expect("timed out waiting for encoded frame callback");
        let encoded = result.expect("failed to encode");
        bitstreams.push(encoded.data().to_vec());
        encoded_frames.push(encoded);
    }

    let mut seen = vec![0u32; frames.len()];
    for frame in &encoded_frames {
        let user_data = *frame.user_data();
        assert!(
            user_data < frames.len(),
            "encoded user_data {user_data} is out of range"
        );
        seen[user_data] += 1;
    }
    for (index, count) in seen.iter().enumerate() {
        assert_eq!(
            *count, 1,
            "callback user_data {index} was expected once but appeared {count} times"
        );
    }

    (encoded_frames, bitstreams)
}

/// デコードしてフレーム一覧を返すヘルパー（フレームごとに decode を呼ぶ）
fn decode(decoder_codec: DecoderCodec, bitstreams: &[Vec<u8>]) -> Vec<DecodedFrameInfo> {
    let num_frames = bitstreams.len();
    let config = DecoderConfig::new(test_adapter(), decoder_codec);
    let (tx, rx) = mpsc::channel::<Result<DecodedFrameInfo, Error>>();
    let mut decoder = Decoder::new(
        config,
        FnDecodeHandler::new(move |result| {
            let info = result.map(|frame| {
                let y_data = frame.y().to_vec();
                DecodedFrameInfo {
                    y_data,
                    pitch: frame.pitch(),
                    width: frame.width(),
                    height: frame.height(),
                }
            });
            tx.send(info)
                .expect("failed to send decoded frame callback result");
        }),
    )
    .expect("failed to create decoder");

    for (index, bs) in bitstreams.iter().enumerate() {
        decoder.decode(bs, index).expect("failed to decode");
    }
    decoder.finish().expect("failed to finish");

    let mut decoded_frames = Vec::new();
    for _ in 0..num_frames {
        let result = rx
            .recv_timeout(Duration::from_secs(10))
            .expect("timed out waiting for decoded frame callback");
        let info = result.expect("failed to decode");
        decoded_frames.push(info);
    }

    decoded_frames
}

/// エンコード→デコードのラウンドトリップを検証するヘルパー
fn roundtrip(
    encoder_config: EncoderConfig,
    decoder_codec: DecoderCodec,
    input_frames: &[Vec<u8>],
) -> (Vec<EncodedFrame<usize>>, Vec<DecodedFrameInfo>) {
    let width = encoder_config.width as usize;
    let height = encoder_config.height as usize;
    let num_frames = input_frames.len();

    let (encoded_frames, bitstreams) = encode(encoder_config, input_frames);

    assert!(
        !encoded_frames.is_empty(),
        "no encoded frames were produced"
    );
    for (i, frame) in encoded_frames.iter().enumerate() {
        assert!(!frame.data().is_empty(), "encoded frame {i} has empty data");
    }

    let decoded_frames = decode(decoder_codec, &bitstreams);

    assert_eq!(
        decoded_frames.len(),
        num_frames,
        "decoded {} frames, expected {num_frames}",
        decoded_frames.len()
    );
    for (i, frame) in decoded_frames.iter().enumerate() {
        assert_eq!(frame.width, width, "decoded frame {i} width mismatch");
        assert_eq!(frame.height, height, "decoded frame {i} height mismatch");
        assert!(
            !frame.y_data.is_empty(),
            "decoded frame {i} has empty y plane"
        );
    }

    (encoded_frames, decoded_frames)
}

/// 指定エンコーダ設定の coded サイズを取得する
fn coded_size_for(config: &EncoderConfig) -> (usize, usize) {
    let encoder: Encoder<FnEncodeHandler<()>> =
        Encoder::new(config.clone(), FnEncodeHandler::new(|_| {}))
            .expect("failed to create encoder");
    encoder.coded_size()
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
    let (coded_width, coded_height) = coded_size_for(&encoder_config);

    let colorbar = generate_colorbar_nv12(width, height, coded_width, coded_height);
    let colorbar_for_psnr = generate_colorbar_nv12(width, height, width, height);
    let input_frames: Vec<Vec<u8>> = (0..num_frames).map(|_| colorbar.clone()).collect();

    let (_, decoded_frames) = roundtrip(encoder_config, decoder_codec, &input_frames);

    for (i, decoded) in decoded_frames.iter().enumerate() {
        let psnr = psnr_y(
            &colorbar_for_psnr,
            &decoded.y_data,
            decoded.pitch,
            width,
            height,
        );
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
        test_adapter(),
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
        test_adapter(),
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
        test_adapter(),
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
    let (tx, rx) = mpsc::channel::<Result<EncodedFrame<usize>, Error>>();
    let mut encoder = Encoder::new(
        config,
        FnEncodeHandler::new(move |result| {
            tx.send(result)
                .expect("failed to send encoded frame callback result");
        }),
    )
    .expect("failed to create encoder");
    let (coded_width, coded_height) = encoder.coded_size();
    let mut encoded_frames = Vec::new();
    let mut bitstreams: Vec<Vec<u8>> = Vec::new();

    for i in 0..15 {
        let frame_data = generate_dummy_nv12(width, height, coded_width, coded_height, i);
        let options = if i == 10 {
            EncodeOptions {
                frame_type: frame_type::IDR | frame_type::I | frame_type::REF,
            }
        } else {
            EncodeOptions {
                frame_type: frame_type::UNKNOWN,
            }
        };
        encoder
            .encode(&frame_data, i, &options)
            .expect("failed to encode");
    }
    encoder.finish().expect("failed to finish");
    for _ in 0..15 {
        let encoded = rx
            .recv_timeout(Duration::from_secs(10))
            .expect("timed out waiting for encoded frame callback")
            .expect("failed to encode");
        bitstreams.push(encoded.data().to_vec());
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
    let decoded_frames = decode(DecoderCodec::H264, &bitstreams);
    assert_eq!(decoded_frames.len(), 15);
}

/// encode に渡した user_data が callback で回収できることを確認する
#[test]
fn test_encode_user_data_callback() {
    let mut config = EncoderConfig::new(
        test_adapter(),
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

    let (tx, rx) = mpsc::channel::<Result<EncodedFrame<usize>, Error>>();
    let mut encoder = Encoder::new(
        config,
        FnEncodeHandler::new(move |result| {
            tx.send(result)
                .expect("failed to send encoded frame callback result");
        }),
    )
    .expect("failed to create encoder");
    let (coded_width, coded_height) = encoder.coded_size();
    let options = EncodeOptions {
        frame_type: frame_type::UNKNOWN,
    };

    for i in 0..8 {
        let frame_data = generate_dummy_nv12(320, 240, coded_width, coded_height, i);
        encoder
            .encode(&frame_data, i, &options)
            .expect("failed to encode");
    }
    encoder.finish().expect("failed to finish");

    let mut seen = [false; 8];
    for _ in 0..8 {
        let encoded = rx
            .recv_timeout(Duration::from_secs(10))
            .expect("timed out waiting for encoded frame callback")
            .expect("failed to encode");
        let user_data = *encoded.user_data();
        assert!(user_data < 8, "user_data {user_data} is out of range");
        seen[user_data] = true;
    }

    for (index, appeared) in seen.iter().enumerate() {
        assert!(*appeared, "user_data {index} did not appear in callback");
    }
}

/// decode に渡した user_data が callback で回収できることを確認する
#[test]
fn test_decode_user_data_callback() {
    let mut config = EncoderConfig::new(
        test_adapter(),
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

    let width = config.width as usize;
    let height = config.height as usize;
    let (coded_width, coded_height) = coded_size_for(&config);
    let num_frames = 8;
    let input_frames: Vec<Vec<u8>> = (0..num_frames)
        .map(|i| generate_dummy_nv12(width, height, coded_width, coded_height, i))
        .collect();
    let (_, bitstreams) = encode(config, &input_frames);

    let decoder_config = DecoderConfig::new(test_adapter(), DecoderCodec::H264);
    let (tx, rx) = mpsc::channel::<Result<usize, Error>>();
    let mut decoder = Decoder::new(
        decoder_config,
        FnDecodeHandler::new(move |result| {
            let user_data = result.map(|frame| *frame.user_data());
            tx.send(user_data)
                .expect("failed to send decoded frame callback result");
        }),
    )
    .expect("failed to create decoder");

    for (i, bs) in bitstreams.iter().enumerate() {
        decoder.decode(bs, i).expect("failed to decode");
    }
    decoder.finish().expect("failed to finish");

    let mut seen = [false; 8];
    for _ in 0..num_frames {
        let user_data = rx
            .recv_timeout(Duration::from_secs(10))
            .expect("timed out waiting for decoded frame callback")
            .expect("failed to decode");
        assert!(
            user_data < num_frames,
            "user_data {user_data} is out of range"
        );
        seen[user_data] = true;
    }

    for (index, appeared) in seen.iter().enumerate() {
        assert!(*appeared, "user_data {index} did not appear in callback");
    }
}

/// drop 時に未完了フレームへキャンセル callback が送信されることを確認する
#[test]
fn test_drop_cancels_pending_callbacks() {
    let mut config = EncoderConfig::new(
        test_adapter(),
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
    // 最初の入力で MFX_ERR_MORE_DATA になりやすい設定にして、
    // finish 前に drop したとき未完了分が残る状況を作る。
    config.gop_ref_dist = Some(3);

    let (tx, rx) = mpsc::channel::<Result<EncodedFrame<usize>, Error>>();
    {
        let mut encoder = Encoder::new(
            config,
            FnEncodeHandler::new(move |result| {
                tx.send(result)
                    .expect("failed to send encoded frame callback result");
            }),
        )
        .expect("failed to create encoder");
        let (coded_width, coded_height) = encoder.coded_size();
        let options = EncodeOptions {
            frame_type: frame_type::UNKNOWN,
        };

        let frame_data = generate_dummy_nv12(320, 240, coded_width, coded_height, 0);
        encoder
            .encode(&frame_data, 0, &options)
            .expect("failed to encode");
    }

    let result = rx
        .recv_timeout(Duration::from_secs(1))
        .expect("timed out waiting for callback result");
    let error = match result {
        Ok(_) => panic!("drop callback must be an error"),
        Err(error) => error,
    };
    assert_eq!(
        error.status_code(),
        Some(shiguredo_vpl::ffi::mfxStatus_MFX_ERR_ABORTED),
        "canceled callback must return MFX_ERR_ABORTED"
    );
    assert!(
        rx.recv_timeout(Duration::from_millis(100)).is_err(),
        "unexpected extra callback after canceled result"
    );
}

// --- H.265 ---

/// H.265 CBR カラーバーのラウンドトリップ（PSNR 検証）
#[test]
fn test_roundtrip_hevc_cbr() {
    let mut config = EncoderConfig::new(
        test_adapter(),
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
        test_adapter(),
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
        test_adapter(),
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
        test_adapter(),
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
fn generate_dummy_yuy2(
    width: usize,
    height: usize,
    coded_width: usize,
    coded_height: usize,
    frame_index: usize,
) -> Vec<u8> {
    assert!(
        width <= coded_width && height <= coded_height,
        "source size {}x{} exceeds coded size {}x{}",
        width,
        height,
        coded_width,
        coded_height,
    );
    let mut data = vec![0u8; coded_width * coded_height * 2];
    for y in 0..height {
        for x in (0..width).step_by(2) {
            let offset = (y * coded_width + x) * 2;
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
fn generate_dummy_bgra(
    width: usize,
    height: usize,
    coded_width: usize,
    coded_height: usize,
    frame_index: usize,
) -> Vec<u8> {
    assert!(
        width <= coded_width && height <= coded_height,
        "source size {}x{} exceeds coded size {}x{}",
        width,
        height,
        coded_width,
        coded_height,
    );
    let mut data = vec![0u8; coded_width * coded_height * 4];
    for y in 0..height {
        for x in 0..width {
            let offset = (y * coded_width + x) * 4;
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
    coded_width: usize,
    coded_height: usize,
    frame_index: usize,
) -> Vec<u8> {
    match format {
        FrameFormat::Nv12 => {
            generate_dummy_nv12(width, height, coded_width, coded_height, frame_index)
        }
        FrameFormat::Yuy2 => {
            generate_dummy_yuy2(width, height, coded_width, coded_height, frame_index)
        }
        FrameFormat::Bgra => {
            generate_dummy_bgra(width, height, coded_width, coded_height, frame_index)
        }
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
    roundtrip_format_with_size(codec_config, decoder_codec, format, num_frames, 320, 240);
}

/// フォーマット指定のラウンドトリップテストを実行するヘルパー（任意サイズ）
fn roundtrip_format_with_size(
    codec_config: CodecConfig,
    decoder_codec: DecoderCodec,
    format: FrameFormat,
    num_frames: usize,
    width: u32,
    height: u32,
) {
    let mut config = EncoderConfig::new(
        test_adapter(),
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
    let (coded_width, coded_height) = coded_size_for(&config);

    let input_frames: Vec<Vec<u8>> = (0..num_frames)
        .map(|i| {
            generate_dummy_frame(
                format,
                width as usize,
                height as usize,
                coded_width,
                coded_height,
                i,
            )
        })
        .collect();

    // フレームサイズが FrameFormat::frame_size() と一致することを確認する
    let expected_size = format
        .frame_size(coded_width, coded_height)
        .expect("frame size calculation overflowed");
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

/// NV12 入力で crop と coded のサイズが異なるケースの H.264 ラウンドトリップ
#[test]
fn test_roundtrip_h264_nv12_alignment_mismatch() {
    let mut config = EncoderConfig::new(
        test_adapter(),
        CodecConfig::H264(H264EncoderConfig {
            profile: Some(H264Profile::High),
        }),
        318,
        238,
        FrameFormat::Nv12,
        30,
        1,
        RateControlMode::Cbr,
    );
    config.target_kbps = Some(1000);
    config.gop_pic_size = Some(30);
    let (coded_width, coded_height) = coded_size_for(&config);
    assert_ne!(coded_width, config.width as usize);
    assert_ne!(coded_height, config.height as usize);

    roundtrip_colorbar(config, DecoderCodec::H264, 10, 25.0);
}

/// BGRA 入力で crop と coded のサイズが異なるケースの H.264 ラウンドトリップ
#[test]
fn test_roundtrip_h264_bgra_alignment_mismatch() {
    roundtrip_format_with_size(
        CodecConfig::H264(H264EncoderConfig {
            profile: Some(H264Profile::High),
        }),
        DecoderCodec::H264,
        FrameFormat::Bgra,
        10,
        318,
        238,
    );
}
