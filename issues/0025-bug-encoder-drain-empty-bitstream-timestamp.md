# Encoder のドレイン空フレームが TimeStamp = 0 のまま生成され pending に引き当てられない

- Priority: Medium
- Created: 2026-08-02
- Completed: {YYYY-MM-DD}
- Model: DeepSeek V4 Flash
- Branch: feature/fix-encoder-drain-empty-bitstream-timestamp
- Polished: {YYYY-MM-DD}

## 目的

`Encoder::finish()` のドレインで生成される空ビットストリーム（`DataLength == 0`）が `TimeStamp = 0` のまま worker に渡され、encoder 側の pending 引き当て（`frame_seq` 完全一致）に失敗して `Err` 通知になる可能性を解消する。issue 0008 の調査で「エンコーダ側の既存の課題であり、対応は別 issue として切り出す」と明記されたものである。

## 現状

`src/encode.rs` の `create_bitstream()` は `Box::new(unsafe { std::mem::zeroed() })` で初期化するため、`mfxBitstream.TimeStamp` は常に `0` になる。`finish()` のドレインループでもこの `create_bitstream()` を使って空フレームをエンコードし、生成されたビットストリームを `WorkerCommand::Sync(SyncData)` として worker に送る。

一方、encoder の pending 引き当ては `frame_seq`（`encode()` 呼び出しごとに 1 ずつ増える値）を `mfxFrameSurface1.Data.TimeStamp` に載せ、出力ビットストリームの `TimeStamp` と完全一致で引き当てる方式である。issue 0013 適用後は `frame_count` が 1 スタートになるため、`frame_seq` が 1 以上の値になり、ドレイン空フレームの `TimeStamp = 0` はどの pending にも引き当てられず「no pending frame for bitstream timestamp 0」の `Err` 通知になる可能性がある。

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

- `src/encode.rs` の `finish()` のドレインループと worker 側の Sync 処理（`sync_and_collect` 等）を確認し、ドレイン空フレームの `TimeStamp = 0` が pending 引き当てに失敗しないようにする。
- 必要に応じてテスト（ラウンドトリップテストのドレイン経路）を追加・更新する。

## 参考

- 関連 issue: 0008（Decoder の user_data 対応付け修正。本 issue を「エンコーダ側の既存の課題」として切り出した）、0013（Encoder の frame_seq / TimeStamp 衝突修正。`frame_count` を 1 スタートにする）
- 前提: issue 0008、0013 が完了していること
