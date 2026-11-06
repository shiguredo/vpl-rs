# 変更履歴

- CHANGE
  - 後方互換のない変更
- ADD
  - 後方互換がある追加
- UPDATE
  - 後方互換がある変更
- FIX
  - バグ修正

## develop

- [CHANGE] プロファイル型とコーデック識別型を統合する
  - `H264EncodingProfile` / `HevcEncodingProfile` / `Vp9EncodingProfile` / `Av1EncodingProfile` を削除し、`H264Profile` / `HevcProfile` / `Vp9Profile` / `Av1Profile` に統合する
  - `DecoderCodec` を削除し、`DecoderConfig` のコーデックを `VideoCodecType` で指定するよう変更する
  - @melpon
- [CHANGE] `Encoder::get_video_param` の戻り値を `VideoParam` に変更する
  - 要求した拡張バッファの実効値を `ExtParams` として返す
  - @melpon
- [CHANGE] `Encoder::get_encode_stat` と旧 `EncoderStats` を削除する
  - `EncoderStats` を共有統計型として定義し直し、`num_frame` / `num_bit` / `num_cached_frame` を廃止する
  - VPL の生統計 (`mfxEncodeStat`) を返す API はなくなり、`Encoder::stats` で統計を取得する
  - @melpon
- [CHANGE] `supported_codecs` が返すコーデック情報を VPL の実装記述に合わせて拡充する
  - `CodecInfo::decoding` / `CodecInfo::encoding` を `Option` に変更し、非対応コーデックでは `None` を返す
  - `DecodingInfo` / `EncodingInfo` から `supported` / `hardware_accelerated` / `supports_multi_pass` を削除する
  - `DecodingInfo` / `EncodingInfo` に `max_codec_level: Option<u16>` を追加し、レベルが不明な場合は `None` にする
  - プロファイルごとの `ProfileCapabilities` と、メモリタイプごとの `MemoryTypeCapabilities` を追加する
  - `MemoryTypeCapabilities` は幅・高さの範囲 (`ValueRange`) と色形式をメモリタイプ別に返し、プロファイルやメモリタイプをまたいだ集約はしない
  - `VideoProfile` / `MemoryType` / `ValueRange` を公開し、未知のプロファイルとメモリタイプは `Unknown(u32)` で保持する
  - `EncodingProfiles` を削除する
  - @melpon
- [ADD] エンコーダーとデコーダーの統計 API を追加する
  - `Encoder::stats` / `Decoder::stats` で受理回数・出力フレーム数・エラー数・出力ビット数・in-flight フレーム数を取得できる
  - `EncoderStats` に reconfigure 成功回数を追加し、`DecoderStats` を追加する
  - 統計の型として `Counter` / `Gauge` を公開する
  - @melpon
- [ADD] エンコーダーから SPS / PPS / VPS を取得する API を追加する
  - `Encoder::get_sequence_params` でスタートコードを含まない NAL / OBU を取得できる
  - `SequenceParams` を公開し、AV1 は Sequence Header OBU、VP9 は `None` を返す
  - @melpon
- [FIX] Decoder の user_data 対応付けを TimeStamp 完全一致方式に変更する
  - B フレームの表示順並び替えがあっても user_data が正しい入力フレームに紐付くようになる
  - @melpon
- [FIX] reconfigure 時に Init 時の拡張バッファを再送するよう修正する
  - `EncoderConfig::look_ahead_depth` / `EncoderConfig::qvbr_quality` / `Vp9EncoderConfig::write_ivf_headers` が reconfigure 後も維持される
  - @melpon
- [FIX] reconfigure が pending frame を回収してから Reset するよう修正する
  - reconfigure 時に未出力フレームが `Ok` で通知され、`finish()` の残留エラーにならない
  - @melpon
- [FIX] `#[cfg(target_os = "linux")]` を削除し、Linux x86_64 以外のターゲットでは `compile_error!` でビルドを拒否する
  - 非対応ターゲットでリンカの未解決シンボルエラーではなく、意図が明確なコンパイルエラーを返す
  - 非 Linux 向けに空 `Vec` を返していた `list_adapters()` を削除する
  - @melpon

### misc

- [ADD] silent pass していたテストを修正する
  - `frame_surface_gpu_required` でテスト用エンコーダーを初期化し、早期 return を assert に置き換える
  - `test_list_adapters_sorted_and_deduped` に空 Vec の自動パスを防ぐ assert を追加する
  - @melpon
- [UPDATE] VPL ローダーの初期化処理を `VplLoader` に集約する
  - セッション生成・アダプタ列挙・コーデック照会で重複していたローダーの生成とフィルタ設定を共通化する
  - ローダーの解放を `VplLoader` の Drop に統一する
  - @melpon
- [UPDATE] Rust ツールチェーンを rust-version と同じ 1.93 に固定する
  - CI と devcontainer のツールチェーンも 1.93 に合わせる
  - @voluntas
- [UPDATE] 公開型から `#[non_exhaustive]` を削除する
  - `DecoderConfig` / `EncoderConfig` / `AdapterSelector` / `MediaAdapterType` / `PciAddress` / `AdapterInfo` が対象
  - 利用側で `match` の網羅性チェックと構造体リテラルによる構築が可能になる
  - 今後フィールドや variant を追加する場合は後方互換のない変更として扱う
  - @melpon
- [FIX] docs.rs のビルドでネットワークに接続できないためドキュメント生成に失敗する問題を修正する
  - `DOCS_RS` 設定時は libvpl の取得と bindgen をスキップし、ドキュメント生成に必要な最小限のダミー定義を出力する
  - @melpon

