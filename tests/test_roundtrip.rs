mod helpers;

use std::sync::mpsc;
use std::time::Duration;

use helpers::test_adapter;
use shiguredo_vpl::{
    Av1EncoderConfig, Av1Profile, CodecConfig, DecodedFrame, Decoder, DecoderConfig, DecoderStats,
    EncodeOptions, EncodedFrame, Encoder, EncoderConfig, Error, FnDecodeHandler, FnEncodeHandler,
    FrameData, FrameFormat, H264EncoderConfig, H264Profile, HevcEncoderConfig, HevcProfile,
    MemoryType, PictureType, RateControlMode, ReconfigureParams, VideoCodecType, VideoProfile,
    Vp9EncoderConfig, Vp9Profile, ffi, frame_type, supported_codecs,
};

/// コールバックから取り出したデコード済みフレーム情報（データをコピーして保持する）
struct DecodedFrameInfo {
    y_data: Vec<u8>,
    uv_data: Vec<u8>,
    pitch: usize,
    width: usize,
    height: usize,
    resolution_changed: bool,
    user_data: usize,
}

/// コールバックで受け取ったフレームからテスト用の情報を構築する
fn decoded_frame_info(frame: &DecodedFrame<'_, usize>) -> DecodedFrameInfo {
    DecodedFrameInfo {
        y_data: frame.y().to_vec(),
        uv_data: frame.uv().to_vec(),
        pitch: frame.pitch(),
        width: frame.width(),
        height: frame.height(),
        resolution_changed: frame.resolution_changed(),
        user_data: *frame.user_data(),
    }
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
    let uv_height = height / 2;
    let uv_size = width * uv_height;
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

/// Y プレーン PSNR の下限 (dB)
///
/// QPI=10 の実測では 60 dB 程度。エンコーダーの実装差を考慮して、AYUV は 30 dB、
/// 10bit 入力は 35 dB を下限にする。
const AYUV_LUMA_PSNR_MIN_DB: f64 = 30.0;
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

/// デコード結果を変換して送信するデコーダと受信用チャネルを生成するヘルパー
fn create_decoder_with<F, T>(
    config: DecoderConfig,
    convert: F,
) -> (
    Decoder<FnDecodeHandler<usize>>,
    mpsc::Receiver<Result<T, Error>>,
)
where
    F: for<'a> Fn(&DecodedFrame<'a, usize>) -> T + Send + 'static,
    T: Send + 'static,
{
    let (tx, rx) = mpsc::channel::<Result<T, Error>>();
    let decoder = Decoder::new(
        config,
        FnDecodeHandler::new(move |result| {
            tx.send(result.map(|frame| convert(&frame)))
                .expect("デコード結果コールバックの送信に失敗した");
        }),
    )
    .expect("デコーダーの生成に失敗した");
    (decoder, rx)
}

/// デコード結果をコールバックで受信するデコーダと受信用チャネルを生成するヘルパー
fn create_decoder(codec: VideoCodecType) -> DecoderAndReceiver {
    create_decoder_with(DecoderConfig::new(test_adapter(), codec), |frame| {
        *frame.user_data()
    })
}

/// 指定した設定のデコーダと、デコード結果をフレーム情報として受信するチャネルを生成するヘルパー
fn create_decoder_with_config(
    config: DecoderConfig,
) -> (
    Decoder<FnDecodeHandler<usize>>,
    mpsc::Receiver<Result<DecodedFrameInfo, Error>>,
) {
    create_decoder_with(config, decoded_frame_info)
}

