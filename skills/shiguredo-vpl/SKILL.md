---
name: shiguredo-vpl
description: shiguredo_vpl (vpl-rs) クレートの徹底リファレンス。Intel VPL (Video Processing Library) v2.17 を libvpl で static link した Rust バインディング。H.264 / H.265 / VP9 / AV1 のハードウェアエンコード・デコード、AdapterSelector による複数 Intel GPU 対応、EncoderConfig / RateControlMode の選び方、Encoder / Decoder のハンドラー方式 (EncodeHandler / DecodeHandler) と user_data 連携、async_depth の調整、frame_type / gop_opt_flag のビットフラグ、coded_size と frame_size の関係、DecodedFrame の pitch 付き借用 Y/UV プレーン、supported_codecs による解像度範囲・対応色形式の照会、MFX_ERR_MORE_DATA を含むエラー処理に関する質問で使用。
---

# shiguredo_vpl クレート

- **バージョン**: 2026.4.0 (依存する libvpl は 2.17.0)
- **リポジトリ**: https://github.com/shiguredo/vpl-rs
- **Rust エディション**: 2024 (rust-version: 1.93、`rust-toolchain.toml` の channel も 1.93 に固定)
- **ライセンス**: Apache-2.0
- **動作要件**: Linux (x86_64) + 第 6 世代 Core 以降の Intel GPU。ビルド時に git と clang が必要。

Intel VPL (Video Processing Library) を libvpl で static link した Rust バインディング。
実行時に Intel VPL 共有ライブラリは不要。

## 設計の前提

- API は `mfxVideoParam` / `mfxInfoMFX` / `mfxFrameInfo` などの VPL 構造体メンバ名に準拠する。 フィールド名を勝手に短縮しない。
- 性能より堅牢性を優先する。 速い実装より「VPL のセマンティクスに正確な」実装を選ぶ。
- 良い設計のためには破壊的変更を積極的に行う。 互換のためのレガシー API を残さない。
- 依存は最小限。 現状 `[dependencies]` は空で、`build-dependencies` に `bindgen` / `shiguredo_cmake` / `shiguredo_toml` のみ。
- VPL ヘッダの構造体・関数を呼ぶ部分は `src/sys.rs` (= bindgen 出力) と `src/vpl.rs` の `VplLibrary` / `Session` / `FrameSurface` に閉じ込める。 利用者が直接触るのは安全な高レベル API のみ。
- リソース管理はライフタイムベース。 `Session` の Drop で `MFXClose` + `MFXUnload` が、`FrameSurface` の Drop で `Unmap` + `Release` が必ず実行される。 利用者が手動解放する API は存在しない。

## 公開モジュール構成

`src/lib.rs` から再エクスポートされる API:

- `adapter` — `AdapterSelector` / `AdapterInfo` / `PciAddress` / `MediaAdapterType` / `list_adapters()`
- `codec_info` — `VideoCodecType` / `H264Profile` / `HevcProfile` / `Vp9Profile` / `Av1Profile` / `VideoProfile` / `MemoryType` / `ValueRange` / `FrameFormat` / `ProfileCapabilities` / `MemoryTypeCapabilities` / `CodecInfo` / `DecodingInfo` / `EncodingInfo`
- `decode` — `Decoder<H>` / `DecoderConfig` / `DecodedFrame<'_, T>` / `DecoderStats` / `DecodeHandler` / `FnDecodeHandler<T, E>`
- `encode` — `Encoder<H>` / `EncoderConfig` / `CodecConfig` / `EncodeOptions` / `EncodedFrame<T>` / `EncoderStats` / `SequenceParams` / `VideoParam` / `ExtParams` / `RateControlMode` / `ReconfigureParams` / `PictureType` / `EncodeHandler` / `FnEncodeHandler<T, E>`
- `error` — `Error`
- `stats` — `Counter` / `Gauge`
- `vpl::frame_type` (定数モジュール) — `UNKNOWN` / `I` / `P` / `B` / `S` / `REF` / `IDR`
- `vpl::gop_opt_flag` (定数モジュール) — `CLOSED` / `STRICT`
- `BUILD_VERSION` — ビルド時参照した libvpl のバージョン文字列

`vpl` モジュール自体は非公開で、`frame_type` / `gop_opt_flag` のみが `pub use vpl::{frame_type, gop_opt_flag};` で `shiguredo_vpl::frame_type` / `shiguredo_vpl::gop_opt_flag` として参照できる。

テスト用に `ffi` モジュールが `#[doc(hidden)]` で公開されているが、利用側コードからは呼ばない。

## アダプタの選択 (`AdapterSelector`)

複数 Intel GPU 環境ではどの物理アダプタ (= どの `/dev/dri/renderD<N>`) を使うかを必ず指定する。

```rust
use shiguredo_vpl::{AdapterSelector, list_adapters};

let adapters = list_adapters()?;
for adapter in &adapters {
    println!(
        "DRM render node {}: {} ({})",
        adapter.drm_render_node, adapter.device_name, adapter.impl_name,
    );
}
let adapter = AdapterSelector::DrmRenderNode(adapters[0].drm_render_node);
```

押さえておく挙動:

