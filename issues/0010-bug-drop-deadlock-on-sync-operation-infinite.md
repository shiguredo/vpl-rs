# Encoder / Decoder の Drop が SyncOperation の `MFX_INFINITE` でデッドロックする

- Priority: High
- Created: 2026-07-01
- Model: Opus 4.7
- Branch: feature/fix-drop-deadlock-on-sync-operation-infinite
- Polished: 2026-07-01

## 目的

Worker スレッドが `MFXVideoCORE_SyncOperation(session, syncp, MFX_INFINITE)` の中で無限待機している最中に `Encoder` / `Decoder` を Drop すると、Worker が Stop コマンドを受信できずデッドロックするバグを修正する。GPU ハング時や long-running な SyncOperation 中にプロセスをシャットダウンできない。

## 優先度根拠

High。以下による。

- **プロセスシャットダウンが阻害される**: `Encoder::drop` / `Decoder::drop` は `stop_worker()` → `handle.join()` を呼ぶが、Worker が SyncOperation で無限待機していると `Stop` を受信できず、`join()` が永久にブロックする。上位プロセスの終了処理が回らない。
- **GPU ハング時に他のリソース解放も止まる**: Drop 経路が返らないので、その後に続くリソース解放（`Session` の Drop → `MFXClose` + `MFXUnload`）にも到達できない。
- **既存の `DEVICE_BUSY` リトライ上限（30 回）を導入した意図が骨抜きになる**: 上限リトライで途中エラーを返せるようにしても、Sync のブロックで止まるなら意味がない。
- **本問題は隠れているだけで実装上の設計欠陥**: 通常運用では SyncOperation は数十 ms 以内に返るため顕在化しないが、GPU ドライバのタイムアウトや GPU 実装のバグで長時間ブロックすると容易に再現する。

## 現状

### Encoder 側

`src/encode.rs:1381-1385`（`sync_and_collect`）:

```rust
let status = lib.mfx_video_core_sync_operation(
    session_handle as sys::mfxSession,
    syncp,
    sys::MFX_INFINITE,
);
Error::check_mfx(status, "MFXVideoCORE_SyncOperation")?;
```

Worker はこのブロッキング呼び出しの中で無限待機する。

Drop 経路（`src/encode.rs:1253-1272`）:

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

- `src/decode.rs:574-579`（`sync_and_callback`）: `MFX_INFINITE` で SyncOperation
- `src/decode.rs:603-607`（`sync_and_drain`）: `MFX_INFINITE` で SyncOperation

`Decoder::drop` (`src/decode.rs:484-495`) も同じ構造で Stop → join でデッドロックする。

## 設計方針

**有限タイムアウト + 停止フラグによる中断を導入する。**

### 基本設計

1. **`Encoder` / `Decoder` に `stopping: Arc<AtomicBool>` を追加する**
   - コンストラクタで `Arc::new(AtomicBool::new(false))` を生成し、Worker スレッドにクローンを渡す。
   - `stop_worker()` 内で `stopping.store(true, Ordering::Release)` を設定した後に `Stop` を送信する。
   - `stopping` フラグは Worker が SyncOperation から早期脱出するための非同期シグナルであり、`Stop` コマンドは Worker ループの停止と pending frame のクリーンアップを担当する。両者は責務が異なるため二重機構は正当化される。

2. **`sync_and_collect` / `sync_and_callback` / `sync_and_drain` の `MFX_INFINITE` を有限タイムアウトに変更する**
   - タイムアウト値は **500 ms** とする（100 ms では通常運用の SyncOperation 完了前に再ループのオーバーヘッドが大きく頻繁な再呼び出しで VPL 側の負荷が増す。1000 ms では shutdown 遅延が長い。500 ms はバランス値）。
   - 生の status 値をチェックするループ構造に変更する:
     ```rust
     loop {
         let status = lib.mfx_video_core_sync_operation(..., 500);
         if status == sys::mfxStatus_MFX_ERR_NONE {
             break; // 成功
         }
         if status == sys::mfxStatus_MFX_WRN_IN_EXECUTION {
             if stopping.load(Ordering::Acquire) {
                 return Err(Error::from_mfx(
                     sys::mfxStatus_MFX_ERR_ABORTED,
                     "MFXVideoCORE_SyncOperation",
                 ));
             }
             continue; // 再試行
         }
         return Err(Error::from_mfx(status, "MFXVideoCORE_SyncOperation"));
     }
     ```
   - `check_mfx` は `MFX_WRN_IN_EXECUTION`（正の警告値）をエラー扱いするため使用しない。

