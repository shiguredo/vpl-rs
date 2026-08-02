# Encoder / Decoder の Drop が SyncOperation の `MFX_INFINITE` でデッドロックする

- Priority: High
- Created: 2026-07-01
- Model: Opus 4.7
- Branch: feature/fix-drop-deadlock-on-sync-operation-infinite
- Polished: 2026-08-02

## 目的

Worker スレッドが `MFXVideoCORE_SyncOperation(session, syncp, MFX_INFINITE)` の中で無限待機している最中に `Encoder` / `Decoder` を Drop すると、Worker が Stop コマンドを受信できずデッドロックするバグを修正する。GPU ハング時や long-running な SyncOperation 中にプロセスをシャットダウンできない。

## 優先度根拠

High。以下による。

- **GPU ハング時に他のリソース解放も止まる**: Drop 経路が返らないので、その後に続くリソース解放（`Session` の Drop → `MFXClose` + `MFXUnload`）にも到達できない。
- **既存の `DEVICE_BUSY` リトライ上限（30 回）を導入した意図が骨抜きになる**: 上限リトライで途中エラーを返せるようにしても、Sync のブロックで止まるなら意味がない。
- **本問題は隠れているだけで実装上の設計欠陥**: 通常運用では SyncOperation は数十 ms 以内に返るため顕在化しないが、GPU ドライバのタイムアウトや GPU 実装のバグで長時間ブロックすると容易に再現する。

## 現状

### Encoder 側

`src/encode.rs` の `sync_and_collect`:

```rust
let status = lib.mfx_video_core_sync_operation(
    session_handle as sys::mfxSession,
    syncp,
    sys::MFX_INFINITE,
);
Error::check_mfx(status, "MFXVideoCORE_SyncOperation")?;
```

Worker はこのブロッキング呼び出しの中で無限待機する。

`Encoder` の Drop 経路（`stop_worker` → `Drop`）:

```rust
fn stop_worker(&mut self) {
    if let Some(handle) = self.worker_handle.take() {
        let _ = self.worker_tx.send(WorkerCommand::Stop);
        let _ = handle.join();
    }
}
```

`worker_tx.send(Stop)` は成功するが、Worker は `worker_rx.recv()` の待機に入る前に SyncOperation の中でブロックしているため、Stop コマンドはチャネルに積まれたまま処理されない。`handle.join()` が返らない。

### Decoder 側

同じ問題が Decoder にも存在する。

- `src/decode.rs` の `sync_and_callback`: `MFX_INFINITE` で SyncOperation
- `src/decode.rs` の `sync_and_drain`: `MFX_INFINITE` で SyncOperation

`Decoder` の `stop_worker` / `Drop` も同じ構造（`send(Stop)` → `join()`）でデッドロックする。

### 二重エラー通知（既存の潜在バグの顕在化）

`sync_and_collect` が `Err` を返すと、呼び出し元の `sync_and_build_frame` が `?` で early return し、`PendingFrameStore::take_by_frame_seq` が呼ばれず pending frame が残留する。その後 `Stop` ハンドラの `drain_all` で同一フレームに再度エラー通知される。この問題は stopping 導入以前から存在する潜在バグ（`sync_and_collect` が任意のエラーを返した場合に発生し得る）だが、stopping 検知でエラー経路が通りやすくなるため、本 issue で確実に防ぐ。

## 設計方針

**有限タイムアウト + 停止フラグによる中断を導入する。**

### 基本設計

1. **`Encoder` / `Decoder` に `stopping: Arc<AtomicBool>` を追加する**
   - コンストラクタで `Arc::new(AtomicBool::new(false))` を生成し、Worker スレッドにクローンを渡す。
   - `stop_worker()` 内で `stopping.store(true, Ordering::Release)` を設定した後に `Stop` を送信する。
   - `stopping` フラグは Worker が SyncOperation から早期脱出するための非同期シグナルであり、`Stop` コマンドは Worker ループの停止と pending frame のクリーンアップを担当する。両者は責務が異なるため二重機構は正当化される。
   - `stopping` のクローンは `run_sync_worker` の引数に追加し、`sync_and_collect` / `sync_and_callback` / `sync_and_drain` / `sync_and_build_frame` へ引き渡す（各関数のシグネチャ変更を伴う）。