- `AdapterSelector` は `#[non_exhaustive]`。 将来 PCI アドレス指定などのバリアントが増えうる。 現状は `DrmRenderNode(u32)` のみ。
- `DrmRenderNode(0)` は libvpl 内部で「未設定」を意味する予約値。 `Encoder::new` / `Decoder::new` 系から呼ばれる `AdapterSelector::validate` で必ず弾かれる。 0 は渡さない。
- `list_adapters()` は `MFXLoad` → `MFXEnumImplementations` → `MFXUnload` を毎回まわす重い処理。 アプリ起動時に 1 回呼んで結果を保持する想定。
- 同一 `DRMRenderNodeNum` の重複エントリは除去され、`drm_render_node` 昇順で並ぶ。
- Linux x86_64 以外のターゲットでは crate 自体がビルドできないため、`list_adapters()` は Linux x86_64 でのみ利用できる。
- `AdapterInfo` も `#[non_exhaustive]`。 `drm_render_node` / `impl_name` / `device_name` / `pci_device_id` / `pci_address` / `media_adapter_type` を持つ。 取れなかった文字列フィールドは空文字列。
- `MediaAdapterType` は `Integrated` / `Discrete` / `Unknown` のいずれか。 `MFX_MEDIA_UNKNOWN` (0xFFFF) と未知値はすべて `Unknown` に丸める。

libvpl 側の関連実装:

- `libvpl/src/mfx_dispatcher_vpl_config.cpp` がプロパティ名 `"DRMRenderNodeNum"` を受理する。
- `libvpl/src/mfx_dispatcher_vpl_loader.cpp` の `LoaderCtxVPL::CreateSession` がフィルタにマッチしないと `MFX_ERR_NOT_FOUND` を返す。 vpl-rs はこのとき DRM render node 番号付きのエラーメッセージに包んで返す。

## セッション生成のフロー

`VplLibrary::create_session` が API 2.x の慣用に沿って次の順で呼ぶ:

1. `MFXLoad`
2. `MFXCreateConfig` (1 つ目) → `MFXSetConfigFilterProperty("mfxImplDescription.Impl", HARDWARE)`
3. `MFXCreateConfig` (2 つ目) → `MFXSetConfigFilterProperty("mfxExtendedDeviceId.DRMRenderNodeNum", render_node)`
4. `MFXCreateSession(loader, 0, &session)`

「HW 実装フィルタ」と「DRM render node フィルタ」を別々の `mfxConfig` ハンドルに設定するのは、 libvpl ヘッダ `mfxdispatcher.h` の `MFX_ADD_PROPERTY_U32` マクロが「1 プロパティ = 1 cfg」で組み立てるスタイルに合わせるため。 1 つにまとめると `MFX_ERR_NOT_FOUND` になる。

エンコーダ・デコーダはどちらもこの関数を内部で呼んでセッションを得てから `MFXVideoENCODE_Init` / `MFXVideoDECODE_Init` する。

## エンコーダ (`Encoder<H>`)

### 最小フロー

エンコーダはハンドラー方式。 `Encoder<H: EncodeHandler>` 型パラメータでハンドラー実装を指定し、`encode()` 呼び出しごとに `H::UserData` を一緒に渡す。 出力ビットストリームはハンドラの `on_encoded` で `EncodedFrame<H::UserData>` として渡される。

```rust
use shiguredo_vpl::{
    AdapterSelector, CodecConfig, EncodeOptions, EncodedFrame, Encoder, EncoderConfig, Error,
    FnEncodeHandler, FrameFormat, H264EncoderConfig, H264Profile, RateControlMode, frame_type,
    list_adapters,
};
use std::sync::mpsc;

let adapter = AdapterSelector::DrmRenderNode(list_adapters()?[0].drm_render_node);
let mut config = EncoderConfig::new(
    adapter,
    CodecConfig::H264(H264EncoderConfig { profile: Some(H264Profile::High) }),
    1920,
    1080,
    FrameFormat::Nv12,
    30, // framerate_num
    1,  // framerate_den
    RateControlMode::Cbr,
);
config.target_kbps = Some(5_000);

// ハンドラを mpsc にブリッジするのが定番。worker スレッドから呼ばれる
let (tx, rx) = mpsc::channel();
let handler = FnEncodeHandler::new(move |result: Result<EncodedFrame<u64>, Error>| {
    let _ = tx.send(result);
});
let mut encoder = Encoder::new(config, handler)?;

let (coded_width, coded_height) = encoder.coded_size();
let frame_size = FrameFormat::Nv12
    .frame_size(coded_width, coded_height)
    .ok_or("frame size overflowed")?;
let frame_data = vec![0u8; frame_size];

// user_data に元フレーム ID やタイムスタンプを乗せると、出力 EncodedFrame と紐付く
encoder.encode(&frame_data, /* user_data */ 0u64, &EncodeOptions { frame_type: frame_type::UNKNOWN })?;

// finish はバリア。 ここに到達した時点で worker は全フレーム処理済み
encoder.finish()?;

while let Ok(result) = rx.try_recv() {
    let encoded = result?;
    let _frame_id: &u64 = encoded.user_data();
    let _bitstream = encoded.data();
}
```

### `EncoderConfig` の主要フィールド

`EncoderConfig` は `#[non_exhaustive]`。 必須項目だけ `EncoderConfig::new(...)` で受け取り、 残りはすべて `Option<_>` で `None` = 「VPL のデフォルト」となる。

- アダプタ: `adapter`
- コーデック: `codec: CodecConfig`
  - `CodecConfig::H264(H264EncoderConfig)` / `Hevc(...)` / `Vp9(...)` / `Av1(...)`
  - 各設定は `profile: Option<...>` を持つ。 `None` でコーデックのデフォルトプロファイル。
  - `Vp9EncoderConfig` だけは `write_ivf_headers: bool` を持つ。 `true` で IVF ヘッダー付き、 `false` で raw VP9 を出力する。 Intel GPU の oneVPL は `WriteIVFHeaders` が既定で ON のため、 従来の IVF 付き出力を維持するには `true` を指定する。 構造体リテラルで構築する場合は必須フィールド。