/// 最大サイズ付きでビットストリーム列をデコードし、結果と統計値を返す
fn decode_with_max_size(
    codec: VideoCodecType,
    bitstreams: &[Vec<u8>],
    max_width: u32,
    max_height: u32,
) -> (Vec<DecodedFrameInfo>, DecoderStats) {
    let mut config = DecoderConfig::new(test_adapter(), codec);
    config.max_width = Some(max_width);
    config.max_height = Some(max_height);
    let (mut decoder, rx) = create_decoder_with_config(config);

    for (index, bitstream) in bitstreams.iter().enumerate() {
        decoder
            .decode(bitstream, index)
            .expect("デコードに失敗した");
    }
    decoder.finish().expect("finish に失敗した");

    let mut frames = Vec::new();
    for _ in 0..bitstreams.len() {
        frames.push(
            rx.recv_timeout(Duration::from_secs(10))
                .expect("デコード結果コールバックの受信がタイムアウトした")
                .expect("デコードに失敗した"),
        );
    }
    let stats = decoder.stats().clone();
    (frames, stats)
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

/// 解像度の異なるビットストリームとエンコード結果を指定順に連結する
///
/// `segments` の各要素は `(width, height, num_frames)` で、それぞれの解像度で
/// `num_frames` 枚をエンコードして連結する。
fn encode_resolution_sequence_with_frames(
    codec: CodecConfig,
    gop_ref_dist: Option<u16>,
    segments: &[(u32, u32, usize)],
) -> (Vec<Vec<u8>>, Vec<EncodedFrame<usize>>) {
    let mut bitstreams = Vec::new();
    let mut encoded_frames = Vec::new();
    for &(width, height, num_frames) in segments {
        let frames: Vec<Vec<u8>> = (0..num_frames)
            .map(|index| generate_dummy_nv12(width as usize, height as usize, index))
            .collect();
        let mut config = test_encoder_config(codec.clone(), width, height);
        config.gop_ref_dist = gop_ref_dist;
        let (mut segment_frames, mut segment) = encode(config, &frames);
        bitstreams.append(&mut segment);
        encoded_frames.append(&mut segment_frames);
    }
    (bitstreams, encoded_frames)
}

/// 解像度の異なるビットストリームを指定順に連結する
///
/// `segments` の各要素は `(width, height, num_frames)` で、それぞれの解像度で
/// `num_frames` 枚をエンコードして連結する。
fn encode_resolution_sequence(
    codec: CodecConfig,
    gop_ref_dist: Option<u16>,
    segments: &[(u32, u32, usize)],
) -> Vec<Vec<u8>> {
    encode_resolution_sequence_with_frames(codec, gop_ref_dist, segments).0
}

/// デコードしてフレーム一覧を返すヘルパー（フレームごとに decode を呼ぶ）
fn decode(decoder_codec: VideoCodecType, bitstreams: &[Vec<u8>]) -> Vec<DecodedFrameInfo> {
    let num_frames = bitstreams.len();
    let config = DecoderConfig::new(test_adapter(), decoder_codec);
    let (tx, rx) = mpsc::channel::<Result<DecodedFrameInfo, Error>>();
    let mut decoder = Decoder::new(
        config,
        FnDecodeHandler::new(move |result| {
            let info = result.map(|frame| decoded_frame_info(&frame));
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

    for (i, frame) in decoded_frames.iter().enumerate() {
        assert_decoded_frame_layout(frame, &format!("デコード結果フレーム {i}"));
    }

    decoded_frames
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

/// H.264 CBR のテスト用エンコーダ設定を返す
///
/// 320x240 / 30fps / 1Mbps / GOP 30 の共通設定を使うテストで利用する。
fn h264_test_config() -> EncoderConfig {
    h264_config_with_size(320, 240)
}

/// 指定した解像度の H.264 CBR テスト用エンコーダ設定を返す
fn h264_config_with_size(width: u32, height: u32) -> EncoderConfig {
    test_encoder_config(h264_high_codec(), width, height)
}

/// H.264 High プロファイルのコーデック設定を返す
fn h264_high_codec() -> CodecConfig {
    CodecConfig::H264(H264EncoderConfig {
        profile: Some(H264Profile::High),
    })
}

/// HEVC Main プロファイルのコーデック設定を返す
fn hevc_main_codec() -> CodecConfig {
    CodecConfig::Hevc(HevcEncoderConfig {
        profile: Some(HevcProfile::Main),
    })
}

/// 指定したコーデックのテスト用エンコーダ設定を返す
fn test_encoder_config(codec: CodecConfig, width: u32, height: u32) -> EncoderConfig {
    let mut config = EncoderConfig::new(
        test_adapter(),
        codec,
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
            let info = result.map(|frame| decoded_frame_info(&frame));
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

/// 解像度列に沿って出力フレームのサイズ・pitch・resolution_changed を検証する
///
/// `segments` の各要素は `(width, height, num_frames)` で、入力順に出力されることを前提とする。
fn assert_resolution_sequence_frames(
    frames: &[DecodedFrameInfo],
    segments: &[(u32, u32, usize)],
    min_pitch: usize,
) {
    let total_frames: usize = segments.iter().map(|(_, _, num_frames)| num_frames).sum();
    assert_eq!(frames.len(), total_frames, "出力フレーム数が一致しない");

    let mut offset = 0;
    for (segment_index, &(width, height, num_frames)) in segments.iter().enumerate() {
        for local_index in 0..num_frames {
            let index = offset + local_index;
            let frame = &frames[index];
            assert_eq!(
                frame.user_data, index,
                "解像度変更後も user_data が入力順に対応すること"
            );
            assert_eq!(
                frame.width, width as usize,
                "フレーム {index} の width が不一致"
            );
            assert_eq!(
                frame.height, height as usize,
                "フレーム {index} の height が不一致"
            );
            assert_eq!(
                frame.resolution_changed,
                segment_index != 0 && local_index == 0,
                "フレーム {index} の resolution_changed が不一致"
            );
            assert!(
                frame.pitch >= min_pitch,
                "フレーム {index} の pitch が {min_pitch} 未満: {}",
                frame.pitch
            );
            assert_decoded_frame_layout(frame, &format!("フレーム {index}"));
        }
        offset += num_frames;
    }
}

/// 最大サイズ内で縮小した後に拡大してもデコードできることを検証する
///
/// 320x240 → 256x160 → 320x240 の 30 フレームを最大サイズ 640x480 でデコードし、
/// すべてのフレームが出力されることと、縮小・拡大の両方で resolution_changed が
/// true になることを確認する。
#[test]
fn test_decoder_max_size_shrink_and_expand() {
    let frames_per_stream = 10;
    let segments = [
        (320u32, 240u32, frames_per_stream),
        (256, 160, frames_per_stream),
        (320, 240, frames_per_stream),
    ];
    let bitstreams = encode_resolution_sequence(h264_high_codec(), None, &segments);
    let (frames, _) = decode_with_max_size(VideoCodecType::H264, &bitstreams, 640, 480);

    assert_resolution_sequence_frames(&frames, &segments, 640);
}

/// 最大サイズが最初のストリームより大きい場合に内部サーフェスが拡大されることを検証する
///
/// 256x160 の 10 フレーム → 320x240 の 10 フレームを最大サイズ 640x480 でデコードする。
/// 内部サーフェスの幅が 640 以上であることを pitch 640 以上で確認する。
#[test]
fn test_decoder_max_size_extends_initial_surface() {
    let frames_per_stream = 10;
    let segments = [
        (256u32, 160u32, frames_per_stream),
        (320, 240, frames_per_stream),
    ];
    let bitstreams = encode_resolution_sequence(h264_high_codec(), None, &segments);
    let (frames, _) = decode_with_max_size(VideoCodecType::H264, &bitstreams, 640, 480);

    assert_resolution_sequence_frames(&frames, &segments, 640);
}

/// HEVC でも最大サイズ内の解像度変更を扱えることを検証する
#[test]
fn test_decoder_max_size_hevc_extends_initial_surface() {
    let frames_per_stream = 10;
    let segments = [
        (256u32, 160u32, frames_per_stream),
        (320, 240, frames_per_stream),
    ];
    let bitstreams = encode_resolution_sequence(hevc_main_codec(), None, &segments);
    let (frames, _) = decode_with_max_size(VideoCodecType::Hevc, &bitstreams, 640, 480);

    assert_resolution_sequence_frames(&frames, &segments, 640);
}

/// B フレーム有効時でも解像度変更を扱え、統計値が矛盾しないことを検証する
#[test]
fn test_decoder_max_size_b_frames_stats() {
    let frames_per_stream = 15;
    let segments = [
        (256u32, 160u32, frames_per_stream),
        (320, 240, frames_per_stream),
    ];
    let (bitstreams, encoded_frames) =
        encode_resolution_sequence_with_frames(h264_high_codec(), Some(3), &segments);
    let b_count = encoded_frames
        .iter()
        .filter(|frame| frame.picture_type() == PictureType::B)
        .count();
    assert!(
        b_count >= 1,
        "B ピクチャが 1 枚以上生成されること (実際は {b_count} 枚)"
    );
    let (frames, stats) = decode_with_max_size(VideoCodecType::H264, &bitstreams, 640, 480);

    let mut seen = vec![0usize; bitstreams.len()];
    for frame in &frames {
        assert!(
            frame.user_data < bitstreams.len(),
            "user_data {} が入力範囲外",
            frame.user_data
        );
        seen[frame.user_data] += 1;
        let (width, height) = if frame.user_data < frames_per_stream {
            (256, 160)
        } else {
            (320, 240)
        };
        assert_eq!(
            frame.width, width,
            "user_data {} の width が不一致",
            frame.user_data
        );
        assert_eq!(
            frame.height, height,
            "user_data {} の height が不一致",
            frame.user_data
        );
        assert!(
            frame.pitch >= 640,
            "user_data {} の pitch が 640 未満: {}",
            frame.user_data,
            frame.pitch
        );
        assert_decoded_frame_layout(frame, &format!("user_data {}", frame.user_data));
    }
    for (index, count) in seen.iter().enumerate() {
        assert_eq!(*count, 1, "user_data {index} が 1 回だけ通知されること");
    }
    assert_eq!(
        frames
            .iter()
            .filter(|frame| frame.resolution_changed)
            .count(),
        1,
        "解像度変更が 1 回だけ通知されること"
    );
    assert_eq!(
        stats.total_decode_count.get(),
        bitstreams.len() as u64,
        "decode 回数が統計値と一致しない"
    );
    assert_eq!(
        stats.total_output_frame_count.get(),
        bitstreams.len() as u64,
        "出力フレーム数が統計値と一致しない"
    );
    assert_eq!(stats.total_error_count.get(), 0, "エラーが記録されている");
    assert_eq!(
        stats.in_flight_frames.get(),
        0,
        "in-flight フレームが残っている"
    );
}

/// 最大サイズを超える解像度変更がエラーになることを検証する
///
/// 最初のストリームの 10 フレームは正常にデコードできること、次の 10 フレームは
/// 正常フレームとして通知されず、decode と finish の両方がエラーになることを確認する。
fn assert_resolution_change_over_limit_fails(
    config: DecoderConfig,
    first_size: (u32, u32),
    second_size: (u32, u32),
) {
    let frames_per_stream = 10;
    let bitstreams = encode_resolution_sequence(
        h264_high_codec(),
        None,
        &[
            (first_size.0, first_size.1, frames_per_stream),
            (second_size.0, second_size.1, frames_per_stream),
        ],
    );

    let (mut decoder, rx) = create_decoder_with_config(config);

    // 最初のストリームは正常に投入できる
    for (index, bitstream) in bitstreams.iter().take(frames_per_stream).enumerate() {
        decoder
            .decode(bitstream, index)
            .expect("最初のストリームのデコードに失敗した");
    }

    let mut decode_errors = Vec::new();
    for (index, bitstream) in bitstreams.iter().enumerate().skip(frames_per_stream) {
        if let Err(error) = decoder.decode(bitstream, index) {
            decode_errors.push(error);
        }
    }
    assert!(
        !decode_errors.is_empty(),
        "解像度が大きくなった場合に decode がエラーを返さなかった"
    );
    for error in &decode_errors {
        assert_eq!(
            error.function(),
            "MFXVideoDECODE_DecodeFrameAsync",
            "解像度変更のエラーはデコード処理のコンテキストを持つこと"
        );
        let status_code = error.status_code();
        assert!(
            status_code == Some(ffi::mfxStatus_MFX_ERR_DEVICE_FAILED)
                || status_code == Some(ffi::mfxStatus_MFX_ERR_INCOMPATIBLE_VIDEO_PARAM),
            "解像度変更のエラーは MFX_ERR_DEVICE_FAILED または \
             MFX_ERR_INCOMPATIBLE_VIDEO_PARAM であること (実際は {status_code:?})"
        );
    }

    // 消費されたフレームはエラーとして通知されるため、finish も成功しない
    decoder
        .finish()
        .expect_err("解像度が大きくなった後の finish はエラーを返すこと");

    // Drop で残留フレームがエラー通知されるため、デコーダーを破棄してから回収する
    drop(decoder);

    let mut ok_count = 0usize;
    let mut error_count = 0usize;
    while let Ok(result) = rx.recv_timeout(Duration::from_millis(200)) {
        match result {
            Ok(frame) => {
                ok_count += 1;
                assert!(
                    frame.user_data < frames_per_stream,
                    "2 つ目のストリームのフレームが Ok で通知された: user_data={}",
                    frame.user_data
                );
                assert!(
                    frame.width <= first_size.0 as usize && frame.height <= first_size.1 as usize,
                    "最初のストリームより大きいフレームが正常に通知された: {}x{}",
                    frame.width,
                    frame.height
                );
                assert_decoded_frame_layout(&frame, &format!("user_data {}", frame.user_data));
            }
            Err(_) => error_count += 1,
        }
    }
    assert_eq!(
        ok_count + error_count,
        frames_per_stream * 2,
        "すべての入力フレームが 1 回ずつ通知されること"
    );
    assert_eq!(
        ok_count, frames_per_stream,
        "最初のストリームのフレームだけが Ok で通知されること"
    );
    assert!(
        error_count >= frames_per_stream,
        "大きくなった解像度のフレームがすべてエラー通知されること (エラー数: {error_count})"
    );
}

/// 最大サイズを 320x240 にした場合に、640x480 のストリームがエラーになることを検証する
#[test]
fn test_decoder_max_size_exceeded_returns_error() {
    let mut config = DecoderConfig::new(test_adapter(), VideoCodecType::H264);
    config.max_width = Some(320);
    config.max_height = Some(240);
    assert_resolution_change_over_limit_fails(config, (256, 160), (640, 480));
}

/// 最大サイズ未指定で最初のストリームより大きい解像度に変わった場合にエラーになることを検証する
#[test]
fn test_decoder_without_max_size_rejects_larger_resolution() {
    let config = DecoderConfig::new(test_adapter(), VideoCodecType::H264);
    assert_resolution_change_over_limit_fails(config, (256, 160), (320, 240));
}

/// 最大サイズが最初のストリームの表示サイズより小さい場合は初期化に失敗することを検証する
///
/// 320x240 のストリームに対して 256x160 の最大サイズを指定し、初期化がエラーになることを確認する。
#[test]
fn test_decoder_max_size_smaller_than_stream_fails() {
    let bitstreams = encode_test_bitstreams(h264_test_config(), 1);

    let mut config = DecoderConfig::new(test_adapter(), VideoCodecType::H264);
    config.max_width = Some(256);
    config.max_height = Some(160);
    let (mut decoder, _rx) = create_decoder_with_config(config);

    let error = decoder
        .decode(&bitstreams[0], 0)
        .expect_err("最初のストリームより小さい最大サイズは初期化に失敗すること");
    assert_eq!(
        error.function(),
        "Decoder::initialize",
        "初期化エラーは Decoder::initialize のコンテキストを持つこと"
    );
    assert_eq!(
        error.status_message(),
        Some("max_width (256) is less than the stream display width (320)"),
        "表示サイズより小さい max_width を示すエラーメッセージであること"
    );
}

/// 表示サイズをそのまま最大サイズに指定できることを検証する
///
/// 638x478 のストリームに対して最大サイズ 638x478 を指定し、Step への切り上げを経て
/// デコードでき、pitch が 640 以上になることを確認する。
#[test]
fn test_decoder_max_size_accepts_unaligned_display_size() {
    let width = 638u32;
    let height = 478u32;
    let num_frames = 3;
    let input_frames: Vec<Vec<u8>> = (0..num_frames)
        .map(|index| generate_dummy_nv12(width as usize, height as usize, index))
        .collect();
    let (_, bitstreams) = encode(h264_config_with_size(width, height), &input_frames);

    let (frames, _) = decode_with_max_size(VideoCodecType::H264, &bitstreams, width, height);

    for (index, frame) in frames.iter().enumerate() {
        assert_eq!(frame.user_data, index, "user_data が入力順に対応すること");
        assert_eq!(frame.width, width as usize, "width が不一致");
        assert_eq!(frame.height, height as usize, "height が不一致");
        assert!(
            !frame.resolution_changed,
            "解像度が変わっていないフレームの resolution_changed は false であること"
        );
        assert!(
            frame.pitch >= 640,
            "Step 切り上げ後の最大サイズ 640 以上の pitch であること: {}",
            frame.pitch
        );
        assert_decoded_frame_layout(frame, &format!("フレーム {index}"));
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

/// フォーマット指定のラウンドトリップテストを実行するヘルパー
///
/// 指定フォーマットでダミーフレームを生成し、エンコード→デコードして
/// フレーム数が一致することを検証する。
fn roundtrip_format(
    codec_config: CodecConfig,
    decoder_codec: VideoCodecType,
    format: FrameFormat,
    num_frames: usize,
) {
    roundtrip_format_with_size(codec_config, decoder_codec, format, num_frames, 320, 240);
}

/// フォーマット指定のラウンドトリップテストを実行するヘルパー（任意サイズ）
fn roundtrip_format_with_size(
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
        VideoCodecType::H264,
        FrameFormat::Bgra,
        10,
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

/// AYUV 入力のラウンドトリップと Y プレーンの画質検証
///
/// 入力の輝度パターンとデコード結果の Y プレーンが一致することを確認する。
/// これにより AYUV のバイト順 (V, U, Y, A) がエンコーダーに正しく伝わることが検証できる。
#[test]
fn test_roundtrip_hevc_ayuv_luma() {
    let codec = VideoCodecType::Hevc;
    let profile = VideoProfile::Hevc(HevcProfile::Main);
    if skip_if_encoder_format_unsupported(codec, profile, FrameFormat::Ayuv) {
        return;
    }

    let width = 320usize;
    let height = 240usize;
    let num_frames = 10usize;
    let mut config = EncoderConfig::new(
        test_adapter(),
        CodecConfig::Hevc(HevcEncoderConfig {
            profile: Some(HevcProfile::Main),
        }),
        width as u32,
        height as u32,
        FrameFormat::Ayuv,
        30,
        1,
        RateControlMode::Cqp,
    );
    config.qpi = Some(10);
    config.gop_pic_size = Some(30);

    let frames: Vec<Vec<u8>> = (0..num_frames)
        .map(|i| generate_dummy_frame(FrameFormat::Ayuv, width, height, i))
        .collect();
    let (_, decoded) = roundtrip(config, codec, &frames);

    for frame in &decoded {
        let frame_index = frame.user_data;
        // generate_dummy_frame と同じパターンの 8bit 輝度を期待値にする
        let expected: Vec<u8> = (0..height)
            .flat_map(|y| (0..width).map(move |x| (16 + (x + y + frame_index * 7) % 220) as u8))
            .collect();
        let psnr = psnr_y(&expected, &frame.y_data, frame.pitch, width, height);
        assert!(
            psnr >= AYUV_LUMA_PSNR_MIN_DB,
            "AYUV 入力の Y プレーンの PSNR が低すぎる (frame {frame_index}): {psnr} dB"
        );
    }
}

/// 10bit 入力の Y プレーンを検証する
///
/// 4:2:0 のプロファイルでは、4:4:4 の Y410 入力は 4:2:0 に変換され、デコード出力は P010 になる。
/// ルマは変換で失われないため、P010 の Y プレーンと比較する。
fn assert_10bit_luma_roundtrip(
    codec: VideoCodecType,
    codec_config: CodecConfig,
    profile: VideoProfile,
    format: FrameFormat,
) {
    if skip_if_encoder_format_unsupported(codec, profile, format) {
        return;
    }

    let width = 320usize;
    let height = 240usize;
    let num_frames = 5usize;
    let mut config = EncoderConfig::new(
        test_adapter(),
        codec_config,
        width as u32,
        height as u32,
        format,
        30,
        1,
        RateControlMode::Cqp,
    );
    config.qpi = Some(10);
    config.gop_pic_size = Some(30);

    let frames: Vec<Vec<u8>> = (0..num_frames)
        .map(|i| generate_dummy_frame(format, width, height, i))
        .collect();
    let (_, decoded) = roundtrip(config, codec, &frames);

    for frame in &decoded {
        let frame_index = frame.user_data;
        assert!(
            frame.pitch >= width * 2,
            "{format:?} の Y プレーンのピッチが小さすぎる (frame {frame_index}): {} < {}",
            frame.pitch,
            width * 2,
        );
        // generate_dummy_frame と同じパターンの 10bit 輝度を期待値にする
        let expected: Vec<u16> = (0..height)
            .flat_map(|y| (0..width).map(move |x| (64 + (x + y + frame_index * 7) % 900) as u16))
            .collect();
        let psnr = psnr_y_10bit(&expected, &frame.y_data, frame.pitch, width, height);
        assert!(
            psnr >= TEN_BIT_LUMA_PSNR_MIN_DB,
            "{format:?} 入力の Y プレーンの PSNR が低すぎる (frame {frame_index}): {psnr} dB"
        );
    }
}

/// P010 (10bit 4:2:0) 入力の HEVC Main10 ラウンドトリップ
#[test]
fn test_roundtrip_hevc_main10_p010() {
    assert_10bit_luma_roundtrip(
        VideoCodecType::Hevc,
        CodecConfig::Hevc(HevcEncoderConfig {
            profile: Some(HevcProfile::Main10),
        }),
        VideoProfile::Hevc(HevcProfile::Main10),
        FrameFormat::P010,
    );
}

/// Y410 (10bit 4:4:4) 入力の HEVC Main10 ラウンドトリップ
#[test]
fn test_roundtrip_hevc_main10_y410() {
    assert_10bit_luma_roundtrip(
        VideoCodecType::Hevc,
        CodecConfig::Hevc(HevcEncoderConfig {
            profile: Some(HevcProfile::Main10),
        }),
        VideoProfile::Hevc(HevcProfile::Main10),
        FrameFormat::Y410,
    );
}

/// AYUV (8bit 4:4:4) 入力の H.264 High ラウンドトリップ
#[test]
fn test_roundtrip_h264_ayuv() {
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
        10,
    );
}

/// P010 (10bit 4:2:0) 入力の AV1 Main ラウンドトリップ
#[test]
fn test_roundtrip_av1_p010() {
    assert_10bit_luma_roundtrip(
        VideoCodecType::Av1,
        CodecConfig::Av1(Av1EncoderConfig {
            profile: Some(Av1Profile::Main),
        }),
        VideoProfile::Av1(Av1Profile::Main),
        FrameFormat::P010,
    );
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
    roundtrip_format_with_size(
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
    roundtrip_format_with_size(
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
    roundtrip_format_with_size(
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
    roundtrip_format_with_size(
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
    roundtrip_format_with_size(
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
    roundtrip_format_with_size(
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
    roundtrip_format_with_size(
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
    roundtrip_format_with_size(
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
