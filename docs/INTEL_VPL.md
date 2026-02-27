# Intel VPL 実装状況

## 概要

Intel oneVPL (Video Processing Library) v2.16.0 の Rust バインディング。
エンコーダのみを対象とする。

## 対応コーデック

| コーデック | CodecId | 対応プロファイル |
|---|---|---|
| H.264/AVC | `MFX_CODEC_AVC` | Baseline, ConstrainedBaseline, Main, High, ConstrainedHigh, High10, High422 |
| H.265/HEVC | `MFX_CODEC_HEVC` | Main, Main10, MainSP, RExt, SCC |
| VP9 | `MFX_CODEC_VP9` | Profile 0, 1, 2, 3 |
| AV1 | `MFX_CODEC_AV1` | Main |

## 入力フレームフォーマット

| フォーマット | FourCC | 説明 |
|---|---|---|
| NV12 | `MFX_FOURCC_NV12` | Semi-Planar YUV 4:2:0 8bit |
| I420 | `MFX_FOURCC_I420` | Planar YUV 4:2:0 8bit |
| YV12 | `MFX_FOURCC_YV12` | Planar YUV 4:2:0 8bit (V, U 順) |
| BGRA | `MFX_FOURCC_RGB4` | Packed BGRA 8bit |
| P010 | `MFX_FOURCC_P010` | Semi-Planar YUV 4:2:0 10bit |

## レート制御モード

| モード | 定数 | 説明 |
|---|---|---|
| CBR | `MFX_RATECONTROL_CBR` | 固定ビットレート |
| VBR | `MFX_RATECONTROL_VBR` | 可変ビットレート |
| CQP | `MFX_RATECONTROL_CQP` | 固定量子化パラメータ |
| ICQ | `MFX_RATECONTROL_ICQ` | Intelligent Constant Quality |
| QVBR | `MFX_RATECONTROL_QVBR` | Quality VBR |
| LA | `MFX_RATECONTROL_LA` | Look Ahead |
| AVBR | `MFX_RATECONTROL_AVBR` | Average VBR |
| VCM | `MFX_RATECONTROL_VCM` | Video Conferencing Mode |
| LA_ICQ | `MFX_RATECONTROL_LA_ICQ` | Look Ahead + ICQ |
| LA_HRD | `MFX_RATECONTROL_LA_HRD` | Look Ahead + HRD 準拠 |

## 使用している VPL API 関数

| VPL 関数 | ラッパー | 用途 |
|---|---|---|
| `MFXInitialize` | `VplLibrary::mfx_initialize` | セッション作成 |
| `MFXClose` | `VplLibrary::mfx_close` | セッション破棄 |
| `MFXVideoENCODE_Init` | `VplLibrary::mfx_video_encode_init` | エンコーダ初期化 |
| `MFXVideoENCODE_Close` | `VplLibrary::mfx_video_encode_close` | エンコーダ破棄 |
| `MFXVideoENCODE_EncodeFrameAsync` | `VplLibrary::mfx_video_encode_frame_async` | 非同期エンコード |
| `MFXVideoCORE_SyncOperation` | `VplLibrary::mfx_video_core_sync_operation` | 非同期操作の完了待機 |
| `MFXVideoENCODE_Query` | `Encoder::query` | パラメータのサポート可否を事前検証 |
| `MFXVideoENCODE_Reset` | `Encoder::reconfigure` | エンコーダパラメータの動的変更 |
| `MFXVideoENCODE_GetVideoParam` | `Encoder::get_video_param` | Init 後の実効パラメータ取得 |
| `MFXVideoENCODE_GetEncodeStat` | `Encoder::get_encode_stat` | エンコード統計情報の取得 |

## 未使用の VPL API 関数

| VPL 関数 | 用途 |
|---|---|
| `MFXVideoENCODE_QueryIOSurf` | 必要なサーフェス数の問い合わせ。AsyncDepth > 1 にする場合に必要 |

## エンコードパラメータ

### フレーム情報 (mfxFrameInfo)

