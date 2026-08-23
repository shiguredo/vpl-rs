# Encoder のドレイン空フレームが TimeStamp = 0 のまま生成され pending に引き当てられない

- Priority: Medium
- Created: 2026-08-02
- Completed: 2026-08-23
- Model: DeepSeek V4 Flash
- Branch: feature/fix-encoder-drain-empty-bitstream-timestamp
- Polished: {YYYY-MM-DD}

## 目的

`Encoder::finish()` のドレインで生成される空ビットストリーム（`DataLength == 0`）が `TimeStamp = 0` のまま worker に渡され、encoder 側の pending 引き当て（`frame_seq` 完全一致）に失敗して `Err` 通知になる可能性を解消する。issue 0008 の調査で「エンコーダ側の既存の課題であり、対応は別 issue として切り出す」と明記されたものである。

## 現状

`src/encode.rs` の `create_bitstream()` は `Box::new(unsafe { std::mem::zeroed() })` で初期化するため、`mfxBitstream.TimeStamp` は常に `0` になる。`finish()` のドレインループでもこの `create_bitstream()` を使って空フレームをエンコードし、生成されたビットストリームを `WorkerCommand::Sync(SyncData)` として worker に送る。

一方、encoder の pending 引き当ては `frame_seq`（`encode()` 呼び出しごとに 1 ずつ増える値）を `mfxFrameSurface1.Data.TimeStamp` に載せ、出力ビットストリームの `TimeStamp` と完全一致で引き当てる方式である。`frame_count` は 0 スタートのまま（issue 0013 は closed で「`frame_count` 1 スタート化」は不採用）であるため、`frame_seq = 0` の pending（1 フレーム目）が未消費で残っている場合、ドレイン空フレームの `TimeStamp = 0` がそれを誤消費し、実フレーム 0 の出力が後から「no pending frame for bitstream timestamp 0」の `Err` 通知になる可能性がある。また、`frame_seq = 0` の pending が残っていない場合は、ドレイン空フレームの `TimeStamp = 0` がどの pending にも引き当てられず、同様に「no pending frame for bitstream timestamp 0」の `Err` 通知になり得る（0008 のテストヘルパーではこの `Err` をドレイン期の空フレームに限って許容する。0008 のテストヘルパー設計参照）。

## 設計方針

- ドレイン空フレーム（`DataLength == 0`）を `Err` 通知にしない。ドレイン期の空データは正常な終了処理の一部であり、エラーとして扱わない。
- 0008 の設計方針は「ドレインの空フレームや異常出力が原因の正常な破棄として扱う」としており、本 issue は encoder 側の空フレーム生成経路をそれに整合させる。
- 対応方法の候補:
  - `finish()` のドレインループで生成した空ビットストリームを pending 引き当ての対象から除外する
  - ドレイン空フレームを引き当て失敗時に silent に破棄する（0008 の「引き当て失敗は Err にしない」方針と同様）
- 実装の詳細は、0008 適用後の encoder / decoder の実装状態を確認してから決定する（0008 が先に適用される前提）。

## 完了条件

以下すべてを満たす。

1. `Encoder::finish()` のドレインで生成される空ビットストリーム（`DataLength == 0`）が `Err` 通知にならず、正常に破棄または処理される。
2. issue 0008 のテストヘルパーの「ドレイン期の空フレームに限って `Err` を許容する」回避策が不要になる（または本 issue の対応と整合する形に更新される）。
3. `CHANGES.md` の `## develop` の `### misc` サブセクションに `[FIX]` として追記する。

## 解決方法

実機（Intel iHD / VA-API）で検証した結果、`Encoder::finish()` のドレインで `DataLength == 0` の空ビットストリームは発生しないことが分かった。VPL はドレイン時に出力すべきフレームが尽きると `MFX_ERR_MORE_DATA`（`encode_frame_async` の `None`）を返して終了するため、`syncp` が返ってくるのは実フレームがある場合のみである。

したがって、本 issue が前提としていた「空フレームの `TimeStamp = 0` による pending 誤消費」は再現しない。対応として以下を行った。

- `src/encode.rs` の `sync_and_collect` にあった `if length == 0` 防御分岐を削除した（空フレームが返る経路が存在しないため）。
- `tests/test_roundtrip.rs` の `roundtrip_b_frames` ヘルパーにあったドレイン空フレームの除外と、`Err` 通知の許容を削除した。

### 完了条件の対応

1. ドレイン空ビットストリーム（`DataLength == 0`）が `Err` 通知にならず正常に破棄または処理される — 空ビットストリーム自体が発生しないため、既存のラウンドトリップテストの pass をもって確認。
2. issue 0008 のテストヘルパーの「ドレイン期の空フレームに限って `Err` を許容する」回避策 — `roundtrip_b_frames` から除去した。
3. `CHANGES.md` への `[FIX]` 追記 — 変更種別として報告するほどの影響がない（VPL が空ビットストリームを返さないことを明らかにしただけで、挙動は変更していない）ため追加しない。

## 参考

- 関連 issue: 0008（Decoder の user_data 対応付け修正。本 issue を「エンコーダ側の既存の課題」として切り出した。0008 のテストヘルパーでは本 issue の対応前の回避策として、ドレイン期の空フレームに限って「no pending frame for bitstream timestamp 0」の `Err` を許容する）、0013（Encoder の frame_seq / TimeStamp 衝突修正。**closed で「`frame_count` 1 スタート化」は不採用のため、本 issue の前提は 0013 に依存しない**）
- 前提: issue 0008 が完了していること（0013 は closed で不採用のため前提としない）
