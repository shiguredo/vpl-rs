mod helpers;

use std::sync::mpsc;
use std::time::Duration;

use helpers::test_adapter;
use shiguredo_vpl::{
    Av1EncoderConfig, Av1Profile, CodecConfig, DecodedFrame, Decoder, DecoderConfig, EncodeOptions,
    EncodedFrame, Encoder, EncoderConfig, Error, FnDecodeHandler, FnEncodeHandler, FrameData,
    FrameFormat, H264EncoderConfig, H264Profile, HevcEncoderConfig, HevcProfile, MemoryType,
    PictureType, RateControlMode, ReconfigureParams, StreamInfo, VideoCodecType, VideoProfile,
    Vp9EncoderConfig, Vp9Profile, ffi, frame_type, supported_codecs,
};

/// コールバックから取り出したデコード済みフレーム情報（データをコピーして保持する）
struct DecodedFrameInfo {
    y_data: Vec<u8>,
    uv_data: Vec<u8>,
    pitch: usize,
    width: usize,
    height: usize,
    user_data: usize,
}

/// デコード結果のプレーンサイズが width / height / pitch と整合していることを検証する
///
/// Y プレーンは `pitch * height` バイト、UV プレーンは `pitch * (height / 2)` バイトである。
fn assert_decoded_frame_layout(frame: &DecodedFrameInfo, label: &str) {
    assert!(
        frame.pitch >= frame.width,
        "{label}: pitch {} が width {} より小さい",
        frame.pitch,
        frame.width
    );
    assert_eq!(
        frame.y_data.len(),
        frame.pitch * frame.height,
        "{label}: Y プレーンの長さが pitch * height と一致しない"
    );
    assert_eq!(
        frame.uv_data.len(),
        frame.pitch * (frame.height / 2),
        "{label}: UV プレーンの長さが pitch * (height / 2) と一致しない"
    );
}

/// 表示サイズのバッファからエンコーダー入力を構築する
fn frame_data_from_buffer<'a>(
    format: FrameFormat,
    width: usize,
    height: usize,
    data: &'a [u8],
) -> FrameData<'a> {
    let pitch = format
        .bytes_per_row(width)
        .expect("表示幅の行バイト数計算がオーバーフローした");
    match format {
        FrameFormat::Nv12 | FrameFormat::P010 => {
            let y_size = pitch * height;
            assert!(
                data.len() >= y_size,
                "NV12 / P010 の Y プレーンに必要な長さが足りない"
            );
            let (y, uv) = data.split_at(y_size);
            FrameData::SemiPlanar {
                y,
                y_pitch: pitch,
                uv,
                uv_pitch: pitch,
            }
        }
        FrameFormat::Yuy2 | FrameFormat::Bgra | FrameFormat::Ayuv | FrameFormat::Y410 => {
            FrameData::Packed { data, pitch }
        }
    }
}

/// 表示サイズのフレームのバイト数を返す
fn frame_buffer_size(format: FrameFormat, width: usize, height: usize) -> usize {
    let row_bytes = format
        .bytes_per_row(width)
        .expect("表示幅の行バイト数計算がオーバーフローした");
    let rows = match format {
        FrameFormat::Nv12 | FrameFormat::P010 => height + height / 2,
        FrameFormat::Yuy2 | FrameFormat::Bgra | FrameFormat::Ayuv | FrameFormat::Y410 => height,
    };
    row_bytes * rows
}

/// プレーンを指定したピッチにパディングする
fn pad_plane(data: &[u8], row_bytes: usize, rows: usize, pitch: usize) -> Vec<u8> {
    assert!(
        pitch >= row_bytes,
        "ピッチ {pitch} が 1 行のバイト数 {row_bytes} より小さい"
    );
    assert!(
        data.len() >= row_bytes * rows,
        "プレーンのデータ長が不足している"
    );
    let mut padded = vec![0u8; pitch * rows];
    for row in 0..rows {
        padded[row * pitch..row * pitch + row_bytes]
            .copy_from_slice(&data[row * row_bytes..(row + 1) * row_bytes]);
    }
    padded
}

/// ダミー NV12 フレームを生成する
///
/// Y プレーンはフレーム番号に応じたグラデーション、UV プレーンは 128 固定。
fn generate_dummy_nv12(width: usize, height: usize, frame_index: usize) -> Vec<u8> {
    let y_size = width * height;
    let uv_height = height / 2;
    let uv_size = width * uv_height;
    let mut data = vec![0u8; y_size + uv_size];
    for i in 0..uv_size {
        data[y_size + i] = 128;
    }

    for y in 0..height {
        for x in 0..width {
            data[y * width + x] = ((x + y + frame_index * 7) % 256) as u8;
        }
    }

    data
}

/// SMPTE カラーバー風の 7 色（白/黄/シアン/緑/マゼンタ/赤/青）
const COLOR_BARS_RGB: [(u8, u8, u8); 7] = [
    (235, 235, 235), // 白
    (235, 235, 16),  // 黄
    (16, 235, 235),  // シアン
    (16, 235, 16),   // 緑
    (235, 16, 235),  // マゼンタ
    (235, 16, 16),   // 赤
    (16, 16, 235),   // 青
];

/// BT.601 の RGB -> YCbCr 変換で U / V を返す
///
/// `U` / `V` は 16〜240 の範囲に制限する。
fn rgb_to_uv((r, g, b): (u8, u8, u8)) -> (u8, u8) {
    let (r, g, b) = (r as f64, g as f64, b as f64);
    let u = (-0.148 * r - 0.291 * g + 0.439 * b + 128.0).clamp(16.0, 240.0) as u8;
    let v = (0.439 * r - 0.368 * g - 0.071 * b + 128.0).clamp(16.0, 240.0) as u8;
    (u, v)
}

/// SMPTE カラーバー風の NV12 フレームを生成する
///
/// 7 色の縦ストライプ（白/黄/シアン/緑/マゼンタ/赤/青）を
/// BT.601 で YUV に変換し NV12 形式で返す。
fn generate_colorbar_nv12(width: usize, height: usize) -> Vec<u8> {
    let y_size = width * height;
    let uv_height = height / 2;
    let uv_size = width * uv_height;
    let mut data = vec![0u8; y_size + uv_size];
    let uv_offset = y_size;
    for i in 0..uv_size {
        data[uv_offset + i] = 128;
    }

    for y in 0..height {
        for x in 0..width {
            let bar_index = x * COLOR_BARS_RGB.len() / width;
            let (r, g, b) = COLOR_BARS_RGB[bar_index];

            // BT.601 RGB -> YCbCr
            let rf = r as f64;
            let gf = g as f64;
            let bf = b as f64;
            let yv = (0.257 * rf + 0.504 * gf + 0.098 * bf + 16.0).clamp(16.0, 235.0) as u8;
            data[y * width + x] = yv;

            // UV は 2x2 ブロック単位（左上ピクセルで代表する）
            if y % 2 == 0 && x % 2 == 0 {
                let (u, v) = rgb_to_uv((r, g, b));
                let uv_row = y / 2;
                let uv_col = x; // NV12 はインターリーブなので x そのまま
                data[uv_offset + uv_row * width + uv_col] = u;
                data[uv_offset + uv_row * width + uv_col + 1] = v;
            }
        }
    }

    data
}

/// MSE の合計から PSNR (dB) を計算する
///
/// `peak` はサンプルの最大値 (8bit は 255、10bit は 1023)。
fn psnr_from_mse(mse_sum: f64, pixels: usize, peak: f64) -> f64 {
    let mse = mse_sum / pixels as f64;
    if mse == 0.0 {
        return f64::INFINITY;
    }
    10.0 * (peak * peak / mse).log10()
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
    psnr_from_mse(mse_sum, pixels, 255.0)
}

/// UV プレーン同士の PSNR を計算する（dB）
///
/// NV12 / P010 の UV プレーンはインターリーブされた 1 行 `width` サンプル、
/// `height / 2` 行として比較する。`decoded_uv` はピッチを含む生データ。
fn psnr_uv(
    original_uv: &[u8],
    decoded_uv: &[u8],
    pitch: usize,
    width: usize,
    height: usize,
) -> f64 {
    let rows = height / 2;
    let samples = width * rows;
    let mut mse_sum: f64 = 0.0;
    for row in 0..rows {
        for col in 0..width {
            let orig = original_uv[row * width + col] as f64;
            let dec = decoded_uv[row * pitch + col] as f64;
            let diff = orig - dec;
            mse_sum += diff * diff;
        }
    }
    psnr_from_mse(mse_sum, samples, 255.0)
}

/// 10bit の UV プレーン同士の PSNR を計算する（dB）
///
/// `expected` はインターリーブされた UV の 16bit 値、`decoded_uv` は P010 の
/// UV プレーン（1 サンプル 2 バイト、有効値は上位 10bit）で、ピッチを含む生データ。
fn psnr_uv_10bit(
    expected: &[u16],
    decoded_uv: &[u8],
    pitch: usize,
    width: usize,
    height: usize,
) -> f64 {
    let rows = height / 2;
    let samples = width * rows;
    let mut mse_sum: f64 = 0.0;
    for row in 0..rows {
        for col in 0..width {
            let offset = row * pitch + col * 2;
            let decoded = u16::from_le_bytes([decoded_uv[offset], decoded_uv[offset + 1]]) >> 6;
            let diff = expected[row * width + col] as f64 - decoded as f64;
            mse_sum += diff * diff;
        }
    }
    psnr_from_mse(mse_sum, samples, 1023.0)
}

/// 10bit の Y プレーン同士の PSNR を計算する（dB）
///
/// 値が大きいほど入力と出力が近い。decoded_y は P010 の Y プレーン
/// (1 サンプル 2 バイト、有効値は上位 10bit) で、ピッチを含む生データ。
fn psnr_y_10bit(
    expected: &[u16],
    decoded_y: &[u8],
    pitch: usize,
    width: usize,
    height: usize,
) -> f64 {
    let mut mse_sum: f64 = 0.0;
    let pixels = width * height;
    for row in 0..height {
        for col in 0..width {
            let offset = row * pitch + col * 2;
            let decoded = u16::from_le_bytes([decoded_y[offset], decoded_y[offset + 1]]) >> 6;
            let diff = expected[row * width + col] as f64 - decoded as f64;
            mse_sum += diff * diff;
        }
    }
    psnr_from_mse(mse_sum, pixels, 1023.0)
}

// QPI = 10 の実測では 60 dB 程度。エンコーダーの実装差を考慮して 35 dB を下限にする
/// P010 入力の Y プレーン PSNR の下限 (dB)
const TEN_BIT_LUMA_PSNR_MIN_DB: f64 = 35.0;

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

/// デコード結果をコールバックで受信するデコーダと受信用チャネルのペア
type DecoderAndReceiver = (
    Decoder<FnDecodeHandler<usize>>,
    mpsc::Receiver<Result<usize, Error>>,
);

/// デコード結果をコールバックで受信するデコーダと受信用チャネルを生成するヘルパー
fn create_decoder(codec: VideoCodecType) -> DecoderAndReceiver {
    let (tx, rx) = mpsc::channel::<Result<usize, Error>>();
    let decoder = Decoder::new(
        DecoderConfig::new(test_adapter(), codec),
        FnDecodeHandler::new(move |result| {
            tx.send(result.map(|frame| *frame.user_data()))
                .expect("デコード結果の送信に失敗した");
        }),
    )
    .expect("デコーダーの生成に失敗した");
    (decoder, rx)
}

/// エンコードしてフレーム一覧とフレームごとのビットストリームを返すヘルパー
fn encode(config: EncoderConfig, frames: &[Vec<u8>]) -> (Vec<EncodedFrame<usize>>, Vec<Vec<u8>>) {
    let format = config.frame_format;
    let width = config.width as usize;
    let height = config.height as usize;
    let (encoder, rx) = create_encoder(config);
    encode_with_encoder(encoder, rx, frames, format, width, height)
}

