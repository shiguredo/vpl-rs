# GPU ハング中に Encoder / Decoder の finish() が永久ブロックする

- Priority: High
- Created: 2026-08-02
- Completed: {YYYY-MM-DD}
- Model: DeepSeek V4 Flash
- Branch: feature/fix-finish-block-on-gpu-hang
- Polished: {YYYY-MM-DD}

## 目的

GPU ハング中に `Encoder::finish()` / `Decoder::finish()` が永久ブロックする問題を解消する。issue 0010 は Drop 経路（`stop_worker` → `join`）のデッドロック修正をスコープとしており、`finish()` のブロックは「別 issue で対応する」と明記されて切り出されたものである。

## 現状

`src/encode.rs` の `finish()` は、ドレインループで `encode_frame_async` を呼び出し、空サーフェスでエンコードして生成された Sync を worker に送った後、`WorkerCommand::WaitIdle` で worker の処理完了を待ち、`rx.recv()` で応答を待つ。`src/decode.rs` の `finish()` も同様の構造である。

GPU ハング中は、先行する Sync の `SyncOperation` がタイムアウト（issue 0010 で有限タイムアウト化される）と再試行を繰り返すため、worker の処理が完了せず、`WaitIdle` 応答が返らず、`finish()` が永久ブロックし得る。

issue 0010 の完了条件は Drop 経路のデッドロック解消のみであり、`finish()` 経路のブロックはスコープ外とされている。

## 設計方針

- issue 0010 の設計（`stopping` フラグ、Sync アームの `MFX_ERR_ABORTED` 中断判別、タイムアウト付き SyncOperation）を `finish()` 経路にも適用する。
- 具体的な設計は issue 0010 適用後の実装状態を確認してから決定する（0010 が先に適用される前提）。
- 修正の有効範囲は「SyncOperation がタイムアウトで必ず制御を返す」という VPL の契約に依存する。ドライバレベルでハングして SyncOperation 自体が返らない場合、有限タイムアウトでもブロックは解消しない点に注意する。

## 完了条件

以下すべてを満たす。

1. GPU ハング中（SyncOperation がタイムアウトする状況）でも `Encoder::finish()` が有限時間で制御を返す（エラーを返すか、中断される）。
2. `Decoder::finish()` も同様に有限時間で制御を返す。
3. `CHANGES.md` の `## develop` に `[FIX]` として追記する。

## 解決方法

- `src/encode.rs` の `finish()` と `src/decode.rs` の `finish()` に、issue 0010 で導入する中断メカニズム（`stopping` フラグ等）を適用する。
- worker 側の `WaitIdle` 応答が停止中に返るようにする。

## 参考

- 関連 issue: 0010（Drop 経路のデッドロック修正。本 issue をスコープ外として切り出した。設計の前提）
- 前提: issue 0010 が完了していること