- フレーム情報 (`mfxFrameInfo` 対応): `width` / `height` / `frame_format` / `framerate_num` / `framerate_den` / `aspect_ratio_w` / `aspect_ratio_h`
- 非同期深度: `async_depth` (`mfxVideoParam.AsyncDepth`)。 `None` の場合は 4 を使用。 1 は最小メモリだが性能が低く、4 が高スループット寄りの推奨値。
- エンコード制御 (`mfxInfoMFX` 対応): `low_power` / `brc_param_multiplier` / `target_usage` (1=最高品質, 4=バランス, 7=最高速)
- GOP 構造: `gop_pic_size` / `gop_ref_dist` (1=Bなし, 2=1B, 3=2B) / `gop_opt_flag` (`gop_opt_flag::CLOSED | STRICT`) / `idr_interval`
- レート制御: `rate_control_mode: RateControlMode` と、 そのモードで使われる union メンバ群
- スライス・参照: `num_slice` / `num_ref_frame`
- 拡張バッファ: `look_ahead_depth` / `qvbr_quality`

### `RateControlMode` と union 共用フィールド

`mfxInfoMFX` の `InitialDelayInKB` / `TargetKbps` / `MaxKbps` などは C 側で union 共用なので、 モードによって意味が変わる。 vpl-rs ではすべて別フィールドに分けているが、 **対応するモードでしか効かない** 点に注意:

| モード | 使うフィールド |
|---|---|
| `Cbr` | `initial_delay_in_kb` / `buffer_size_in_kb` / `target_kbps` |
| `Vbr` | `initial_delay_in_kb` / `buffer_size_in_kb` / `target_kbps` / `max_kbps` |
| `Cqp` | `qpi` / `qpp` / `qpb` |
| `Icq` | `icq_quality` |
| `Qvbr` | `initial_delay_in_kb` / `target_kbps` / `max_kbps` / `qvbr_quality` |
| `La` | `target_kbps` / `look_ahead_depth` |
| `Avbr` | `target_kbps` / `accuracy` / `convergence` |
| `Vcm` | `initial_delay_in_kb` / `target_kbps` / `max_kbps` |
| `LaIcq` | `icq_quality` / `look_ahead_depth` |
| `LaHrd` | `initial_delay_in_kb` / `target_kbps` / `max_kbps` / `look_ahead_depth` |

union 別メンバを同時に立てると意味不明な設定になる。 モードに応じて必要なフィールドだけ埋める。

### `FrameFormat` と `coded_size` / `frame_size`

入力フォーマットは次の 6 種類:

| 列挙子 | FourCC | チャネル |
|---|---|---|
| `FrameFormat::Nv12` | `MFX_FOURCC_NV12` | Y plane + interleaved UV plane (4:2:0 8bit) |
| `FrameFormat::P010` | `MFX_FOURCC_P010` | Y plane + interleaved UV plane (4:2:0 10bit)。 10bit の値は 16bit ワードの上位 10bit に置く |
| `FrameFormat::Yuy2` | `MFX_FOURCC_YUY2` | YUYV interleaved (4:2:2 8bit) |
| `FrameFormat::Bgra` | `MFX_FOURCC_RGB4` | BGRA packed (8bit) |
| `FrameFormat::Ayuv` | `MFX_FOURCC_AYUV` | Packed YUV 4:4:4 8bit。 バイト順は V, U, Y, A |
| `FrameFormat::Y410` | `MFX_FOURCC_Y410` | Packed YUV 4:4:4 10bit。 32bit ワードの下位から U (10bit) / Y (10bit) / V (10bit) / A (2bit) |

- `Encoder::coded_size()` は 16 ピクセルにアラインされた符号化サイズを返す。 入力バッファサイズは `width` / `height` ではなくこの coded サイズで計算する。
- `FrameFormat::frame_size(width, height) -> Option<usize>` で必要バイト数を取る。 `usize::checked_mul` 系でオーバーフロー検出する。 NV12 は `pixels * 3 / 2`、 P010 は `pixels * 3`、 YUY2 は `pixels * 2`、 BGRA / AYUV / Y410 は `pixels * 4`。
- P010 は `mfxFrameInfo.Shift` に 1 を設定する。 VPL の仕様では Shift が 1 のとき有効データが 16bit ワードの上位 10bit にある (Shift が 0 なら下位 10bit)。
- 入力フォーマットとプロファイルの組み合わせの可否は `MFXVideoENCODE_Init` の結果に従う。 事前判定は `supported_codecs()` の `MemoryTypeCapabilities::color_formats` を使うが、 その結果は初期化の成功を保証しない。
- パック形式のプレーンコピーは VPL のチャネルポインタに従う。 BGRA は `Data.B`、 AYUV は `Data.V` (メモリ上のバイト順 V, U, Y, A の先頭)、 Y410 は `Data.Y410` を行の先頭として扱う。

### `EncodeOptions.frame_type` の使い方

`frame_type` モジュール定数を **ビット OR** で組み立てる:

```rust
use shiguredo_vpl::{EncodeOptions, frame_type};

// 自動 (推奨デフォルト)
let auto = EncodeOptions { frame_type: frame_type::UNKNOWN };

// IDR を強制
let idr = EncodeOptions {
    frame_type: frame_type::IDR | frame_type::I | frame_type::REF,
};
```

`UNKNOWN = 0` は「エンコーダが自動決定」。 IDR を強制したいときは `IDR | I | REF` の組合せで、 `I` と `REF` を落とすと VPL 側でフレームタイプを再決定されることがある。

### `EncodeHandler` / `FnEncodeHandler` とハンドラー駆動

```rust
pub trait EncodeHandler: Send + 'static {
    type UserData: Send + 'static;
    type Error: From<shiguredo_vpl::Error> + Send + 'static;
    fn on_encoded(&mut self, result: Result<EncodedFrame<Self::UserData>, Self::Error>);
}
```

押さえておく挙動:

