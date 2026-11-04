# SPS / PPS / VPS を取得する API を追加する

- Priority: High
- Created: 2026-11-04
- Completed: 2026-11-05
- Model: DeepSeek V4 Flash
- Branch: feature/add-sequence-params-api
- Polished: {YYYY-MM-DD}

## 目的

MP4 の SampleEntry (`avcC` / `hvcC` / `av1C`) や RTP の `fmtp` を組み立てるには、SPS / PPS / VPS / Sequence Header OBU の生バイト列が必要になる。

nvcodec-rs は `Encoder::get_sequence_params()` で、video-toolbox-rs は `EncodedFrame` の `sps_list` / `pps_list` / `vps_list` で提供している。vpl-rs には取得手段が無いため、vpl バックエンドでは MP4 / RTP 用のパラメータセットを組み立てられない。

## 現状

- `src/encode.rs` の `Encoder::get_video_param()` は `mfxExtCodingOption2` / `mfxExtCodingOption3` / `mfxExtVP9Param` の読み戻しを行うが、`mfxExtCodingOptionSPSPPS` / `mfxExtCodingOptionVPS` は扱っていない。
- VPL の `MFXVideoENCODE_GetVideoParam` は、attach された `mfxExtCodingOptionSPSPPS` にビットストリームヘッダのコピーを書き戻すと明記されている (`api/vpl/mfxvideo.h`)。
- Intel の GPU 実装 (vpl-gpu-rt) は次のように対応している。
  - H.264: `ImplementationAvc::GetVideoParam` が SPS / PPS の NAL を生成して書き込む
  - HEVC: `Legacy::CopyConfigurable` が `mfxExtCodingOptionVPS` と `mfxExtCodingOptionSPSPPS` に VPS / SPS / PPS をコピーする
  - AV1: `Packer::GenerateSPS` が `SPSBuffer` に Sequence Header OBU を書き、`PPSBufSize = 0` にする
- VPL が返すバイト列は、H.264 / HEVC では `00 00 00 01` のスタートコードを含む (H.264 の `WriteSpsHeader`、HEVC の `PackNALU`)。AV1 は OBU ヘッダのみでスタートコードを含まない。
- コンテナーと RTP が必要とするのはスタートコードを含まない NAL / OBU である (video-toolbox-rs の `sps_list` も同様)。
- H.264 の SPS の VUI と HEVC の VPS / SPS の VUI には、`FrameInfo` のフレームレートが timing info として入る。`Encoder::reconfigure()` でフレームレートを変えると、これらのパラメータセットの内容が変わる。
- `Encoder::reconfigure()` は未出力フレームを drain してから `Reset` するため、`Ok` を返した時点で旧パラメータセットを使うフレームは 1 つも in-flight に残らない。

## 実機確認

Meteor Lake (Intel Arc Graphics, `mfx-gen`) で `MFXVideoENCODE_GetVideoParam` に `mfxExtCodingOptionSPSPPS` を attach して確認した。

- H.264: SPS 39 バイト、PPS 8 バイト。どちらも先頭が `00 00 00 01`
- HEVC: VPS 38 バイト、SPS 57 バイト、PPS 12 バイト。どれも先頭が `00 00 00 01`
- AV1: Sequence Header OBU 15 バイト (OBU タイプ 1)、PPS は 0 バイト
- H.264 / AV1 で `mfxExtCodingOptionVPS` も attach すると `MFXVideoENCODE_GetVideoParam` が `MFX_ERR_UNSUPPORTED` を返す。VPS は HEVC の場合のみ attach する
- フレームレート 30/1 → 60/1 の `reconfigure` で H.264 の SPS が変化した (39 バイトのうち `level_idc` が `0x14` → `0x15`)。完了条件 7 のテストは実現可能

## 設計方針