## 2026.4.0

**リリース日**: 2026-08-23

- [CHANGE] MSRV (rust-version) を 1.88 から 1.93 に上げる
  - @voluntas
- [CHANGE] `Vp9EncoderConfig` に `write_ivf_headers` を追加する
  - `Encoder` が oneVPL へ要求した値を返す `Encoder::write_ivf_headers` getter を追加する
  - 初期化時に `mfxExtVP9Param::WriteIVFHeaders` の実効値を読み戻し、要求値と一致しない場合はエラーを返す
  - `Vp9EncoderConfig` を構造体リテラルで構築する既存コードは `write_ivf_headers` の明示指定が必須になりコンパイル不能になる
  - Intel GPU の oneVPL は `WriteIVFHeaders` が既定で ON のため、従来の IVF 付き出力を維持するには `write_ivf_headers: true` を指定する
  - @melpon
- [UPDATE] libvpl を 2.16.0 から 2.17.0 に更新する
  - @voluntas
- [FIX] Decoder の DEVICE_BUSY / MORE_SURFACE リトライに上限を設ける
  - Encoder と同様に 1ms スリープで最大 30 回リトライし、上限超過時はエラーを返す
  - `finish` のドレインループの `FrameSurface::new` 呼び出し順序を `decode_bitstream` と統一する
  - @melpon

### misc

- [ADD] CI に ubuntu-26.04 を追加する
  - @voluntas

## 2026.3.0

**リリース日**: 2026-06-23

- [UPDATE] Guard 系の実装をやめてリソースに対するライフタイムで管理する
  - `VplLibrary` / `frame_type` / `gop_opt_flag` を `src/vpl.rs` に移動する
  - `SurfaceGuard` / `DecodedSurfaceGuard` を `FrameSurface` に統合する
  - `Session` 型を新設し、Encoder/Decoder の lib/loader/session フィールドを統合する
  - `CloseGuard` を廃止し、`Session` の Drop による RAII 解放に移行する
  - @melpon
- [CHANGE] Decoder の完了通知をハンドラートレイト方式に変更する
  - Decoder が型パラメータ `Decoder<H: DecodeHandler>` になり、コンストラクタでハンドラーを受け取るように変更
  - `DecodeHandler` トレイト (`on_decoded`) と `FnDecodeHandler<T, E>` ラッパーを追加
  - `Decoder::decode` のシグネチャが `decode(&mut self, data: &[u8], user_data: H::UserData)` に変更
  - デコード結果はハンドラー経由で `DecodedFrame<'_, T>` として受け取る
  - `DecodedFrame` は y()/uv() スライスと pitch() を提供し、ハンドラー呼び出し中のみ有効
  - ハンドラーの `Error` 関連型によりカスタムエラー型が利用可能
  - `Decoder::next_frame` を削除
  - `DecoderConfig` に `async_depth` フィールドを追加（None の場合は 4）
  - `surface_work=NULL` の VPL 内部割り当て方式に移行し、自前のサーフェスプール管理を廃止
  - @melpon
- [CHANGE] Encoder の完了通知をハンドラートレイト方式に変更する
  - Encoder が型パラメータ `Encoder<H: EncodeHandler>` になり、コンストラクタでハンドラーを受け取るように変更
  - `EncodeHandler` トレイト (`on_encoded`) と `FnEncodeHandler<T, E>` ラッパーを追加
  - `Encoder::encode` のシグネチャが `encode(&mut self, data: &[u8], user_data: H::UserData)` に変更
  - エンコード結果はハンドラー経由で `EncodedFrame<T>` として受け取る
  - `EncodedFrame` に `user_data()` / `into_user_data()` メソッドを追加
  - ハンドラーの `Error` 関連型によりカスタムエラー型が利用可能
  - `MFXMemory_GetSurfaceForEncode` による VPL 内部サーフェス利用に移行し、自前のサーフェスプール管理を廃止
  - `EncoderConfig` に `async_depth` フィールドを追加（None の場合は 4）
  - DEVICE_BUSY 最大リトライ回数を 10 → 30 に変更
  - ドレイン時の空ビットストリームをエラーではなく空データとして正常処理するよう修正
  - @melpon

### misc

- [ADD] container コマンドで macOS から clippy 検証ができるようにする
  - Apple Silicon Mac の arm64 ネイティブで x86_64-unknown-linux-gnu へクロスビルドする
  - `.devcontainer/Dockerfile` を VS Code Dev Container と prek の cargo-clippy フックで共用する
  - `make container-build` でイメージを用意し `prek run cargo-clippy` で実行する
  - @voluntas

## 2026.2.0

**リリース日**: 2026-05-13

- [ADD] shiguredo_vpl::list_adapters と AdapterSelector / AdapterInfo / PciAddress / MediaAdapterType を追加する
  - @voluntas
- [CHANGE] EncoderConfig::new / DecoderConfig::new / codec_info::supported_codecs にアダプタ指定を必須化する
  - @voluntas

## 2026.1.2

**リリース日**: 2026-04-08

- [FIX] 正しいアライメントのバッファを VPL エンコーダーに渡すように修正
  - 以前はアライメントされていないサイズでバッファを計算していたため、バッファサイズが足りずに SIGSEGV が発生していた
  - @melpon

## 2026.1.1

**リリース日**: 2026-04-08

- [FIX] パック済みフォーマットでも Y/U/V に値を設定する
  - mfxFrameData のドキュメントに、NV12 や YUY2 のようなフォーマットの場合であっても、Y/U/V をそれぞれ設定する必要があると書かれているため
  - @melpon

## 2026.1.0

**リリース日**: 2026-03-31