2. **`sync_and_collect` / `sync_and_callback` / `sync_and_drain` の `MFX_INFINITE` を有限タイムアウトに変更する**
   - タイムアウト値は **500 ms** とする（100 ms では異常時にポーリング間隔が短すぎて再呼び出し頻度が増え、1000 ms では shutdown 遅延が長い。500 ms はバランス値）。
   - 生の status 値をチェックするループ構造に変更する。**ループ冒頭で stopping を事前チェックし、立っていたら即座に中断する**（キュー内に残っている Sync コマンドを 1 件ずつ 500 ms ブロックして処理し続けることを防ぐ）:
     ```rust
     loop {
         if stopping.load(Ordering::Acquire) {
             return Err(Error::from_mfx(
                 sys::mfxStatus_MFX_ERR_ABORTED,
                 "MFXVideoCORE_SyncOperation",
             ));
         }
         let status = lib.mfx_video_core_sync_operation(..., 500);
         if status == sys::mfxStatus_MFX_ERR_NONE {
             break; // 成功
         }
         if status == sys::mfxStatus_MFX_WRN_IN_EXECUTION {
             continue; // 再試行
         }
         return Err(Error::from_mfx(status, "MFXVideoCORE_SyncOperation"));
     }
     ```
   - `check_mfx` は `MFX_WRN_IN_EXECUTION`（正の警告値）をエラー扱いするため使用しない。
   - 停止設定後に完了間際の SyncOperation が `MFX_ERR_NONE` を返した場合は成功として break する（Drop 開始後に正常コールバック `on_encoded(Ok(...))` / `on_decoded(Ok(...))` が発火し得る。これは完了条件 4 と矛盾しない仕様であり、実装者・レビュアーはこの挙動を前提とする）。

3. **Encoder 側の pending frame 二重エラー通知を防止する**
   - `SyncData` に `frame_seq: u64` を追加する（`encode()` 時点で frame_seq は既知であり、stopping 検知時は SyncOperation 未完了のため `bitstream.TimeStamp` が信頼できない。このため SyncData に保持する）。
   - `sync_and_collect` が `Err` を返した場合、`sync_and_build_frame` は **`Err` の内容（`MFX_ERR_ABORTED` かどうか）で stopping 検知による中断かどうかを判別**し（stopping フラグの状態では区別できない。SyncOperation が `MFX_ERR_NONE` を返した後の停止時以外のエラー直後に stopping が立った場合もフラグは立つため）、stopping 中断（`MFX_ERR_ABORTED`）時のみ `SyncData.frame_seq` を使って `PendingFrameStore::take_by_frame_seq` を呼び、pending を消費してから `Err` を返す（`frame_seq` は u64 で Copy のため、`sync_and_collect` に `sync_data` を move する前に先取りしておく）。これにより `Stop` ハンドラの `drain_all` での二重通知を防ぐ。判別方法は依存 issue 0008 の Decoder 側（`MFX_ERR_ABORTED` で判別）と揃える。
   - ただし `finish()` のドレインループで生成される `SyncData` には `frame_seq` を設定できない（送信時点で対応する `frame_seq` が決定不能。SyncOperation 完了前は `bitstream.TimeStamp` が読めないため）。このため `take_by_frame_seq` が `None` を返した場合は pending を消費せず、残留エントリは `Stop` アームの一括 `MFX_ERR_ABORTED` 通知に委ねる（この場合、Sync アームでの `Err` 通知は行わない。停止中の中断は Stop アームの一括通知に一本化する。**Sync アームでの `Err` 通知の抑制は `run_sync_worker` の Sync アームで `MFX_ERR_ABORTED` の中断を判別して通知しない分岐を追加する**ことで実現する）。
   - 停止時以外のエラー（bitstream 範囲検証エラー等）では pending を消費しない。この場合は `on_encoded(Err)` 通知後に残留し、Drop 時の `Stop` ハンドラで再度通知される既存の挙動のままとする（本 issue のスコープは stopping 中断時の二重通知防止のみ）。

4. **Decoder 側の停止時挙動**
   - `sync_and_drain` は戻り値が `()` だが、内部で SyncOperation を有限タイムアウトでループし、stopping 検知時は `return;` で早期中断する。
   - `sync_and_callback` が stopping 検知で `Err` を返した場合、Decoder 側は Encoder 側と異なり「どの pending エントリを消費すべきか特定できない」（SyncOperation 完了前に timestamp が確定しないため）ので、**handler へのエラー通知とエントリ消費を行わず**、残留エントリは `Stop` アームの一括 `MFX_ERR_ABORTED` 通知に委ねる（依存 issue 0008 が規定する方式と整合させる）。
   - 注意: 現行の Decoder の `run_sync_worker` は `pending_values.pop_front()` が Sync アームの冒頭で先行実行されるため、「エントリ消費を行わず」は現行の `VecDeque` 構造では実現できず、**0008（HashMap 方式への変更）適用後に実現される挙動**である。0010 単独適用時は `pop_front` 先行の構造のままで、stopping 中断時も pop 済みのため二重通知は発生しない（この場合、`sync_and_callback` の stopping 中断 Err は `on_decoded(Err)` で通知される）。
   - 未完了の `syncp` / `frame_surface` は Drop により解放される。SyncOperation 未完了のサーフェス解放が VPL 仕様上問題ないかは実機確認を要する（`sync_and_drain` の早期中断時と `sync_and_callback` の stopping 中断時の両方に同じ懸念がある）。

5. **Drop の保証**
   - `stop_worker` が `stopping` フラグを立ててから `Stop` を送信することで、Worker は「実行中の SyncOperation の残り最大 500 ms + キュー内コマンドの即時中断（ループ冒頭の事前チェックにより 500 ms を消費しない）」で抜ける。
   - Worker が抜けた後、`handle.join()` で回収し、残りの pending frame は通常の Stop 処理で `MFX_ERR_ABORTED` として通知される。

### スコープ外の明記