- `Encoder::new(config, handler)` 内部で `vpl-encoder-sync` 名前付きの専用 worker スレッドが起動する。 `on_encoded` はこの worker スレッドから呼ばれる。 ハンドラ実装は `Send + 'static`。
- `UserData` 型は呼び出し側が自由に決める。 元フレームの ID やタイムスタンプ、トレーシング ID などを乗せる。 出力 `EncodedFrame::user_data()` / `into_user_data()` で取り出す。
- `Error` 関連型は `From<shiguredo_vpl::Error>` を実装した型なら何でも使える。 自前のエラー型に持ち上げたいケースに対応する。
- `FnEncodeHandler<T, E = shiguredo_vpl::Error>` は `FnMut(Result<EncodedFrame<T>, E>) + Send + 'static` を `EncodeHandler` に持ち上げるラッパー。 mpsc にブリッジするなら `FnEncodeHandler::new(move |r| tx.send(r).unwrap())` が定番。
- VPL の遅延 (LookAhead / B フレーム並び替え) のため、`encode` 1 回が即 1 回の `on_encoded` 呼び出しに対応するわけではない。 `encode` を複数回呼んでから `on_encoded` がまとめて呼ばれることもある。
- `encode` のたびに渡した `user_data` は worker 内の `PendingFrameStore` に `frame_seq` で登録され、 出力 bitstream の `TimeStamp` と完全一致で引き当てられる (B フレーム並び替えがあっても元の入力に正しく戻る)。 1 ID が二度使われると重複エラーになる。
- `MFX_ERR_MORE_DATA` と `MFX_WRN_DEVICE_BUSY` は内部で吸収される。 `DEVICE_BUSY` は 1ms スリープで最大 30 回までリトライ (旧 10 回から拡張)。 30 回を超えると致命的エラー。
- `MFX_ERR_MORE_SURFACE` は **Decoder 側のみ** 1ms スリープで最大 30 回リトライし、上限超過時はエラーになる (内部割り当て `surface_work=NULL` では通常発生しないが、AV1 の `FilmGrain != 0` 等のサーフェス不足で返り得る)。 **Encoder 側は `MFX_ERR_MORE_SURFACE` をリトライせず即エラー** として扱う。

### `Encoder::encode` / `finish` のセマンティクス

- `encode(&mut self, frame_data: &[u8], user_data: H::UserData, options: &EncodeOptions) -> Result<(), Error>`。 `frame_data` の長さは `FrameFormat::frame_size(coded_w, coded_h)` 以上が必要。 不足すると即エラー。
- `finish(&mut self) -> Result<(), Error>` は EOS シグナル + バリア。 null surface でドレインを呼び切り、worker に `WaitIdle` を送って **すべてのハンドラ呼び出しが完了するまでブロック** する。 戻ったときには `on_encoded` がすべて呼ばれ終わっている。
- `Encoder` を `Drop` すると worker スレッドへ `Stop` が送られ、 ペンディング中のフレームはすべて `MFX_ERR_ABORTED` のエラー結果として `on_encoded` に通知される。 失敗フレームを取りこぼさない設計。
- `EncodedFrame<T>` から取れる情報: `data() -> &[u8]` / `into_data() -> Vec<u8>` / `timestamp() -> u64` / `picture_type() -> PictureType` / `user_data() -> &T` / `into_user_data() -> T`。
- ドレイン時の空ビットストリーム (PictureType 未確定の null フレーム) はエラーではなく空 `data` の `EncodedFrame` として正常通知される。 利用側で空チェックして捨てる。

### 動的再構成 (`reconfigure`)

`MFXVideoENCODE_Reset` のラッパー。 `ReconfigureParams` の `None` フィールドは変更しない:

```rust
use shiguredo_vpl::ReconfigureParams;

encoder.reconfigure(ReconfigureParams {
    target_kbps: Some(3_000),
    max_kbps: Some(4_000),
    framerate_num: None,
    framerate_den: None,
})?;
```

警告ステータス (`MFX_WRN_*`) は内部で許容される (`check_mfx_allow_warn`)。 リセット自体は成功している。

- 変更できるのはビットレートとフレームレートだけであり、解像度を変更する場合は `Encoder` を作り直す。
- reconfigure が `Ok` を返した場合、呼び出し時点で未出力だったフレームはすべて `on_encoded` に `Ok` で通知されている。 `Err` を返した場合は、一部が `Err` で通知されるか、通知されないまま残ることがある。
- frame_seq は reconfigure 後も継続し、リセットされない。
- reconfigure は、呼び出し時点までに送信したフレームの Sync 完了と、すべてのコールバックの完了までブロックする。
- reconfigure が `Err` を返した場合、エンコーダーが使用できない状態のまま残ることがあるため、 `Encoder` を作り直す。

### 出力設定の検証 (`write_ivf_headers`)

- `Encoder::write_ivf_headers() -> bool` は VP9 の IVF ヘッダー出力設定を返す。 `Vp9EncoderConfig::write_ivf_headers` の要求値そのもの。
- 初期化時に `mfxExtVP9Param::WriteIVFHeaders` の実効値を GetVideoParam で読み戻し、 要求値と一致しない場合はエラーを返す。 非 VP9 コーデックでは常に `false`。

### 実効パラメータの取得 (`get_video_param`)

- `Encoder::get_video_param() -> Result<VideoParam, Error>` は、基本パラメータと、要求した拡張バッファの実効値を返す。
- 基本パラメータは VPL の生の `mfxVideoParam`。共用体メンバの読み出しには `unsafe` が要る。
- 拡張バッファは `ExtParams` に `mfxExtCodingOption2` / `mfxExtCodingOption3` / `mfxExtVP9Param` の生の構造体として入る。要求していないバッファは `None`。
- 値は要求値ではなく VPL が報告した実効値。VPL が書き戻さなかった場合はゼロ初期化された値のままになる。