- `Encoder::get_sequence_params(&self) -> Result<Option<SequenceParams>, Error>` を追加する。`GetVideoParam` は Init 後なら呼べるため、最初のフレームをエンコードする前でも取得できる。
- `SequenceParams` はコーデックごとに型を分けず、単一の構造体にする。`SequenceParams` が存在する時点で SPS (AV1 では Sequence Header OBU) は必ず存在し、VPS は HEVC のみ、PPS は AV1 では存在しない。

  ```rust
  pub struct SequenceParams {
      /// VPS (HEVC のみ)
      pub vps: Option<Vec<u8>>,
      /// SPS (AV1 の場合は Sequence Header OBU)
      pub sps: Vec<u8>,
      /// PPS (AV1 と VP9 では None)
      pub pps: Option<Vec<u8>>,
  }
  ```

- VP9 はパラメータセットを持たないため `Ok(None)` を返す。
- 返す `Vec<u8>` は「スタートコードを含まない NAL / OBU」とする。H.264 / HEVC は先頭の `00 00 00 01` または `00 00 01` を除去し、AV1 はそのまま返す。
- `mfxExtCodingOptionSPSPPS` / `mfxExtCodingOptionVPS` は読み戻し専用のローカルバッファとして構築し、`Encoder::get_video_param()` と同じ手順 (ゼロ初期化した `mfxVideoParam` に attach → `MFXVideoENCODE_GetVideoParam` → `ExtParam` をクリア) で取得する。
- `mfxExtCodingOptionVPS` は HEVC の場合のみ attach する。H.264 / AV1 で attach すると `MFX_ERR_UNSUPPORTED` を返すことを実機で確認済み。
- バッファは 1 KiB 程度を確保し、返ってきた `SPSBufSize` / `PPSBufSize` / `VPSBufSize` で切り詰める。Intel 実装は容量不足時に `MFX_ERR_NOT_ENOUGH_BUFFER` を投げるが、`GetVideoParam` の block 実行は戻り値を無視するため呼び出し側には現れない。十分なサイズを確保する。
- SPS のサイズが 0 の場合 (VP9 を除く) は `Err` にする。「パラメータセットが存在しない」のではなく「取得できなかった」ことを表すため。PPS は AV1 で 0 が正常なので `None` とし、H.264 / HEVC で 0 なら `Err` にする。VPS は HEVC でのみ取得し、0 なら `Err` にする。
- `EncodedFrame` にパラメータセットを載せる方式 (video-toolbox-rs 方式) は採用しない。VPL はフレームと一緒にパラメータセットを返さないため、ワーカーで毎キーフレーム `GetVideoParam` を呼ぶか、`reconfigure` と共有する状態を持つ必要があり、かえって複雑になる。
- 呼び出しスレッドの契約:
  - `encode()` と同じスレッドで、`encode()` と直列になるように呼ぶ
  - コールバック (`EncodeHandler::on_encoded`) から呼び出してはならない。`Encoder` は `Send` だが `!Sync` であり、ハンドラーはワーカーが所有しているため通常はアクセスできないが、自己参照を作れば呼べてしまう。セッションへの別スレッドからのアクセスを増やさないため、禁止を rustdoc に明記する
  - `get_video_param()` と同じく、呼び出しスレッドでローカルバッファを使って `GetVideoParam` を呼ぶ。ワーカー経由にすると、ハンドラーが詰まったときに getter もブロックする
- 取得タイミングの契約:
  - 返るのは現在の作業パラメータセットで、次にエンコードするフレームに適用される
  - in-flight のフレームに適用されたものではない
  - SPS / PPS / VPS の内容が変わるのは `Encoder::new` (Init) と `Encoder::reconfigure` (Reset) だけで、`encode()` では変わらない。`EncodeOptions::frame_type` で IDR を強制しても、その時点の SPS / PPS が挿入されるだけで内容は変わらない。この契約は現在の公開 API の範囲でのもので、実行時にシーケンスを変えられる拡張バッファを公開する場合は見直す
  - `reconfigure()` は未出力フレームを drain してから `Reset` するため、`Ok` を返した後にエンコードするフレームは新しいパラメータセットを使う。パラメータセットの切り替えはコールバックの順序で判別できる
  - `reconfigure()` でフレームレートを変えた場合は `get_sequence_params()` を呼び直すこと。解像度を変更する場合は `Encoder` を作り直す
  - MP4 の `avc1` / `hvc1` は SampleEntry を途中で差し替えられないため、SPS が変わる変更を行う場合は `avc3` / `hev1` にするか、変更しない運用にすること