3. **Encoder 側の pending frame 二重エラー通知を防止する**
   - `sync_and_collect` が stopping 検知で `Err` を返すと、呼び出し元の `sync_and_build_frame` で `?` により early return し、`take_by_frame_seq` が呼ばれず pending frame が `pending_store` に残留する。
   - その後 `Stop` ハンドラの `pending_store.drain_all()` で同一フレームに再度 `canceled_error()` が通知され、**1 フレームに 2 回のエラーコールバックが発火する**。
   - 対策: `sync_and_build_frame` で stopping 検知時は `pending_store.take_by_frame_seq` を呼んで pending を消費してから `Err` を返す。または `sync_and_collect` の停止時は `pending_store` を操作しない別経路で通知する。実装時に選択する。

4. **`sync_and_drain` の stopping 対応**
   - `sync_and_drain` は戻り値が `()` だが、内部で SyncOperation を有限タイムアウトでループし、stopping 検知時は `return;` で早期中断する。未完了の `syncp` / `frame_surface` は Drop により解放される。SyncOperation 未完了のサーフェス解放が VPL 仕様上問題ないかは実機確認を要する。

5. **Drop の保証**
   - `stop_worker` が `stopping` フラグを立ててから `Stop` を送信することで、SyncOperation 中の Worker は最大 500 ms 以内に抜ける。
   - Worker が抜けた後、`handle.join()` で回収し、残りの pending フレームは通常の Stop 処理で `MFX_ERR_ABORTED` として通知される。

## 完了条件

以下すべてを満たす。

1. Encoder / Decoder の `sync_and_*` 3 関数（`sync_and_collect`, `sync_and_callback`, `sync_and_drain`）が `MFX_INFINITE` の代わりに有限タイムアウト（500 ms）で SyncOperation を呼び、stopping フラグによる中断が可能なループ構造に修正する。
2. `sync_and_build_frame` で stopping 検知時は pending frame を消費してからエラーを返し、二重通知を防止する。
3. Encoder / Decoder に `stopping: Arc<AtomicBool>` を追加し、`stop_worker()` でフラグを立ててから `Stop` を送信する。
4. Stop 経由で抜けた場合、未消費の pending frame は `MFX_ERR_ABORTED` としてハンドラに 1 回のみ通知する。
5. コードレビューで Drop 経路が最大 500 ms 以内に返る構造であることを確認する。タイムアウトと stopping フラグの相互作用は設計の正しさで担保し、実 GPU 依存の単体テストは不要（`AGENTS.md` のモック禁止に従う）。
6. `CHANGES.md` の `## develop` に `[FIX]` として追記する。

## 影響範囲

- `src/encode.rs`: `Encoder` 構造体 (`stopping` 追加)、`Encoder::new` (`Arc<AtomicBool>` 生成)、`stop_worker` (`stopping` フラグ設定)、`sync_and_collect` (タイムアウト + 生 status 分岐 + stopping チェック)、`sync_and_build_frame` (stopping 検知時の pending 消費と早期 return)
- `src/decode.rs`: `Decoder` 構造体 (`stopping` 追加)、`Decoder::new` (`Arc<AtomicBool>` 生成)、`stop_worker` (`stopping` フラグ設定)、`sync_and_callback` (タイムアウト + 生 status 分岐 + stopping チェック)、`sync_and_drain` (タイムアウト + stopping 検知時早期 return)
- `CHANGES.md`