### シーケンスパラメータの取得 (`get_sequence_params`)

- `Encoder::get_sequence_params() -> Result<Option<SequenceParams>, Error>` は、現在の作業パラメータセットを返す。
- `SequenceParams` は `vps: Option<Vec<u8>>` / `sps: Vec<u8>` / `pps: Option<Vec<u8>>` を持つ。
  H.264 は `sps` に SPS、`pps` に PPS を入れ、`vps` は `None`。
  HEVC は `vps` に VPS、`sps` に SPS、`pps` に PPS を入れる。
  AV1 は `sps` に Sequence Header OBU を入れ、`vps` と `pps` は `None`。
- VP9 はパラメータセットを持たないため `Ok(None)` を返す。
- 返るバイト列は、先頭のスタートコード (`00 00 00 01` / `00 00 01`) を除いた NAL / OBU。
  AV1 の OBU はスタートコードを持たないためそのまま返る。
- 返るのは次にエンコードするフレームに適用されるパラメータセットであり、出力待ちのフレームに適用されたものではない。
  内容が変わるのは `Encoder::new` と `Encoder::reconfigure` だけで、`encode()` や IDR の強制では変わらない。
- `encode()` と同じスレッドで直列に呼ぶ。`on_encoded` コールバックの中から呼んではならない。
- `reconfigure()` 後に新しいパラメータセットが必要な場合は `get_sequence_params()` を呼び直す。
- MP4 の `avc1` / `hvc1` は SampleEntry を途中で差し替えられないため、SPS が変わる変更を行う場合は `avc3` / `hev1` を使う。

```rust
let params = encoder.get_sequence_params()?;
if let Some(params) = params {
    // H.264 / HEVC は sps / pps、HEVC はさらに vps を持つ
    // AV1 の sps は Sequence Header OBU で、vps / pps は None
    let _sps: &[u8] = &params.sps;
    let _pps = params.pps.as_deref();
    let _vps = params.vps.as_deref();
}
```

### `Encoder::query` / 統計

- `query` で `MFXVideoENCODE_Query` を叩いてパラメータのサポート可否を検証できる (どこまで通るかを確認したいとき)。
- `Encoder::stats() -> &EncoderStats` は統計値への参照を返す。統計値はエンコード処理の進行に伴って更新される。
  `EncoderStats` は `total_encode_count` (VPL に受理させた回数) / `total_output_frame_count` (`Ok` 通知回数) /
  `total_error_count` (`Err` 通知回数) / `total_reconfigure_count` (reconfigure 成功回数) /
  `total_output_bit_count` (出力ビットストリームの総ビット数) / `in_flight_frames` (`Gauge`) を持つ。
- `in_flight_frames` は `encode()` がフレームを受理した時点で増え、対応する結果を `on_encoded` に通知する直前に減る。
  正常に完了した場合、 `finish()` と `reconfigure()` の処理が完了した時点で 0 になる。
  処理の途中でエラーになった場合は残留することがある。
- `Counter` は `new()` / `get()` を公開し、`inc()` / `add()` は crate 内専用。
- `Gauge` は `new()` / `get()` を公開し、`inc()` / `dec()` は crate 内専用。
- `clone()` は現在値のスナップショットを返す。`EncoderStats` / `DecoderStats` の `clone()` は
  フィールドごとにコピーするため、フィールド間の一貫性は保証されない。

## デコーダ (`Decoder<H>`)

デコーダもエンコーダと同じくハンドラー方式。 `Decoder<H: DecodeHandler>` 型パラメータでハンドラ実装を指定し、`decode()` 呼び出しごとに `H::UserData` を一緒に渡す。 デコード結果は `on_decoded` で **借用** された `DecodedFrame<'_, H::UserData>` として渡される。

```rust
use shiguredo_vpl::{
    AdapterSelector, DecodedFrame, Decoder, DecoderConfig, Error, FnDecodeHandler,
    VideoCodecType, list_adapters,
};
use std::sync::mpsc;

let adapter = AdapterSelector::DrmRenderNode(list_adapters()?[0].drm_render_node);
let config = DecoderConfig::new(adapter, VideoCodecType::H264);

// DecodedFrame は借用ベース。コールバック内で Vec にコピーしてから外へ渡す
let (tx, rx) = mpsc::channel();
let handler = FnDecodeHandler::new(move |result: Result<DecodedFrame<'_, u64>, Error>| {
    let copied = result.map(|frame| {
        (
            frame.y().to_vec(),
            frame.uv().to_vec(),
            frame.pitch(),
            frame.width(),
            frame.height(),
        )
    });
    let _ = tx.send(copied);
});
let mut decoder = Decoder::new(config, handler)?;

decoder.decode(&bitstream_data, /* user_data */ 0u64)?;

// finish はバリア。 ここに到達した時点で worker は全フレーム処理済み
decoder.finish()?;

while let Ok(result) = rx.try_recv() {
    let (_y, _uv, _pitch, _w, _h) = result?;
}
```

押さえておく挙動:

