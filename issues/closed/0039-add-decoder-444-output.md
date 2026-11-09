# デコーダーの 4:4:4 出力を正しく扱い、恒等性を検証する

- Priority: Medium
- Created: 2026-11-07
- Completed: 2026-11-09
- Model: DeepSeek V4 Flash
- Branch: feature/add-decoder-444-output
- Polished: {YYYY-MM-DD}

## 目的

エンコーダーが AYUV / Y410 入力に対応したことで、HEVC の Rext / Scc プロファイルでは 4:4:4 のストリームを生成できる。しかしデコーダーは 4:4:4 の出力を正しく扱えず、AYUV では壊れたデータが返り、Y410 ではデコード自体がエラーになる。デコード出力のフォーマットを公開し、4:4:4 のプレーンを正しく取得できるようにしたうえで、エンコード→デコードがほぼ恒等になることを検証する。

## 現状

- `Decoder::initialize` は `DecodeHeader` の前に `MFX_FOURCC_NV12` を設定しているが、`DecodeHeader` がストリームの色形式で上書きする
- `read_decoded_surface` は `mfxFrameData.Y` / `UV` と `Pitch` から NV12 を前提にプレーンサイズを計算し、`DecodedFrame::y()` / `uv()` に詰めている
- 4:4:4 のパック形式 (AYUV / Y410) は Y / U / V が画素内にインターリーブされるため、この計算は成立しない
- `DecodedFrame` から実際の出力フォーマット (FourCC / ChromaFormat / BitDepth / Shift) を取得する API は無い
- 4:4:4 出力の恒等性テストは存在しない

## 実機確認

Meteor Lake (mfx-gen) で確認した現在の挙動:

| ストリーム | 現在の挙動 |
|---|---|
| HEVC Rext + AYUV | デコードは成功するが、`y()` はフレーム先頭から 2 バイトずれた全体サイズ、`uv()` はフレーム途中からの読み出しになり、壊れたデータになる |
| HEVC Rext + Y410 | `mfxFrameData.Y` が null のため `read_decoded_surface` が "decoded surface has null plane pointers" を返し、デコード結果を取得できない |
| HEVC Scc + AYUV / Y410 | 同様 |

## 設計方針

- `DecodedFrame` に実際の出力フォーマットを返す API を追加する (例: `frame_format() -> FrameFormat`)。必要に応じて `bit_depth()` / `shift()` も公開する
- 4:4:4 パック形式のプレーンアクセス方法を決める。候補:
  - `data()` でフレーム全体を返し、フォーマットと組み合わせて利用者側で解釈する
  - `plane(Plane)` のような API を追加し、パック形式では U / V を `None` にする
  - 4:4:4 専用のアクセサを追加する
- `read_decoded_surface` のプレーンサイズ計算をフォーマットごとに分ける
- 既存の `y()` / `uv()` は NV12 / P010 では挙動を変えない。4:4:4 の扱いを追加 API にするか、破壊的変更として `CHANGES.md` に `[CHANGE]` を記載する
- デコーダーに出力フォーマットの指定 API は追加しない。VPL では出力フォーマットがストリームのネイティブ形式で決まり、実機で試した限り YUY2 出力は不可、8bit ストリームへの P010 指定もプロファイル依存で不安定なため
- 恒等性テスト: HEVC Rext / Scc + AYUV / Y410 で参照フレームをエンコード→デコードし、同じ 4:4:4 フォーマットで Y / U / V の PSNR を比較する。QPI は 10〜16、フレーム間でパターンを動かす

## 完了条件

1. `DecodedFrame` から実際の出力フォーマットを取得できる
2. HEVC Rext / Scc + AYUV / Y410 のデコードで、4:4:4 の Y / U / V を正しく取得できる (Y410 で null ポインタエラーにならない)
3. 4:4:4 のエンコード→デコード恒等性テスト (PSNR) が追加され、実機で通る
4. 既存の NV12 / P010 のデコード API の挙動が変わらない (変える場合は `CHANGES.md` に記載する)
5. `CHANGES.md` と `README.md` / `skills/shiguredo-vpl/SKILL.md` を更新する

## 参考

- VPL の出力フォーマット: `api/vpl/mfxstructures.h` の `mfxFrameData` / `mfxY410`
- エンコーダー側の 4:4:4 入力対応: `FrameFormat::Ayuv` / `FrameFormat::Y410`
- デコーダーの色形式ケーパビリティ: `supported_codecs` の `DecodingInfo` / `MemoryTypeCapabilities::color_formats`
- 汎用の恒等性テスト: issue 0038

## 解決方法

`DecodedFrame` の API を出力形式に依存しない形に変更し、4:4:4 のデコードと恒等性検証に対応した。

### 変更内容

- `DecodedFrame` に `frame_format` と `data` を追加し、`y` / `uv` / `pitch` を削除した
  - `data` は `FrameData::SemiPlanar` / `FrameData::Packed` を返す
  - `FrameData` はエンコード入力とデコード出力の共通型として `codec_info` に移動した
- `read_decoded_surface` を形式別に読み取るようにした
  - NV12 / P010 は `Data.Y` / `Data.UV`
  - AYUV は `Data.V`
  - Y410 は `Data.Y410`
  - P010 は `Info.Shift` を検証する
  - ピッチは `PitchHigh` / `PitchLow` の合成値を使い、`bytes_per_row` 以上であることを検証する
- 4:4:4 の恒等性テストを追加した
  - HEVC Rext / Scc + AYUV / Y410 の 320x240
  - HEVC Rext + AYUV / Y410 の 318x238（ピッチが行のバイト数より大きくなる経路）
  - Scc + Y410 は実機で QPI / QPP 20 以下だとエンコードが完了しないため、22 を使う
- テストの形式対応表を `FormatInfo` / `format_info` に集約した
- `README.md` / `skills/shiguredo-vpl/SKILL.md` / `CHANGES.md` を更新した

### 確認

Intel GPU (Meteor Lake, mfx-gen) を搭載した実機で確認した。

| 確認内容 | 結果 |
| --- | --- |
| `cargo fmt --all -- --check` | pass |
| `cargo clippy --workspace --all-targets -- -D warnings` | pass |
| `cargo test -- --test-threads=1` | pass (lib 55 件、adapter 5 件、roundtrip 67 件、doctest 2 件) |
| HEVC Rext / Scc + AYUV / Y410 の恒等性 | pass |
| 318x238 の 4:4:4 ピッチ検証 | pass |