/// 生成済みエンコーダでフレームをエンコードして結果を回収するヘルパー
fn encode_with_encoder(
    mut encoder: Encoder<FnEncodeHandler<usize>>,
    rx: mpsc::Receiver<Result<EncodedFrame<usize>, Error>>,
    frames: &[Vec<u8>],
    format: FrameFormat,
    width: usize,
    height: usize,
) -> (Vec<EncodedFrame<usize>>, Vec<Vec<u8>>) {
    let options = EncodeOptions {
        frame_type: frame_type::UNKNOWN,
    };

    for (index, frame) in frames.iter().enumerate() {
        encoder
            .encode(
                frame_data_from_buffer(format, width, height, frame),
                index,
                &options,
            )
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

/// デコード結果を [DecodedFrameInfo] として受信するデコーダと受信用チャネルのペア
type InfoDecoderAndReceiver = (
    Decoder<FnDecodeHandler<usize>>,
    mpsc::Receiver<Result<DecodedFrameInfo, Error>>,
);

/// デコード結果を [DecodedFrameInfo] として受信するデコーダと受信用チャネルを生成する
fn create_info_decoder(codec: VideoCodecType) -> InfoDecoderAndReceiver {
    let (tx, rx) = mpsc::channel::<Result<DecodedFrameInfo, Error>>();
    let decoder = Decoder::new(
        DecoderConfig::new(test_adapter(), codec),
        FnDecodeHandler::new(move |result| {
            let info = result.map(|frame| {
                let y_data = frame.y().to_vec();
                let uv_data = frame.uv().to_vec();
                DecodedFrameInfo {
                    y_data,
                    uv_data,
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
    (decoder, rx)
}

/// デコード結果を指定件数だけ受信する
fn receive_decoded_frames(
    rx: &mpsc::Receiver<Result<DecodedFrameInfo, Error>>,
    count: usize,
) -> Vec<DecodedFrameInfo> {
    let mut decoded_frames = Vec::new();
    for _ in 0..count {
        let result = rx
            .recv_timeout(Duration::from_secs(10))
            .expect("デコード結果コールバックの受信がタイムアウトした");
        let info = result.expect("デコードに失敗した");
        decoded_frames.push(info);
    }

    for (i, frame) in decoded_frames.iter().enumerate() {
        assert_decoded_frame_layout(frame, &format!("デコード結果フレーム {i}"));
    }

    decoded_frames
}

/// 生成済みデコーダでビットストリームをデコードして結果を回収する
fn decode_with_decoder(
    mut decoder: Decoder<FnDecodeHandler<usize>>,
    rx: mpsc::Receiver<Result<DecodedFrameInfo, Error>>,
    bitstreams: &[Vec<u8>],
) -> Vec<DecodedFrameInfo> {
    for (index, bs) in bitstreams.iter().enumerate() {
        decoder.decode(bs, index).expect("デコードに失敗した");
    }
    decoder.finish().expect("finish に失敗した");

    receive_decoded_frames(&rx, bitstreams.len())
}

/// デコーダーの統計値のスナップショットを返す
fn stats_snapshot(decoder: &Decoder<FnDecodeHandler<usize>>) -> (u64, u64, u64, u64) {
    let stats = decoder.stats();
    (
        stats.total_decode_count.get(),
        stats.total_output_frame_count.get(),
        stats.total_error_count.get(),
        stats.in_flight_frames.get(),
    )
}

/// デコードしてフレーム一覧を返すヘルパー（フレームごとに decode を呼ぶ）
fn decode(decoder_codec: VideoCodecType, bitstreams: &[Vec<u8>]) -> Vec<DecodedFrameInfo> {
    let (decoder, rx) = create_info_decoder(decoder_codec);
    decode_with_decoder(decoder, rx, bitstreams)
}

/// エンコード→デコードのラウンドトリップを検証するヘルパー
fn roundtrip(
    encoder_config: EncoderConfig,
    decoder_codec: VideoCodecType,
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

/// coded サイズが表示サイズ以上で、16 の倍数であることを検証する
///
/// 実機の mfx-gen は 16 の倍数にアラインするため、その前提で検証する。
/// VPL 仕様の最小アラインメントは 8 であり、別の実装では 16 の倍数にならない
/// 可能性がある。
fn assert_coded_size(coded: (usize, usize), width: usize, height: usize) {
    assert!(
        coded.0 >= width,
        "coded 幅 {} が表示幅 {width} より小さい",
        coded.0
    );
    assert!(
        coded.1 >= height,
        "coded 高さ {} が表示高さ {height} より小さい",
        coded.1
    );
    assert_eq!(coded.0 % 16, 0, "coded 幅 {} が 16 の倍数でない", coded.0);
    assert_eq!(coded.1 % 16, 0, "coded 高さ {} が 16 の倍数でない", coded.1);
}

/// [StreamInfo] の coded サイズが表示サイズ以上で、VPL 仕様の最小アラインメント
/// (8 の倍数) を満たすことを検証する
///
/// `mfxFrameInfo.Width` / `Height` の最小アラインメントは 8 と定義されている。
fn assert_stream_info_coded_size(info: &StreamInfo, label: &str) {
    assert!(
        info.coded_width >= info.width,
        "{label}: coded_width {} が width {} より小さい",
        info.coded_width,
        info.width
    );
    assert!(
        info.coded_height >= info.height,
        "{label}: coded_height {} が height {} より小さい",
        info.coded_height,
        info.height
    );
    assert_eq!(
        info.coded_width % 8,
        0,
        "{label}: coded_width {} が 8 の倍数でない",
        info.coded_width
    );
    assert_eq!(
        info.coded_height % 8,
        0,
        "{label}: coded_height {} が 8 の倍数でない",
        info.coded_height
    );
}

/// H.264 CBR のテスト用エンコーダ設定を返す
///
/// 320x240 / 30fps / 1Mbps / GOP 30 の共通設定を使うテストで利用する。
fn h264_test_config() -> EncoderConfig {
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
    config
}

/// ヘッダ解析テスト用のエンコーダ設定を返す
///
/// 指定コーデックを Main プロファイル / CBR / GOP 30 でエンコードする設定である。
/// 先頭フレームがキーフレーム、2 フレーム目が P フレームになる。
fn header_test_config(codec: VideoCodecType, width: u32, height: u32) -> EncoderConfig {
    let codec_config = match codec {
        VideoCodecType::H264 => CodecConfig::H264(H264EncoderConfig {
            profile: Some(H264Profile::Main),
        }),
        VideoCodecType::Hevc => CodecConfig::Hevc(HevcEncoderConfig {
            profile: Some(HevcProfile::Main),
        }),
        VideoCodecType::Av1 => CodecConfig::Av1(Av1EncoderConfig {
            profile: Some(Av1Profile::Main),
        }),
        VideoCodecType::Vp9 => panic!("VP9 はヘッダ解析テストの対象外である"),
    };
    let mut config = EncoderConfig::new(
        test_adapter(),
        codec_config,
        width,
        height,
        FrameFormat::Nv12,
        30,
        1,
        RateControlMode::Cbr,
    );
    config.target_kbps = Some(1000);
    config.gop_pic_size = Some(30);
    config
}

/// エンコード結果がキーフレームであることを検証する
fn assert_keyframe(encoded: &EncodedFrame<usize>, label: &str) {
    assert!(
        matches!(encoded.picture_type(), PictureType::Idr | PictureType::I),
        "{label}: ピクチャータイプがキーフレームでない: {:?}",
        encoded.picture_type()
    );
}

/// 指定コーデック・解像度のキーフレームのビットストリームを生成する
fn encode_header_test_keyframe(codec: VideoCodecType, width: u32, height: u32) -> Vec<u8> {
    let config = header_test_config(codec, width, height);
    let input_frames = [generate_dummy_nv12(width as usize, height as usize, 0)];
    let (encoded_frames, bitstreams) = encode(config, &input_frames);
    assert_keyframe(&encoded_frames[0], &format!("{codec:?} {width}x{height}"));
    bitstreams
        .into_iter()
        .next()
        .expect("キーフレームのビットストリームが無い")
}

/// 指定コーデック・解像度のキーフレームと後続フレームのビットストリームを生成する
///
/// 返る 2 要素は (キーフレーム, 非キーフレーム) である。
fn encode_header_test_stream(codec: VideoCodecType, width: u32, height: u32) -> (Vec<u8>, Vec<u8>) {
    let config = header_test_config(codec, width, height);
    let input_frames: Vec<Vec<u8>> = (0..2)
        .map(|index| generate_dummy_nv12(width as usize, height as usize, index))
        .collect();
    let (encoded_frames, bitstreams) = encode(config, &input_frames);
    assert_keyframe(&encoded_frames[0], &format!("{codec:?} {width}x{height}"));
    assert_eq!(
        encoded_frames[1].picture_type(),
        PictureType::P,
        "{codec:?} {width}x{height}: 2 フレーム目が P フレームでない"
    );

    let mut bitstreams = bitstreams.into_iter();
    let keyframe = bitstreams
        .next()
        .expect("キーフレームのビットストリームが無い");
    let delta = bitstreams
        .next()
        .expect("後続フレームのビットストリームが無い");
    (keyframe, delta)
}

/// カラーバーを使ったラウンドトリップで PSNR を検証するヘルパー
///
/// 同一のカラーバーフレームを num_frames 回エンコードし、デコード後に
/// 元の Y プレーンとの PSNR が min_psnr_db 以上であることを確認する。
fn roundtrip_colorbar(
    encoder_config: EncoderConfig,
    decoder_codec: VideoCodecType,
    num_frames: usize,
    min_psnr_db: f64,
) {
    let width = encoder_config.width as usize;
    let height = encoder_config.height as usize;

    let colorbar = generate_colorbar_nv12(width, height);
    let input_frames: Vec<Vec<u8>> = (0..num_frames).map(|_| colorbar.clone()).collect();

    let (_, decoded_frames) = roundtrip(encoder_config, decoder_codec, &input_frames);

    let y_size = width * height;
    for (i, decoded) in decoded_frames.iter().enumerate() {
        let y_psnr = psnr_y(&colorbar, &decoded.y_data, decoded.pitch, width, height);
        assert!(
            y_psnr >= min_psnr_db,
            "フレーム {i}: Y プレーンの PSNR {y_psnr:.1} dB が {min_psnr_db} dB 未満"
        );
        let uv_psnr = psnr_uv(
            &colorbar[y_size..],
            &decoded.uv_data,
            decoded.pitch,
            width,
            height,
        );
        assert!(
            uv_psnr >= min_psnr_db,
            "フレーム {i}: UV プレーンの PSNR {uv_psnr:.1} dB が {min_psnr_db} dB 未満"
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
    decoder_codec: VideoCodecType,
    input_frames: &[Vec<u8>],
) -> Vec<DecodedFrameInfo> {
    let width = encoder_config.width as usize;
    let height = encoder_config.height as usize;
    let format = encoder_config.frame_format;
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
            .encode(
                frame_data_from_buffer(format, width, height, frame),
                index,
                &options,
            )
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
                let uv_data = frame.uv().to_vec();
                DecodedFrameInfo {
                    y_data,
                    uv_data,
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

    for (i, frame) in decoded_frames.iter().enumerate() {
        assert_eq!(
            frame.width, width,
            "B フレームテストのデコード結果フレーム {i} の width が不一致"
        );
        assert_eq!(
            frame.height, height,
            "B フレームテストのデコード結果フレーム {i} の height が不一致"
        );
        assert_decoded_frame_layout(
            frame,
            &format!("B フレームテストのデコード結果フレーム {i}"),
        );
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
/// `input_frames_for_psnr` は PSNR 計算用に表示サイズの入力フレーム
/// (`generate_dummy_nv12(width, height, i)`) を渡すこと。
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
    roundtrip_colorbar(h264_test_config(), VideoCodecType::H264, 30, 25.0);
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

    roundtrip_colorbar(config, VideoCodecType::H264, 10, 25.0);
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
    let mut encoded_frames = Vec::new();
    let mut bitstreams: Vec<Vec<u8>> = Vec::new();

    for i in 0..15 {
        let frame_data = generate_dummy_nv12(width, height, i);
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
            .encode(
                frame_data_from_buffer(FrameFormat::Nv12, width, height, &frame_data),
                i,
                &options,
            )
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
    let decoded_frames = decode(VideoCodecType::H264, &bitstreams);
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
    let input_frames: Vec<Vec<u8>> = (0..num_frames)
        .map(|i| generate_dummy_nv12(width, height, i))
        .collect();

    let decoded_frames = roundtrip_b_frames(config, VideoCodecType::H264, &input_frames);

    assert_user_data_matches_input(&decoded_frames, &input_frames, width, height);
}

/// encode に渡した user_data が callback で回収できることを確認する
#[test]
fn test_encode_user_data_callback() {
    let (mut encoder, rx) = create_encoder(h264_test_config());
    let options = EncodeOptions {
        frame_type: frame_type::UNKNOWN,
    };

    for i in 0..8 {
        let frame_data = generate_dummy_nv12(320, 240, i);
        encoder
            .encode(
                frame_data_from_buffer(FrameFormat::Nv12, 320, 240, &frame_data),
                i,
                &options,
            )
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
    let config = h264_test_config();
    let width = config.width as usize;
    let height = config.height as usize;
    let num_frames = 8;
    let input_frames: Vec<Vec<u8>> = (0..num_frames)
        .map(|i| generate_dummy_nv12(width, height, i))
        .collect();
    let (_, bitstreams) = encode(config, &input_frames);

    let (mut decoder, rx) = create_decoder(VideoCodecType::H264);

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

/// キーフレームのヘッダから coded / 表示サイズとフレームレートを取得できること
///
/// 256x160 と 320x240 では coded サイズと表示サイズが一致し、318x238 では
/// coded サイズが表示サイズより大きくなる。
#[test]
fn test_decode_header_reports_stream_info() {
    for codec in [
        VideoCodecType::H264,
        VideoCodecType::Hevc,
        VideoCodecType::Av1,
    ] {
        let (decoder, rx) = create_info_decoder(codec);
        for (width, height) in [(256u32, 160u32), (320u32, 240u32), (318u32, 238u32)] {
            let keyframe = encode_header_test_keyframe(codec, width, height);
            let stats_before = stats_snapshot(&decoder);

            // 最初の decode の前 (未初期化の状態) でもヘッダを解析できること
            let info = decoder.decode_header(&keyframe).unwrap_or_else(|error| {
                panic!("{codec:?} {width}x{height}: ヘッダの解析に失敗した: {error}")
            });

            // ヘッダ解析はデコード統計とコールバックに影響しないこと
            assert_eq!(
                stats_snapshot(&decoder),
                stats_before,
                "{codec:?} {width}x{height}: ヘッダ解析でデコード統計が変化した"
            );
            assert!(
                rx.try_recv().is_err(),
                "{codec:?} {width}x{height}: ヘッダ解析でコールバックが発生した"
            );

            // codec は DecoderConfig で指定した値がそのまま返る
            assert_eq!(
                info.codec, codec,
                "{codec:?} {width}x{height}: codec が DecoderConfig の指定と一致しない"
            );
            assert_eq!(
                info.width, width as usize,
                "{codec:?} {width}x{height}: 表示サイズの幅が一致しない"
            );
            assert_eq!(
                info.height, height as usize,
                "{codec:?} {width}x{height}: 表示サイズの高さが一致しない"
            );
            assert_stream_info_coded_size(&info, &format!("{codec:?} {width}x{height}"));

            // 表示サイズが VPL のアラインメント (8 の倍数) に揃っていない場合、
            // coded サイズは表示サイズより大きくなる。coded と表示を取り違えると
            // この assert が失敗する。
            if width % 8 != 0 || height % 8 != 0 {
                assert!(
                    info.coded_width > info.width,
                    "{codec:?} {width}x{height}: coded_width {} が width {} より大きくない",
                    info.coded_width,
                    info.width
                );
                assert!(
                    info.coded_height > info.height,
                    "{codec:?} {width}x{height}: coded_height {} が height {} より大きくない",
                    info.coded_height,
                    info.height
                );
            }

            // H.264 の 318x238 は coded 320x240 になることが既存テストで確認されている
            if codec == VideoCodecType::H264 && (width, height) == (318, 238) {
                assert_eq!(info.coded_width, 320, "H.264: coded_width が 320 でない");
                assert_eq!(info.coded_height, 240, "H.264: coded_height が 240 でない");
            }

            // ビットストリームにフレームレートの情報が無い場合 (AV1) は None になる
            match info.framerate {
                Some(framerate) => {
                    // 約分されているとは限らないため、比がエンコーダー設定の 30fps と等しいことを検証する
                    assert_eq!(
                        u64::from(framerate.num),
                        u64::from(framerate.den) * 30,
                        "{codec:?} {width}x{height}: フレームレートがエンコーダー設定の 30fps と一致しない"
                    );
                }
                None => assert_eq!(
                    codec,
                    VideoCodecType::Av1,
                    "{codec:?} {width}x{height}: フレームレートの情報が無いのは AV1 だけであること"
                ),
            }
        }
    }
}

/// SPS / PPS / sequence header を含まないフレーム単体ではエラーになること
#[test]
fn test_decode_header_rejects_frame_without_sequence_header() {
    for codec in [
        VideoCodecType::H264,
        VideoCodecType::Hevc,
        VideoCodecType::Av1,
    ] {
        let (_keyframe, delta) = encode_header_test_stream(codec, 320, 240);
        let (decoder, _rx) = create_info_decoder(codec);

        let result = decoder.decode_header(&delta);
        let error = result.expect_err(&format!(
            "{codec:?}: パラメータセットを含まないフレームで decode_header が成功した"
        ));
        // 返るエラーコードは VPL 実装依存のため、特定のコードは固定しない
        assert_eq!(
            error.function(),
            "MFXVideoDECODE_DecodeHeader",
            "{codec:?}: エラーの発生元が MFXVideoDECODE_DecodeHeader でない"
        );
    }
}

/// decode_header が入力を消費せず、同じキーフレームを decode に流せること
#[test]
fn test_decode_header_does_not_consume_access_unit() {
    for codec in [
        VideoCodecType::H264,
        VideoCodecType::Hevc,
        VideoCodecType::Av1,
    ] {
        let (keyframe, delta) = encode_header_test_stream(codec, 320, 240);
        let (decoder, rx) = create_info_decoder(codec);
        let info = decoder
            .decode_header(&keyframe)
            .unwrap_or_else(|error| panic!("{codec:?}: ヘッダの解析に失敗した: {error}"));

        // decode_header に渡したキーフレームをそのまま decode に流す
        let decoded_frames = decode_with_decoder(decoder, rx, &[keyframe, delta]);

        // ヘッダ解析に使ったキーフレームが入力 0 としてデコードされていること
        let keyframe_frame = decoded_frames
            .iter()
            .find(|frame| frame.user_data == 0)
            .unwrap_or_else(|| panic!("{codec:?}: キーフレームのデコード結果が無い"));
        assert_eq!(
            keyframe_frame.width, info.width,
            "{codec:?}: デコード結果の幅がヘッダの width と一致しない"
        );
        assert_eq!(
            keyframe_frame.height, info.height,
            "{codec:?}: デコード結果の高さがヘッダの height と一致しない"
        );
        assert!(
            decoded_frames.iter().any(|frame| frame.user_data == 1),
            "{codec:?}: 2 フレーム目のデコード結果が無い"
        );
    }
}

/// デコーダーの初期化後にも別のストリームのヘッダを解析できること
///
/// 320x240 のストリーム A でデコーダーを初期化した後、256x160 のストリーム B の
/// キーフレームから B の解像度を取得できることを検証する。B のアクセスユニットは
/// 旧デコーダーに流さず、A の後続フレームだけでデコードを継続する。
#[test]
fn test_decode_header_after_initialization() {
    for codec in [
        VideoCodecType::H264,
        VideoCodecType::Hevc,
        VideoCodecType::Av1,
    ] {
        let (keyframe_a, delta_a) = encode_header_test_stream(codec, 320, 240);
        let keyframe_b = encode_header_test_keyframe(codec, 256, 160);
        let (mut decoder, rx) = create_info_decoder(codec);

        let info_a = decoder
            .decode_header(&keyframe_a)
            .unwrap_or_else(|error| panic!("{codec:?}: ストリーム A の解析に失敗した: {error}"));
        assert_eq!(info_a.codec, codec, "ストリーム A の codec が一致しない");
        assert_eq!(info_a.width, 320, "ストリーム A の表示幅が一致しない");
        assert_eq!(info_a.height, 240, "ストリーム A の表示高さが一致しない");

        // 最初の decode でデコーダーが初期化される
        decoder.decode(&keyframe_a, 0).unwrap_or_else(|error| {
            panic!("{codec:?}: ストリーム A のデコードに失敗した: {error}")
        });

        // 初期化後でも、別ストリーム B のキーフレームを解析できること
        let info_b = decoder
            .decode_header(&keyframe_b)
            .unwrap_or_else(|error| panic!("{codec:?}: ストリーム B の解析に失敗した: {error}"));
        assert_eq!(info_b.codec, codec, "ストリーム B の codec が一致しない");
        assert_eq!(info_b.width, 256, "ストリーム B の表示幅が一致しない");
        assert_eq!(info_b.height, 160, "ストリーム B の表示高さが一致しない");
        assert_ne!(info_b, info_a, "ストリーム A と B の解析結果が同じ");

        // パラメータセットを含まないフレームでも呼べる。返るエラーコードは
        // VPL 実装依存なので、エラーになってもデコードを継続できることだけを検証する。
        decoder.decode_header(&delta_a).expect_err(&format!(
            "{codec:?}: パラメータセットを含まないフレームで decode_header が成功した"
        ));

        // ストリーム A の後続フレームだけをデコードして完了できること
        decoder.decode(&delta_a, 1).unwrap_or_else(|error| {
            panic!("{codec:?}: ストリーム A の 2 フレーム目のデコードに失敗した: {error}")
        });
        decoder
            .finish()
            .unwrap_or_else(|error| panic!("{codec:?}: finish に失敗した: {error}"));

        let decoded_frames = receive_decoded_frames(&rx, 2);
        for frame in &decoded_frames {
            assert_eq!(
                frame.width, 320,
                "{codec:?}: ストリーム A のデコード結果の幅が一致しない"
            );
            assert_eq!(
                frame.height, 240,
                "{codec:?}: ストリーム A のデコード結果の高さが一致しない"
            );
        }
        assert!(
            rx.try_recv().is_err(),
            "{codec:?}: ヘッダ解析で余分なコールバックが発生した"
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
        let options = EncodeOptions {
            frame_type: frame_type::UNKNOWN,
        };

        let frame_data = generate_dummy_nv12(320, 240, 0);
        encoder
            .encode(
                frame_data_from_buffer(FrameFormat::Nv12, 320, 240, &frame_data),
                0,
                &options,
            )
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

/// reconfigure を挟んだエンコードで、すべてのフレームが Ok で通知されることを確認する
///
/// reconfigure の前後で encode したフレームが、破棄や重複なく 1 回ずつ Ok で
/// 通知されることを検証する。
#[test]
fn test_reconfigure_drains_pending_frames() {
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
    // 入力を複数枚まとめてから出力する GOP 構造にして、
    // reconfigure の時点で pending frame が残る状況を作る。
    config.gop_ref_dist = Some(3);

    let (tx, rx) = mpsc::channel::<Result<EncodedFrame<usize>, Error>>();
    let mut encoder = Encoder::new(
        config,
        FnEncodeHandler::new(move |result| {
            tx.send(result)
                .expect("エンコード結果コールバックの送信に失敗した");
        }),
    )
    .expect("エンコーダの生成に失敗した");
    let options = EncodeOptions {
        frame_type: frame_type::UNKNOWN,
    };
    let before_count = 8;
    let after_count = 8;

    for i in 0..before_count {
        let frame_data = generate_dummy_nv12(320, 240, i);
        encoder
            .encode(
                frame_data_from_buffer(FrameFormat::Nv12, 320, 240, &frame_data),
                i,
                &options,
            )
            .expect("エンコードに失敗した");
    }

    encoder
        .reconfigure(ReconfigureParams {
            target_kbps: Some(1200),
            ..Default::default()
        })
        .expect("reconfigure に失敗した");

    // reconfigure は pending を drain してから Reset するため、戻った時点で
    // in-flight は 0 に戻り、成功回数が計上されている
    assert_eq!(
        encoder.stats().in_flight_frames.get(),
        0,
        "reconfigure の drain 完了後に in-flight が 0 であること"
    );
    assert_eq!(
        encoder.stats().total_reconfigure_count.get(),
        1,
        "reconfigure の成功回数が計上されること"
    );

    for i in before_count..before_count + after_count {
        let frame_data = generate_dummy_nv12(320, 240, i);
        encoder
            .encode(
                frame_data_from_buffer(FrameFormat::Nv12, 320, 240, &frame_data),
                i,
                &options,
            )
            .expect("エンコードに失敗した");
    }

    // finish() は残留した pending をエラーにする。この呼び出しが Ok であることで、
    // reconfigure が pending を残していないことを確認できる。
    encoder.finish().expect("finish に失敗した");
    assert_eq!(
        encoder.stats().in_flight_frames.get(),
        0,
        "finish 完了後に in-flight が 0 であること"
    );

    let mut ok_user_data = Vec::new();
    let mut errors = Vec::new();
    while let Ok(result) = rx.recv_timeout(Duration::from_millis(100)) {
        match result {
            Ok(frame) => ok_user_data.push(*frame.user_data()),
            Err(error) => errors.push(error),
        }
    }

    // reconfigure の前後で、全フレームが Ok で 1 回ずつ通知される
    assert!(
        errors.is_empty(),
        "reconfigure でフレームが破棄された: {}",
        errors
            .iter()
            .map(|error| error.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );
    let mut sorted_ok_user_data = ok_user_data.clone();
    sorted_ok_user_data.sort_unstable();
    let expected: Vec<usize> = (0..before_count + after_count).collect();
    assert_eq!(
        sorted_ok_user_data, expected,
        "全フレームが Ok で 1 回ずつ通知されること"
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

    roundtrip_colorbar(config, VideoCodecType::Hevc, 30, 25.0);
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

    roundtrip_colorbar(config, VideoCodecType::Hevc, 10, 25.0);
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
    let input_frames: Vec<Vec<u8>> = (0..num_frames)
        .map(|i| generate_dummy_nv12(width, height, i))
        .collect();

    let decoded_frames = roundtrip_b_frames(config, VideoCodecType::Hevc, &input_frames);

    assert_user_data_matches_input(&decoded_frames, &input_frames, width, height);
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

    roundtrip_colorbar(config, VideoCodecType::Av1, 30, 25.0);
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

    roundtrip_colorbar(config, VideoCodecType::Av1, 10, 25.0);
}

// ---------------------------------------------------------------------------
// 各フレームフォーマットのダミーデータ生成ヘルパー
//
// 各生成関数は表示サイズのバッファを返す。
// ---------------------------------------------------------------------------

/// ダミー YUY2 フレームを生成する
///
/// Packed YUV 4:2:2: YUYV の繰り返し（2 ピクセルで 4 バイト）。
fn generate_dummy_yuy2(width: usize, height: usize, frame_index: usize) -> Vec<u8> {
    let mut data = vec![0u8; width * height * 2];
    for y in 0..height {
        for x in (0..width).step_by(2) {
            let offset = (y * width + x) * 2;
            let y0 = ((x + y + frame_index * 7) % 256) as u8;
            let y1 = ((x + 1 + y + frame_index * 7) % 256) as u8;
            let u = ((x / 2 + y * 3 + frame_index * 5) % 256) as u8;
            let v = ((x / 2 + y * 5 + 64 + frame_index * 11) % 256) as u8;
            data[offset] = y0; // Y0
            data[offset + 1] = u; // U
            data[offset + 2] = y1; // Y1
            data[offset + 3] = v; // V
        }
    }
    data
}

/// YUY2 の密なフレームから NV12 相当の UV プレーンを構築する
fn yuy2_uv_plane(data: &[u8], width: usize, height: usize) -> Vec<u8> {
    let row_bytes = width * 2;
    let mut uv = vec![0u8; width * (height / 2)];
    for row in 0..height / 2 {
        for pair in 0..width / 2 {
            let src = (row * 2) * row_bytes + pair * 4;
            let dst = row * width + pair * 2;
            uv[dst] = data[src + 1];
            uv[dst + 1] = data[src + 3];
        }
    }
    uv
}

/// P010 の密なフレームから UV プレーンの 10bit 値を取り出す
fn p010_uv_values(data: &[u8], width: usize, height: usize) -> Vec<u16> {
    let row_bytes = width * 2;
    let y_size = row_bytes * height;
    let mut values = Vec::with_capacity(width * (height / 2));
    for row in 0..height / 2 {
        for col in 0..width {
            let offset = y_size + row * row_bytes + col * 2;
            values.push(u16::from_le_bytes([data[offset], data[offset + 1]]) >> 6);
        }
    }
    values
}

/// ダミー P010 フレームを生成する
///
/// 1 サンプル 2 バイトで、10bit の値は 16bit ワードの上位ビットに格納する。
/// Y プレーンはフレーム番号に応じたグラデーション、UV プレーンは 10bit の中間値 (512) 固定。
fn generate_dummy_p010(width: usize, height: usize, frame_index: usize) -> Vec<u8> {
    let row_bytes = width * 2;
    let y_size = row_bytes * height;
    let uv_height = height / 2;
    let mut data = vec![0u8; y_size + row_bytes * uv_height];
    for y in 0..height {
        for x in 0..width {
            let luma = (64 + (x + y + frame_index * 7) % 900) as u16;
            let offset = y * row_bytes + x * 2;
            data[offset..offset + 2].copy_from_slice(&(luma << 6).to_le_bytes());
        }
    }
    // UV は 2 画素につき 1 組をインターリーブする (NV12 と同じ並び)
    for row in 0..uv_height {
        for x in (0..width).step_by(2) {
            let pair = x / 2;
            let u = 64 + ((pair + row * 7) % 900) as u16;
            let v = 64 + ((pair + row * 11 + 300) % 900) as u16;
            let offset = y_size + row * row_bytes + x * 2;
            data[offset..offset + 2].copy_from_slice(&(u << 6).to_le_bytes());
            data[offset + 2..offset + 4].copy_from_slice(&(v << 6).to_le_bytes());
        }
    }
    data
}

/// ダミー AYUV フレームを生成する
///
/// 1 画素を 4 バイト (V, U, Y, A) で格納する。
/// Y はフレーム番号に応じたグラデーション、U/V は中間値、A は最大値。
fn generate_dummy_ayuv(width: usize, height: usize, frame_index: usize) -> Vec<u8> {
    let mut data = vec![0u8; width * height * 4];
    for y in 0..height {
        for x in 0..width {
            let offset = (y * width + x) * 4;
            data[offset] = 128; // V
            data[offset + 1] = 128; // U
            data[offset + 2] = (16 + (x + y + frame_index * 7) % 220) as u8; // Y
            data[offset + 3] = 255; // A
        }
    }
    data
}

/// ダミー Y410 フレームを生成する
///
/// 1 画素を 4 バイトの 32bit ワードで格納する。
/// ワードの下位から U (10bit) / Y (10bit) / V (10bit) / A (2bit) の順に入れる。
/// Y はフレーム番号に応じたグラデーション、U/V は 10bit の中間値 (512)、A は最大値。
fn generate_dummy_y410(width: usize, height: usize, frame_index: usize) -> Vec<u8> {
    let mut data = vec![0u8; width * height * 4];
    for y in 0..height {
        for x in 0..width {
            let offset = (y * width + x) * 4;
            let luma = (64 + (x + y + frame_index * 7) % 900) as u32;
            let word = 512u32 | (luma << 10) | (512u32 << 20) | (3u32 << 30);
            data[offset..offset + 4].copy_from_slice(&word.to_le_bytes());
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
        FrameFormat::P010 => generate_dummy_p010(width, height, frame_index),
        FrameFormat::Ayuv => generate_dummy_ayuv(width, height, frame_index),
        FrameFormat::Y410 => generate_dummy_y410(width, height, frame_index),
    }
}

/// 指定したフォーマット・サイズでエンコード→デコードのラウンドトリップを検証する
///
/// フレーム数とサイズ（レイアウト）だけを検証し、画素値は検証しない。
fn roundtrip_format(
    codec_config: CodecConfig,
    decoder_codec: VideoCodecType,
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

    let coded = coded_size_for(&config);
    assert_coded_size(coded, width as usize, height as usize);

    let input_frames: Vec<Vec<u8>> = (0..num_frames)
        .map(|i| generate_dummy_frame(format, width as usize, height as usize, i))
        .collect();

    // フレームサイズが表示サイズのバイト数と一致することを確認する
    let expected_size = frame_buffer_size(format, width as usize, height as usize);
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

/// エンコーダーが指定の色形式に対応していない場合のスキップ理由を返す
///
/// `supported_codecs()` のエンコード情報から、指定コーデック・プロファイル・
/// システムメモリの組み合わせで色形式が列挙されているかを確認する。
/// 対応している場合は `None` を返す。
fn encoder_format_skip_reason(
    codec: VideoCodecType,
    profile: VideoProfile,
    format: FrameFormat,
) -> Option<String> {
    let codecs = supported_codecs(test_adapter()).expect("supported_codecs に失敗した");
    let Some(info) = codecs.iter().find(|info| info.codec == codec) else {
        return Some(format!("{codec:?} がケーパビリティ照会に含まれていない"));
    };
    let Some(encoding) = &info.encoding else {
        return Some(format!(
            "{codec:?} のエンコード情報がケーパビリティ照会に含まれていない"
        ));
    };
    let Some(profile_info) = encoding.profiles.iter().find(|p| p.profile == profile) else {
        return Some(format!(
            "{codec:?} の {profile:?} がケーパビリティ照会に含まれていない"
        ));
    };
    let supported = profile_info
        .memory_types
        .iter()
        .filter(|m| m.memory_type == MemoryType::System)
        .any(|m| m.color_formats.contains(&format));
    if supported {
        None
    } else {
        Some(format!(
            "{codec:?} の {profile:?} はシステムメモリで {format:?} に対応していない"
        ))
    }
}

/// ケーパビリティ照会で非対応の場合はスキップ理由を出力して true を返す
///
/// 非対応環境でテストを黙って通さないよう、理由を必ず出力する。
fn skip_if_encoder_format_unsupported(
    codec: VideoCodecType,
    profile: VideoProfile,
    format: FrameFormat,
) -> bool {
    match encoder_format_skip_reason(codec, profile, format) {
        Some(reason) => {
            eprintln!("スキップ: {reason}");
            true
        }
        None => false,
    }
}

/// ケーパビリティ照会で非対応の形式は Encoder::new がエラーを返すこと
///
/// 候補の中からケーパビリティ照会で非対応と判定される組み合わせを選び、
/// 初期化が成功してしまわないことを確認する。非対応の組み合わせが 1 つも無い場合は、
/// テスト自体を見直す必要があるため失敗させる。
#[test]
fn test_encoder_new_rejects_unsupported_format() {
    let codec = VideoCodecType::Av1;
    let profile = VideoProfile::Av1(Av1Profile::Main);
    let codec_config = CodecConfig::Av1(Av1EncoderConfig {
        profile: Some(Av1Profile::Main),
    });
    let candidates = [
        FrameFormat::Ayuv,
        FrameFormat::Yuy2,
        FrameFormat::Bgra,
        FrameFormat::Y410,
    ];
    let Some((format, reason)) = candidates.into_iter().find_map(|format| {
        encoder_format_skip_reason(codec, profile, format).map(|reason| (format, reason))
    }) else {
        panic!(
            "ケーパビリティ照会で非対応となる組み合わせが見つからないため、このテストを見直す必要がある"
        );
    };
    eprintln!("非対応の確認: {reason}");

    let config = EncoderConfig::new(
        test_adapter(),
        codec_config,
        320,
        240,
        format,
        30,
        1,
        RateControlMode::Cqp,
    );
    let result = Encoder::new(
        config,
        FnEncodeHandler::new(|_: Result<EncodedFrame<()>, Error>| {}),
    );
    let error = result
        .err()
        .expect("ケーパビリティ照会で非対応の形式なのに Encoder::new が成功した");
    assert_eq!(error.function(), "MFXVideoENCODE_Init");
}

/// 入力フォーマットとプロファイルの組み合わせの可否が Init の結果に従うこと
///
/// `Encoder::new` が Init に成功した場合は実効パラメータが要求した色形式と一致し、
/// 失敗した場合は `MFXVideoENCODE_Init` のエラーを返すことを確認する。
#[test]
fn test_encoder_new_profile_bit_depth_depends_on_init() {
    let cases = [
        (
            CodecConfig::H264(H264EncoderConfig {
                profile: Some(H264Profile::High),
            }),
            FrameFormat::P010,
            "H.264 + P010",
        ),
        (
            CodecConfig::Hevc(HevcEncoderConfig {
                profile: Some(HevcProfile::Main10),
            }),
            FrameFormat::Nv12,
            "HEVC Main10 + NV12",
        ),
        (
            CodecConfig::Hevc(HevcEncoderConfig {
                profile: Some(HevcProfile::Main),
            }),
            FrameFormat::P010,
            "HEVC Main + P010",
        ),
        (
            CodecConfig::Vp9(Vp9EncoderConfig {
                profile: Some(Vp9Profile::Profile0),
                write_ivf_headers: false,
            }),
            FrameFormat::P010,
            "VP9 Profile0 + P010",
        ),
    ];
    for (codec, format, name) in cases {
        let config = EncoderConfig::new(
            test_adapter(),
            codec,
            320,
            240,
            format,
            30,
            1,
            RateControlMode::Cqp,
        );
        match Encoder::new(
            config,
            FnEncodeHandler::new(|_: Result<EncodedFrame<()>, Error>| {}),
        ) {
            Ok(encoder) => {
                let param = encoder.get_video_param().expect("GetVideoParam に失敗した");
                let frame_info = unsafe { param.param.__bindgen_anon_1.mfx.FrameInfo };
                let (fourcc, bit_depth, chroma_format, shift) = expected_frame_info(format);
                assert_eq!(frame_info.FourCC, fourcc, "{name}: FourCC が要求値と違う");
                assert_eq!(
                    frame_info.BitDepthLuma, bit_depth,
                    "{name}: BitDepthLuma が要求値と違う"
                );
                assert_eq!(
                    frame_info.BitDepthChroma, bit_depth,
                    "{name}: BitDepthChroma が要求値と違う"
                );
                assert_eq!(
                    frame_info.ChromaFormat, chroma_format,
                    "{name}: ChromaFormat が要求値と違う"
                );
                assert_eq!(frame_info.Shift, shift, "{name}: Shift が要求値と違う");
            }
            Err(error) => {
                assert_eq!(
                    error.function(),
                    "MFXVideoENCODE_Init",
                    "{name}: MFXVideoENCODE_Init 以外のエラーが返った"
                );
            }
        }
    }
}

/// 指定した色形式の実効パラメータ (FourCC / BitDepthLuma / ChromaFormat / Shift)
fn expected_frame_info(format: FrameFormat) -> (u32, u16, u16, u16) {
    match format {
        FrameFormat::Nv12 => (
            ffi::MFX_FOURCC_NV12,
            8,
            ffi::MFX_CHROMAFORMAT_YUV420 as u16,
            0,
        ),
        FrameFormat::P010 => (
            ffi::MFX_FOURCC_P010,
            10,
            ffi::MFX_CHROMAFORMAT_YUV420 as u16,
            1,
        ),
        FrameFormat::Yuy2 => (
            ffi::MFX_FOURCC_YUY2,
            8,
            ffi::MFX_CHROMAFORMAT_YUV422 as u16,
            0,
        ),
        FrameFormat::Bgra => (
            ffi::MFX_FOURCC_RGB4,
            8,
            ffi::MFX_CHROMAFORMAT_YUV444 as u16,
            0,
        ),
        FrameFormat::Ayuv => (
            ffi::MFX_FOURCC_AYUV,
            8,
            ffi::MFX_CHROMAFORMAT_YUV444 as u16,
            0,
        ),
        FrameFormat::Y410 => (
            ffi::MFX_FOURCC_Y410,
            10,
            ffi::MFX_CHROMAFORMAT_YUV444 as u16,
            0,
        ),
    }
}

/// Init 後の実効パラメータが要求した色形式と一致すること
///
/// FourCC / ビット深度 / ChromaFormat / `mfxFrameInfo.Shift` が Init の前後で変わらないことを確認する。
#[test]
fn test_encoder_keeps_requested_frame_format() {
    let cases = [
        (HevcProfile::Main10, FrameFormat::P010),
        (HevcProfile::Main10, FrameFormat::Y410),
        (HevcProfile::Main, FrameFormat::Ayuv),
        (HevcProfile::Rext, FrameFormat::Ayuv),
        (HevcProfile::Rext, FrameFormat::Y410),
        (HevcProfile::Scc, FrameFormat::Ayuv),
        (HevcProfile::Scc, FrameFormat::Y410),
    ];
    for (profile, format) in cases {
        let video_profile = VideoProfile::Hevc(profile);
        if skip_if_encoder_format_unsupported(VideoCodecType::Hevc, video_profile, format) {
            continue;
        }

        let config = EncoderConfig::new(
            test_adapter(),
            CodecConfig::Hevc(HevcEncoderConfig {
                profile: Some(profile),
            }),
            320,
            240,
            format,
            30,
            1,
            RateControlMode::Cqp,
        );
        let encoder = Encoder::new(
            config,
            FnEncodeHandler::new(|_: Result<EncodedFrame<()>, Error>| {}),
        )
        .unwrap_or_else(|error| {
            panic!("{profile:?} + {format:?} のエンコーダー初期化に失敗した: {error}")
        });

        let param = encoder.get_video_param().expect("GetVideoParam に失敗した");
        let frame_info = unsafe { param.param.__bindgen_anon_1.mfx.FrameInfo };
        let (fourcc, bit_depth, chroma_format, shift) = expected_frame_info(format);
        assert_eq!(
            frame_info.FourCC, fourcc,
            "{profile:?} + {format:?} の FourCC が Init 後に変わった"
        );
        assert_eq!(
            frame_info.BitDepthLuma, bit_depth,
            "{profile:?} + {format:?} の BitDepthLuma が Init 後に変わった"
        );
        assert_eq!(
            frame_info.BitDepthChroma, bit_depth,
            "{profile:?} + {format:?} の BitDepthChroma が Init 後に変わった"
        );
        assert_eq!(
            frame_info.ChromaFormat, chroma_format,
            "{profile:?} + {format:?} の ChromaFormat が Init 後に変わった"
        );
        assert_eq!(
            frame_info.Shift, shift,
            "{profile:?} + {format:?} の Shift が Init 後に変わった"
        );
    }
}

// ---------------------------------------------------------------------------
// フレームフォーマット別ラウンドトリップテスト
// ---------------------------------------------------------------------------

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
    assert_coded_size(
        (coded_width, coded_height),
        config.width as usize,
        config.height as usize,
    );
    assert_ne!(coded_width, config.width as usize);
    assert_ne!(coded_height, config.height as usize);

    roundtrip_colorbar(config, VideoCodecType::H264, 10, 25.0);
}

/// HEVC 入力で crop と coded のサイズが異なるケースのラウンドトリップ
#[test]
fn test_roundtrip_hevc_nv12_alignment_mismatch() {
    let codec = VideoCodecType::Hevc;
    let profile = VideoProfile::Hevc(HevcProfile::Main);
    if skip_if_encoder_format_unsupported(codec, profile, FrameFormat::Nv12) {
        return;
    }
    roundtrip_format(
        CodecConfig::Hevc(HevcEncoderConfig {
            profile: Some(HevcProfile::Main),
        }),
        codec,
        FrameFormat::Nv12,
        5,
        318,
        238,
    );
}

/// AV1 入力で crop と coded のサイズが異なるケースのラウンドトリップ
#[test]
fn test_roundtrip_av1_nv12_alignment_mismatch() {
    let codec = VideoCodecType::Av1;
    let profile = VideoProfile::Av1(Av1Profile::Main);
    if skip_if_encoder_format_unsupported(codec, profile, FrameFormat::Nv12) {
        return;
    }
    roundtrip_format(
        CodecConfig::Av1(Av1EncoderConfig {
            profile: Some(Av1Profile::Main),
        }),
        codec,
        FrameFormat::Nv12,
        5,
        318,
        238,
    );
}

/// VP9 入力で crop と coded のサイズが異なるケースのラウンドトリップ
#[test]
fn test_roundtrip_vp9_nv12_alignment_mismatch() {
    let codec = VideoCodecType::Vp9;
    let profile = VideoProfile::Vp9(Vp9Profile::Profile0);
    if skip_if_encoder_format_unsupported(codec, profile, FrameFormat::Nv12) {
        return;
    }
    roundtrip_format(
        CodecConfig::Vp9(Vp9EncoderConfig {
            profile: Some(Vp9Profile::Profile0),
            write_ivf_headers: false,
        }),
        codec,
        FrameFormat::Nv12,
        5,
        318,
        238,
    );
}

/// P010 入力で crop と coded のサイズが異なるケースのラウンドトリップ
#[test]
fn test_roundtrip_hevc_main10_p010_alignment_mismatch() {
    let codec = VideoCodecType::Hevc;
    let profile = VideoProfile::Hevc(HevcProfile::Main10);
    if skip_if_encoder_format_unsupported(codec, profile, FrameFormat::P010) {
        return;
    }
    roundtrip_format(
        CodecConfig::Hevc(HevcEncoderConfig {
            profile: Some(HevcProfile::Main10),
        }),
        codec,
        FrameFormat::P010,
        5,
        318,
        238,
    );
}

/// AYUV 入力で crop と coded のサイズが異なるケースのラウンドトリップ
#[test]
fn test_roundtrip_h264_ayuv_alignment_mismatch() {
    let codec = VideoCodecType::H264;
    let profile = VideoProfile::H264(H264Profile::High);
    if skip_if_encoder_format_unsupported(codec, profile, FrameFormat::Ayuv) {
        return;
    }
    roundtrip_format(
        CodecConfig::H264(H264EncoderConfig {
            profile: Some(H264Profile::High),
        }),
        codec,
        FrameFormat::Ayuv,
        5,
        318,
        238,
    );
}

/// Y410 入力で crop と coded のサイズが異なるケースのラウンドトリップ
#[test]
fn test_roundtrip_hevc_main10_y410_alignment_mismatch() {
    let codec = VideoCodecType::Hevc;
    let profile = VideoProfile::Hevc(HevcProfile::Main10);
    if skip_if_encoder_format_unsupported(codec, profile, FrameFormat::Y410) {
        return;
    }
    roundtrip_format(
        CodecConfig::Hevc(HevcEncoderConfig {
            profile: Some(HevcProfile::Main10),
        }),
        codec,
        FrameFormat::Y410,
        5,
        318,
        238,
    );
}

/// BGRA 入力で crop と coded のサイズが異なるケースの H.264 ラウンドトリップ
#[test]
fn test_roundtrip_h264_bgra_alignment_mismatch() {
    roundtrip_format(
        CodecConfig::H264(H264EncoderConfig {
            profile: Some(H264Profile::High),
        }),
        VideoCodecType::H264,
        FrameFormat::Bgra,
        10,
        318,
        238,
    );
}

/// YUY2 入力で crop と coded のサイズが異なるケースの H.264 ラウンドトリップ
#[test]
fn test_roundtrip_h264_yuy2_alignment_mismatch() {
    roundtrip_format(
        CodecConfig::H264(H264EncoderConfig {
            profile: Some(H264Profile::High),
        }),
        VideoCodecType::H264,
        FrameFormat::Yuy2,
        10,
        318,
        238,
    );
}

/// 1920x1080 (coded 1920x1088) の表示サイズ入力で H.264 ラウンドトリップできること
///
/// coded サイズとの差が高さだけにある一般的な解像度でも、表示サイズの
/// バッファを受け付けることを確認する。
#[test]
fn test_roundtrip_h264_nv12_1920x1080() {
    let mut config = EncoderConfig::new(
        test_adapter(),
        CodecConfig::H264(H264EncoderConfig {
            profile: Some(H264Profile::High),
        }),
        1920,
        1080,
        FrameFormat::Nv12,
        30,
        1,
        RateControlMode::Cbr,
    );
    config.target_kbps = Some(5000);
    config.gop_pic_size = Some(30);

    let (coded_width, coded_height) = coded_size_for(&config);
    assert_coded_size(
        (coded_width, coded_height),
        config.width as usize,
        config.height as usize,
    );
    assert_eq!(coded_width, config.width as usize);
    assert_ne!(coded_height, config.height as usize);

    roundtrip_colorbar(config, VideoCodecType::H264, 2, 25.0);
}

/// NV12 / P010 の奇数サイズはエンコーダーが拒否すること
#[test]
fn test_encoder_rejects_odd_nv12_and_p010_dimensions() {
    for format in [FrameFormat::Nv12, FrameFormat::P010] {
        for (width, height, expected) in [(319, 238, "even width"), (318, 239, "even height")] {
            let config = EncoderConfig::new(
                test_adapter(),
                CodecConfig::H264(H264EncoderConfig {
                    profile: Some(H264Profile::High),
                }),
                width,
                height,
                format,
                30,
                1,
                RateControlMode::Cbr,
            );
            let error = Encoder::new(
                config,
                FnEncodeHandler::new(|_: Result<EncodedFrame<()>, Error>| {}),
            )
            .err()
            .expect("奇数サイズは拒否されること");
            assert_eq!(error.function(), "Encoder::new");
            assert!(
                error.to_string().contains(expected),
                "エラーメッセージに {expected} が含まれること: {error}"
            );
        }
    }
}

/// YUY2 の奇数幅はエンコーダーが拒否すること
#[test]
fn test_encoder_rejects_odd_yuy2_width() {
    let config = EncoderConfig::new(
        test_adapter(),
        CodecConfig::H264(H264EncoderConfig {
            profile: Some(H264Profile::High),
        }),
        319,
        240,
        FrameFormat::Yuy2,
        30,
        1,
        RateControlMode::Cbr,
    );
    let error = Encoder::new(
        config,
        FnEncodeHandler::new(|_: Result<EncodedFrame<()>, Error>| {}),
    )
    .err()
    .expect("奇数幅の YUY2 は拒否されること");
    assert_eq!(error.function(), "Encoder::new");
    assert!(
        error.to_string().contains("even width"),
        "エラーメッセージに even width が含まれること: {error}"
    );
}

// ---------------------------------------------------------------------------
// エンコーダーの入力検証
// ---------------------------------------------------------------------------

/// `Encoder::get_video_param` が Init 後の実効フレーム情報を返すこと
#[test]
fn test_encoder_get_video_param_reports_effective_frame_info() {
    let width = 318usize;
    let height = 238usize;
    let mut config = EncoderConfig::new(
        test_adapter(),
        CodecConfig::H264(H264EncoderConfig {
            profile: Some(H264Profile::High),
        }),
        width as u32,
        height as u32,
        FrameFormat::Nv12,
        30,
        1,
        RateControlMode::Cbr,
    );
    config.target_kbps = Some(1000);
    config.gop_pic_size = Some(30);

    let (encoder, _rx) = create_encoder(config);
    let param = encoder.get_video_param().expect("GetVideoParam に失敗した");
    let frame_info = unsafe { param.param.__bindgen_anon_1.mfx.FrameInfo };
    let crop = unsafe { frame_info.__bindgen_anon_1.__bindgen_anon_1 };
    assert_eq!(crop.CropX, 0, "CropX が 0 であること");
    assert_eq!(crop.CropY, 0, "CropY が 0 であること");
    assert_eq!(crop.CropW, width as u16, "CropW が表示幅と一致すること");
    assert_eq!(crop.CropH, height as u16, "CropH が表示高さと一致すること");
    assert_eq!(crop.Width, 320, "Width が coded 幅と一致すること");
    assert_eq!(crop.Height, 240, "Height が coded 高さと一致すること");
    assert_eq!(
        frame_info.FourCC,
        ffi::MFX_FOURCC_NV12,
        "FourCC が NV12 であること"
    );
    assert_eq!(
        frame_info.ChromaFormat,
        ffi::MFX_CHROMAFORMAT_YUV420 as u16,
        "ChromaFormat が 4:2:0 であること"
    );
    assert_eq!(frame_info.BitDepthLuma, 8, "BitDepthLuma が 8 であること");
    assert_eq!(
        frame_info.BitDepthChroma, 8,
        "BitDepthChroma が 8 であること"
    );
    assert_eq!(frame_info.Shift, 0, "Shift が 0 であること");
}

/// 表示サイズのフレームデータを受け付け、不正な入力を拒否すること
#[test]
fn test_encode_accepts_frame_data_and_rejects_invalid() {
    let width = 318usize;
    let height = 238usize;
    let mut config = EncoderConfig::new(
        test_adapter(),
        CodecConfig::H264(H264EncoderConfig {
            profile: Some(H264Profile::High),
        }),
        width as u32,
        height as u32,
        FrameFormat::Nv12,
        30,
        1,
        RateControlMode::Cbr,
    );
    config.target_kbps = Some(1000);
    config.gop_pic_size = Some(30);

    let (mut encoder, rx) = create_encoder(config);
    let (coded_width, coded_height) = encoder.coded_size();
    assert_coded_size((coded_width, coded_height), width, height);
    assert_ne!(coded_width, width, "coded 幅が表示幅と異なること");
    assert_ne!(coded_height, height, "coded 高さが表示高さと異なること");

    let frame_data = generate_dummy_nv12(width, height, 0);
    let expected = frame_buffer_size(FrameFormat::Nv12, width, height);
    let coded_expected = frame_buffer_size(FrameFormat::Nv12, coded_width, coded_height);
    assert_eq!(
        frame_data.len(),
        expected,
        "入力フレームは表示サイズのバイト数であること"
    );
    assert!(
        expected < coded_expected,
        "表示サイズのバッファが coded サイズのバッファより小さいこと"
    );

    let row_bytes = FrameFormat::Nv12
        .bytes_per_row(width)
        .expect("表示幅の行バイト数計算がオーバーフローした");
    let FrameData::SemiPlanar {
        y,
        y_pitch,
        uv,
        uv_pitch,
    } = frame_data_from_buffer(FrameFormat::Nv12, width, height, &frame_data)
    else {
        panic!("NV12 の入力は SemiPlanar であること");
    };
    let options = EncodeOptions {
        frame_type: frame_type::UNKNOWN,
    };

    // pitch が 1 行のバイト数未満の入力は拒否される
    let error = encoder
        .encode(
            FrameData::SemiPlanar {
                y,
                y_pitch: row_bytes - 1,
                uv,
                uv_pitch,
            },
            0,
            &options,
        )
        .expect_err("pitch 不足の入力は拒否されること");
    assert!(
        error.to_string().contains("Y plane pitch"),
        "pitch 不足のエラーメッセージが返ること: {error}"
    );

    // データ長が 1 バイト足りない入力は拒否される
    let error = encoder
        .encode(
            FrameData::SemiPlanar {
                y: &y[..y.len() - 1],
                y_pitch,
                uv,
                uv_pitch,
            },
            0,
            &options,
        )
        .expect_err("データ長不足の入力は拒否されること");
    assert!(
        error.to_string().contains("Y plane data is too small"),
        "データ長不足のエラーメッセージが返ること: {error}"
    );

    // UV の pitch が 1 行のバイト数未満の入力は拒否される
    let error = encoder
        .encode(
            FrameData::SemiPlanar {
                y,
                y_pitch,
                uv,
                uv_pitch: row_bytes - 1,
            },
            0,
            &options,
        )
        .expect_err("UV の pitch 不足の入力は拒否されること");
    assert!(
        error.to_string().contains("UV plane pitch"),
        "UV の pitch 不足のエラーメッセージが返ること: {error}"
    );

    // UV のデータ長が 1 バイト足りない入力は拒否される
    let error = encoder
        .encode(
            FrameData::SemiPlanar {
                y,
                y_pitch,
                uv: &uv[..uv.len() - 1],
                uv_pitch,
            },
            0,
            &options,
        )
        .expect_err("UV のデータ長不足の入力は拒否されること");
    assert!(
        error.to_string().contains("UV plane data is too small"),
        "UV のデータ長不足のエラーメッセージが返ること: {error}"
    );

    // ピッチが巨大でプレーンの必要サイズがオーバーフローする入力は拒否される
    let error = encoder
        .encode(
            FrameData::SemiPlanar {
                y,
                y_pitch: usize::MAX,
                uv,
                uv_pitch,
            },
            0,
            &options,
        )
        .expect_err("必要サイズがオーバーフローする入力は拒否されること");
    assert!(
        error
            .to_string()
            .contains("Y plane size calculation overflowed"),
        "必要サイズのオーバーフローのエラーメッセージが返ること: {error}"
    );

    // 形式と variant が一致しない入力は拒否される
    let error = encoder
        .encode(
            FrameData::Packed {
                data: y,
                pitch: y_pitch,
            },
            0,
            &options,
        )
        .expect_err("形式と variant が一致しない入力は拒否されること");
    assert!(
        error.to_string().contains("does not match"),
        "variant 不一致のエラーメッセージが返ること: {error}"
    );

    // 正しい表示サイズの入力は受理される
    encoder
        .encode(
            FrameData::SemiPlanar {
                y,
                y_pitch,
                uv,
                uv_pitch,
            },
            1,
            &options,
        )
        .expect("表示サイズのバッファは受理されること");
    encoder.finish().expect("finish に失敗した");
    let encoded = rx
        .recv_timeout(Duration::from_secs(10))
        .expect("エンコード結果コールバックの受信がタイムアウトした")
        .expect("エンコードに失敗した");
    assert_eq!(
        *encoded.user_data(),
        1,
        "受理したフレームの user_data が返ること"
    );
}

/// パック形式の入力でピッチが 1 行のバイト数未満なら拒否すること
#[test]
fn test_encode_rejects_packed_pitch_short() {
    let width = 318usize;
    let height = 240usize;
    let config = EncoderConfig::new(
        test_adapter(),
        CodecConfig::H264(H264EncoderConfig {
            profile: Some(H264Profile::High),
        }),
        width as u32,
        height as u32,
        FrameFormat::Yuy2,
        30,
        1,
        RateControlMode::Cbr,
    );
    let (mut encoder, _rx) = create_encoder(config);
    let frame = generate_dummy_yuy2(width, height, 0);
    let row_bytes = FrameFormat::Yuy2
        .bytes_per_row(width)
        .expect("表示幅の行バイト数計算がオーバーフローした");
    let options = EncodeOptions {
        frame_type: frame_type::UNKNOWN,
    };
    let error = encoder
        .encode(
            FrameData::Packed {
                data: &frame,
                pitch: row_bytes - 1,
            },
            0,
            &options,
        )
        .expect_err("パック形式の pitch 不足の入力は拒否されること");
    assert!(
        error.to_string().contains("packed frame plane pitch"),
        "パック形式の pitch 不足のエラーメッセージが返ること: {error}"
    );
}

/// NV12 の入力でピッチが表示幅より大きいバッファをエンコードできること
#[test]
fn test_encode_accepts_strided_nv12() {
    let width = 318usize;
    let height = 238usize;
    let y_pitch = 384usize;
    let uv_pitch = 400usize;
    let mut config = EncoderConfig::new(
        test_adapter(),
        CodecConfig::H264(H264EncoderConfig {
            profile: Some(H264Profile::High),
        }),
        width as u32,
        height as u32,
        FrameFormat::Nv12,
        30,
        1,
        RateControlMode::Cbr,
    );
    config.target_kbps = Some(1000);
    config.gop_pic_size = Some(30);

    let colorbar = generate_colorbar_nv12(width, height);
    let row_bytes = FrameFormat::Nv12
        .bytes_per_row(width)
        .expect("表示幅の行バイト数計算がオーバーフローした");
    let y_size = row_bytes * height;
    let y = pad_plane(&colorbar[..y_size], row_bytes, height, y_pitch);
    let uv = pad_plane(&colorbar[y_size..], row_bytes, height / 2, uv_pitch);

    let (mut encoder, rx) = create_encoder(config);
    let options = EncodeOptions {
        frame_type: frame_type::UNKNOWN,
    };
    encoder
        .encode(
            FrameData::SemiPlanar {
                y: &y,
                y_pitch,
                uv: &uv,
                uv_pitch,
            },
            0,
            &options,
        )
        .expect("エンコードに失敗した");
    encoder.finish().expect("finish に失敗した");

    let encoded = rx
        .recv_timeout(Duration::from_secs(10))
        .expect("エンコード結果コールバックの受信がタイムアウトした")
        .expect("エンコードに失敗した");
    let decoded = decode(VideoCodecType::H264, &[encoded.data().to_vec()]);
    assert_eq!(decoded.len(), 1, "デコード結果が 1 フレームであること");
    let frame = &decoded[0];
    assert_eq!(
        frame.width, width,
        "デコード結果の width が表示幅と一致すること"
    );
    assert_eq!(
        frame.height, height,
        "デコード結果の height が表示高さと一致すること"
    );
    let y_psnr = psnr_y(&colorbar, &frame.y_data, frame.pitch, width, height);
    assert!(
        y_psnr >= 25.0,
        "ピッチ付き NV12 入力の Y プレーン PSNR が低すぎる: {y_psnr:.1} dB"
    );
    let uv_psnr = psnr_uv(
        &colorbar[y_size..],
        &frame.uv_data,
        frame.pitch,
        width,
        height,
    );
    assert!(
        uv_psnr >= 25.0,
        "ピッチ付き NV12 入力の UV プレーン PSNR が低すぎる: {uv_psnr:.1} dB"
    );
}

/// YUY2 の入力でピッチが表示幅より大きいバッファをエンコードできること
#[test]
fn test_encode_accepts_strided_yuy2() {
    let width = 318usize;
    let height = 238usize;
    let pitch = 640usize;
    let mut config = EncoderConfig::new(
        test_adapter(),
        CodecConfig::H264(H264EncoderConfig {
            profile: Some(H264Profile::High),
        }),
        width as u32,
        height as u32,
        FrameFormat::Yuy2,
        30,
        1,
        RateControlMode::Cqp,
    );
    config.qpi = Some(10);
    config.qpp = Some(10);
    config.qpb = Some(10);
    config.gop_pic_size = Some(30);

    let data = generate_dummy_yuy2(width, height, 0);
    let row_bytes = FrameFormat::Yuy2
        .bytes_per_row(width)
        .expect("表示幅の行バイト数計算がオーバーフローした");
    let padded = pad_plane(&data, row_bytes, height, pitch);

    let (mut encoder, rx) = create_encoder(config);
    let options = EncodeOptions {
        frame_type: frame_type::UNKNOWN,
    };
    encoder
        .encode(
            FrameData::Packed {
                data: &padded,
                pitch,
            },
            0,
            &options,
        )
        .expect("エンコードに失敗した");
    encoder.finish().expect("finish に失敗した");

    let encoded = rx
        .recv_timeout(Duration::from_secs(10))
        .expect("エンコード結果コールバックの受信がタイムアウトした")
        .expect("エンコードに失敗した");
    let decoded = decode(VideoCodecType::H264, &[encoded.data().to_vec()]);
    assert_eq!(decoded.len(), 1, "デコード結果が 1 フレームであること");
    let frame = &decoded[0];
    assert_eq!(
        frame.width, width,
        "デコード結果の width が表示幅と一致すること"
    );
    assert_eq!(
        frame.height, height,
        "デコード結果の height が表示高さと一致すること"
    );

    let expected_y: Vec<u8> = (0..height)
        .flat_map(|y| (0..width).map(move |x| ((x + y) % 256) as u8))
        .collect();
    let y_psnr = psnr_y(&expected_y, &frame.y_data, frame.pitch, width, height);
    assert!(
        y_psnr >= 25.0,
        "ピッチ付き YUY2 入力の Y プレーン PSNR が低すぎる: {y_psnr:.1} dB"
    );
    let expected_uv = yuy2_uv_plane(&data, width, height);
    let uv_psnr = psnr_uv(&expected_uv, &frame.uv_data, frame.pitch, width, height);
    assert!(
        uv_psnr >= 20.0,
        "ピッチ付き YUY2 入力の UV プレーン PSNR が低すぎる: {uv_psnr:.1} dB"
    );
}

/// デコード結果の Y/UV プレーンをそのまま `FrameData` に渡して再エンコードできること
#[test]
fn test_encode_decoded_frame_planes_directly() {
    let width = 318usize;
    let height = 238usize;
    let mut config = EncoderConfig::new(
        test_adapter(),
        CodecConfig::H264(H264EncoderConfig {
            profile: Some(H264Profile::High),
        }),
        width as u32,
        height as u32,
        FrameFormat::Nv12,
        30,
        1,
        RateControlMode::Cbr,
    );
    config.target_kbps = Some(1000);
    config.gop_pic_size = Some(30);

    let colorbar = generate_colorbar_nv12(width, height);
    let (_, decoded_frames) = roundtrip(config.clone(), VideoCodecType::H264, &[colorbar]);
    let decoded = &decoded_frames[0];

    let (mut encoder, rx) = create_encoder(config);
    let options = EncodeOptions {
        frame_type: frame_type::UNKNOWN,
    };
    encoder
        .encode(
            FrameData::SemiPlanar {
                y: &decoded.y_data,
                y_pitch: decoded.pitch,
                uv: &decoded.uv_data,
                uv_pitch: decoded.pitch,
            },
            0,
            &options,
        )
        .expect("デコード結果のプレーンを直接エンコードできない");
    encoder.finish().expect("finish に失敗した");

    let encoded = rx
        .recv_timeout(Duration::from_secs(10))
        .expect("エンコード結果コールバックの受信がタイムアウトした")
        .expect("エンコードに失敗した");
    let redecode = decode(VideoCodecType::H264, &[encoded.data().to_vec()]);
    assert_eq!(
        redecode.len(),
        1,
        "再エンコード後のデコード結果が 1 フレームであること"
    );
    assert_eq!(redecode[0].width, width, "再エンコード後の width が不一致");
    assert_eq!(
        redecode[0].height, height,
        "再エンコード後の height が不一致"
    );
}

/// P010 の入力で Y/UV のピッチが表示サイズより大きいバッファをエンコードできること
#[test]
fn test_encode_accepts_strided_p010() {
    let width = 318usize;
    let height = 238usize;
    let y_pitch = 640usize;
    let uv_pitch = 672usize;
    if skip_if_encoder_format_unsupported(
        VideoCodecType::Hevc,
        VideoProfile::Hevc(HevcProfile::Main10),
        FrameFormat::P010,
    ) {
        return;
    }

    let mut config = EncoderConfig::new(
        test_adapter(),
        CodecConfig::Hevc(HevcEncoderConfig {
            profile: Some(HevcProfile::Main10),
        }),
        width as u32,
        height as u32,
        FrameFormat::P010,
        30,
        1,
        RateControlMode::Cqp,
    );
    config.qpi = Some(10);
    config.gop_pic_size = Some(30);

    let p010 = generate_dummy_p010(width, height, 0);
    let row_bytes = FrameFormat::P010
        .bytes_per_row(width)
        .expect("表示幅の行バイト数計算がオーバーフローした");
    let y_size = row_bytes * height;
    let y = pad_plane(&p010[..y_size], row_bytes, height, y_pitch);
    let uv = pad_plane(&p010[y_size..], row_bytes, height / 2, uv_pitch);

    let (mut encoder, rx) = create_encoder(config);
    let options = EncodeOptions {
        frame_type: frame_type::UNKNOWN,
    };
    encoder
        .encode(
            FrameData::SemiPlanar {
                y: &y,
                y_pitch,
                uv: &uv,
                uv_pitch,
            },
            0,
            &options,
        )
        .expect("エンコードに失敗した");
    encoder.finish().expect("finish に失敗した");

    let encoded = rx
        .recv_timeout(Duration::from_secs(10))
        .expect("エンコード結果コールバックの受信がタイムアウトした")
        .expect("エンコードに失敗した");
    let decoded = decode(VideoCodecType::Hevc, &[encoded.data().to_vec()]);
    assert_eq!(decoded.len(), 1, "デコード結果が 1 フレームであること");
    let frame = &decoded[0];
    assert_eq!(
        frame.width, width,
        "デコード結果の width が表示幅と一致すること"
    );
    assert_eq!(
        frame.height, height,
        "デコード結果の height が表示高さと一致すること"
    );

    let expected_y: Vec<u16> = (0..height)
        .flat_map(|y| (0..width).map(move |x| (64 + (x + y) % 900) as u16))
        .collect();
    let y_psnr = psnr_y_10bit(&expected_y, &frame.y_data, frame.pitch, width, height);
    assert!(
        y_psnr >= TEN_BIT_LUMA_PSNR_MIN_DB,
        "ピッチ付き P010 入力の Y プレーン PSNR が低すぎる: {y_psnr:.1} dB"
    );

    let expected_uv = p010_uv_values(&p010, width, height);
    let uv_psnr = psnr_uv_10bit(&expected_uv, &frame.uv_data, frame.pitch, width, height);
    assert!(
        uv_psnr >= 30.0,
        "ピッチ付き P010 入力の UV プレーン PSNR が低すぎる: {uv_psnr:.1} dB"
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
/// `reconfigure` が true の場合はエンコード前に reconfigure を呼び出す。
/// IVF 付きの場合は sora-rust-sdk の payload 処理と同様にヘッダーを除去してからデコードする。
fn roundtrip_vp9(write_ivf_headers: bool, reconfigure: bool) {
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

    // getter 検証とエンコードに同じエンコーダを使い回す
    let (mut encoder, rx) = create_encoder(config.clone());

    if reconfigure {
        encoder
            .reconfigure(ReconfigureParams {
                target_kbps: Some(800),
                ..Default::default()
            })
            .expect("reconfigure に失敗した");
    }

    // getter が設定した値 (write_ivf_headers) をそのまま返すことを検証する
    assert_eq!(
        encoder.write_ivf_headers(),
        write_ivf_headers,
        "write_ivf_headers の getter は設定した値を返すこと"
    );

    let width = config.width as usize;
    let height = config.height as usize;
    let colorbar = generate_colorbar_nv12(width, height);
    let input_frames: Vec<Vec<u8>> = (0..10).map(|_| colorbar.clone()).collect();

    let (encoded_frames, bitstreams) = encode_with_encoder(
        encoder,
        rx,
        &input_frames,
        config.frame_format,
        width,
        height,
    );

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
        decode(VideoCodecType::Vp9, &stripped)
    } else {
        decode(VideoCodecType::Vp9, &bitstreams)
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
        let psnr = psnr_y(&colorbar, &decoded.y_data, decoded.pitch, width, height);
        assert!(
            psnr >= 25.0,
            "フレーム {i}: PSNR {psnr:.1} dB が 25.0 dB 未満"
        );
    }
}

/// VP9 raw (write_ivf_headers=false) のラウンドトリップ
#[test]
fn test_roundtrip_vp9_write_ivf_headers_false() {
    roundtrip_vp9(false, false);
}

/// VP9 IVF 付き (write_ivf_headers=true) のラウンドトリップ
#[test]
fn test_roundtrip_vp9_write_ivf_headers_true() {
    roundtrip_vp9(true, false);
}

/// reconfigure 後も VP9 raw (write_ivf_headers=false) が維持されることを検証する
#[test]
fn test_reconfigure_vp9_write_ivf_headers_false() {
    roundtrip_vp9(false, true);
}

/// reconfigure 後も VP9 IVF 付き (write_ivf_headers=true) が維持されることを検証する
#[test]
fn test_reconfigure_vp9_write_ivf_headers_true() {
    roundtrip_vp9(true, true);
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

// --- シーケンスパラメータ ---

/// シーケンスパラメータ取得用のエンコーダ設定を返す
///
/// パラメータセットの内容だけを検証するため、小さい解像度と CQP を使う。
fn sequence_params_config(codec: CodecConfig) -> EncoderConfig {
    let mut config = EncoderConfig::new(
        test_adapter(),
        codec,
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
    config
}

/// シーケンスパラメータ取得用のエンコーダを生成する
fn create_sequence_params_encoder(config: EncoderConfig) -> Encoder<FnEncodeHandler<()>> {
    Encoder::new(
        config,
        FnEncodeHandler::new(|_: Result<EncodedFrame<()>, Error>| {}),
    )
    .expect("エンコーダーの生成に失敗した")
}

/// バイト列の先頭にスタートコードが含まれないことを検証する
fn assert_no_start_code(name: &str, data: &[u8]) {
    assert!(
        !data.starts_with(&[0x00, 0x00, 0x01]),
        "{name} の先頭に 3 バイトのスタートコードが含まれている"
    );
    assert!(
        !data.starts_with(&[0x00, 0x00, 0x00, 0x01]),
        "{name} の先頭に 4 バイトのスタートコードが含まれている"
    );
}

/// H.264 の SPS / PPS を Init 直後に取得できることを検証する
#[test]
fn test_sequence_params_h264() {
    let encoder = create_sequence_params_encoder(sequence_params_config(CodecConfig::H264(
        H264EncoderConfig {
            profile: Some(H264Profile::High),
        },
    )));

    // フレームを 1 枚もエンコードしていない状態でも取得できること
    let params = encoder
        .get_sequence_params()
        .expect("シーケンスパラメータの取得に失敗した")
        .expect("H.264 はシーケンスパラメータを返すこと");

    // H.264 の NAL ヘッダは下位 5 ビットが nal_unit_type である
    assert_eq!(
        params.sps[0] & 0x1F,
        7,
        "SPS の NAL タイプが 7 (sequence parameter set) であること"
    );
    let pps = params.pps.as_ref().expect("H.264 は PPS を返すこと");
    assert_eq!(
        pps[0] & 0x1F,
        8,
        "PPS の NAL タイプが 8 (picture parameter set) であること"
    );
    assert!(params.vps.is_none(), "H.264 は VPS を返さないこと");
    assert_no_start_code("SPS", &params.sps);
    assert_no_start_code("PPS", pps);
}

/// HEVC の VPS / SPS / PPS を Init 直後に取得できることを検証する
#[test]
fn test_sequence_params_hevc() {
    let encoder = create_sequence_params_encoder(sequence_params_config(CodecConfig::Hevc(
        HevcEncoderConfig {
            profile: Some(HevcProfile::Main),
        },
    )));

    let params = encoder
        .get_sequence_params()
        .expect("シーケンスパラメータの取得に失敗した")
        .expect("HEVC はシーケンスパラメータを返すこと");

    // HEVC の NAL ヘッダは先頭 1 バイトのビット 1-6 が nal_unit_type である
    let nal_unit_type = |data: &[u8]| (data[0] >> 1) & 0x3F;
    let vps = params.vps.as_ref().expect("HEVC は VPS を返すこと");
    assert_eq!(nal_unit_type(vps), 32, "VPS の NAL タイプが 32 であること");
    assert_eq!(
        nal_unit_type(&params.sps),
        33,
        "SPS の NAL タイプが 33 であること"
    );
    let pps = params.pps.as_ref().expect("HEVC は PPS を返すこと");
    assert_eq!(nal_unit_type(pps), 34, "PPS の NAL タイプが 34 であること");
    assert_no_start_code("VPS", vps);
    assert_no_start_code("SPS", &params.sps);
    assert_no_start_code("PPS", pps);
}

/// AV1 の Sequence Header OBU を Init 直後に取得できることを検証する
#[test]
fn test_sequence_params_av1() {
    let encoder = create_sequence_params_encoder(sequence_params_config(CodecConfig::Av1(
        Av1EncoderConfig {
            profile: Some(Av1Profile::Main),
        },
    )));

    let params = encoder
        .get_sequence_params()
        .expect("シーケンスパラメータの取得に失敗した")
        .expect("AV1 はシーケンスパラメータを返すこと");

    // AV1 の OBU ヘッダは forbidden bit の次の 4 ビットが obu_type である
    let obu = &params.sps;
    assert_eq!(obu[0] & 0x80, 0, "OBU の forbidden bit が 0 であること");
    assert_eq!(
        (obu[0] >> 3) & 0x0F,
        1,
        "Sequence Header OBU (OBU タイプ 1) であること"
    );
    assert!(params.vps.is_none(), "AV1 は VPS を返さないこと");
    assert!(params.pps.is_none(), "AV1 は PPS を返さないこと");
    assert_no_start_code("Sequence Header OBU", obu);
}

/// VP9 はパラメータセットを持たず `Ok(None)` を返すことを検証する
#[test]
fn test_sequence_params_vp9_returns_none() {
    let encoder = create_sequence_params_encoder(sequence_params_config(CodecConfig::Vp9(
        Vp9EncoderConfig {
            profile: Some(Vp9Profile::Profile0),
            write_ivf_headers: false,
        },
    )));

    let params = encoder
        .get_sequence_params()
        .expect("VP9 のシーケンスパラメータ取得に失敗した");
    assert!(params.is_none(), "VP9 は Ok(None) を返すこと");
}

/// encode の前後でシーケンスパラメータが変わらないことを検証する
#[test]
fn test_sequence_params_unchanged_by_encode() {
    let codecs = [
        (
            CodecConfig::H264(H264EncoderConfig {
                profile: Some(H264Profile::High),
            }),
            "H.264",
        ),
        (
            CodecConfig::Hevc(HevcEncoderConfig {
                profile: Some(HevcProfile::Main),
            }),
            "HEVC",
        ),
        (
            CodecConfig::Av1(Av1EncoderConfig {
                profile: Some(Av1Profile::Main),
            }),
            "AV1",
        ),
    ];

    for (codec, codec_name) in codecs {
        let mut encoder = create_sequence_params_encoder(sequence_params_config(codec));
        let before = encoder
            .get_sequence_params()
            .expect("シーケンスパラメータの取得に失敗した");

        let frame_data = generate_dummy_nv12(320, 240, 0);
        encoder
            .encode(
                frame_data_from_buffer(FrameFormat::Nv12, 320, 240, &frame_data),
                (),
                &EncodeOptions {
                    frame_type: frame_type::UNKNOWN,
                },
            )
            .expect("エンコードに失敗した");
        encoder.finish().expect("finish に失敗した");

        let after = encoder
            .get_sequence_params()
            .expect("シーケンスパラメータの取得に失敗した");
        assert_eq!(
            before, after,
            "{codec_name} で encode の前後でシーケンスパラメータが変わらないこと"
        );
    }
}

/// reconfigure でフレームレートを変えると SPS が変わり、その後も取得できることを検証する
#[test]
fn test_sequence_params_change_after_reconfigure() {
    let mut encoder = create_sequence_params_encoder(sequence_params_config(CodecConfig::H264(
        H264EncoderConfig {
            profile: Some(H264Profile::High),
        },
    )));

    let before = encoder
        .get_sequence_params()
        .expect("シーケンスパラメータの取得に失敗した")
        .expect("H.264 はシーケンスパラメータを返すこと");

    // H.264 の SPS の VUI にはフレームレートが timing info として入る
    encoder
        .reconfigure(ReconfigureParams {
            framerate_num: Some(60),
            framerate_den: Some(1),
            ..Default::default()
        })
        .expect("reconfigure に失敗した");

    // reconfigure 後も取得でき、SPS の内容が変わっていること
    let after = encoder
        .get_sequence_params()
        .expect("reconfigure 後のシーケンスパラメータ取得に失敗した")
        .expect("H.264 は reconfigure 後もシーケンスパラメータを返すこと");
    assert_ne!(
        before.sps, after.sps,
        "reconfigure でフレームレートを変えると SPS の内容が変わること"
    );
    assert_eq!(
        after.sps[0] & 0x1F,
        7,
        "reconfigure 後の SPS の NAL タイプが 7 であること"
    );
    assert_no_start_code("SPS", &after.sps);
}

// --- 統計 ---

/// 指定した設定でフレームをエンコードし、フレームごとのビットストリームを返す
fn encode_test_bitstreams(config: EncoderConfig, num_frames: usize) -> Vec<Vec<u8>> {
    let width = config.width as usize;
    let height = config.height as usize;
    let input_frames: Vec<Vec<u8>> = (0..num_frames)
        .map(|i| generate_dummy_nv12(width, height, i))
        .collect();
    let (_, bitstreams) = encode(config, &input_frames);
    bitstreams
}

/// エンコーダーの統計値が finish 完了後の通知結果と一致することを検証する
#[test]
fn test_encoder_stats() {
    let (mut encoder, rx) = create_encoder(h264_test_config());
    let options = EncodeOptions {
        frame_type: frame_type::UNKNOWN,
    };
    let num_frames = 8;

    for i in 0..num_frames {
        let frame_data = generate_dummy_nv12(320, 240, i);
        encoder
            .encode(
                frame_data_from_buffer(FrameFormat::Nv12, 320, 240, &frame_data),
                i,
                &options,
            )
            .expect("エンコードに失敗した");
    }

    // finish はバリアなので、戻った時点で全通知と統計の更新が済んでいる
    encoder.finish().expect("finish に失敗した");

    let mut total_bits = 0u64;
    for _ in 0..num_frames {
        let encoded = rx
            .recv_timeout(Duration::from_secs(10))
            .expect("エンコード結果コールバックの受信がタイムアウトした")
            .expect("エンコードに失敗した");
        total_bits += encoded.data().len() as u64 * 8;
    }

    let stats = encoder.stats();
    assert_eq!(
        stats.in_flight_frames.get(),
        0,
        "finish 完了後に in-flight が 0 であること"
    );
    assert_eq!(
        stats.total_encode_count.get(),
        num_frames as u64,
        "encode() が受理した回数が計上されること"
    );
    assert_eq!(
        stats.total_output_frame_count.get(),
        num_frames as u64,
        "通知された出力フレーム数が計上されること"
    );
    assert_eq!(
        stats.total_error_count.get(),
        0,
        "正常に完了したエンコードでエラー数が増えないこと"
    );
    assert_eq!(
        stats.total_output_bit_count.get(),
        total_bits,
        "出力ビット数の合計が通知されたビットストリーム長と一致すること"
    );
    assert_eq!(
        stats.total_reconfigure_count.get(),
        0,
        "reconfigure を呼んでいないため成功回数が 0 であること"
    );
}

/// reconfigure の成功だけが統計に計上されることを検証する
#[test]
fn test_encoder_stats_reconfigure_count() {
    let mut encoder: Encoder<FnEncodeHandler<()>> = Encoder::new(
        h264_test_config(),
        FnEncodeHandler::new(|_: Result<EncodedFrame<()>, Error>| {}),
    )
    .expect("エンコーダーの生成に失敗した");

    // パラメータ検証で拒否された reconfigure は成功回数に計上しない
    let rejected = encoder.reconfigure(ReconfigureParams {
        framerate_den: Some(0),
        ..Default::default()
    });
    assert!(
        rejected.is_err(),
        "framerate_den が 0 の reconfigure は拒否されること"
    );
    assert_eq!(
        encoder.stats().total_reconfigure_count.get(),
        0,
        "拒否された reconfigure は成功回数に計上されないこと"
    );

    encoder
        .reconfigure(ReconfigureParams {
            target_kbps: Some(1200),
            ..Default::default()
        })
        .expect("reconfigure に失敗した");
    assert_eq!(
        encoder.stats().total_reconfigure_count.get(),
        1,
        "成功した reconfigure が 1 回計上されること"
    );

    encoder
        .reconfigure(ReconfigureParams {
            target_kbps: Some(1400),
            ..Default::default()
        })
        .expect("reconfigure に失敗した");
    assert_eq!(
        encoder.stats().total_reconfigure_count.get(),
        2,
        "成功した reconfigure が 2 回計上されること"
    );
}

/// 入力検証で拒否したフレームがエンコーダーの統計に計上されないことを検証する
#[test]
fn test_encoder_stats_ignores_rejected_frame() {
    let mut encoder: Encoder<FnEncodeHandler<()>> = Encoder::new(
        h264_test_config(),
        FnEncodeHandler::new(|_: Result<EncodedFrame<()>, Error>| {}),
    )
    .expect("エンコーダーの生成に失敗した");
    let options = EncodeOptions {
        frame_type: frame_type::UNKNOWN,
    };

    // 必要な長さに満たないプレーンは入力検証で拒否される
    let y = [0u8; 16];
    let uv = [0u8; 16];
    let result = encoder.encode(
        FrameData::SemiPlanar {
            y: &y,
            y_pitch: 320,
            uv: &uv,
            uv_pitch: 320,
        },
        (),
        &options,
    );
    assert!(
        result.is_err(),
        "表示サイズに満たないフレームデータは拒否されること"
    );

    let stats = encoder.stats();
    assert_eq!(
        stats.total_encode_count.get(),
        0,
        "拒否されたフレームは受理回数に計上されないこと"
    );
    assert_eq!(
        stats.in_flight_frames.get(),
        0,
        "拒否されたフレームは in-flight に残らないこと"
    );
}

/// in_flight_frames が受理したフレームに追従することを検証する
///
/// 最初のコールバックをブロックして worker を停止させ、その間に 1 フレームだけ
/// 追加で受理させて加算を観測する。AsyncDepth が 4 のため、停止中の追加は
/// 1 フレームに限定する。
#[test]
fn test_encoder_stats_in_flight_frames() {
    let (result_tx, result_rx) = mpsc::channel::<Result<usize, Error>>();
    let (started_tx, started_rx) = mpsc::channel::<()>();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let mut first_callback = true;
    let mut encoder = Encoder::new(
        h264_test_config(),
        FnEncodeHandler::new(move |result: Result<EncodedFrame<usize>, Error>| {
            let frame = result.expect("エンコードに失敗した");
            result_tx
                .send(Ok(*frame.user_data()))
                .expect("エンコード結果の送信に失敗した");
            if first_callback {
                first_callback = false;
                started_tx
                    .send(())
                    .expect("コールバック開始の送信に失敗した");
                // 解放されるまで worker を停止させる
                let _ = release_rx.recv();
            }
        }),
    )
    .expect("エンコーダーの生成に失敗した");
    let options = EncodeOptions {
        frame_type: frame_type::UNKNOWN,
    };

    // 2 フレームを投入すると 1 枚目のコールバックが worker で始まる
    for i in 0..2 {
        let frame_data = generate_dummy_nv12(320, 240, i);
        encoder
            .encode(
                frame_data_from_buffer(FrameFormat::Nv12, 320, 240, &frame_data),
                i,
                &options,
            )
            .expect("エンコードに失敗した");
    }
    started_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("最初のコールバックが開始されなかった");

    // 1 枚目は通知済み、2 枚目は pending に残っている
    let before = encoder.stats().in_flight_frames.get();
    assert_eq!(before, 1, "コールバック中の in-flight が 1 であること");

    // worker が停止している間に 1 フレームだけ受理させる
    let frame_data = generate_dummy_nv12(320, 240, 2);
    encoder
        .encode(
            frame_data_from_buffer(FrameFormat::Nv12, 320, 240, &frame_data),
            2,
            &options,
        )
        .expect("エンコードに失敗した");
    assert_eq!(
        encoder.stats().in_flight_frames.get(),
        before + 1,
        "受理したフレームが in-flight に加算されること"
    );

    release_tx.send(()).expect("コールバックの解放に失敗した");
    drop(release_tx);
    encoder.finish().expect("finish に失敗した");
    assert_eq!(
        encoder.stats().in_flight_frames.get(),
        0,
        "finish 完了後に in-flight が 0 であること"
    );

    let mut user_data = Vec::new();
    for _ in 0..3 {
        user_data.push(
            result_rx
                .recv_timeout(Duration::from_secs(10))
                .expect("エンコード結果の受信がタイムアウトした")
                .expect("エンコードに失敗した"),
        );
    }
    user_data.sort_unstable();
    assert_eq!(
        user_data,
        vec![0, 1, 2],
        "全フレームが 1 回ずつ通知されること"
    );

    let stats = encoder.stats();
    assert_eq!(
        stats.total_encode_count.get(),
        3,
        "受理回数が絶対値で一致すること"
    );
    assert_eq!(
        stats.total_output_frame_count.get(),
        3,
        "出力フレーム数が絶対値で一致すること"
    );
    assert_eq!(
        stats.total_error_count.get(),
        0,
        "正常に完了したエンコードでエラー数が増えないこと"
    );
}

/// デコーダーの統計値が finish 完了後の通知結果と一致することを検証する
#[test]
fn test_decoder_stats() {
    let num_frames = 8;
    let bitstreams = encode_test_bitstreams(h264_test_config(), num_frames);
    let (mut decoder, rx) = create_decoder(VideoCodecType::H264);

    for (i, bs) in bitstreams.iter().enumerate() {
        decoder.decode(bs, i).expect("デコードに失敗した");
    }
    decoder.finish().expect("finish に失敗した");

    let mut output_count = 0usize;
    while let Ok(result) = rx.recv_timeout(Duration::from_millis(200)) {
        result.expect("デコードに失敗した");
        output_count += 1;
    }
    assert_eq!(
        output_count, num_frames,
        "全ての入力フレームが Ok で通知されること"
    );

    let stats = decoder.stats();
    assert_eq!(
        stats.in_flight_frames.get(),
        0,
        "finish 完了後に in-flight が 0 であること"
    );
    assert_eq!(
        stats.total_decode_count.get(),
        num_frames as u64,
        "decode() が受理した回数が計上されること"
    );
    assert_eq!(
        stats.total_output_frame_count.get(),
        num_frames as u64,
        "通知された出力フレーム数が計上されること"
    );
    assert_eq!(
        stats.total_error_count.get(),
        0,
        "正常に完了したデコードでエラー数が増えないこと"
    );
}

/// 初期化に失敗したデコードが受理回数に計上されないことを検証する
#[test]
fn test_decoder_stats_ignores_rejected_bitstream() {
    let (mut decoder, rx) = create_decoder(VideoCodecType::H264);

    // 空のビットストリームはヘッダ解析に失敗し、初期化エラーとして拒否される
    let result = decoder.decode(&[], 0);
    assert!(
        result.is_err(),
        "空のビットストリームは初期化エラーで拒否されること"
    );

    let stats = decoder.stats();
    assert_eq!(
        stats.total_decode_count.get(),
        0,
        "拒否されたデコードは受理回数に計上されないこと"
    );
    assert_eq!(
        stats.in_flight_frames.get(),
        0,
        "拒否されたデコードは in-flight に残らないこと"
    );

    // 受信側を破棄する前にデコーダーを破棄し、worker の終了を待つ
    drop(decoder);
    drop(rx);
}

/// 残留 pending のエラー通知でエラー数が増え、in-flight が 0 に戻ることを検証する
#[test]
fn test_decoder_stats_error_notification() {
    let num_frames = 4;
    let bitstreams = encode_test_bitstreams(h264_test_config(), num_frames);
    let (mut decoder, rx) = create_decoder(VideoCodecType::H264);

    for (i, bs) in bitstreams.iter().enumerate() {
        decoder.decode(bs, i).expect("デコードに失敗した");
    }

    // 空の入力は VPL を呼ばずに pending だけを登録するため、
    // finish() の残留チェックで必ずエラーとして通知される
    decoder
        .decode(&[], num_frames)
        .expect("空のビットストリームは受理されること");
    let finish_error = decoder
        .finish()
        .expect_err("残留 pending がある finish はエラーを返すこと");
    assert_eq!(
        finish_error.function(),
        "Decoder::finish",
        "finish のエラーは Decoder::finish のコンテキストを持つこと"
    );

    let mut ok_count = 0usize;
    let mut error_count = 0usize;
    while let Ok(result) = rx.recv_timeout(Duration::from_millis(200)) {
        match result {
            Ok(_) => ok_count += 1,
            Err(error) => {
                assert_eq!(
                    error.function(),
                    "Decoder::finish",
                    "残留 pending のエラーは finish のコンテキストを持つこと"
                );
                error_count += 1;
            }
        }
    }
    assert_eq!(
        ok_count, num_frames,
        "有効な入力の出力フレームはエラーにならないこと"
    );
    assert_eq!(
        error_count, 1,
        "残留した pending 1 件だけがエラー通知されること"
    );

    let stats = decoder.stats();
    assert_eq!(
        stats.in_flight_frames.get(),
        0,
        "残留 pending の通知後に in-flight が 0 であること"
    );
    assert_eq!(
        stats.total_decode_count.get(),
        (num_frames + 1) as u64,
        "有効な入力と空の入力を合わせて受理回数に計上されること"
    );
    assert_eq!(
        stats.total_output_frame_count.get(),
        num_frames as u64,
        "出力フレーム数は有効な入力の分だけ計上されること"
    );
    assert_eq!(
        stats.total_error_count.get(),
        1,
        "残留 pending のエラー通知がエラー数に計上されること"
    );
}

/// pending 登録後にデコードが失敗した呼び出しも受理回数に計上されることを検証する
#[test]
fn test_decoder_stats_counts_failed_decode() {
    let vp9_config = EncoderConfig::new(
        test_adapter(),
        CodecConfig::Vp9(Vp9EncoderConfig {
            profile: Some(Vp9Profile::Profile0),
            write_ivf_headers: false,
        }),
        320,
        240,
        FrameFormat::Nv12,
        30,
        1,
        RateControlMode::Cbr,
    );
    let vp9_bitstreams = encode_test_bitstreams(vp9_config, 1);
    let h264_bitstreams = encode_test_bitstreams(h264_test_config(), 1);

    let (mut decoder, rx) = create_decoder(VideoCodecType::Vp9);
    decoder
        .decode(&vp9_bitstreams[0], 0)
        .expect("VP9 デコーダーの初期化に失敗した");
    assert_eq!(
        decoder.stats().total_decode_count.get(),
        1,
        "初期化に成功したデコードが計上されること"
    );

    // 初期化済みの VP9 デコーダーに H.264 のビットストリームを渡すと
    // DecodeFrameAsync がエラーを返す。pending 登録後の失敗なので計上される。
    let result = decoder.decode(&h264_bitstreams[0], 1);
    assert!(
        result.is_err(),
        "異なるコーデックのビットストリームはエラーになること"
    );
    let stats = decoder.stats();
    assert_eq!(
        stats.total_decode_count.get(),
        2,
        "pending 登録後に失敗したデコードも計上されること"
    );
    assert!(
        stats.in_flight_frames.get() >= 1,
        "失敗した入力は pending に残ること"
    );

    // 受信側を破棄する前にデコーダーを破棄し、worker の終了を待つ
    drop(decoder);
    drop(rx);
}

/// in_flight_frames が受理した入力に追従することを検証する
///
/// 4 フレームを投入して最初のコールバックをブロックし、worker を停止させる。
/// その間に 1 フレームだけ追加で受理させて加算を観測する。AsyncDepth が 4 の
/// ため、停止中の追加は 1 フレームに限定する。
#[test]
fn test_decoder_stats_in_flight_frames() {
    let num_frames = 5;
    let bitstreams = encode_test_bitstreams(h264_test_config(), num_frames);
    let (result_tx, result_rx) = mpsc::channel::<Result<usize, Error>>();
    let (started_tx, started_rx) = mpsc::channel::<()>();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let mut first_callback = true;
    let mut decoder = Decoder::new(
        DecoderConfig::new(test_adapter(), VideoCodecType::H264),
        FnDecodeHandler::new(move |result: Result<DecodedFrame<'_, usize>, Error>| {
            let frame = result.expect("デコードに失敗した");
            result_tx
                .send(Ok(*frame.user_data()))
                .expect("デコード結果の送信に失敗した");
            if first_callback {
                first_callback = false;
                started_tx
                    .send(())
                    .expect("コールバック開始の送信に失敗した");
                // 解放されるまで worker を停止させる
                let _ = release_rx.recv();
            }
        }),
    )
    .expect("デコーダーの生成に失敗した");

    // 4 フレームを投入すると 1 枚目のコールバックが worker で始まる
    for (i, bs) in bitstreams.iter().take(4).enumerate() {
        decoder.decode(bs, i).expect("デコードに失敗した");
    }
    started_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("最初のコールバックが開始されなかった");

    // 4 枚中 1 枚は通知済みで、残り 3 枚が pending に残っている
    let before = decoder.stats().in_flight_frames.get();
    assert_eq!(before, 3, "コールバック中の in-flight が 3 であること");

    // worker が停止している間に 1 フレームだけ受理させる
    decoder
        .decode(&bitstreams[4], 4)
        .expect("デコードに失敗した");
    assert_eq!(
        decoder.stats().in_flight_frames.get(),
        before + 1,
        "受理した入力が in-flight に加算されること"
    );

    release_tx.send(()).expect("コールバックの解放に失敗した");
    drop(release_tx);
    decoder.finish().expect("finish に失敗した");
    assert_eq!(
        decoder.stats().in_flight_frames.get(),
        0,
        "finish 完了後に in-flight が 0 であること"
    );

    let mut user_data = Vec::new();
    for _ in 0..num_frames {
        user_data.push(
            result_rx
                .recv_timeout(Duration::from_secs(10))
                .expect("デコード結果の受信がタイムアウトした")
                .expect("デコードに失敗した"),
        );
    }
    user_data.sort_unstable();
    assert_eq!(
        user_data,
        (0..num_frames).collect::<Vec<_>>(),
        "全フレームが 1 回ずつ通知されること"
    );

    let stats = decoder.stats();
    assert_eq!(
        stats.total_decode_count.get(),
        num_frames as u64,
        "受理回数が絶対値で一致すること"
    );
    assert_eq!(
        stats.total_output_frame_count.get(),
        num_frames as u64,
        "出力フレーム数が絶対値で一致すること"
    );
    assert_eq!(
        stats.total_error_count.get(),
        0,
        "正常に完了したデコードでエラー数が増えないこと"
    );
}

// ---------------------------------------------------------------------------
// エンコード→デコードの恒等性テスト
// ---------------------------------------------------------------------------

/// エンコード→デコードで入力と出力の内容が一致することを検証するテスト群
///
/// デコーダーの出力フォーマットはストリームのネイティブ形式で決まり、入力フォーマットと
/// 異なる場合があるため、実機で確認した主要な経路を明示的なテーブルで持つ。
///
/// Intel GPU を搭載した実機が必須である。閾値の実測値は Meteor Lake / mfx-gen 環境で取得した。
/// BGRA 入力のデコード結果は limited range の Y である前提で検証する。
mod identity {
    use super::*;

    /// 表示幅
    const WIDTH: usize = 320;

    /// 表示高さ
    const HEIGHT: usize = 240;

    /// 1 ケースあたりのフレーム数
    const NUM_FRAMES: usize = 5;

    // 量子化誤差を十分小さくしつつ、実機で安定してエンコードできる 10〜16 の範囲に収める
    /// I フレームの量子化パラメータ
    const QPI: u16 = 12;

    // QPI と同じ範囲に収める
    /// P フレームの量子化パラメータ
    const QPP: u16 = 12;

    /// 色バーの本数
    const BAR_COUNT: usize = COLOR_BARS_RGB.len();

    /// クロマ領域平均を計算する縦方向の範囲（上端）
    const CHROMA_ROW_START: usize = HEIGHT / 4;

    /// クロマ領域平均を計算する縦方向の範囲（下端）
    const CHROMA_ROW_END: usize = HEIGHT * 3 / 4;

    // QPI = QPP = 12 の実測値は Y が 52.1 dB 以上、U / V が 53.7 dB 以上だった。
    // 実装差を見込んで 45 dB を下限にする。これは実測の最小値より 7 dB 低く、MSE では
    // 約 5 倍の余裕がある。変換経路でもルマは変換で失われないため同じ下限を使う。
    /// Y / U / V PSNR の下限 (dB)
    const PSNR_MIN_DB: f64 = 45.0;

    // 実測値の最大は 0.09 だった。4:2:0 への変換で色バー境界の影響を受けるため 2.0 まで
    // 許容する。色バーの取り違えやクロマプレーンの欠落は 10 以上の差になるが、小さな
    // クロマの歪みは検出しない。
    /// 8bit のクロマ領域平均の許容差（サンプル値）
    const CHROMA_MEAN_TOLERANCE_8BIT: f64 = 2.0;

    // 実測値の最大は 0.53 だった。10bit のサンプル値は 8bit の 4 倍のスケールなので、
    // 8bit と同じ比率になる 8.0 まで許容する。
    /// 10bit のクロマ領域平均の許容差（サンプル値）
    const CHROMA_MEAN_TOLERANCE_10BIT: f64 = 8.0;

    // 参照フレームのグラデーション・チェッカー・移動縞による実測値は 2312 以上だった。
    // 単色やフラットな壊れた出力を検出できるよう、実測値の約 65% を下限にする。
    /// 8bit のルマ分散の下限
    const LUMA_VARIANCE_MIN_8BIT: f64 = 1500.0;

    // 実測値は 37006 以上だった。10bit のサンプル値は 8bit の 4 倍のスケールなので、
    // 分散は 16 倍になり、8bit の下限 1500 の 16 倍を下限にする。
    /// 10bit のルマ分散の下限
    const LUMA_VARIANCE_MIN_10BIT: f64 = 24000.0;

    // フレームごとに移動する縞による実測値は 8.4 以上だった。同じフレームの繰り返しや
    // 移動パターンが失われた出力を検出できるよう、実測値の約 35% を下限にする。
    /// 8bit の隣接フレーム間の平均絶対差分の下限
    const FRAME_DIFF_MIN_8BIT: f64 = 3.0;

    // 実測値は 33.7 以上だった。10bit のサンプル値は 8bit の 4 倍のスケールなので、
    // 8bit の下限 3.0 の 4 倍を下限にする。
    /// 10bit の隣接フレーム間の平均絶対差分の下限
    const FRAME_DIFF_MIN_10BIT: f64 = 12.0;

    // 7 色の色バーによる実測値は 193 以上だった。クロマが単色になった場合は 0 になるため、
    // 実測値の約 60% を下限にする。
    /// 8bit の色バー間のクロマ平均の広がりの下限
    const CHROMA_SPREAD_MIN_8BIT: f64 = 120.0;

    // 実測値は 772 以上だった。10bit のサンプル値は 8bit の 4 倍のスケールなので、
    // 8bit の下限 120 の 4 倍を下限にする。
    /// 10bit の色バー間のクロマ平均の広がりの下限
    const CHROMA_SPREAD_MIN_10BIT: f64 = 480.0;

    // R = G = B のランプに対する実測値は 1.0000 だった。量子化の影響を見込んで 0.995 を
    // 下限にする。
    /// BGRA 入力のグレーランプとルマの相関の下限
    const BGRA_LUMA_CORRELATION_MIN: f64 = 0.995;

    // 各フレームのグレー値のレンジは 180 で、limited range の Y に変換すると 154〜155 に
    // なる。単色の場合は 0 になるため、120 を下限にする。
    /// BGRA 入力のルマレンジの下限
    const BGRA_LUMA_RANGE_MIN: f64 = 120.0;

    // 行方向と列方向のグラデーションによる輝度の分散は実測で 1311 だった。単色の場合は 0 に
    // なるため、1000 を下限にする。
    /// BGRA 入力のルマ分散の下限
    const BGRA_LUMA_VARIANCE_MIN: f64 = 1000.0;

    // グレー値をフレームごとに 3 増やしており、limited range の輝度の差は約 2.6 になる。
    // 実測値は 2.6 だったため、量子化誤差と実装差を見込んで約 42% の余裕がある 1.5 を
    // 下限にする。
    /// BGRA 入力の隣接フレーム間の平均絶対差分の下限
    const BGRA_FRAME_DIFF_MIN: f64 = 1.5;

    // BGRA 入力のデコード結果の Y は limited range であるため、期待するグレー平均を
    // 16 + 219 / 255 * 平均 に変換して比較する。正しいフレームの残差は 0.1 以下、
    // 隣接フレームの取り違えでは 2.5 以上変わるため、0.5 を許容差にする。
    /// BGRA 入力のルマ平均の許容差（サンプル値）
    const BGRA_LUMA_MEAN_TOLERANCE: f64 = 0.5;

    /// デコード出力のサンプル形式
    #[derive(Debug, Clone, Copy)]
    enum DecodeFormat {
        /// 8bit の NV12
        Nv12,
        /// 10bit の P010
        P010,
    }

    /// クロマの検証方法
    #[derive(Debug, Clone, Copy)]
    enum ChromaCheck {
        /// U / V プレーンの PSNR を検証する
        Psnr,
        /// 色バー領域の平均値だけを検証する
        RegionMean,
    }

    /// 検証するエンコーダー・入力フォーマット・デコード出力の組み合わせ
    struct Case {
        /// テストログに出す経路の名前
        label: &'static str,
        /// エンコーダーのコーデック設定
        codec_config: CodecConfig,
        /// デコーダーのコーデック種別
        decoder_codec: VideoCodecType,
        /// エンコーダーへの入力フォーマット
        input_format: FrameFormat,
        /// デコーダーの出力フォーマット
        decode_format: DecodeFormat,
        /// クロマの検証方法
        chroma_check: ChromaCheck,
    }

    /// H.264 High のコーデック設定を返す
    fn h264_high() -> CodecConfig {
        CodecConfig::H264(H264EncoderConfig {
            profile: Some(H264Profile::High),
        })
    }

    /// HEVC Main のコーデック設定を返す
    fn hevc_main() -> CodecConfig {
        CodecConfig::Hevc(HevcEncoderConfig {
            profile: Some(HevcProfile::Main),
        })
    }

    /// HEVC Main10 のコーデック設定を返す
    fn hevc_main10() -> CodecConfig {
        CodecConfig::Hevc(HevcEncoderConfig {
            profile: Some(HevcProfile::Main10),
        })
    }

    /// AV1 Main のコーデック設定を返す
    fn av1_main() -> CodecConfig {
        CodecConfig::Av1(Av1EncoderConfig {
            profile: Some(Av1Profile::Main),
        })
    }

    /// VP9 のコーデック設定を返す
    ///
    /// IVF ヘッダーを含まない raw VP9 を出力する。デコーダーにはそのまま渡せる。
    fn vp9(profile: Vp9Profile) -> CodecConfig {
        CodecConfig::Vp9(Vp9EncoderConfig {
            profile: Some(profile),
            write_ivf_headers: false,
        })
    }

    /// identity テスト用のエンコーダー設定を返す
    ///
    /// QPI = QPP = 12、`gop_ref_dist = 1`、全フレームを 1 GOP に収める設定である。
    fn identity_config(codec_config: CodecConfig, input_format: FrameFormat) -> EncoderConfig {
        let mut config = EncoderConfig::new(
            test_adapter(),
            codec_config,
            WIDTH as u32,
            HEIGHT as u32,
            input_format,
            30,
            1,
            RateControlMode::Cqp,
        );
        config.qpi = Some(QPI);
        config.qpp = Some(QPP);
        // 全フレームを 1 GOP に収め、最初の I フレーム以外を P フレームにする
        config.gop_pic_size = Some(NUM_FRAMES as u16);
        config.gop_ref_dist = Some(1);
        config
    }

    // 参照フレームは入力フォーマットに依存しない YUV 4:4:4 で保持し、BGRA を除く
    // 各入力フォーマットへ変換する
    /// 参照フレームの YUV 4:4:4 プレーン（8bit）
    struct ReferenceFrame {
        /// Y プレーン
        y: Vec<u8>,
        /// U プレーン
        u: Vec<u8>,
        /// V プレーン
        v: Vec<u8>,
    }

    impl ReferenceFrame {
        /// フレーム番号に応じた参照フレームを生成する
        ///
        /// 返り値の `y` / `u` / `v` はそれぞれ `WIDTH * HEIGHT` サンプルの 4:4:4 プレーンである。
        fn new(frame_index: usize) -> Self {
            // Y は水平・垂直のグラデーション、高周波チェッカー、フレームごとに移動する縞を
            // 合成する。U / V は 7 色の縦ストライプ（色バー）に、フレームごとに上下へ移動する
            // 帯を加える。フレーム対応は Y の移動縞と PSNR で検証する
            let bar_uv: [(u8, u8); BAR_COUNT] = COLOR_BARS_RGB.map(rgb_to_uv);

            let mut y = vec![0u8; WIDTH * HEIGHT];
            let mut u = vec![0u8; WIDTH * HEIGHT];
            let mut v = vec![0u8; WIDTH * HEIGHT];
            for row in 0..HEIGHT {
                for col in 0..WIDTH {
                    let gradient = 16 + col * 150 / (WIDTH - 1) + row * 30 / (HEIGHT - 1);
                    let checker = if (col / 8 + row / 8) % 2 == 0 {
                        16
                    } else {
                        -16
                    };
                    let moving = if (col + row * 2 + frame_index * 17) % 96 < 48 {
                        12
                    } else {
                        -12
                    };
                    y[row * WIDTH + col] = (gradient as i32 + checker + moving).max(16) as u8;

                    let (bar_u, bar_v) = bar_uv[col * BAR_COUNT / WIDTH];
                    let band = if (row + frame_index * 13) % 64 < 32 {
                        8
                    } else {
                        -8
                    };
                    u[row * WIDTH + col] = (bar_u as i32 + band).clamp(16, 240) as u8;
                    v[row * WIDTH + col] = (bar_v as i32 - band).clamp(16, 240) as u8;
                }
            }
            Self { y, u, v }
        }
    }

    /// 指定フォーマットのエンコーダー入力バッファを生成する
    ///
    /// # Panics
    ///
    /// `format` が `FrameFormat::Bgra` の場合はパニックする。
    fn input_frame(reference: &ReferenceFrame, format: FrameFormat) -> Vec<u8> {
        let data = match format {
            FrameFormat::Nv12 => nv12_from_reference(reference),
            FrameFormat::P010 => p010_from_reference(reference),
            FrameFormat::Yuy2 => yuy2_from_reference(reference),
            FrameFormat::Ayuv => ayuv_from_reference(reference),
            FrameFormat::Y410 => y410_from_reference(reference),
            FrameFormat::Bgra => panic!("BGRA はグレーランプ用の生成関数を使う"),
        };
        assert_eq!(
            data.len(),
            frame_buffer_size(format, WIDTH, HEIGHT),
            "入力フレームのサイズが表示サイズと一致しない"
        );
        data
    }

    /// 参照フレームから NV12 の入力バッファを生成する
    fn nv12_from_reference(reference: &ReferenceFrame) -> Vec<u8> {
        let u = chroma_420(&reference.u);
        let v = chroma_420(&reference.v);
        let mut data = reference.y.clone();
        for (u, v) in u.iter().zip(&v) {
            data.push(*u);
            data.push(*v);
        }
        data
    }

    /// 参照フレームから P010 の入力バッファを生成する
    fn p010_from_reference(reference: &ReferenceFrame) -> Vec<u8> {
        let mut data = Vec::new();
        for &y in &reference.y {
            // 8bit を 10bit 相当（4 倍）に拡大し、16bit ワードの上位 10bit に格納する
            data.extend_from_slice(&((y as u16) << 8).to_le_bytes());
        }
        let u = chroma_420(&reference.u);
        let v = chroma_420(&reference.v);
        for (u, v) in u.iter().zip(&v) {
            data.extend_from_slice(&((*u as u16) << 8).to_le_bytes());
            data.extend_from_slice(&((*v as u16) << 8).to_le_bytes());
        }
        data
    }

    /// 参照フレームから YUY2 の入力バッファを生成する
    fn yuy2_from_reference(reference: &ReferenceFrame) -> Vec<u8> {
        let u = chroma_422(&reference.u);
        let v = chroma_422(&reference.v);
        let mut data = Vec::new();
        for row in 0..HEIGHT {
            for col in (0..WIDTH).step_by(2) {
                data.push(reference.y[row * WIDTH + col]);
                data.push(u[row * (WIDTH / 2) + col / 2]);
                data.push(reference.y[row * WIDTH + col + 1]);
                data.push(v[row * (WIDTH / 2) + col / 2]);
            }
        }
        data
    }

    /// 参照フレームから AYUV の入力バッファを生成する
    fn ayuv_from_reference(reference: &ReferenceFrame) -> Vec<u8> {
        let mut data = Vec::new();
        for index in 0..WIDTH * HEIGHT {
            data.push(reference.v[index]);
            data.push(reference.u[index]);
            data.push(reference.y[index]);
            data.push(255);
        }
        data
    }

    /// 参照フレームから Y410 の入力バッファを生成する
    fn y410_from_reference(reference: &ReferenceFrame) -> Vec<u8> {
        let mut data = Vec::new();
        for index in 0..WIDTH * HEIGHT {
            // 10bit に拡大した各サンプルを、下位から U / Y / V / A の順に詰める
            let u = (reference.u[index] as u32) << 2;
            let y = (reference.y[index] as u32) << 2;
            let v = (reference.v[index] as u32) << 2;
            let word = u | (y << 10) | (v << 20) | (3 << 30);
            data.extend_from_slice(&word.to_le_bytes());
        }
        data
    }

    /// 4:4:4 のクロマプレーンを 4:2:0 に縮小したプレーンを返す
    ///
    /// 2x2 の平均で縮小する。`plane` は `WIDTH * HEIGHT` サンプルの 4:4:4 プレーンであること。
    ///
    /// # Panics
    ///
    /// `plane` の長さが `WIDTH * HEIGHT` と異なる場合はパニックする。
    fn chroma_420(plane: &[u8]) -> Vec<u8> {
        assert_eq!(
            plane.len(),
            WIDTH * HEIGHT,
            "4:4:4 クロマプレーンの長さが表示サイズと一致しない"
        );
        let plane_width = WIDTH / 2;
        let mut data = vec![0u8; plane_width * (HEIGHT / 2)];
        for row in 0..HEIGHT / 2 {
            for col in 0..plane_width {
                let index = row * 2 * WIDTH + col * 2;
                let sum = plane[index] as u32
                    + plane[index + 1] as u32
                    + plane[index + WIDTH] as u32
                    + plane[index + WIDTH + 1] as u32;
                data[row * plane_width + col] = ((sum + 2) / 4) as u8;
            }
        }
        data
    }

    /// 4:4:4 のクロマプレーンを 4:2:2 に縮小したプレーンを返す
    ///
    /// 水平 2 タップの平均で縮小する。`plane` は `WIDTH * HEIGHT` サンプルの 4:4:4 プレーンであること。
    ///
    /// # Panics
    ///
    /// `plane` の長さが `WIDTH * HEIGHT` と異なる場合はパニックする。
    fn chroma_422(plane: &[u8]) -> Vec<u8> {
        assert_eq!(
            plane.len(),
            WIDTH * HEIGHT,
            "4:4:4 クロマプレーンの長さが表示サイズと一致しない"
        );
        let plane_width = WIDTH / 2;
        let mut data = vec![0u8; plane_width * HEIGHT];
        for row in 0..HEIGHT {
            for col in 0..plane_width {
                let index = row * WIDTH + col * 2;
                let sum = plane[index] as u32 + plane[index + 1] as u32;
                data[row * plane_width + col] = sum.div_ceil(2) as u8;
            }
        }
        data
    }

    /// U / V プレーン（8bit または 10bit）
    enum ChromaPlanes {
        /// 8bit のプレーン
        Bit8 { u: Vec<u8>, v: Vec<u8> },
        /// 10bit のプレーン
        Bit10 { u: Vec<u16>, v: Vec<u16> },
    }

    /// 参照フレームの 4:4:4 クロマを 2x2 平均で 4:2:0 に縮小した期待値を返す
    ///
    /// `format` が `Nv12` の場合は 8bit、`P010` の場合は 10bit のプレーンを返す。
    fn expected_chroma(reference: &ReferenceFrame, format: DecodeFormat) -> ChromaPlanes {
        // デコード出力のクロマは 4:2:0 であり、入力フォーマットが異なっても同じ期待値と
        // 比較できる
        let u = chroma_420(&reference.u);
        let v = chroma_420(&reference.v);
        match format {
            DecodeFormat::Nv12 => ChromaPlanes::Bit8 { u, v },
            DecodeFormat::P010 => ChromaPlanes::Bit10 {
                u: u.iter().map(|&value| (value as u16) << 2).collect(),
                v: v.iter().map(|&value| (value as u16) << 2).collect(),
            },
        }
    }

    /// デコード結果から U / V プレーンを取り出す
    fn decoded_chroma(frame: &DecodedFrameInfo, format: DecodeFormat) -> ChromaPlanes {
        let plane_width = frame.width / 2;
        let plane_height = frame.height / 2;
        match format {
            DecodeFormat::Nv12 => {
                let mut u = vec![0u8; plane_width * plane_height];
                let mut v = vec![0u8; plane_width * plane_height];
                for row in 0..plane_height {
                    for col in 0..plane_width {
                        let index = row * frame.pitch + col * 2;
                        u[row * plane_width + col] = frame.uv_data[index];
                        v[row * plane_width + col] = frame.uv_data[index + 1];
                    }
                }
                ChromaPlanes::Bit8 { u, v }
            }
            DecodeFormat::P010 => {
                let mut u = vec![0u16; plane_width * plane_height];
                let mut v = vec![0u16; plane_width * plane_height];
                for row in 0..plane_height {
                    for col in 0..plane_width {
                        let index = row * frame.pitch + col * 4;
                        let decoded_u =
                            u16::from_le_bytes([frame.uv_data[index], frame.uv_data[index + 1]])
                                >> 6;
                        let decoded_v = u16::from_le_bytes([
                            frame.uv_data[index + 2],
                            frame.uv_data[index + 3],
                        ]) >> 6;
                        u[row * plane_width + col] = decoded_u;
                        v[row * plane_width + col] = decoded_v;
                    }
                }
                ChromaPlanes::Bit10 { u, v }
            }
        }
    }

    /// U / V プレーンの PSNR を計算する
    fn chroma_psnr(expected: &ChromaPlanes, decoded: &ChromaPlanes) -> (f64, f64) {
        match (expected, decoded) {
            (
                ChromaPlanes::Bit8 {
                    u: expected_u,
                    v: expected_v,
                },
                ChromaPlanes::Bit8 {
                    u: decoded_u,
                    v: decoded_v,
                },
            ) => (
                psnr_plane_8bit(expected_u, decoded_u),
                psnr_plane_8bit(expected_v, decoded_v),
            ),
            (
                ChromaPlanes::Bit10 {
                    u: expected_u,
                    v: expected_v,
                },
                ChromaPlanes::Bit10 {
                    u: decoded_u,
                    v: decoded_v,
                },
            ) => (
                psnr_plane_10bit(expected_u, decoded_u),
                psnr_plane_10bit(expected_v, decoded_v),
            ),
            // expected_chroma と decoded_chroma は同じ decode_format から生成するため、
            // ビット深度が異なる組み合わせには到達しない
            _ => panic!("期待値とデコード結果のビット深度が一致しない"),
        }
    }

    /// 密な 8bit プレーン同士の PSNR を計算する（dB）
    ///
    /// ピッチを持たないプレーン用である。ピッチを含む Y プレーンには [`psnr_y`] を使う。
    fn psnr_plane_8bit(expected: &[u8], decoded: &[u8]) -> f64 {
        assert_eq!(
            expected.len(),
            decoded.len(),
            "PSNR を計算するプレーンの長さが一致しない"
        );
        let mut mse_sum = 0.0;
        for (expected_sample, decoded_sample) in expected.iter().zip(decoded) {
            let diff = *expected_sample as f64 - *decoded_sample as f64;
            mse_sum += diff * diff;
        }
        psnr_from_mse(mse_sum, expected.len(), 255.0)
    }

    /// 密な 10bit プレーン同士の PSNR を計算する（dB）
    fn psnr_plane_10bit(expected: &[u16], decoded: &[u16]) -> f64 {
        assert_eq!(
            expected.len(),
            decoded.len(),
            "PSNR を計算するプレーンの長さが一致しない"
        );
        let mut mse_sum = 0.0;
        for (expected_sample, decoded_sample) in expected.iter().zip(decoded) {
            let diff = *expected_sample as f64 - *decoded_sample as f64;
            mse_sum += diff * diff;
        }
        psnr_from_mse(mse_sum, expected.len(), 1023.0)
    }

    /// 指定した番号の色バーの、左右の境界を除いた領域を `x0..x1` の半開区間で返す
    ///
    /// # Panics
    ///
    /// `bar` が `BAR_COUNT` 以上の場合はパニックする。
    fn bar_region(bar: usize) -> (usize, usize) {
        assert!(bar < BAR_COUNT, "色バーの番号が範囲外である");
        // 色バーの境界は 4:2:0 のクロマで隣の色と混ざるため、左右を 1/4 ずつ内側に狭める
        let start = (bar * WIDTH).div_ceil(BAR_COUNT);
        let end = ((bar + 1) * WIDTH).div_ceil(BAR_COUNT);
        let inset = (end - start) / 4;
        (start + inset, end - inset)
    }

    /// 8bit プレーンの指定領域の平均を返す
    ///
    /// `x0 < x1`、`y0 < y1` であり、領域がプレーンの範囲内であること。
    ///
    /// # Panics
    ///
    /// 領域が空の場合、または領域がプレーンの範囲外の場合はパニックする。
    fn region_mean_8bit(
        plane: &[u8],
        plane_width: usize,
        x0: usize,
        x1: usize,
        y0: usize,
        y1: usize,
    ) -> f64 {
        let mut sum = 0.0;
        let mut count = 0;
        for row in y0..y1 {
            for col in x0..x1 {
                sum += plane[row * plane_width + col] as f64;
                count += 1;
            }
        }
        assert!(count > 0, "領域平均を計算する画素が無い");
        sum / count as f64
    }

    /// 10bit プレーンの指定領域の平均を返す
    ///
    /// `x0 < x1`、`y0 < y1` であり、領域がプレーンの範囲内であること。
    ///
    /// # Panics
    ///
    /// 領域が空の場合、または領域がプレーンの範囲外の場合はパニックする。
    fn region_mean_10bit(
        plane: &[u16],
        plane_width: usize,
        x0: usize,
        x1: usize,
        y0: usize,
        y1: usize,
    ) -> f64 {
        let mut sum = 0.0;
        let mut count = 0;
        for row in y0..y1 {
            for col in x0..x1 {
                sum += plane[row * plane_width + col] as f64;
                count += 1;
            }
        }
        assert!(count > 0, "領域平均を計算する画素が無い");
        sum / count as f64
    }

    /// 色バーごとのクロマ領域平均の最大差を返す
    fn max_chroma_mean_diff(reference: &ReferenceFrame, decoded: &ChromaPlanes) -> f64 {
        match decoded {
            ChromaPlanes::Bit8 { u, v } => {
                let mut max_diff = 0.0_f64;
                for bar in 0..BAR_COUNT {
                    let (x0, x1) = bar_region(bar);
                    let expected_u = region_mean_8bit(
                        &reference.u,
                        WIDTH,
                        x0,
                        x1,
                        CHROMA_ROW_START,
                        CHROMA_ROW_END,
                    );
                    let expected_v = region_mean_8bit(
                        &reference.v,
                        WIDTH,
                        x0,
                        x1,
                        CHROMA_ROW_START,
                        CHROMA_ROW_END,
                    );
                    let actual_u = region_mean_8bit(
                        u,
                        WIDTH / 2,
                        x0 / 2,
                        x1 / 2,
                        CHROMA_ROW_START / 2,
                        CHROMA_ROW_END / 2,
                    );
                    let actual_v = region_mean_8bit(
                        v,
                        WIDTH / 2,
                        x0 / 2,
                        x1 / 2,
                        CHROMA_ROW_START / 2,
                        CHROMA_ROW_END / 2,
                    );
                    max_diff = max_diff.max((expected_u - actual_u).abs());
                    max_diff = max_diff.max((expected_v - actual_v).abs());
                }
                max_diff
            }
            ChromaPlanes::Bit10 { u, v } => {
                let mut max_diff = 0.0_f64;
                for bar in 0..BAR_COUNT {
                    let (x0, x1) = bar_region(bar);
                    // 期待値は 8bit の平均を 10bit 相当（4 倍）にしたもの
                    let expected_u = region_mean_8bit(
                        &reference.u,
                        WIDTH,
                        x0,
                        x1,
                        CHROMA_ROW_START,
                        CHROMA_ROW_END,
                    ) * 4.0;
                    let expected_v = region_mean_8bit(
                        &reference.v,
                        WIDTH,
                        x0,
                        x1,
                        CHROMA_ROW_START,
                        CHROMA_ROW_END,
                    ) * 4.0;
                    let actual_u = region_mean_10bit(
                        u,
                        WIDTH / 2,
                        x0 / 2,
                        x1 / 2,
                        CHROMA_ROW_START / 2,
                        CHROMA_ROW_END / 2,
                    );
                    let actual_v = region_mean_10bit(
                        v,
                        WIDTH / 2,
                        x0 / 2,
                        x1 / 2,
                        CHROMA_ROW_START / 2,
                        CHROMA_ROW_END / 2,
                    );
                    max_diff = max_diff.max((expected_u - actual_u).abs());
                    max_diff = max_diff.max((expected_v - actual_v).abs());
                }
                max_diff
            }
        }
    }

    /// 色バー間のクロマ平均の広がりを返す
    fn chroma_spread(decoded: &ChromaPlanes) -> f64 {
        match decoded {
            ChromaPlanes::Bit8 { u, v } => {
                let mut min = f64::INFINITY;
                let mut max = f64::NEG_INFINITY;
                for plane in [u.as_slice(), v.as_slice()] {
                    for bar in 0..BAR_COUNT {
                        let (x0, x1) = bar_region(bar);
                        let mean = region_mean_8bit(
                            plane,
                            WIDTH / 2,
                            x0 / 2,
                            x1 / 2,
                            CHROMA_ROW_START / 2,
                            CHROMA_ROW_END / 2,
                        );
                        min = min.min(mean);
                        max = max.max(mean);
                    }
                }
                max - min
            }
            ChromaPlanes::Bit10 { u, v } => {
                let mut min = f64::INFINITY;
                let mut max = f64::NEG_INFINITY;
                for plane in [u.as_slice(), v.as_slice()] {
                    for bar in 0..BAR_COUNT {
                        let (x0, x1) = bar_region(bar);
                        let mean = region_mean_10bit(
                            plane,
                            WIDTH / 2,
                            x0 / 2,
                            x1 / 2,
                            CHROMA_ROW_START / 2,
                            CHROMA_ROW_END / 2,
                        );
                        min = min.min(mean);
                        max = max.max(mean);
                    }
                }
                max - min
            }
        }
    }

    /// 参照フレームとデコード結果の Y プレーンの PSNR を計算する
    fn luma_psnr(
        decode_format: DecodeFormat,
        reference: &ReferenceFrame,
        frame: &DecodedFrameInfo,
    ) -> f64 {
        match decode_format {
            DecodeFormat::Nv12 => psnr_y(&reference.y, &frame.y_data, frame.pitch, WIDTH, HEIGHT),
            DecodeFormat::P010 => {
                let expected: Vec<u16> = reference
                    .y
                    .iter()
                    .map(|&value| (value as u16) << 2)
                    .collect();
                psnr_y_10bit(&expected, &frame.y_data, frame.pitch, WIDTH, HEIGHT)
            }
        }
    }

    /// デコード結果のルマのサンプル値を返す
    ///
    /// `row` は `frame.height` 未満、`col` は `frame.width` 未満であること。
    ///
    /// # Panics
    ///
    /// `row` または `col` がフレームの範囲外の場合はパニックする。
    fn luma_sample(frame: &DecodedFrameInfo, format: DecodeFormat, row: usize, col: usize) -> f64 {
        match format {
            DecodeFormat::Nv12 => frame.y_data[row * frame.pitch + col] as f64,
            DecodeFormat::P010 => {
                let offset = row * frame.pitch + col * 2;
                (u16::from_le_bytes([frame.y_data[offset], frame.y_data[offset + 1]]) >> 6) as f64
            }
        }
    }

    /// デコード結果のルマの分散を返す
    fn luma_variance(frame: &DecodedFrameInfo, format: DecodeFormat) -> f64 {
        let count = (frame.width * frame.height) as f64;
        let mut sum = 0.0;
        let mut sum_sq = 0.0;
        for row in 0..frame.height {
            for col in 0..frame.width {
                let value = luma_sample(frame, format, row, col);
                sum += value;
                sum_sq += value * value;
            }
        }
        sum_sq / count - (sum / count) * (sum / count)
    }

    /// デコード結果のルマのレンジ（最大値 - 最小値）を返す
    fn luma_range(frame: &DecodedFrameInfo, format: DecodeFormat) -> f64 {
        let mut min = f64::INFINITY;
        let mut max = f64::NEG_INFINITY;
        for row in 0..frame.height {
            for col in 0..frame.width {
                let value = luma_sample(frame, format, row, col);
                min = min.min(value);
                max = max.max(value);
            }
        }
        max - min
    }

    /// 2 つのデコード結果のルマの平均絶対差分を返す
    ///
    /// # Panics
    ///
    /// 2 つのフレームのサイズが異なる場合はパニックする。
    fn mean_abs_luma_diff(
        previous: &DecodedFrameInfo,
        current: &DecodedFrameInfo,
        format: DecodeFormat,
    ) -> f64 {
        assert_eq!(
            (previous.width, previous.height),
            (current.width, current.height),
            "フレーム間差分を計算するフレームのサイズが一致しない"
        );
        let mut sum = 0.0;
        for row in 0..current.height {
            for col in 0..current.width {
                let previous_sample = luma_sample(previous, format, row, col);
                let current_sample = luma_sample(current, format, row, col);
                sum += (previous_sample - current_sample).abs();
            }
        }
        sum / (current.width * current.height) as f64
    }

    /// 2 つの等長な配列のピアソン相関を返す
    ///
    /// # Panics
    ///
    /// 配列の長さが異なる場合、配列が空の場合、またはどちらかの分散が 0 の場合は
    /// パニックする。
    fn pearson_correlation(x: &[f64], y: &[f64]) -> f64 {
        assert_eq!(x.len(), y.len(), "相関を計算する配列の長さが一致しない");
        assert!(!x.is_empty(), "相関を計算する配列が空である");
        let count = x.len() as f64;
        let mean_x = x.iter().sum::<f64>() / count;
        let mean_y = y.iter().sum::<f64>() / count;
        let mut covariance = 0.0;
        let mut variance_x = 0.0;
        let mut variance_y = 0.0;
        for (xi, yi) in x.iter().zip(y) {
            let dx = xi - mean_x;
            let dy = yi - mean_y;
            covariance += dx * dy;
            variance_x += dx * dx;
            variance_y += dy * dy;
        }
        assert!(
            variance_x > 0.0 && variance_y > 0.0,
            "相関を計算する配列の分散が 0 である"
        );
        covariance / (variance_x.sqrt() * variance_y.sqrt())
    }

    /// エンコード結果のフレーム種別が想定どおりであることを検証する
    ///
    /// 先頭が IDR または I フレームで、残りが P フレームであること。
    ///
    /// # Panics
    ///
    /// フレーム数が `NUM_FRAMES` と異なる場合、またはフレーム種別が想定と異なる場合は
    /// パニックする。
    fn assert_frame_types(encoded_frames: &[EncodedFrame<usize>], label: &str) {
        assert_eq!(
            encoded_frames.len(),
            NUM_FRAMES,
            "{label}: エンコード結果のフレーム数が入力と一致しない"
        );
        assert!(
            matches!(
                encoded_frames[0].picture_type(),
                PictureType::Idr | PictureType::I
            ),
            "{label}: 先頭フレームが IDR / I フレームでない"
        );
        let p_frame_count = encoded_frames
            .iter()
            .filter(|frame| frame.picture_type() == PictureType::P)
            .count();
        assert_eq!(
            p_frame_count,
            NUM_FRAMES - 1,
            "{label}: P フレーム数が {} でない",
            NUM_FRAMES - 1
        );
    }

    /// 1 ケースを実行し、入力とデコード出力の恒等性を検証する
    fn run_case(case: &Case) {
        println!("実行: {}", case.label);

        let config = identity_config(case.codec_config.clone(), case.input_format);

        let references: Vec<ReferenceFrame> = (0..NUM_FRAMES).map(ReferenceFrame::new).collect();
        let input_frames: Vec<Vec<u8>> = references
            .iter()
            .map(|reference| input_frame(reference, case.input_format))
            .collect();

        let (encoded_frames, decoded_frames) = roundtrip(config, case.decoder_codec, &input_frames);
        assert_frame_types(&encoded_frames, case.label);

        // P010 のデコード結果は 1 サンプル 2 バイトなので、ピッチが表示幅の 2 倍以上である
        // ことを確認する。NV12 のピッチは assert_decoded_frame_layout が検証する。
        if matches!(case.decode_format, DecodeFormat::P010) {
            for frame in &decoded_frames {
                assert!(
                    frame.pitch >= WIDTH * 2,
                    "{}: P010 のデコード結果のピッチ {} が表示幅 {} の 2 倍未満",
                    case.label,
                    frame.pitch,
                    WIDTH
                );
            }
        }

        for (index, frame) in decoded_frames.iter().enumerate() {
            // gop_ref_dist = 1 なので B フレームによる表示順の並び替えは起きない。
            // デコーダーが decode 呼び出し順にフレームを返すことを確認する。エンコーダー
            // 出力の順序は、各フレームの PSNR が入力順の参照フレームと一致することで検証する。
            assert_eq!(
                frame.user_data, index,
                "{}: フレーム {index} の user_data が入力の順序と一致しない",
                case.label
            );
            let reference = &references[index];

            // ルマの PSNR
            let y_psnr = luma_psnr(case.decode_format, reference, frame);
            assert!(
                y_psnr >= PSNR_MIN_DB,
                "{}: フレーム {index} の Y PSNR {y_psnr:.1} dB が下限 {PSNR_MIN_DB:.1} dB 未満",
                case.label
            );

            // クロマの PSNR または色バー領域平均
            let decoded_chroma_planes = decoded_chroma(frame, case.decode_format);
            match case.chroma_check {
                ChromaCheck::Psnr => {
                    let expected_chroma_planes = expected_chroma(reference, case.decode_format);
                    let (u_psnr, v_psnr) =
                        chroma_psnr(&expected_chroma_planes, &decoded_chroma_planes);
                    assert!(
                        u_psnr >= PSNR_MIN_DB && v_psnr >= PSNR_MIN_DB,
                        "{}: フレーム {index} の U PSNR {u_psnr:.1} dB / V PSNR {v_psnr:.1} dB が下限 {PSNR_MIN_DB:.1} dB 未満",
                        case.label
                    );
                }
                ChromaCheck::RegionMean => {
                    let diff = max_chroma_mean_diff(reference, &decoded_chroma_planes);
                    let tolerance = match case.decode_format {
                        DecodeFormat::Nv12 => CHROMA_MEAN_TOLERANCE_8BIT,
                        DecodeFormat::P010 => CHROMA_MEAN_TOLERANCE_10BIT,
                    };
                    assert!(
                        diff <= tolerance,
                        "{}: フレーム {index} のクロマ領域平均の最大差 {diff:.2} が許容値 {tolerance:.1} を超えた",
                        case.label
                    );
                }
            }

            // 以下は診断用のサニティチェックである。Y PSNR とクロマの検証（U / V PSNR
            // または色バー領域平均）が下限を満たせば多くは自動的に満たされるが、単色・
            // 静止画・プレーン欠落といった壊れ方をしたときに原因を特定しやすくするために残す。
            // クロマが単色でないこと（色バー間で平均が変化すること）
            let spread = chroma_spread(&decoded_chroma_planes);
            let minimum_spread = match case.decode_format {
                DecodeFormat::Nv12 => CHROMA_SPREAD_MIN_8BIT,
                DecodeFormat::P010 => CHROMA_SPREAD_MIN_10BIT,
            };
            assert!(
                spread >= minimum_spread,
                "{}: フレーム {index} の色バー間のクロマ平均の広がり {spread:.1} が下限 {minimum_spread:.1} 未満",
                case.label
            );

            // ルマが単色でないこと
            let variance = luma_variance(frame, case.decode_format);
            let minimum_variance = match case.decode_format {
                DecodeFormat::Nv12 => LUMA_VARIANCE_MIN_8BIT,
                DecodeFormat::P010 => LUMA_VARIANCE_MIN_10BIT,
            };
            assert!(
                variance >= minimum_variance,
                "{}: フレーム {index} のルマ分散 {variance:.1} が下限 {minimum_variance:.1} 未満",
                case.label
            );

            // フレームごとの移動パターンがデコード結果にも現れていること
            if index > 0 {
                let diff =
                    mean_abs_luma_diff(&decoded_frames[index - 1], frame, case.decode_format);
                let minimum_diff = match case.decode_format {
                    DecodeFormat::Nv12 => FRAME_DIFF_MIN_8BIT,
                    DecodeFormat::P010 => FRAME_DIFF_MIN_10BIT,
                };
                assert!(
                    diff >= minimum_diff,
                    "{}: フレーム {index} と前フレームの平均絶対差分 {diff:.2} が下限 {minimum_diff:.1} 未満",
                    case.label
                );
            }
        }
    }

    /// 複数のケースを順に実行する
    fn run_cases(cases: &[Case]) {
        for case in cases {
            run_case(case);
        }
    }

    /// H.264 High の 8bit 入力を NV12 にデコードする経路
    #[test]
    fn test_identity_h264_formats() {
        run_cases(&[
            Case {
                label: "H.264 High / NV12 -> NV12",
                codec_config: h264_high(),
                decoder_codec: VideoCodecType::H264,
                input_format: FrameFormat::Nv12,
                decode_format: DecodeFormat::Nv12,
                chroma_check: ChromaCheck::Psnr,
            },
            Case {
                label: "H.264 High / YUY2 -> NV12",
                codec_config: h264_high(),
                decoder_codec: VideoCodecType::H264,
                input_format: FrameFormat::Yuy2,
                decode_format: DecodeFormat::Nv12,
                chroma_check: ChromaCheck::RegionMean,
            },
            Case {
                label: "H.264 High / AYUV -> NV12",
                codec_config: h264_high(),
                decoder_codec: VideoCodecType::H264,
                input_format: FrameFormat::Ayuv,
                decode_format: DecodeFormat::Nv12,
                chroma_check: ChromaCheck::RegionMean,
            },
        ]);
    }

    /// HEVC Main の 8bit 入力を NV12 にデコードする経路
    #[test]
    fn test_identity_hevc_formats() {
        run_cases(&[
            Case {
                label: "HEVC Main / NV12 -> NV12",
                codec_config: hevc_main(),
                decoder_codec: VideoCodecType::Hevc,
                input_format: FrameFormat::Nv12,
                decode_format: DecodeFormat::Nv12,
                chroma_check: ChromaCheck::Psnr,
            },
            Case {
                label: "HEVC Main / YUY2 -> NV12",
                codec_config: hevc_main(),
                decoder_codec: VideoCodecType::Hevc,
                input_format: FrameFormat::Yuy2,
                decode_format: DecodeFormat::Nv12,
                chroma_check: ChromaCheck::RegionMean,
            },
            Case {
                label: "HEVC Main / AYUV -> NV12",
                codec_config: hevc_main(),
                decoder_codec: VideoCodecType::Hevc,
                input_format: FrameFormat::Ayuv,
                decode_format: DecodeFormat::Nv12,
                chroma_check: ChromaCheck::RegionMean,
            },
        ]);
    }

    /// HEVC Main10 の 10bit 入力を P010 にデコードする経路
    #[test]
    fn test_identity_hevc_main10_formats() {
        run_cases(&[
            Case {
                label: "HEVC Main10 / P010 -> P010",
                codec_config: hevc_main10(),
                decoder_codec: VideoCodecType::Hevc,
                input_format: FrameFormat::P010,
                decode_format: DecodeFormat::P010,
                chroma_check: ChromaCheck::Psnr,
            },
            Case {
                label: "HEVC Main10 / Y410 -> P010",
                codec_config: hevc_main10(),
                decoder_codec: VideoCodecType::Hevc,
                input_format: FrameFormat::Y410,
                decode_format: DecodeFormat::P010,
                chroma_check: ChromaCheck::RegionMean,
            },
        ]);
    }

    /// AV1 Main の NV12 / P010 入力を同じフォーマットにデコードする経路
    #[test]
    fn test_identity_av1_formats() {
        run_cases(&[
            Case {
                label: "AV1 Main / NV12 -> NV12",
                codec_config: av1_main(),
                decoder_codec: VideoCodecType::Av1,
                input_format: FrameFormat::Nv12,
                decode_format: DecodeFormat::Nv12,
                chroma_check: ChromaCheck::Psnr,
            },
            Case {
                label: "AV1 Main / P010 -> P010",
                codec_config: av1_main(),
                decoder_codec: VideoCodecType::Av1,
                input_format: FrameFormat::P010,
                decode_format: DecodeFormat::P010,
                chroma_check: ChromaCheck::Psnr,
            },
        ]);
    }

    /// VP9 Profile0 / Profile2 の入力を同じフォーマットにデコードする経路
    #[test]
    fn test_identity_vp9_formats() {
        run_cases(&[
            Case {
                label: "VP9 Profile0 / NV12 -> NV12",
                codec_config: vp9(Vp9Profile::Profile0),
                decoder_codec: VideoCodecType::Vp9,
                input_format: FrameFormat::Nv12,
                decode_format: DecodeFormat::Nv12,
                chroma_check: ChromaCheck::Psnr,
            },
            Case {
                label: "VP9 Profile2 / P010 -> P010",
                codec_config: vp9(Vp9Profile::Profile2),
                decoder_codec: VideoCodecType::Vp9,
                input_format: FrameFormat::P010,
                decode_format: DecodeFormat::P010,
                chroma_check: ChromaCheck::Psnr,
            },
        ]);
    }

    /// BGRA のグレーランプフレームと、期待するグレー値の配列を生成する
    ///
    /// 返り値は (BGRA バッファ, 画素ごとのグレー値) である。R = G = B のグレーランプを生成する。
    fn bgra_gray_frame(frame_index: usize) -> (Vec<u8>, Vec<f64>) {
        // R = G = B のグレー値は、輝度に変換すると画素値に対して単調に増える
        let mut data = Vec::new();
        let mut expected = Vec::new();
        for row in 0..HEIGHT {
            for col in 0..WIDTH {
                // 行方向にも値を変え、上下反転を検出できるようにする。行の入れ替え、
                // 横方向の 1 画素ずれ、B / R の入れ替えは検出できない。
                let value = u8::try_from(
                    16 + col * 140 / (WIDTH - 1) + row * 40 / (HEIGHT - 1) + frame_index * 3,
                )
                .expect("BGRA のグレー値が u8 の範囲を超えた");
                data.push(value);
                data.push(value);
                data.push(value);
                data.push(255);
                expected.push(value as f64);
            }
        }
        (data, expected)
    }

    /// BGRA 入力のルマ検証
    ///
    /// グレーランプに対する輝度の相関・平均・レンジ・分散と、フレーム間差分を検証する。
    #[test]
    fn test_identity_h264_bgra_luma() {
        println!("実行: H.264 High / BGRA -> NV12");

        // 入力の BGRA は RGB、デコード結果は YUV の Y であり、値のスケールが異なるため
        // 直接 PSNR で比較できない
        let config = identity_config(h264_high(), FrameFormat::Bgra);

        let (input_frames, expected_values): (Vec<Vec<u8>>, Vec<Vec<f64>>) =
            (0..NUM_FRAMES).map(bgra_gray_frame).unzip();
        let (encoded_frames, decoded_frames) =
            roundtrip(config, VideoCodecType::H264, &input_frames);
        assert_frame_types(&encoded_frames, "BGRA");

        for (index, frame) in decoded_frames.iter().enumerate() {
            assert_eq!(
                frame.user_data, index,
                "BGRA: フレーム {index} の user_data が入力の順序と一致しない"
            );
            let expected = &expected_values[index];
            let mut decoded = Vec::new();
            for row in 0..HEIGHT {
                for col in 0..WIDTH {
                    decoded.push(luma_sample(frame, DecodeFormat::Nv12, row, col));
                }
            }

            let correlation = pearson_correlation(expected, &decoded);
            assert!(
                correlation >= BGRA_LUMA_CORRELATION_MIN,
                "BGRA: フレーム {index} のグレーランプとデコード結果の相関 {correlation:.4} が下限 {BGRA_LUMA_CORRELATION_MIN} 未満"
            );

            // デコード結果の Y は limited range であるため、期待するグレー平均も
            // limited range の Y に変換して比較する。フレームの取り違えは平均値の差として現れる
            let expected_mean =
                16.0 + 219.0 / 255.0 * (expected.iter().sum::<f64>() / expected.len() as f64);
            let decoded_mean = decoded.iter().sum::<f64>() / decoded.len() as f64;
            assert!(
                (decoded_mean - expected_mean).abs() <= BGRA_LUMA_MEAN_TOLERANCE,
                "BGRA: フレーム {index} のルマ平均 {decoded_mean:.2} が期待値 {expected_mean:.2} から許容差 {BGRA_LUMA_MEAN_TOLERANCE} 以上離れている"
            );

            let range = luma_range(frame, DecodeFormat::Nv12);
            assert!(
                range >= BGRA_LUMA_RANGE_MIN,
                "BGRA: フレーム {index} のルマレンジ {range:.1} が下限 {BGRA_LUMA_RANGE_MIN} 未満"
            );

            let variance = luma_variance(frame, DecodeFormat::Nv12);
            assert!(
                variance >= BGRA_LUMA_VARIANCE_MIN,
                "BGRA: フレーム {index} のルマ分散 {variance:.1} が下限 {BGRA_LUMA_VARIANCE_MIN} 未満"
            );

            if index > 0 {
                let diff =
                    mean_abs_luma_diff(&decoded_frames[index - 1], frame, DecodeFormat::Nv12);
                assert!(
                    diff >= BGRA_FRAME_DIFF_MIN,
                    "BGRA: フレーム {index} と前フレームの平均絶対差分 {diff:.2} が下限 {BGRA_FRAME_DIFF_MIN} 未満"
                );
            }
        }
    }
}
