# GPU ハング中に Encoder / Decoder の finish() が永久ブロックする

- Priority: High
- Created: 2026-08-02
- Model: DeepSeek V4 Flash
- Polished: 2026-08-21

## 目的

GPU ハング中に `Encoder::finish()` / `Decoder::finish()` が永久ブロックする問題を解消する。issue 0010 の調査で「真の GPU ハングで Drop を有限時間で返すことは仕様上達成不能」と確定し、`finish()` のブロックは 0010 のスコープ外として切り出されたものである。

## 現状

`src/encode.rs` の `finish()` は、ドレインループで `encode_frame_async` を呼び出し、空サーフェスでエンコードして生成された Sync を worker に送った後、`WorkerCommand::WaitIdle` で worker の処理完了を待ち、`rx.recv()` で応答を待つ。`src/decode.rs` の `finish()` も同様の構造である。

GPU ハング中は、先行する Sync の `SyncOperation` が制御を返さない（`MFX_INFINITE` でタスク完了を待ち続ける）ため、worker の処理が完了せず、`WaitIdle` 応答が返らず、`finish()` が永久ブロックし得る。

issue 0010 は真の GPU ハングでの Drop の有限時間化が仕様上達成不能であると確定した。`finish()` 経路のブロックも 0010 の対象外である。なお `finish()` の中断（タイムアウト + stopping フラグ）は Close を伴わない中断であり Drop とは性質が異なるため、本 issue で検討する。

## 設計方針

- 中断メカニズム（`stopping` フラグ、タイムアウト付き SyncOperation のループ）を本 issue で新規に設計し、`finish()` 経路に適用する。0010 はこの中断メカニズムを導入しない（Drop 経路では Close との非同期契約のため適用不可）。
- 具体的な設計は issue 0010 適用後の実装状態を確認してから決定する（0010 が先に適用される前提）。
- 修正の有効範囲は「SyncOperation がタイムアウトで必ず制御を返す」という VPL の契約に依存する。ドライバレベルでハングして SyncOperation 自体が返らない場合、有限タイムアウトでもブロックは解消しない点に注意する。
- **中断後のセッション破棄の設計が必要**: 中断メカニズムはタスクを in-flight のまま残す。中断後に `Encoder` / `Decoder` を Drop すると、未完了タスクがあるため Close で libmfx-gen が SIGSEGV する（0010 の実機検証結果）。中断後の Drop 経路（Close をスキップするか、タスク完了を待つか）を設計に含める必要がある。タスク完了を待つなら `finish()` のブロックが Drop に移るだけであり、真の GPU ハングでの有限時間化は本 issue でも達成できない可能性がある。

## 完了条件

以下すべてを満たす。

1. GPU ハング中（SyncOperation がタイムアウトする状況）でも `Encoder::finish()` が有限時間で制御を返す（エラーを返すか、中断される）。
2. `Decoder::finish()` も同様に有限時間で制御を返す。
3. `CHANGES.md` の `## develop` に `[FIX]` として追記する。

## 解決方法

- `src/encode.rs` の `finish()` と `src/decode.rs` の `finish()` に、本 issue で新規に導入する中断メカニズム（`stopping` フラグ、タイムアウト付き SyncOperation）を適用する。
- worker 側の `WaitIdle` 応答が停止中に返るようにする。
- 中断後の Drop 経路のセッション破棄（Close の扱い）を設計に含める（設計方針参照）。

## 参考

- 関連 issue: 0010（調査により真の GPU ハングでの Drop の有限時間化が仕様上達成不能と確定。本 issue は 0010 のスコープ外として切り出された。0010 は中断メカニズムを導入しないため、本 issue で新規に設計する。中断後の Close と in-flight タスクの SIGSEGV 相互作用は 0010 の実機検証結果に基づく）
- 前提: issue 0010 が完了していること（0010 の Encoder 側の `SyncData.frame_seq` 変更と干渉しないよう、0010 適用後に重ねる）

## pending の理由

本 issue の前提だった「SyncOperation を有限タイムアウト化して中断する」設計は、issue 0010 の調査で廃案となった。調査の結果、以下のとおり本 issue の対応は成立しない。

- **ドライバがタスク結果としてデバイスエラーを返す場合**: SyncOperation はエラーで wait が起きて制御を返すため、worker は処理を続行し `WaitIdle` に応答し、`finish()` は現状の実装で既に有限時間でエラーを返す（修正不要）。
- **真の GPU ハング（ドライバがエラーを返さない）場合**: タイムアウト + stopping フラグで `finish()` を有限時間で返すには、タスクを in-flight のまま残す必要がある。その後の `Drop` → Close は未完了タスクで libmfx-gen が SIGSEGV する（0010 の実機検証結果）ため、Close をスキップしてセッションをリークする未文書化の機構を導入しない限り達成できない。

将来ドライバのハング検知が改善され、真のハング時にも必ずデバイスエラーが返るようになれば、`finish()` は自動的に有限時間で返るため本 issue は実質不要になる。ドライバ実装の改善を待つため保留する。
