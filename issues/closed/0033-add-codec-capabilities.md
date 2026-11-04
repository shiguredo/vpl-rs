# エンコーダー / デコーダーのケーパビリティ照会を拡充する

- Priority: High
- Created: 2026-11-04
- Completed: 2026-11-05
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
  - `max_codec_level` (`MaxcodecLevel`)
  - `profiles: Vec<ProfileCapabilities>` (プロファイルごとの情報)
- `ProfileCapabilities` は `profile: VideoProfile` と `memory_types: Vec<MemoryTypeCapabilities>` を持つ。
- `MemoryTypeCapabilities` は `memory_type: MemoryType` / `width: Range` / `height: Range` / `color_formats: Vec<FrameFormat>` を持つ。
- `Range` は VPL の `mfxRange32U` に対応し、`min` / `max` / `step` を持つ。
- 解像度範囲と色形式は、プロファイルとメモリタイプごとの定義をそのまま返す。プロファイルやメモリタイプをまたいだ集約は行わない。
- すべてのメモリタイプを返す。このクレートの `Encoder` / `Decoder` が実際に使うのは `MemoryType::System` だけであることを rustdoc に明記する。
- `CodecInfo` の `decoding` / `encoding` を `Option` にし、非対応コーデックは `None` を返す。`DecodingInfo` / `EncodingInfo` の `supported` / `hardware_accelerated` と `EncodingProfiles` は `Option` と per-profile 構造で表現できるため削除する。
- `ColorFormats` の FourCC は `FrameFormat` にマッピングする。未知の FourCC は無視する。issue 0034 で追加する形式も含めること。
- 未知のプロファイル ID とメモリタイプは `VideoProfile::Unknown(u32)` / `MemoryType::Unknown(u32)` として生の値を保持する。
- `supports_multi_pass` は削除するか、VPL に対応する情報が無いことを rustdoc に明記する。常に false を返すフィールドは誤解を招くため、削除を優先する。
- B フレーム数の上限は提供しない。`supports_frame_reordering` で代替する。
- レート制御モードの一覧は提供しない。experimental API が必要になった時点で別 issue にする。
- 新しい query 関数は追加せず、`supported_codecs()` の戻り値を拡張する。
- `max_codec_level` は VPL が 0 を返す場合とコーデックのエントリが無い場合に `None` を返す。プロファイルやメモリタイプが空の場合は該当する `Vec` を空にする。その意味を rustdoc に明記する。

## 完了条件

1. `supported_codecs()` がエンコード・デコードの両方で、プロファイル別・メモリタイプ別の解像度範囲・最大レベル・対応色形式を返す。
2. CI の Intel GPU で、システムメモリの `color_formats` に NV12 が含まれること、幅・高さの範囲が 0 でないことをテストで固定する。
3. VPL の階層 (コーデック → プロファイル → メモリタイプ) と `None` / 空 `Vec` の意味を rustdoc に明記する。
4. `supports_multi_pass` の扱い (削除または明記) が決まっている。
5. `CHANGES.md` と `README.md` / `skills/shiguredo-vpl/SKILL.md` を更新する。

## 参考

- VPL の仕様: `mfxEncoderDescription` / `mfxDecoderDescription` (`api/vpl/mfxcommon.h`)
- 同種の API: nvcodec-rs の `query_encoder_caps` / `query_decoder_caps`、video-toolbox-rs の `query_encoding_capabilities`

## 解決方法

`supported_codecs()` の戻り値を VPL の実装記述の階層に合わせ、プロファイル別・メモリタイプ別のケーパビリティを返すようにした。`CodecInfo` の `decoding` / `encoding` は `Option` にし、VPL に対応する情報が無く常に false を返していた `supports_multi_pass` は削除した。

### 変更内容

- `CodecInfo::decoding` / `CodecInfo::encoding` を `Option` にし、非対応コーデックでは `None` を返すようにした
- `DecodingInfo` / `EncodingInfo` の `supported` / `hardware_accelerated` と `EncodingProfiles` を削除した
- `ProfileCapabilities` / `MemoryTypeCapabilities` / `Range` / `VideoProfile` / `MemoryType` を追加した
- プロファイルやメモリタイプをまたいだ集約をやめ、VPL の `Profiles` / `MemDesc` をそのまま反映するようにした
- `Range` に VPL の `mfxRange32U` に対応する `step` を含めた
- 未知のプロファイル ID とメモリタイプを `VideoProfile::Unknown(u32)` / `MemoryType::Unknown(u32)` で保持するようにした
- `FrameFormat` に FourCC からの変換を追加し、`FrameFormat` で表現できない FourCC は無視するようにした
- `EncodingInfo::supports_multi_pass` を削除した
- 構造と `None` / 空 `Vec` の意味を rustdoc、README、`skills/shiguredo-vpl/SKILL.md` に明記した
- `CHANGES.md` の `## develop` に `[CHANGE]` として追記した

`color_formats` は `FrameFormat` で表現できる FourCC だけを返す。P010 / AYUV / Y410 は issue 0034 で `FrameFormat` に追加される予定のため現時点では含まれない。0034 で `FrameFormat::from_fourcc` の対応を増やせば `color_formats` にも反映される。

### 確認

`shiguredo-gmktec-k9` の `~/melpon/vpl-rs` で確認した。

| 確認内容 | 結果 |
| --- | --- |
| `cargo fmt --all -- --check` | pass |
| `INTEL_VPL=1 cargo clippy --workspace --all-targets -- -D warnings` | pass |
| `INTEL_VPL=1 cargo test --workspace -- --test-threads=1` | pass（lib 44 件、adapter 6 件、codec_info 1 件、roundtrip 36 件、doctest 2 件） |
| `DOCS_RS=1 cargo doc --no-deps` | pass |