- `DecoderConfig` は `#[non_exhaustive]`。 必須は `adapter` と `codec` のみ。 解像度・フレームレートはビットストリームヘッダ (SPS/PPS など) から自動検出される。
- `async_depth: Option<u16>` を持つ。 `None` の場合は 4。 エンコーダと同じ意味。
- 最初の `decode` 呼び出しで `MFXVideoDECODE_DecodeHeader` → `MFXVideoDECODE_Init` が自動的に走る。 ヘッダ未到達のうちは `MFX_ERR_MORE_DATA` を内部で吸収して何も出さない (`initialized` フラグで管理)。
- 出力フォーマットはストリームのネイティブ形式で決まる (8bit は NV12、10bit は P010)。 出力形式を選択する API はない。 4:4:4 ストリームのデコードは未対応。 `IOPattern = OUT_SYSTEM_MEMORY` は変わらない。
- サーフェスは VPL の **内部割り当て** (`surface_work = NULL`)。 アプリ側でサーフェスプールを持つ必要はなく、`FrameSurface` の Drop で `Release` が自動的に呼ばれる。
- worker スレッド名は `vpl-decoder-sync`。 `user_data` は入力ビットストリームの `TimeStamp` に載せた連番と、 出力フレームの `TimeStamp` の**完全一致**で対応付く (Encoder と同じ方式)。 B フレームの表示順並び替えがあっても正しい入力フレームに戻る。 なお、 1 フレームが複数の `decode()` 呼び出しに分割して供給されるケースでは、 出力フレームの TimeStamp がどの呼び出しの連番になるかが VPL 実装依存のため、 user_data の対応付けは保証されない (分割入力で未消費の連番が残留した場合、 `finish()` が残留チェックエラーを返す)。
- `decode` 呼び出しごとに `user_data` を 1 つ供給する。 VPL が内部蓄積でフレームを出さないターンでは消費されず、 出力フレーム数より供給数が多い場合の残余は `finish()` の残留チェックでエラーになる (エラーが通知され、 `finish()` が `Err` を返す)。 `decode` がエラーを返した場合も `frame_seq` は消費済みで、 失敗フレームの `user_data` は出力に紐付かず `finish()` の残留チェック (または `Drop` 時の `MFX_ERR_ABORTED` 通知) でエラーとして通知される。
- `finish(&mut self) -> Result<(), Error>` はエンコーダと同じくバリア。 null bitstream でドレインを呼び切り、worker の `WaitIdle` が応答するまでブロックする。 `decode` を一度も呼んでいない (未初期化) 場合は即 `Ok(())` を返す。
- `Decoder::stats() -> &DecoderStats` は `total_decode_count` (`decode()` が入力を pending に登録した回数) /
  `total_output_frame_count` (`Ok` 通知回数) / `total_error_count` (`Err` 通知回数) /
  `in_flight_frames` (`Gauge`) を持つ。 `finish()` が残留 pending をエラー通知した場合も
  `total_error_count` が増え、 `in_flight_frames` は 0 に戻る。処理の途中でエラーになった場合は残留することがある。
- `total_decode_count` は pending 登録時点で増える。登録後にデコードが失敗した場合も計上し、
  空のビットストリームも登録に成功すれば計上する。
- `Decoder` を `Drop` すると worker スレッドへ `Stop` が送られ、未消費の `user_data` は `MFX_ERR_ABORTED` のエラー結果として `on_decoded` に通知される。

### `DecodedFrame<'_, T>` の使い方

```rust
pub struct DecodedFrame<'a, T> { /* private */ }

impl<'a, T> DecodedFrame<'a, T> {
    pub fn y(&self) -> &[u8];    // 長さ pitch() * height() バイト
    pub fn uv(&self) -> &[u8];   // 長さ pitch() * height() / 2 バイト
    pub fn pitch(&self) -> usize;
    pub fn width(&self) -> usize;
    pub fn height(&self) -> usize;
    pub fn user_data(&self) -> &T;
    pub fn into_user_data(self) -> T;
}
```

- `y()` / `uv()` は **ピッチ込みの生サーフェス**。 8bit は各行の先頭 `width()` バイト、 P010 は各行の先頭 `width() * 2` バイトのみが有効データで、残りはパディング。 P010 は各サンプルが 16bit ワードで上位 10bit が有効。 `pitch() != width()` が普通なので、 そのまま `width * height * 3 / 2` バイトのバッファにコピーすると壊れる。 行ごとにコピーする。
- スライスのライフタイム `'a` は `on_decoded` 呼び出し中のみ。 コールバックの外に持ち出そうとするとコンパイルエラーになる。 外で使いたいなら `to_vec()` でコピーする。
- VPL から返るサーフェスが不正 (null プレーン / `crop = 0` / `pitch < crop_w`) なら、 `read_decoded_surface` が `Error` を返し `on_decoded(Err(...))` に変換される。 仕様違反のサーフェスをそのまま読まない安全装置。

## コーデック情報の照会 (`supported_codecs`)

`supported_codecs` は静的なテーブルではなく、 **指定アダプタに対する VPL の実装記述から実際に取れる値**を返す。

- H.264 / HEVC / VP9 / AV1 を順に問い合わせて `CodecInfo` を組み立てる。
- `CodecInfo` は `decoding: Option<DecodingInfo>` / `encoding: Option<EncodingInfo>` を持つ。 エンコード・デコードに対応していないコーデックは `None` になる。
- `DecodingInfo` / `EncodingInfo` は `max_codec_level: Option<u16>` と `profiles: Vec<ProfileCapabilities>` を持つ。 `EncodingInfo` はこのほかに `supports_frame_reordering` (B フレーム) を持つ。
- `ProfileCapabilities` はコーデックごとの `profile: VideoProfile` と、メモリタイプごとの `memory_types: Vec<MemoryTypeCapabilities>` を持つ。
- `MemoryTypeCapabilities` は `memory_type: MemoryType` / `width: ValueRange` / `height: ValueRange` / `color_formats: Vec<FrameFormat>` を持つ。 `ValueRange` は VPL の `mfxRange32U` に対応し、 `min` / `max` / `step` を持つ。
- 解像度範囲と色形式は、プロファイルとメモリタイプごとの記述をそのまま返す。 プロファイルやメモリタイプをまたいだ集約はしない。 色形式は `FrameFormat` で表現できる FourCC だけを、 VPL の `ColorFormats` に並んでいる順序で返す。
- 既知の型で表現できないプロファイル ID とメモリタイプは `VideoProfile::Unknown(u32)` / `MemoryType::Unknown(u32)` として生の値を保持する。
- すべてのメモリタイプを返すが、 このクレートの `Encoder` / `Decoder` が実際に使うのは `MemoryType::System` だけ。
- `max_codec_level` は最大コーデックレベルを返す。 レベルが不明な場合は `None`。
- VPL がプロファイルを返さない場合は `profiles` が空、 メモリタイプを返さない場合は `memory_types` が空、 色形式を返さない場合や表現できる FourCC が無い場合は `color_formats` が空になる。
- 「このマシンで実際に何が動くか」を起動時に確認する用途。 ハードコードしたサポート表ではなく必ず実行時に取る。