| パラメータ | VPL フィールド | `EncoderConfig` フィールド |
|---|---|---|
| フレーム幅 | `mfxFrameInfo.Width` | `width` (16 アライメント自動) |
| フレーム高さ | `mfxFrameInfo.Height` | `height` (16 アライメント自動) |
| フレームフォーマット | `mfxFrameInfo.FourCC` | `frame_format` |
| フレームレート | `mfxFrameInfo.FrameRateExtN/D` | `framerate_num/den` |
| アスペクト比 | `mfxFrameInfo.AspectRatioW/H` | `aspect_ratio_w/h` |
| ビット深度 | `mfxFrameInfo.BitDepthLuma/Chroma` | `frame_format` から自動設定 |

### エンコード制御 (mfxInfoMFX)

| パラメータ | VPL フィールド | `EncoderConfig` フィールド |
|---|---|---|
| LowPower (VDENC) | `mfxInfoMFX.LowPower` | `low_power` |
| BRC パラメータ乗数 | `mfxInfoMFX.BRCParamMultiplier` | `brc_param_multiplier` |
| ターゲット品質 (1-7) | `mfxInfoMFX.TargetUsage` | `target_usage` |

### GOP 構造 (mfxInfoMFX)

| パラメータ | VPL フィールド | `EncoderConfig` フィールド |
|---|---|---|
| GOP サイズ | `mfxInfoMFX.GopPicSize` | `gop_pic_size` |
| GOP 参照距離 | `mfxInfoMFX.GopRefDist` | `gop_ref_dist` |
| GOP オプションフラグ | `mfxInfoMFX.GopOptFlag` | `gop_opt_flag` |
| IDR 間隔 | `mfxInfoMFX.IdrInterval` | `idr_interval` |

### レート制御 (mfxInfoMFX)

| パラメータ | VPL フィールド | `EncoderConfig` フィールド | 使用するレート制御モード |
|---|---|---|---|
| レート制御モード | `mfxInfoMFX.RateControlMethod` | `rate_control_mode` | — |
| VBV 初期バッファサイズ | `mfxInfoMFX.InitialDelayInKB` | `initial_delay_in_kb` | CBR/VBR/VCM/QVBR/LA_HRD |
| バッファサイズ | `mfxInfoMFX.BufferSizeInKB` | `buffer_size_in_kb` | 全モード (自動計算可) |
| ターゲットビットレート | `mfxInfoMFX.TargetKbps` | `target_kbps` | CBR/VBR/LA/VCM/QVBR/LA_HRD/AVBR |
| 最大ビットレート | `mfxInfoMFX.MaxKbps` | `max_kbps` | VBR/VCM/QVBR/LA_HRD |
| CQP QP 値 | `mfxInfoMFX.QPI/QPP/QPB` | `qpi/qpp/qpb` | CQP |
| ICQ 品質値 | `mfxInfoMFX.ICQQuality` | `icq_quality` | ICQ/LA_ICQ |
| AVBR 精度 | `mfxInfoMFX.Accuracy` | `accuracy` | AVBR |
| AVBR 収束期間 | `mfxInfoMFX.Convergence` | `convergence` | AVBR |

### スライス・参照フレーム (mfxInfoMFX)

| パラメータ | VPL フィールド | `EncoderConfig` フィールド |
|---|---|---|
| スライス数 | `mfxInfoMFX.NumSlice` | `num_slice` |
| 最大参照フレーム数 | `mfxInfoMFX.NumRefFrame` | `num_ref_frame` |

### 拡張バッファ

| パラメータ | VPL フィールド | `EncoderConfig` フィールド | 使用するレート制御モード |
|---|---|---|---|
| Look Ahead depth | `mfxExtCodingOption2.LookAheadDepth` | `look_ahead_depth` | LA/LA_ICQ/LA_HRD |
| QVBR 品質値 | `mfxExtCodingOption3.QVBRQuality` | `qvbr_quality` | QVBR |

### mfxInfoMFX の union レイアウト

VPL のエンコードパラメータはレート制御モードに応じて union で共用される。

