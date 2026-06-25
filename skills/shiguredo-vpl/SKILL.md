---
name: shiguredo-vpl
description: shiguredo_vpl (vpl-rs) クレートの徹底リファレンス。Intel VPL (Video Processing Library) v2.17 を libvpl で static link した Rust バインディング。H.264 / H.265 / VP9 / AV1 のハードウェアエンコード・デコード、AdapterSelector による複数 Intel GPU 対応、EncoderConfig / RateControlMode の選び方、Encoder / Decoder のハンドラー方式 (EncodeHandler / DecodeHandler) と user_data 連携、async_depth の調整、frame_type / gop_opt_flag のビットフラグ、coded_size と frame_size の関係、DecodedFrame の pitch 付き借用 Y/UV プレーン、MFX_ERR_MORE_DATA を含むエラー処理に関する質問で使用。
---

# shiguredo_vpl クレート

- **バージョン**: 2026.3.0 (依存する libvpl は 2.17.0)
- **リポジトリ**: https://github.com/shiguredo/vpl-rs
- **Rust エディション**: 2024 (rust-version: 1.88)
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
- `codec_info` — `VideoCodecType` / `CodecInfo` / `DecodingInfo` / `EncodingInfo` / 各コーデックプロファイル一覧型
- `decode` — `Decoder<H>` / `DecoderConfig` / `DecoderCodec` / `DecodedFrame<'_, T>` / `DecodeHandler` / `FnDecodeHandler<T, E>`
- `encode` — `Encoder<H>` / `EncoderConfig` / `CodecConfig` / `EncodeOptions` / `EncodedFrame<T>` / `EncoderStats` / `FrameFormat` / `RateControlMode` / `ReconfigureParams` / `PictureType` / `EncodeHandler` / `FnEncodeHandler<T, E>` / 各プロファイル enum
- `error` — `Error`
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
- Linux 以外では `list_adapters()` は常に空 `Vec` を返す (`target_os = "linux"` のフィーチャ分岐)。
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

入力フォーマットは `Nv12` / `Yuy2` / `Bgra` のみ:

| 列挙子 | FourCC | チャネル |
|---|---|---|
| `FrameFormat::Nv12` | `MFX_FOURCC_NV12` | Y plane + interleaved UV plane (4:2:0 8bit) |
| `FrameFormat::Yuy2` | `MFX_FOURCC_YUY2` | YUYV interleaved (4:2:2 8bit) |
| `FrameFormat::Bgra` | `MFX_FOURCC_RGB4` | BGRA packed (8bit) |

- `Encoder::coded_size()` は 16 ピクセルにアラインされた符号化サイズを返す。 入力バッファサイズは `width` / `height` ではなくこの coded サイズで計算する。
- `FrameFormat::frame_size(width, height) -> Option<usize>` で必要バイト数を取る。 `usize::checked_mul` 系でオーバーフロー検出する。 NV12 は `pixels * 3 / 2`、 YUY2 は `pixels * 2`、 BGRA は `pixels * 4`。
- `set_planes` の内部実装は libvpl ヘッダ (`api/vpl/mfxstructures.h:344-349`) の指示通り、 NV12 / YUY2 のようなパック済みフォーマットでも Y/U/V を「先頭サンプル」へ向ける。 これは VPL 仕様の制約なので、 サーフェスを直接組むときは同じ規約に従う。

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

### `Encoder::query` / 統計

- `query` で `MFXVideoENCODE_Query` を叩いてパラメータのサポート可否を検証できる (どこまで通るかを確認したいとき)。
- `EncoderStats` (`num_frame` / `num_bit` / `num_cached_frame`) は `MFXVideoENCODE_GetEncodeStat` の結果。

## デコーダ (`Decoder<H>`)

デコーダもエンコーダと同じくハンドラー方式。 `Decoder<H: DecodeHandler>` 型パラメータでハンドラ実装を指定し、`decode()` 呼び出しごとに `H::UserData` を一緒に渡す。 デコード結果は `on_decoded` で **借用** された `DecodedFrame<'_, H::UserData>` として渡される。

