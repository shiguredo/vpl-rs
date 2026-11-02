#![cfg(target_os = "linux")]

use std::sync::OnceLock;
use std::sync::mpsc;
use std::time::Duration;

use shiguredo_vpl::{
    AdapterSelector, Av1EncoderConfig, Av1Profile, CodecConfig, Decoder, DecoderCodec,
    DecoderConfig, EncodeOptions, EncodedFrame, Encoder, EncoderConfig, Error, FnDecodeHandler,
    FnEncodeHandler, FrameFormat, H264EncoderConfig, H264Profile, HevcEncoderConfig, HevcProfile,
    PictureType, RateControlMode, Vp9EncoderConfig, Vp9Profile, frame_type, list_adapters,
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
    user_data: usize,
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
        "入力サイズ {}x{} が coded サイズ {}x{} を超えている",
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
        "入力サイズ {}x{} が coded サイズ {}x{} を超えている",
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

/// エンコード結果をコールバックで受信するエンコーダと受信用チャネルのペア
type EncoderAndReceiver = (
    Encoder<FnEncodeHandler<usize>>,
    mpsc::Receiver<Result<EncodedFrame<usize>, Error>>,
);

/// エンコード結果をコールバックで受信するエンコーダと受信用チャネルを生成するヘルパー
fn create_encoder(config: EncoderConfig) -> EncoderAndReceiver {
    let (tx, rx) = mpsc::channel::<Result<EncodedFrame<usize>, Error>>();
    let encoder = Encoder::new(
        config,
        FnEncodeHandler::new(move |result| {
            tx.send(result).expect("エンコード結果の送信に失敗した");
        }),
    )
    .expect("エンコーダの生成に失敗した");
    (encoder, rx)
}

/// エンコードしてフレーム一覧とフレームごとのビットストリームを返すヘルパー
fn encode(config: EncoderConfig, frames: &[Vec<u8>]) -> (Vec<EncodedFrame<usize>>, Vec<Vec<u8>>) {
    let (encoder, rx) = create_encoder(config);
    encode_with_encoder(encoder, rx, frames)
}

/// 生成済みエンコーダでフレームをエンコードして結果を回収するヘルパー
fn encode_with_encoder(
    mut encoder: Encoder<FnEncodeHandler<usize>>,
    rx: mpsc::Receiver<Result<EncodedFrame<usize>, Error>>,
    frames: &[Vec<u8>],
) -> (Vec<EncodedFrame<usize>>, Vec<Vec<u8>>) {
    let options = EncodeOptions {
        frame_type: frame_type::UNKNOWN,
    };

    for (index, frame) in frames.iter().enumerate() {
        encoder
            .encode(frame, index, &options)
            .expect("エンコードに失敗した");
    }
    encoder.finish().expect("finish に失敗した");

    let mut encoded_frames = Vec::new();
    let mut bitstreams: Vec<Vec<u8>> = Vec::new();
    for _ in 0..frames.len() {
        let result = rx
            .recv_timeout(Duration::from_secs(10))
            .expect("エンコード結果の受信がタイムアウトした");
        let encoded = result.expect("エンコードに失敗した");
        bitstreams.push(encoded.data().to_vec());
        encoded_frames.push(encoded);
    }

    let mut seen = vec![0u32; frames.len()];
    for frame in &encoded_frames {
        let user_data = *frame.user_data();
        assert!(
            user_data < frames.len(),
            "エンコード結果の user_data {user_data} が範囲外"
        );
        seen[user_data] += 1;
    }
    for (index, count) in seen.iter().enumerate() {
        assert_eq!(
            *count, 1,
            "コールバックの user_data {index} は 1 回だけ呼ばれること (実際は {count} 回)"
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
                    user_data: *frame.user_data(),
                }
            });
            tx.send(info)
                .expect("デコード結果コールバックの送信に失敗した");
        }),
    )
    .expect("デコーダの生成に失敗した");

    for (index, bs) in bitstreams.iter().enumerate() {
        decoder.decode(bs, index).expect("デコードに失敗した");
    }
    decoder.finish().expect("finish に失敗した");

    let mut decoded_frames = Vec::new();
    for _ in 0..num_frames {
        let result = rx
            .recv_timeout(Duration::from_secs(10))
            .expect("デコード結果コールバックの受信がタイムアウトした");
        let info = result.expect("デコードに失敗した");
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
        "エンコードされたフレームが生成されなかった"
    );
    for (i, frame) in encoded_frames.iter().enumerate() {
        assert!(
            !frame.data().is_empty(),
            "エンコード結果フレーム {i} のデータが空"
        );
    }

    let decoded_frames = decode(decoder_codec, &bitstreams);

    assert_eq!(
        decoded_frames.len(),
        num_frames,
        "デコード結果 {} フレーム、期待値 {num_frames} フレーム",
        decoded_frames.len()
    );
    for (i, frame) in decoded_frames.iter().enumerate() {
        assert_eq!(
            frame.width, width,
            "デコード結果フレーム {i} の width が不一致"
        );
        assert_eq!(
            frame.height, height,
            "デコード結果フレーム {i} の height が不一致"
        );
        assert!(
            !frame.y_data.is_empty(),
            "デコード結果フレーム {i} の Y プレーンが空"
        );
    }

    (encoded_frames, decoded_frames)
}