```
__bindgen_anon_1: InitialDelayInKB | QPI | Accuracy
__bindgen_anon_2: TargetKbps       | QPP | ICQQuality
__bindgen_anon_3: MaxKbps          | QPB | Convergence
```

同じ union メンバを同時に使用することはできない。

## 動的パラメータ変更 (Reconfigure)

`Encoder::reconfigure` で以下のパラメータを動的に変更可能。

| パラメータ | `ReconfigureParams` フィールド |
|---|---|
| ターゲットビットレート | `target_kbps` |
| 最大ビットレート | `max_kbps` |
| フレームレート | `framerate_num`, `framerate_den` |

## ファイル構成

```
src/
├── lib.rs        - VplLibrary (静的リンク関数ラッパー)、公開 API の re-export
├── sys.rs        - bindgen 生成バインディングの include
├── bindings.rs   - bindgen が生成した C バインディング
├── encode.rs     - Encoder, EncoderConfig, CodecConfig, FrameFormat, CloseGuard
└── error.rs      - Error 型、mfxStatus テーブル
```

## ビルド構成

- `build.rs` が GitHub から libvpl v2.16.0 を `git clone --depth=1` で取得する
- bindgen でヘッダからバインディングを生成し `src/bindings.rs` に書き込む
- Linux では CMake で libvpl を static build してリンクする
- `DOCS_RS=1` 環境変数が設定されている場合、ライブラリのビルドとリンクをスキップする

## エンコードフロー

1. `Encoder::new(config)` — セッション初期化 → パラメータ設定 → `MFXVideoENCODE_Init`
2. `encoder.encode(frame_data, options)` — サーフェス設定 → `EncodeFrameAsync` → `SyncOperation` → 内部キューに蓄積
3. `encoder.next_frame()` — 内部キューからエンコード済みフレームを取り出す
4. `encoder.finish()` — `EncodeFrameAsync(null surface)` でフラッシュして残りフレームを排出する
5. `Drop` — `MFXVideoENCODE_Close` → `MFXClose`

## 設計上の特徴

- `AsyncDepth = 1` で同期的に動作する (非同期パイプラインは未使用)
- `GopRefDist` は設定可能（デフォルト 1 = B フレームなし）
- `IOPattern = MFX_IOPATTERN_IN_SYSTEM_MEMORY` でシステムメモリ経由の入力
- `AccelerationMode = 0` (NA) だがランタイムが自動的にハードウェアを検出する
- デバイスビジー時は最大 10 回 (各 1ms 間隔) リトライする
- `CloseGuard` でエラー時のリソースリークを防止する
- `unsafe impl Send for Encoder` で単一スレッド前提の Send を実装する
- `BitDepthLuma/Chroma` はフレームフォーマットから自動設定する

## 未実装

### VPL API 関数

| 関数 | 用途 |
|---|---|
| `MFXVideoENCODE_QueryIOSurf` | 必要なサーフェス数の問い合わせ。AsyncDepth > 1 にする場合に必要 |

### エンコードパラメータ

| パラメータ | VPL フィールド | 説明 |
|---|---|---|
| AsyncDepth > 1 | `mfxVideoParam.AsyncDepth` | パイプライン並列化による性能向上 |

### 拡張バッファ (ExtBuffer)

VPL は `mfxExtBuffer` を通じて細かいエンコード制御を提供する。
現在 `mfxExtCodingOption2` (LookAheadDepth) と `mfxExtCodingOption3` (QVBRQuality) のみ対応。

| 拡張バッファ | 用途 | 状態 |
|---|---|---|
| `mfxExtCodingOption` | CAVLC/CABAC 選択、AUD 挿入、最大スライスサイズ等 | 未対応 |
| `mfxExtCodingOption2` | MBBRC、Look Ahead depth、最大フレームサイズ等 | 部分対応 (LookAheadDepth) |
| `mfxExtCodingOption3` | QVBR 品質、QP オフセット、GPU コピー制御等 | 部分対応 (QVBRQuality) |
| `mfxExtEncoderResetOption` | Reset 時に IDR を挿入するかの制御 | 未対応 |
