# エンコーダーの入力フォーマットを拡充する (10bit / 4:4:4)

- Priority: Medium
- Created: 2026-11-04
- Completed: 2026-11-07
- Model: DeepSeek V4 Flash
- Branch: feature/add-encoder-input-formats
- Polished: {YYYY-MM-DD}

## 目的

nvcodec-rs は 10bit / 4:4:4 を含む入力バッファフォーマットを受け付け、video-toolbox-rs も I420 / NV12 を選べる。vpl-rs の `FrameFormat` は NV12 / YUY2 / BGRA の 8bit に限られる。

vpl-rs は `H264Profile::High10`、`HevcProfile::Main10`、`Vp9Profile::Profile2` / `Profile3` (10bit) を公開しているが、10bit の入力を受け付ける手段が無いため、これらのプロファイルは実質使えない。

planar 4:2:0 (I420 / YV12 / I010) は VPL の FourCC としては定義されているが、Intel のエンコーダーは入力として受け付けないことを実機で確認した。本 issue では 10bit と 4:4:4 を対象とし、planar 入力は対象外とする。

## 現状

- `src/encode.rs` の `FrameFormat` は `Nv12` / `Yuy2` / `Bgra` の 3 種類。
- `FrameFormat::bit_depth()` は常に 8 を返す。
- `FrameFormat::frame_size()` と `copy_to_surface_planes()` は 8bit の NV12 / YUY2 / BGRA のみを扱う。
- `Encoder::new` のピッチ検証は `aligned_width * 1/2/4` を前提にしている。
- `copy_to_surface_planes()` はピッチを `mfxFrameData.Pitch` から取得しており、`PitchHigh` を見ていない。

## 実機確認

Meteor Lake (Intel Arc Graphics, `mfx-gen`) で `MFXVideoENCODE_Init` と 1 フレームのエンコードを試した結果は次のとおり。

| 形式 | AVC | HEVC | AV1 |
|---|---|---|---|
| NV12 | OK | OK | OK |
| YUY2 | OK | OK | NG |
| RGB4 (BGRA) | OK | OK | NG |
| AYUV (4:4:4 8bit) | OK | OK | NG |
| P010 (4:2:0 10bit) | NG | OK | OK |
| Y410 (4:4:4 10bit) | NG | OK | NG |
| IYUV (I420) | NG | NG | NG |
| YV12 | NG | NG | NG |
| I010 (4:2:0 10bit planar) | NG | NG | NG |

- `MFXVideoENCODE_Init` が成功した場合も `GetVideoParam` で実効値を確認し、要求した FourCC とビット深度が維持されていることを確認した
- IYUV / YV12 / I010 は `MFX_ERR_INVALID_VIDEO_PARAM` で拒否された。`mfxImplDescription` のエンコーダーの `ColorFormats` にも含まれていない
- HEVC Main10 で NV12 (8bit) を渡すと `MFX_ERR_INVALID_VIDEO_PARAM` になった。プロファイルと入力ビット深度の整合が必要
- AVC は 10bit 入力を受け付けない (`P010` / `Y410` が NG)
- AV1 は `NV12` と `P010` のみ受け付ける
- VP9 は issue 0001 のため今回の検証対象外

## 設計方針

- `FrameFormat` に次の形式を追加する。
  - `P010` (`MFX_FOURCC_P010`、semi-planar 4:2:0 10bit)
  - `Ayuv` (`MFX_FOURCC_AYUV`、packed 4:4:4 8bit)
  - `Y410` (`MFX_FOURCC_Y410`、packed 4:4:4 10bit)
- `fourcc()` / `chroma_format()` / `bit_depth()` を各形式に対応させる。
- `frame_size()` を各形式のサンプル数に合わせる。
  - P010: `pixels * 3` (1 サンプル 2 バイト)
  - AYUV / Y410: `pixels * 4`
