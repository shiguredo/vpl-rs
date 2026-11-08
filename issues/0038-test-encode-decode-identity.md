# エンコード→デコードの恒等性を検証するテストを追加する

- Priority: Medium
- Created: 2026-11-07
- Completed: 2026-11-09
- Model: DeepSeek V4 Flash
- Branch: feature/add-encode-decode-identity-test
- Polished: {YYYY-MM-DD}

## 目的

現在のラウンドトリップテストは、デコード結果についてフレーム数とサイズしか検証しておらず、壊れた映像が返っても成功扱いになる。エンコード→デコードした結果が入力とほぼ同じデータになることを、主要な入力フォーマットとコーデックの組み合わせで検証する。

## 現状

- `tests/test_roundtrip.rs` の `roundtrip_format` は、デコード結果のフレーム数と幅・高さのみを検証している
- 内容を検証しているのは、NV12 のカラーバー PSNR と AYUV 入力のルマ PSNR のみ
- P010 / Y410 / YUY2 / BGRA 入力は、フレームが返ることしか確認していない
- デコーダーの出力フォーマットはストリームのネイティブ形式で決まり、入力フォーマットと異なる場合がある (例: YUY2 / AYUV 入力 → NV12 出力)

## 実機確認

主要な入力フォーマットのデコード出力 (Meteor Lake, mfx-gen):

| エンコーダ | 入力 | デコード出力 |
|---|---|---|
| H.264 High | NV12 / YUY2 / AYUV | NV12 |
| HEVC Main | NV12 / YUY2 / AYUV | NV12 |
| HEVC Main10 | P010 | P010 |
| HEVC Main10 | Y410 | P010 |
| AV1 Main | NV12 / P010 | NV12 / P010 |
| VP9 Profile0 | NV12 | NV12 |
| VP9 Profile2 | P010 | P010 |

## 設計方針

- `supported_codecs` の全組み合わせはテストしない。capability に載っていても Init が失敗する組み合わせがあるため、主要な経路を明示的なテーブルで持つ
- 同一フォーマットでデコードされる経路 (NV12 → NV12、P010 → P010) は、Y / U / V の PSNR を計算して閾値以上を検証する
- 異なるフォーマットに変換される経路 (YUY2 / AYUV / Y410 → NV12 / P010) は、変換で失われないルマの PSNR と、色バーの領域ごとのクロマ平均で検証する
- BGRA 入力は RGB→YUV 変換が入るため、グレーランプのルマ相関と「単色でない」ことの検証にとどめる
- 参照フレームは、グラデーション + 高周波チェッカー + 色バー + フレームごとの移動パターンで生成する
- エンコードは `RateControlMode::Cqp` (QPI / QPP は 10〜16)、`gop_ref_dist = 1`、5 フレーム程度とし、`user_data` で入力と出力を対応付ける
- PSNR は 8bit / 10bit のサンプル値に対して計算する
- 壊れていないことの確認として、ルマの分散、隣接フレームの差分、クロマの変化も検証する
- 閾値は実測して決め、根拠をコメントに残す
- 4:4:4 出力 (AYUV / Y410) の検証は issue 0039 で扱うため、本 issue の対象外とする

## 完了条件

1. NV12 / P010 の同一フォーマット経路で、Y / U / V の PSNR 検証が追加される
2. YUY2 / AYUV / Y410 入力のルマ PSNR 検証と、クロマ領域平均の検証が追加される
3. BGRA 入力で、ルマの相関と単色でないことの検証が追加される
4. ルマの分散・フレーム間差分のサニティチェックが追加される
5. 閾値の根拠がコメントに残される
6. 既存の重複テスト (NV12 カラーバー PSNR、AYUV ルマ PSNR) が整理される

## 参考

- 現在のテスト: `tests/test_roundtrip.rs` の `identity` モジュール
- デコーダーの出力フォーマット: `DecodedFrame` / `FrameFormat`
- 4:4:4 出力の対応と検証: issue 0039

## 解決方法

`tests/test_roundtrip.rs` に `identity` モジュールを追加し、エンコード→デコードの恒等性を主要な経路で検証するようにした。

### 変更内容

- 参照フレーム生成を追加した
  - グラデーション、高周波チェッカー、7 色の色バー、フレームごとに移動する縞と帯を合成する
  - 同じ参照フレームから NV12 / P010 / YUY2 / AYUV / Y410 の入力バッファを生成する
  - 8bit の参照値を 10bit の P010 / Y410 では 4 倍して格納する
- エンコーダー設定を QPI = QPP = 12、`gop_ref_dist = 1`、5 フレームに統一した
- 主要な経路を明示的なテーブルで検証するテストを追加した
  - H.264 High: NV12 / YUY2 / AYUV → NV12
  - HEVC Main: NV12 / YUY2 / AYUV → NV12
  - HEVC Main10: P010 / Y410 → P010
  - AV1 Main: NV12 / P010 → NV12 / P010
  - VP9 Profile0 / Profile2: NV12 / P010 → NV12 / P010
  - BGRA 入力: グレーランプのルマ相関と単色でないことを検証
- 同一フォーマット経路では Y / U / V の PSNR を検証するようにした
- 変換経路ではルマの PSNR と色バー領域ごとのクロマ平均を検証するようにした
- サニティチェックとして、ルマの分散、隣接フレームの平均絶対差分、色バー間のクロマ平均の広がりを追加した
- 重複していたテストを削除した
  - NV12 カラーバーの CBR / CQP テスト (H.264 / HEVC / AV1)
  - AYUV のルマ PSNR テスト
  - 10bit 入力のルマ PSNR テスト (HEVC Main10 / AV1 Main)
  - BGRA / AYUV のフレーム数のみを検証するテスト
- `CHANGES.md` にテスト追加のエントリを追加した
- 閾値は実機の実測値に基づいて決め、根拠を定数のコメントに残した

### 確認

Intel GPU (Meteor Lake, mfx-gen) を搭載した実機で確認した。

| 確認内容 | 結果 |
| --- | --- |
| `cargo fmt --all -- --check` | pass |
| `cargo clippy --workspace --all-targets -- -D warnings` | pass |
| `INTEL_VPL=1 cargo test --workspace -- --test-threads=1` | pass (lib 56 件、adapter 6 件、codec_info 1 件、roundtrip 59 件、doctest 2 件) |