- SPS / PPS の挿入制御 (`mfxExtInsertHeaders` / `mfxExtCodingOption2.RepeatPPS`) は本 issue の対象外とする。VPL の既定で SPS は I フレーム、PPS は AVC なら毎フレーム、HEVC なら IDR に付く。`mfxExtInsertHeaders` は Intel 実装では未実装で無視され、`RepeatPPS` が制御できるのは PPS の繰り返しだけであり、取得 API とは独立している。

## 完了条件

1. H.264 / HEVC / AV1 で `get_sequence_params()` からパラメータセットが取得できる。
2. H.264 は SPS (NAL タイプ 7) と PPS (NAL タイプ 8)、HEVC は VPS (32) / SPS (33) / PPS (34)、AV1 は Sequence Header OBU (OBU タイプ 1) が返ることをテストで固定する。
3. 返るバイト列にスタートコードが含まれないことをテストで固定する。
4. `Encoder::new` 直後 (フレームをエンコードする前) に呼べることをテストで固定する。
5. `encode()` の前後で SPS / PPS の内容が変わらないことをテストで固定する。
6. VP9 では `Ok(None)` が返ることをテストで固定する。
7. `reconfigure()` でフレームレートを変えると SPS の内容が変わることをテストで固定する。
8. `reconfigure()` 後も呼び出せることをテストで固定する。
9. `CHANGES.md` の `## develop` に `[ADD]` として追記する。
10. `README.md` と `skills/shiguredo-vpl/SKILL.md` に API の説明を追加する。

## 参考

- VPL の仕様: `MFXVideoENCODE_GetVideoParam` の説明 (`api/vpl/mfxvideo.h`)
- Intel 実装: vpl-gpu-rt の H.264 / HEVC / AV1 エンコーダー実装
- 同種の API: nvcodec-rs の `Encoder::get_sequence_params()`、video-toolbox-rs の `EncodedFrame::sps_list` / `pps_list` / `vps_list`

## 解決方法

`SequenceParams` と `Encoder::get_sequence_params()` を追加し、エンコーダーが現在使用している SPS / PPS / VPS を取得できるようにした。H.264 / HEVC はスタートコードを除去した NAL、AV1 は Sequence Header OBU をそのまま返し、VP9 は `Ok(None)` を返す。

### 変更内容

- `SequenceParams` と `Encoder::get_sequence_params()` を追加した
- H.264 / HEVC / AV1 / VP9 のパラメータセット取得とエラー条件を実装した
- パラメータセットの型・スタートコード除去・reconfigure のテストを追加した
- `CHANGES.md` / README.md / `skills/shiguredo-vpl/SKILL.md` を更新した
- docs.rs 向けのダミーバインドに必要な型を追加した
- VPS / PPS の不要な読み戻しバッファを削減し、GetVideoParam の読み戻し処理を共通化した

### 確認

`shiguredo-gmktec-k9` の `~/melpon/vpl-rs` で確認した。

| 確認内容 | 結果 |
| --- | --- |
| `cargo fmt --all -- --check` | pass |
| `cargo clippy --workspace --all-targets -- -D warnings` | pass |
| `INTEL_VPL=1 cargo test --workspace` | pass（lib 34 件、adapter 6 件、roundtrip 36 件、doctest 2 件） |
| `DOCS_RS=1 cargo doc --no-deps` | pass |