- `copy_to_surface_planes()` を各形式に対応させる。
  - P010 は 1 サンプル 2 バイトとしてコピーする。10bit の値が 16bit ワードの上位ビットに入るレイアウトであることを rustdoc に明記する
  - packed 4:4:4 形式のデータポインタは VPL の実装に従う (既存の BGRA は `B` を使用している)
- ピッチは `(PitchHigh << 16) + PitchLow` で計算する。
- `Encoder::new` のピッチ検証を形式ごとのバイト数に合わせる。
- プロファイルと入力ビット深度の整合を検証する。HEVC Main10 と 10bit 対応コーデックでは 10bit 入力が必要で、8bit 入力を渡すと Init が失敗する。`EncoderConfig` の検証で早期にエラーにするか、rustdoc で明記する。
- GPU が対応していない形式は `Encoder::new` が VPL のエラーを返す。事前に判定したい場合は issue 0033 のケーパビリティ照会を使う。
- 入力バッファのサイズ契約 (coded size を要求する) は本 issue では変更しない。
- テストは、ケーパビリティ照会で対応を確認できた形式についてラウンドトリップを行う。非対応の場合はスキップ理由を出力する (silent pass にしない)。

## 完了条件

1. `FrameFormat` に `P010` / `Ayuv` / `Y410` が追加される。
2. 追加した形式で `Encoder::new` / `encode()` が動く (GPU とコーデックが対応している場合)。
3. `frame_size()` の単体テストを追加する。
4. 対応している形式のラウンドトリップテストを追加する。
5. 非対応形式とプロファイルの組み合わせの扱い (エラー / スキップ) を rustdoc とテストで明確にする。
6. `CHANGES.md` と `README.md` / `skills/shiguredo-vpl/SKILL.md` を更新する。

## 解決方法

`FrameFormat` に `P010` / `Ayuv` / `Y410` を追加し、エンコーダーの入力フォーマットとして扱えるようにした。

### 変更内容

- `FrameFormat` に `P010` / `Ayuv` / `Y410` を追加し、各 variant のデータ配置をドキュメント化した
- `frame_size()` を各形式のバイト数に対応させ、P010 は `mfxFrameInfo.Shift` に 1 を設定した
- `mfxFrameData` のピッチを `PitchHigh` と `PitchLow` から計算し、P010 / AYUV / Y410 のプレーンコピーに対応した
- 入力フォーマットとプロファイルの組み合わせは事前検証せず、`MFXVideoENCODE_Init` の結果に従うようにした。将来の GPU / VPL の更新で受け付ける組み合わせが増える可能性があるため
- P010 / AYUV / Y410 の初期化パラメータ検証と、P010 / Y410 / AYUV 入力のラウンドトリップおよびルマ PSNR をテストに追加した
- `CHANGES.md` / `README.md` / `skills/shiguredo-vpl/SKILL.md` を更新し、SKILL.md に実機で確認した制約を追記した
- 4:4:4 エンコードは GPU / プロファイル依存で、Scc + Y410 では出力が得られない組み合わせがあることを SKILL.md に記録した

4:4:4 出力のデコード対応 (AYUV / Y410) は本 issue の対象外とし、別 issue で扱う。

### 確認

Intel GPU を搭載した実機で確認した。

| 確認内容 | 結果 |
| --- | --- |
| `cargo fmt --all -- --check` | pass |
| `cargo clippy --workspace --all-targets -- -D warnings` | pass |
| `INTEL_VPL=1 cargo test --workspace -- --test-threads=1` | pass (lib 56 件、adapter 6 件、codec_info 1 件、roundtrip 44 件、doctest 2 件) |
| `INTEL_VPL=1 cargo doc --no-deps` | pass |

## 参考

- VPL の FourCC: `api/vpl/mfxstructures.h`
- 同種の API: nvcodec-rs の `BufferFormat`、video-toolbox-rs の `PixelFormat` / `FrameData`