/// 指定エンコーダ設定の coded サイズを取得する
fn coded_size_for(config: &EncoderConfig) -> (usize, usize) {
    let encoder: Encoder<FnEncodeHandler<()>> =
        Encoder::new(config.clone(), FnEncodeHandler::new(|_| {}))
            .expect("エンコーダの生成に失敗した");
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
            "フレーム {i}: PSNR {psnr:.1} dB が {min_psnr_db} dB 未満"
        );
    }
}

/// B フレーム有効時のラウンドトリップで user_data 対応付けを検証するヘルパー
///
/// `gop_ref_dist >= 2`（B フレーム有り）のエンコーダ設定で N フレームをエンコードし、
/// エンコーダ出力の `EncodedFrame::user_data()` をデコーダ入力の user_data に転送してから
/// デコードする。デコード結果のフレーム一覧を返す。
fn roundtrip_b_frames(
    encoder_config: EncoderConfig,
    decoder_codec: DecoderCodec,
    input_frames: &[Vec<u8>],
) -> Vec<DecodedFrameInfo> {
    let num_frames = input_frames.len();
    assert!(
        num_frames >= 15,
        "B フレームテストは N >= 15 で行うこと (実際は {num_frames})"
    );

    let (mut encoder, rx) = create_encoder(encoder_config);
    let options = EncodeOptions {
        frame_type: frame_type::UNKNOWN,
    };
    for (index, frame) in input_frames.iter().enumerate() {
        encoder
            .encode(frame, index, &options)
            .expect("エンコードに失敗した");
    }
    encoder.finish().expect("finish に失敗した");

    // エンコード結果を収集する。実フレームが num_frames 個そろうまでループする。
    // チャネルの送信側はエンコーダ Drop まで生き残るため、while let Ok でチャネル終端を
    // 待つと全メッセージ受信後に必ず 10 秒タイムアウト待ちになる。実フレーム数で束縛する。
    let mut encoded_frames: Vec<EncodedFrame<usize>> = Vec::new();
    while encoded_frames.len() < num_frames {
        let encoded = rx
            .recv_timeout(Duration::from_secs(10))
            .expect("エンコード結果の受信がタイムアウトした")
            .expect("エンコードに失敗した");
        encoded_frames.push(encoded);
    }

    // B ピクチャが実際に生成されていることを確認する
    let b_count = encoded_frames
        .iter()
        .filter(|frame| frame.picture_type() == PictureType::B)
        .count();
    assert!(
        b_count >= 1,
        "B ピクチャが 1 枚以上生成されること (実際は {b_count} 枚)"
    );

    // デコードする。Encoder 出力の user_data を Decoder 入力に転送する。
    let decoder_config = DecoderConfig::new(test_adapter(), decoder_codec);
    let (tx, rx) = mpsc::channel::<Result<DecodedFrameInfo, Error>>();
    let mut decoder = Decoder::new(
        decoder_config,
        FnDecodeHandler::new(move |result| {
            let info = result.map(|frame| {
                let y_data = frame.y().to_vec();
                DecodedFrameInfo {
                    y_data,
                    pitch: frame.pitch(),
                    width: frame.width(),
                    height: frame.height(),
                    user_data: *frame.user_data(),
                }
            });
            tx.send(info)
                .expect("デコード結果コールバックの送信に失敗した");
        }),
    )
    .expect("デコーダの生成に失敗した");

    for frame in &encoded_frames {
        decoder
            .decode(frame.data(), *frame.user_data())
            .expect("デコードに失敗した");
    }
    decoder.finish().expect("finish に失敗した");

    // デコード結果を収集する。実フレームが num_frames 個そろうまでループする。
    let mut decoded_frames = Vec::new();
    while decoded_frames.len() < num_frames {
        let result = rx
            .recv_timeout(Duration::from_secs(10))
            .expect("デコード結果コールバックの受信がタイムアウトした");
        decoded_frames.push(result.expect("デコードに失敗した"));
    }

    decoded_frames
}