```rust
use shiguredo_vpl::{AdapterSelector, list_adapters, supported_codecs};

let adapter = AdapterSelector::DrmRenderNode(list_adapters()?[0].drm_render_node);
for codec in supported_codecs(adapter)? {
    if let Some(encoding) = &codec.encoding {
        for profile in &encoding.profiles {
            for memory_type in &profile.memory_types {
                println!(
                    "{:?} {:?} {:?}: width={:?} height={:?} {:?}",
                    codec.codec,
                    profile.profile,
                    memory_type.memory_type,
                    memory_type.width,
                    memory_type.height,
                    memory_type.color_formats,
                );
            }
        }
    }
}
```

## 実機で確認した制約

次の環境で確認した `supported_codecs` と実際の Init / Encode / Decode の差、および実装上の制約をまとめる。
これらは API の保証ではなく、この環境での観測結果である。別の GPU やドライバでは再確認すること。

- 実機: GMKtec NucBox K9、Intel Meteor Lake-P [Intel Arc Graphics] (8086:7d55)
- OS / kernel: Ubuntu 24.04.5 LTS / 6.8.0-85-generic
- VA-API: 1.22 (libva 2.22.0)、iHD 24.3.4
- oneVPL: libvpl 2.17.0 (static link)
- 確認日: 2026-11-07
- 測定条件: 320x240、System メモリ、CQP (QPI=10)、AsyncDepth 4

### エンコーダー

| コーデック / プロファイル | 入力 | capability | Init | EncodeFrameAsync |
|---|---|---|---|---|
| H.264 High | P010 / Y410 | 非対応 | `MFX_ERR_INVALID_VIDEO_PARAM` | - |
| HEVC Main10 | NV12 / AYUV | 対応 | `MFX_ERR_INVALID_VIDEO_PARAM` | - |
| VP9 Profile0 / Profile1 | P010 / Y410 | 対応 | `MFX_ERR_INVALID_VIDEO_PARAM` | - |
| VP9 Profile2 | AYUV / Y410 | 対応 | `MFX_ERR_INVALID_VIDEO_PARAM` | - |
| AV1 Main | AYUV | 非対応 | `MFX_ERR_INVALID_VIDEO_PARAM` | - |
| HEVC Rext / Scc | AYUV / Y410 | 対応 | 成功 | 出力なし (CQP) / CBR は Init 失敗 |

補足:

- HEVC Rext / Scc + AYUV / Y410 は Init が成功し、`get_video_param` の FourCC / BitDepth / ChromaFormat も要求どおりになる。しかし CQP (QPI=10、3 フレーム) では `Encoder::finish` が pending frames のエラーを返して出力が得られず、10 フレームでは `EncodeFrameAsync` が device busy で失敗する。CBR (1000 kbps) では Init が `MFX_ERR_DEVICE_FAILED` になる。
- HEVC Main + P010 は Init が成功し、実効 FourCC は P010 のままになる。エンコード結果のプロファイルとビット深度の整合は未確認。
- capability が「対応」でも Init が失敗する組み合わせがある。`supported_codecs` の結果だけで Init の成功を判断しない。

### デコーダー

| ストリーム | 出力 | 備考 |
|---|---|---|
| 8bit (H.264 / HEVC Main / VP9 Profile0 / AV1 8bit) | NV12 | - |
| 10bit (HEVC Main10 / AV1 10bit) | P010 | - |
| HEVC Main10 + Y410 | P010 | 4:4:4 から 4:2:0 に変換される |
| HEVC Rext / Scc + AYUV / Y410 | AYUV / Y410 | `DecodedFrame` は未対応。AYUV は壊れたスライス、Y410 は null ポインタエラー |
| VP9 Profile2 + NV12 | P016 (12bit) | `FrameFormat` に P016 がない |

- 出力形式はストリームのネイティブ形式で決まり、選択する API はない。
- `DecodeHeader` 後の FourCC 上書きはプロファイル依存で、YUY2 出力は確認できていない。
- 4:4:4 出力のデコードは未対応。

### 再確認方法

- `vainfo` で VA-API のプロファイルとドライバを確認する。
- `supported_codecs()` で capability を取得する。
- `Encoder::new` で Init を試し、成功した場合は `EncodeFrameAsync` の出力を確認する。
- `Decoder` でデコードし、実際の `mfxFrameInfo` (FourCC / BitDepth / ChromaFormat) を確認する。

## エラー型 (`Error`)

- `Error::from_mfx(status, fn_name)` で `mfxStatus` を Rust 側エラーに包む。
- `Error::check_mfx(status, fn_name)` は `MFX_ERR_NONE` 以外を即エラーにする。
- `Error::check_mfx_allow_warn(status, fn_name)` は `MFX_WRN_*` を許容する (Init / Reset 系のラッパーで使う)。 「警告は返るが処理は成功している」ケース。
- `Error::new_custom(fn_name, message)` でラッパー側固有のエラーを作る。
- `with_message(...)` でユーザ向けにコンテキストを足せる。 例: render node 番号付きの `MFX_ERR_NOT_FOUND` メッセージ。

