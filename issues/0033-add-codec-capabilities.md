# エンコーダー / デコーダーのケーパビリティ照会を拡充する

- Priority: High
- Created: 2026-11-04
- Completed: {YYYY-MM-DD}
- Model: DeepSeek V4 Flash
- Branch: feature/add-codec-capabilities
- Polished: {YYYY-MM-DD}

## 目的

利用側がバックエンドを問わず同じ情報でエンジン選択・入力形式の検証を行えるようにする。

nvcodec-rs は `query_encoder_caps` / `query_decoder_caps` で最大・最小解像度、10bit / 4:4:4 / 4:2:2 の可否、B フレーム数、レート制御モードなどを返し、`supported_codecs` の `CodecInfo` にも同じ情報を含める。video-toolbox-rs は `supported_codecs` のエンコーダー一覧と `query_encoding_capabilities(codec, width, height)` で解像度別の情報を返す。

vpl-rs の `supported_codecs` は対応可否・ハードウェアアクセラレーション・プロファイルしか返さず、解像度範囲・対応色形式・レベルが分からない。

## 現状

- `src/codec_info.rs` の `supported_codecs()` は `MFXEnumImplementations` で `mfxImplDescription` を取得し、`Dec.Codecs` / `Enc.Codecs` の `CodecID` だけを見て対応可否を判定している。
- `mfxDecoderDescription.decoder.decprofile.decmemdesc` と `mfxEncoderDescription.encoder.encprofile.encmemdesc` には `Width` / `Height` (`mfxRange32U` の Min / Max / Step) と `ColorFormats` (FourCC 配列) が入っているが、読んでいない。
- `MaxcodecLevel` も読んでいない。
- `EncodingInfo::supports_multi_pass` は VPL に対応する情報が無いため常に false を返している。
- VPL の `mfxEncExtDescription.RateControlMethods` は `ONEVPL_EXPERIMENTAL` ビルドでのみ提供されるため、レート制御モードの一覧は取得できない。
- VPL には B フレーム数の上限を返すフィールドが無く、`BiDirectionalPrediction` (bool) だけがある。

## 実機確認

Meteor Lake (Intel Arc Graphics, `mfx-gen`) の `mfxImplDescription` で確認した。

- エンコード: AV1 / VP9 / AVC / MPEG-2 / HEVC / JPEG の 6 コーデック
- デコード: VC1 / AV1 / VP8 / VP9 / AVC / VVC / MPEG-2 / HEVC / JPEG の 9 コーデック
- 各コーデックのプロファイルとメモリタイプごとに `Width` / `Height` の範囲と `ColorFormats` が入っている (`MemType=1` がシステムメモリ)
- 解像度範囲の例: AVC エンコード 16..4096 (step 16)、HEVC エンコード 8..16384 (step 8)、AV1 エンコード 128..8192 (step 8)
- `MaxcodecLevel` の例: AVC 52、HEVC 318、AV1 53
- `ColorFormats` には P010 / NV12 / AYUV / Y410 / P016 / Y210 / Y216 / Y416 / RG10 / P210 / BGR4 / RGB4 / YUY2 などが含まれる。`FrameFormat` で表現できない FourCC は無視する
- デコードの `ColorFormats` はプロファイルごとに異なる (例: HEVC Main は NV12 のみ、Main10 は NV12 と P010、Rext は 4:4:4 の形式も含む)

## 設計方針

- `DecodingInfo` / `EncodingInfo` に次を追加する。
  - `min_width` / `max_width` / `min_height` / `max_height` (解像度範囲)
  - `max_codec_level` (`MaxcodecLevel`)
  - `color_formats: Vec<FrameFormat>` (対応色形式)
- 解像度範囲と色形式は、プロファイルとメモリタイプごとに定義されている。システムメモリ (`MFX_RESOURCE_SYSTEM_SURFACE`) のエントリを対象に、解像度範囲は最小値の最小・最大値の最大を集約し、色形式は和集合を返す。
- `ColorFormats` の FourCC は `FrameFormat` にマッピングする。未知の FourCC は無視する。issue 0034 で追加する形式も含めること。
- `supports_multi_pass` は削除するか、VPL に対応する情報が無いことを rustdoc に明記する。常に false を返すフィールドは誤解を招くため、削除を優先する。
- B フレーム数の上限は提供しない。`supports_frame_reordering` で代替する。
- レート制御モードの一覧は提供しない。experimental API が必要になった時点で別 issue にする。
- 新しい query 関数は追加せず、`supported_codecs()` の戻り値を拡張する。
- 値が取得できない場合 (プロファイルやメモリタイプが空) は 0 や空 `Vec` を返す。その意味を rustdoc に明記する。

## 完了条件

1. `supported_codecs()` がエンコード・デコードの両方で解像度範囲・最大レベル・対応色形式を返す。
2. CI の Intel GPU で、NV12 が `color_formats` に含まれること、解像度範囲が 0 でないことをテストで固定する。
3. 解像度範囲と色形式の集約規則 (プロファイル・メモリタイプ) を rustdoc に明記する。
4. `supports_multi_pass` の扱い (削除または明記) が決まっている。
5. `CHANGES.md` と `README.md` / `skills/shiguredo-vpl/SKILL.md` を更新する。

## 参考

- VPL の仕様: `mfxEncoderDescription` / `mfxDecoderDescription` (`api/vpl/mfxcommon.h`)
- 同種の API: nvcodec-rs の `query_encoder_caps` / `query_decoder_caps`、video-toolbox-rs の `query_encoding_capabilities`