/// `user_data == i` の出力フレームが入力フレーム i の内容と一致することを検証する
///
/// 各 `user_data` が集合 `{0..N-1}` と過不足なく一致し、ちょうど 1 回出現することを確認する。
/// さらに、`user_data == i` の出力フレームの Y プレーンが入力フレーム i と一致すること
/// （PSNR が閾値以上）を、誤対応フレーム（入力 j, j ≠ i）との PSNR より十分高いことを
/// もって直接検証する。これにより B フレームの表示順並び替えがあっても user_data が
/// 正しい入力フレームに対応付くことを確認できる。
///
/// `input_frames_for_psnr` は PSNR 計算用に `width` 幅ストライドで生成した入力フレーム
/// （`generate_dummy_nv12(width, height, width, height, i)`）を渡すこと。
fn assert_user_data_matches_input(
    decoded_frames: &[DecodedFrameInfo],
    input_frames_for_psnr: &[Vec<u8>],
    width: usize,
    height: usize,
) {
    // 各 user_data が集合 {0..N-1} と一致し、ちょうど 1 回出現すること
    let mut seen = vec![0u32; input_frames_for_psnr.len()];
    for frame in decoded_frames {
        let user_data = frame.user_data;
        assert!(
            user_data < input_frames_for_psnr.len(),
            "user_data {user_data} が範囲外"
        );
        seen[user_data] += 1;
    }
    for (index, count) in seen.iter().enumerate() {
        assert_eq!(
            *count, 1,
            "user_data {index} は 1 回だけ出現すること (実際は {count} 回)"
        );
    }

    // user_data == i の出力が入力 i と一致すること（対応付けの正しさの直接検証）
    for frame in decoded_frames {
        let i = frame.user_data;
        let psnr_with_i = psnr_y(
            &input_frames_for_psnr[i],
            &frame.y_data,
            frame.pitch,
            width,
            height,
        );
        assert!(
            psnr_with_i >= 25.0,
            "user_data {i}: 入力 {i} との PSNR {psnr_with_i:.1} dB が 25.0 dB 未満"
        );

        // 誤対応フレーム（入力 j, j != i）との PSNR の最大値
        let mut max_wrong_psnr = 0.0_f64;
        for (j, other) in input_frames_for_psnr.iter().enumerate() {
            if j == i {
                continue;
            }
            let psnr = psnr_y(other, &frame.y_data, frame.pitch, width, height);
            max_wrong_psnr = max_wrong_psnr.max(psnr);
        }
        assert!(
            psnr_with_i > max_wrong_psnr + 5.0,
            "user_data {i}: 正しい入力との PSNR {psnr_with_i:.1} dB が誤対応の最大 {max_wrong_psnr:.1} dB より十分高いこと"
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
                .expect("エンコード結果コールバックの送信に失敗した");
        }),
    )
    .expect("エンコーダの生成に失敗した");
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
            .expect("エンコードに失敗した");
    }
    encoder.finish().expect("finish に失敗した");
    for _ in 0..15 {
        let encoded = rx
            .recv_timeout(Duration::from_secs(10))
            .expect("エンコード結果コールバックの受信がタイムアウトした")
            .expect("エンコードに失敗した");
        bitstreams.push(encoded.data().to_vec());
        encoded_frames.push(encoded);
    }

    let idr_count = encoded_frames
        .iter()
        .filter(|f| f.picture_type() == PictureType::Idr)
        .count();
    assert!(
        idr_count >= 2,
        "IDR フレームが 2 枚以上であること (実際は {idr_count} 枚)"
    );

    // デコードで復号できることを確認する
    let decoded_frames = decode(DecoderCodec::H264, &bitstreams);
    assert_eq!(decoded_frames.len(), 15);

    // デコード結果の user_data が集合 {0..14} と過不足なく一致することを検証する。
    // decode() はビットストリームの index を user_data として渡しているため、
    // 各 index がちょうど 1 回出現することを確認する。
    let mut seen = [0u32; 15];
    for frame in &decoded_frames {
        let user_data = frame.user_data;
        assert!(
            user_data < 15,
            "デコード結果の user_data {user_data} が範囲外"
        );
        seen[user_data] += 1;
    }
    for (index, count) in seen.iter().enumerate() {
        assert_eq!(
            *count, 1,
            "デコード結果の user_data {index} は 1 回だけ出現すること (実際は {count} 回)"
        );
    }
}