- **`finish()` のブロック**: `finish()` は `WaitIdle` 応答を待つが、GPU ハング中は先行する Sync がタイムアウトで再試行を繰り返し、`finish()` が永久ブロックし得る。本 issue は Drop 経路（`stop_worker` → `join`）のデッドロック修正が目的であり、`finish()` のブロックはスコープ外とする（別 issue で対応する）。
- 修正の有効範囲は「SyncOperation が wait ミリ秒で必ず制御を返す」という VPL の契約に依存する。ドライバレベルでハングして SyncOperation 自体が返らない場合、有限タイムアウトでもデッドロックは解消しない。

## 完了条件

以下すべてを満たす。

1. Encoder / Decoder の `sync_and_*` 3 関数（`sync_and_collect`, `sync_and_callback`, `sync_and_drain`）が `MFX_INFINITE` の代わりに有限タイムアウト（500 ms）で SyncOperation を呼び、ループ冒頭の stopping 事前チェックによる中断が可能なループ構造に修正する。
2. `SyncData` に `frame_seq: u64` を追加し、`sync_and_build_frame` で stopping 検知時は `PendingFrameStore::take_by_frame_seq` を呼んで pending frame を消費してからエラーを返し、二重通知を防止する。
3. Encoder / Decoder に `stopping: Arc<AtomicBool>` を追加し、`stop_worker()` でフラグを立ててから `Stop` を送信する。
4. Stop 経由で抜けた場合、未消費の pending frame は `MFX_ERR_ABORTED` としてハンドラに 1 回のみ通知する。Decoder 側は stopping 中断時に `sync_and_callback` のエラー通知とエントリ消費を行わず、残留エントリは Stop アームの一括通知に委ねる（設計方針 4 参照）。**この「通知と消費を行わない」挙動は 0008 適用後の最終状態で成立するものであり、0010 単独適用時（0008 未適用）は `pop_front` 先行構造のままのため、stopping 中断の `Err` は `on_decoded(Err)` で通知される**（設計方針 4 参照）。
5. コードレビューで `handle.join()` が最大 500 ms 以内に返る構造であることを確認する（「Drop 経路」全体ではなく `join()` に限定する。`mfx_video_encode_close` / `mfx_video_decode_close` / `Session` の Drop は本設計のスコープ外）。タイムアウトと stopping フラグの相互作用は設計の正しさで担保し、実 GPU 依存の単体テストは不要（`AGENTS.md` のモック禁止に従う）。
6. SyncOperation 未完了のサーフェス解放（`sync_and_drain` の早期中断時と `sync_and_callback` の stopping 中断時）が VPL 仕様上問題ないことを実機で確認する（確認できなかった場合は issue 化して対応する）。
7. `CHANGES.md` の `## develop` に `[FIX]` として追記する。

## 影響範囲

- `src/encode.rs`: `Encoder` 構造体 (`stopping` 追加)、`Encoder::new` (`Arc<AtomicBool>` 生成と worker への引き渡し)、`stop_worker` (`stopping` フラグ設定)、`run_sync_worker` (引数に `stopping` 追加、Sync アームの stopping 中断 (`MFX_ERR_ABORTED`) 時の `Err` 通知抑制分岐)、`SyncData` (`frame_seq` 追加)、`sync_and_collect` (タイムアウト + 生 status 分岐 + stopping チェック + シグネチャ変更)、`sync_and_build_frame` (stopping 中断時の pending 消費と早期 return + シグネチャ変更)、`encode()` (`SyncData` への `frame_seq` 設定)、`finish()` (`SyncData` 生成箇所。ドレイン由来のため `frame_seq` は設定不能。設計方針 3 参照)
- `src/decode.rs`: `Decoder` 構造体 (`stopping` 追加)、`Decoder::new` (`Arc<AtomicBool>` 生成と worker への引き渡し)、`stop_worker` (`stopping` フラグ設定)、`run_sync_worker` (引数に `stopping` 追加)、`sync_and_callback` (タイムアウト + 生 status 分岐 + stopping チェック + シグネチャ変更)、`sync_and_drain` (タイムアウト + stopping 検知時早期 return + シグネチャ変更)
- `CHANGES.md`

## 依存 issue

- **issue 0008** (`0008-bug-decoder-b-frame-user-data-mismatch`): 0008 は本 issue を依存先として「0010 を先に適用し、その差分の上に 0008 の変更を重ねること」と明記している。0008 は Decoder 側の user_data 対応付けを HashMap + TimeStamp 方式に変更するため、`sync_and_callback` のシグネチャ変更と、stopping 中断時の Decoder 側挙動（エントリ消費せず Stop アームに委ねる）が 0008 の設計前提となる。
- **issue 0009** (`0009-bug-decoder-device-busy-infinite-retry`): 0009 は本 issue を依存先として「0010 を先に適用すること」と明記している。0009 の完了条件（`finish` エラー後の Drop で `stop_worker` が join できること）は本 issue 適用後を前提とする。
- **issue 0014** (`0014-bug-frame-surface-drop-silently-swallows-errors`): `sync_and_drain` 内の `FrameSurface::Drop` のエラー処理を変更する。本 issue の後に適用する。