エラーメッセージは英語、 ログメッセージも英語、 コメントとテストメッセージは日本語、 というのがリポジトリ全体のルール (`AGENTS.md` 参照)。

## ビルドの仕組み

- `build.rs` が `Cargo.toml` の `[package.metadata.external-dependencies]` を読んで libvpl を GitHub から取得し、 `shiguredo_cmake` 経由で CMake static build する。
- bindgen の出力は `src/sys.rs` 相当に展開される。 マクロ (`#define`) で定義された定数や一部 static 変数は bindgen が拾えないため、 必要なら `build.rs` で手書きする。
- Linux x86_64 上では `DOCS_RS=1 cargo doc --no-deps` で libvpl をビルドせずにドキュメントだけは生成できる。
- `BUILD_VERSION` 定数で「ビルド時参照した libvpl のバージョン」が拾える。

## ライブラリ利用上の落とし穴

- **アダプタ列挙の重さ**: `list_adapters()` は MFXLoad/Unload を毎回まわす。 起動時 1 回だけ呼ぶ。
- **DRM render node 0**: 「未設定」を意味するので絶対に `AdapterSelector::DrmRenderNode(0)` を渡さない。
- **`coded_size` vs `width`/`height`**: 入力フレームのバッファ確保は **必ず** `Encoder::coded_size()` の結果で行う。 `EncoderConfig.width` / `height` は 16 ピクセルアラインされて内部で大きくなりうる。
- **union メンバ**: `RateControlMode` ごとに有効な `EncoderConfig` フィールドが違う。 関係ないフィールドを埋めても効かないが、 別 union メンバを同時に埋めると意図しない値が選ばれる。
- **`finish` 忘れ**: `encode` / `decode` だけ呼んで `finish` を呼ばないと内部に滞留するフレームのハンドラ通知が抜ける。 EOS では必ず `finish` を呼んで完了を待つ。 `finish` はバリアなので、 リターン後にはハンドラ呼び出しがすべて済んでいる。
- **`frame_type` の組合せ**: IDR を強制したいときは `IDR | I | REF`。 IDR 単体だと VPL が再決定することがある。
- **`MFX_WRN_*` の解釈**: `Init` / `Reset` 系では警告は「成功」扱い。 自分でラッパーを書くときは `check_mfx_allow_warn` の方を使う。
- **ハンドラはワーカースレッドから呼ばれる**: `EncodeHandler::on_encoded` / `DecodeHandler::on_decoded` は `vpl-encoder-sync` / `vpl-decoder-sync` のワーカースレッドから呼ばれる。 ハンドラ実装は `Send + 'static`。 メインスレッドへ届けるなら mpsc などのチャネルにブリッジする。
- **`DecodedFrame` の借用**: `DecodedFrame<'_, T>` の `y()` / `uv()` スライスはコールバック呼び出し中のみ有効。 外に持ち出すには `to_vec()` などでコピーする必要がある。 `pitch() != width()` が普通で、 単純連結すると壊れる。
- **`user_data` の対応付け**: `Decoder` は入力 `decode()` の連番 (`TimeStamp`) と出力フレームの `TimeStamp` を完全一致で対応付ける。 出力フレームの TimeStamp がどの入力にも一致しない (= 供給不足) 場合は worker 内で drain 扱いとなり破棄される。 1 入力で複数枚出ることはないが、 元入力との 1:1 対応を確実にするには `decode` 1 回ごとに `user_data` を必ず供給する。
- **動作 OS の制約**: 実機サポートは Linux x86_64 + Intel GPU のみ。 Linux x86_64 以外のターゲットでは `compile_error!` により crate 自体がビルドできない。
- **静的リンク**: 実行時に libvpl 共有ライブラリは不要だが、 ビルド時に対応 GPU ドライバ (Intel Media Driver / iHD) が動作環境にないと実機テストは通らない。

## テスト・ベンチの位置

リポジトリ自身のテスト配置 (CLAUDE.md / AGENTS.md 準拠):

- 単体テスト: `tests/test_<module>.rs` (例: `tests/test_adapter.rs`, `tests/test_roundtrip.rs`)
- PBT (proptest): `pbt/tests/prop_<module>.rs` に置く規約。 vpl-rs では現状未配置だが、 増やすときはこの規約で。
- `#[ignore]` は使わない。
- PBT で書けるものは単体テストに書かない。 単体テストはエラーパス・境界値・「PBT で実現できないケース」専用。

## チェンジログ・破壊的変更の方針

- 変更は `CHANGES.md` の `## develop` セクションへ追記する。 種別ラベルは `[CHANGE]` / `[UPDATE]` / `[ADD]` / `[FIX]` (CHANGES.md 冒頭の凡例順) を使う。 担当者は内容のサブ箇条書きの最後に `- @user` で書く。
- **vpl-rs は良い設計のためには破壊的変更を積極的に行う**。 古い API を残すよりも、 VPL の概念に正しくマップした新 API に置き換える。 互換シムは作らない。
- 直近の破壊的変更例:
  - `2026.4.0` — `Vp9EncoderConfig` に `write_ivf_headers` を追加 (構造体リテラル構築では明示指定が必須化)、 Decoder の DEVICE_BUSY / MORE_SURFACE リトライに上限 (30 回) を導入
  - `2026.3.0` — `Encoder<H>` / `Decoder<H>` のハンドラー方式化、`next_frame` 廃止、`async_depth` 追加、`DECODE_SURFACE_POOL_SIZE` を廃止し VPL 内部割り当てに移行、`DEVICE_BUSY` リトライ 10 → 30
  - `2026.2.0` — `EncoderConfig::new` / `DecoderConfig::new` / `codec_info::supported_codecs` にアダプタ指定を必須化
- 詳細は `shiguredo-changelog` スキルの規約も併用すること。