/// B フレーム（2 枚）有りの H.264 ラウンドトリップで user_data 対応付けを検証する
#[test]
fn test_roundtrip_h264_b_frame_user_data() {
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
    // 2 枚の B フレームを含む GOP 構造にする
    config.gop_ref_dist = Some(3);

    let num_frames = 15;
    let width = config.width as usize;
    let height = config.height as usize;
    let (coded_width, coded_height) = coded_size_for(&config);
    let input_frames: Vec<Vec<u8>> = (0..num_frames)
        .map(|i| generate_dummy_nv12(width, height, coded_width, coded_height, i))
        .collect();
    // PSNR 計算用は width 幅ストライドの入力フレームで生成する
    let input_for_psnr: Vec<Vec<u8>> = (0..num_frames)
        .map(|i| generate_dummy_nv12(width, height, width, height, i))
        .collect();

    let decoded_frames = roundtrip_b_frames(config, DecoderCodec::H264, &input_frames);

    assert_user_data_matches_input(&decoded_frames, &input_for_psnr, width, height);
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
                .expect("エンコード結果コールバックの送信に失敗した");
        }),
    )
    .expect("エンコーダの生成に失敗した");
    let (coded_width, coded_height) = encoder.coded_size();
    let options = EncodeOptions {
        frame_type: frame_type::UNKNOWN,
    };

    for i in 0..8 {
        let frame_data = generate_dummy_nv12(320, 240, coded_width, coded_height, i);
        encoder
            .encode(&frame_data, i, &options)
            .expect("エンコードに失敗した");
    }
    encoder.finish().expect("finish に失敗した");

    let mut seen = [false; 8];
    for _ in 0..8 {
        let encoded = rx
            .recv_timeout(Duration::from_secs(10))
            .expect("エンコード結果コールバックの受信がタイムアウトした")
            .expect("エンコードに失敗した");
        let user_data = *encoded.user_data();
        assert!(user_data < 8, "user_data {user_data} が範囲外");
        seen[user_data] = true;
    }

    for (index, appeared) in seen.iter().enumerate() {
        assert!(
            *appeared,
            "user_data {index} がコールバックに出現しなかった"
        );
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
                .expect("デコード結果コールバックの送信に失敗した");
        }),
    )
    .expect("デコーダの生成に失敗した");

    for (i, bs) in bitstreams.iter().enumerate() {
        decoder.decode(bs, i).expect("デコードに失敗した");
    }
    decoder.finish().expect("finish に失敗した");

    let mut seen = [false; 8];
    for _ in 0..num_frames {
        let user_data = rx
            .recv_timeout(Duration::from_secs(10))
            .expect("デコード結果コールバックの受信がタイムアウトした")
            .expect("デコードに失敗した");
        assert!(user_data < num_frames, "user_data {user_data} が範囲外");
        seen[user_data] = true;
    }

    for (index, appeared) in seen.iter().enumerate() {
        assert!(
            *appeared,
            "user_data {index} がコールバックに出現しなかった"
        );
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
                    .expect("エンコード結果コールバックの送信に失敗した");
            }),
        )
        .expect("エンコーダの生成に失敗した");
        let (coded_width, coded_height) = encoder.coded_size();
        let options = EncodeOptions {
            frame_type: frame_type::UNKNOWN,
        };

        let frame_data = generate_dummy_nv12(320, 240, coded_width, coded_height, 0);
        encoder
            .encode(&frame_data, 0, &options)
            .expect("エンコードに失敗した");
    }

    let result = rx
        .recv_timeout(Duration::from_secs(1))
        .expect("コールバック結果の受信がタイムアウトした");
    let error = match result {
        Ok(_) => panic!("drop 時のコールバックはエラーであること"),
        Err(error) => error,
    };
    assert_eq!(
        error.status_code(),
        Some(shiguredo_vpl::ffi::mfxStatus_MFX_ERR_ABORTED),
        "キャンセルされたコールバックは MFX_ERR_ABORTED を返すこと"
    );
    assert!(
        rx.recv_timeout(Duration::from_millis(100)).is_err(),
        "キャンセル結果の後に予期しない追加コールバックが送信された"
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

/// B フレーム（2 枚）有りの H.265 ラウンドトリップで user_data 対応付けを検証する
#[test]
fn test_roundtrip_hevc_b_frame_user_data() {
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
    // 2 枚の B フレームを含む GOP 構造にする
    config.gop_ref_dist = Some(3);

    let num_frames = 15;
    let width = config.width as usize;
    let height = config.height as usize;
    let (coded_width, coded_height) = coded_size_for(&config);
    let input_frames: Vec<Vec<u8>> = (0..num_frames)
        .map(|i| generate_dummy_nv12(width, height, coded_width, coded_height, i))
        .collect();
    // PSNR 計算用は width 幅ストライドの入力フレームで生成する
    let input_for_psnr: Vec<Vec<u8>> = (0..num_frames)
        .map(|i| generate_dummy_nv12(width, height, width, height, i))
        .collect();

    let decoded_frames = roundtrip_b_frames(config, DecoderCodec::Hevc, &input_frames);

    assert_user_data_matches_input(&decoded_frames, &input_for_psnr, width, height);
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
        "入力サイズ {}x{} が coded サイズ {}x{} を超えている",
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
        "入力サイズ {}x{} が coded サイズ {}x{} を超えている",
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
        .expect("フレームサイズ計算がオーバーフローした");
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

// --- VP9 ---

/// IVF ファイルヘッダー (DKIF, 32 byte) とフレームヘッダー (12 byte) を除去する
///
/// sora-rust-sdk の `vp9_payload_from_vpl` と同じ per-frame 除去ロジックを再現する。
/// ヘッダーが不足する入力は関数内の assert で検出される。
fn strip_vp9_ivf_headers(data: &[u8]) -> Vec<u8> {
    let after_file_header = if data.starts_with(b"DKIF") {
        assert!(
            data.len() >= 32,
            "IVF ファイルヘッダー (32 byte) 未満の入力: {} byte",
            data.len()
        );
        &data[32..]
    } else {
        data
    };
    assert!(
        after_file_header.len() >= 12,
        "IVF フレームヘッダー (12 byte) 未満の入力: {} byte",
        after_file_header.len()
    );
    after_file_header[12..].to_vec()
}

/// VP9 のラウンドトリップテストを実行するヘルパー
///
/// `write_ivf_headers` の値で encode → decode を実行し、getter の戻り値・
/// 出力の IVF ヘッダー有無・デコード後の PSNR を検証する。
/// IVF 付きの場合は sora-rust-sdk の payload 処理と同様にヘッダーを除去してからデコードする。
fn roundtrip_vp9(write_ivf_headers: bool) {
    let mut config = EncoderConfig::new(
        test_adapter(),
        CodecConfig::Vp9(Vp9EncoderConfig {
            profile: Some(Vp9Profile::Profile0),
            write_ivf_headers,
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

    // getter 検証・coded_size 取得・エンコードに同じエンコーダを使い回す
    let (encoder, rx) = create_encoder(config.clone());

    // getter が設定した値 (write_ivf_headers) をそのまま返すことを検証する
    assert_eq!(
        encoder.write_ivf_headers(),
        write_ivf_headers,
        "write_ivf_headers の getter は設定した値を返すこと"
    );

    let width = config.width as usize;
    let height = config.height as usize;
    let (coded_width, coded_height) = encoder.coded_size();
    let colorbar = generate_colorbar_nv12(width, height, coded_width, coded_height);
    let colorbar_for_psnr = generate_colorbar_nv12(width, height, width, height);
    let input_frames: Vec<Vec<u8>> = (0..10).map(|_| colorbar.clone()).collect();

    let (encoded_frames, bitstreams) = encode_with_encoder(encoder, rx, &input_frames);

    // IVF ヘッダーの有無を検証する
    for (i, frame) in encoded_frames.iter().enumerate() {
        if write_ivf_headers {
            if i == 0 {
                // 先頭フレームには 32 byte のファイルヘッダー (DKIF) と 12 byte のフレームヘッダーが付く
                assert!(
                    frame.data().starts_with(b"DKIF"),
                    "IVF 付きの先頭フレームは DKIF で始まること"
                );
                assert!(
                    frame.data().len() >= 44,
                    "先頭フレームは 32 + 12 byte のヘッダー以上が必要 (実際は {} byte)",
                    frame.data().len()
                );
            } else {
                // 2 フレーム目以降は 12 byte のフレームヘッダーのみで、ファイルヘッダー (DKIF) は付かないこと
                assert!(
                    !frame.data().starts_with(b"DKIF"),
                    "IVF 付きのフレーム {i} が DKIF で始まってはいけない"
                );
            }
        } else {
            // raw VP9 なのでどのフレームも DKIF で始まらないこと
            assert!(
                !frame.data().starts_with(b"DKIF"),
                "raw VP9 のフレーム {i} が DKIF で始まってはいけない"
            );
        }
    }

    // デコードする (IVF 付きはヘッダーを除去してから)
    let decoded_frames = if write_ivf_headers {
        let stripped: Vec<Vec<u8>> = bitstreams
            .iter()
            .map(|bs| strip_vp9_ivf_headers(bs))
            .collect();
        decode(DecoderCodec::Vp9, &stripped)
    } else {
        decode(DecoderCodec::Vp9, &bitstreams)
    };
    assert_eq!(
        decoded_frames.len(),
        input_frames.len(),
        "デコードされたフレーム数が入力と一致すること"
    );

    // デコード結果の width/height と PSNR を検証する
    for (i, decoded) in decoded_frames.iter().enumerate() {
        assert_eq!(
            decoded.width, width,
            "デコード結果のフレーム {i} の width が入力と一致すること"
        );
        assert_eq!(
            decoded.height, height,
            "デコード結果のフレーム {i} の height が入力と一致すること"
        );
        let psnr = psnr_y(
            &colorbar_for_psnr,
            &decoded.y_data,
            decoded.pitch,
            width,
            height,
        );
        assert!(
            psnr >= 25.0,
            "フレーム {i}: PSNR {psnr:.1} dB が 25.0 dB 未満"
        );
    }
}

/// VP9 raw (write_ivf_headers=false) のラウンドトリップ
#[test]
fn test_roundtrip_vp9_write_ivf_headers_false() {
    roundtrip_vp9(false);
}

/// VP9 IVF 付き (write_ivf_headers=true) のラウンドトリップ
#[test]
fn test_roundtrip_vp9_write_ivf_headers_true() {
    roundtrip_vp9(true);
}

/// 非 VP9 コーデックの write_ivf_headers getter が常に false を返すことを検証する
///
/// getter の false は `CodecConfig` の単一の非 VP9 分岐で決まるため、代表として H264 のみ検証する。
/// HEVC / AV1 の初期化自体は既存の roundtrip テストで検証済み。
#[test]
fn test_write_ivf_headers_getter_false_for_non_vp9() {
    let config = EncoderConfig::new(
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
    let encoder: Encoder<FnEncodeHandler<()>> = Encoder::new(
        config,
        FnEncodeHandler::new(|_: Result<EncodedFrame<()>, Error>| {}),
    )
    .expect("エンコーダの生成に失敗した");
    assert!(
        !encoder.write_ivf_headers(),
        "非 VP9 コーデックの getter は false を返すこと"
    );
}