```rust
use shiguredo_vpl::{
    AdapterSelector, DecodedFrame, Decoder, DecoderCodec, DecoderConfig, Error, FnDecodeHandler,
    list_adapters,
};
use std::sync::mpsc;

let adapter = AdapterSelector::DrmRenderNode(list_adapters()?[0].drm_render_node);
let config = DecoderConfig::new(adapter, DecoderCodec::H264);

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
- 出力フォーマットは **NV12 固定** (`IOPattern = OUT_SYSTEM_MEMORY`)。 別フォーマットへの変換は VPP 等で別途処理する。
- サーフェスは VPL の **内部割り当て** (`surface_work = NULL`)。 アプリ側でサーフェスプールを持つ必要はなく、`FrameSurface` の Drop で `Release` が自動的に呼ばれる。
- worker スレッド名は `vpl-decoder-sync`。 `user_data` は **FIFO キュー** (`VecDeque`) で `decode()` 呼び出し順に対応付く。 ビットストリームがフレーム境界をまたいでも順序は保たれる。
- `decode` 呼び出しごとに `user_data` を 1 つ供給する。 VPL が内部蓄積でフレームを出さないターンでは消費されず、出力フレーム数より供給数が多い場合の残余は `finish` 後の Drop までキューに残る。
- `finish(&mut self) -> Result<(), Error>` はエンコーダと同じくバリア。 null bitstream でドレインを呼び切り、worker の `WaitIdle` が応答するまでブロックする。 `decode` を一度も呼んでいない (未初期化) 場合は即 `Ok(())` を返す。
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

- `y()` / `uv()` は **ピッチ込みの生サーフェス**。 各行の先頭 `width()` バイトのみが有効データで、残りはパディング。 `pitch() != width()` が普通なので、 そのまま `width * height * 3 / 2` バイトのバッファにコピーすると壊れる。 行ごとにコピーする。
- スライスのライフタイム `'a` は `on_decoded` 呼び出し中のみ。 コールバックの外に持ち出そうとするとコンパイルエラーになる。 外で使いたいなら `to_vec()` でコピーする。
- VPL から返るサーフェスが不正 (null プレーン / `crop = 0` / `pitch < crop_w`) なら、 `read_decoded_surface` が `Error` を返し `on_decoded(Err(...))` に変換される。 仕様違反のサーフェスをそのまま読まない安全装置。

## コーデック情報の照会 (`codec_info`)

`shiguredo_vpl::codec_info` は静的なテーブルではなく、 **指定アダプタに対する VPL の実装記述から実際に取れる値**を返す。

- `VideoCodecType::all()` (内部) で H.264 / HEVC / VP9 / AV1 を順に問い合わせて `CodecInfo` を組み立てる API がある。
- `CodecInfo` は `decoding: DecodingInfo` / `encoding: EncodingInfo` を持つ。
- `EncodingInfo` には `supports_frame_reordering` (B フレーム) / `supports_multi_pass` / コーデック別 `profiles: EncodingProfiles` が入る。
- 「このマシンで実際に何が動くか」を起動時に確認する用途。 ハードコードしたサポート表ではなく必ず実行時に取る。

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
- docs.rs 向けに `DOCS_RS=1 cargo doc --no-deps` で libvpl なしでもドキュメントだけは生成できる。
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
- **`user_data` の枯渇**: `Decoder` 側で `decode` の呼び出し数より VPL の出力フレーム数が多い (= キューが枯渇する) と、 余分なフレームは worker 内で drain 扱いとなり破棄される。 1 入力で複数枚出ることはないが、 順序保証は **FIFO のみ** なので元入力との 1:1 対応が必要なら `decode` 1 回ごとに `user_data` を必ず供給する。
- **動作 OS の制約**: 実機サポートは Linux x86_64 + Intel GPU のみ。 macOS / Windows でビルドできるとしても `list_adapters()` は空を返し、 セッション生成は失敗する。
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
  - `2026.3.0` — `Encoder<H>` / `Decoder<H>` のハンドラー方式化、`next_frame` 廃止、`async_depth` 追加、`DECODE_SURFACE_POOL_SIZE` を廃止し VPL 内部割り当てに移行、`DEVICE_BUSY` リトライ 10 → 30
  - `2026.2.0` — `EncoderConfig::new` / `DecoderConfig::new` / `codec_info::supported_codecs` にアダプタ指定を必須化
- 詳細は `shiguredo-changelog` スキルの規約も併用すること。
