# Encoder / Decoder の Drop が SyncOperation の MFX_INFINITE でデッドロックする

- Priority: High
- Created: 2026-07-01
- Completed: {YYYY-MM-DD}
- Model: Opus 4.7
- Branch: feature/fix-drop-deadlock-on-sync-operation-infinite
- Polished: {YYYY-MM-DD}

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

impl<H: EncodeHandler> Drop for Encoder<H> {
    fn drop(&mut self) {
        self.stop_worker();
        let _ = self.session.lib().mfx_video_encode_close(self.session.as_ptr());
    }
}
```

`worker_tx.send(Stop)` は成功するが、Worker は `worker_rx.recv()` の待機に入る前に SyncOperation の中でブロックしているため、Stop コマンドはチャネルに積まれたまま処理されない。`handle.join()` が返らない。

### Decoder 側

同じ問題が Decoder にも存在する。

- `src/decode.rs:574-579`（`sync_and_callback`）: `MFX_INFINITE` で SyncOperation
- `src/decode.rs:603-607`（`sync_and_drain`）: `MFX_INFINITE` で SyncOperation

`Decoder::drop` (`src/decode.rs:484-495`) も同じ構造で Stop → join でデッドロックする。

### 派生: 0009 の DEVICE_BUSY リトライ問題との相互作用

Issue 0009 で Decoder に DEVICE_BUSY 上限リトライを導入しても、`decode_bitstream` は `DecodeFrameAsync` で BUSY を検出して sleep + retry するのに対し、本問題は Worker が SyncOperation でブロックする経路。両者は独立で、両方の対応が必要。

## 設計方針

### 案 A: SyncOperation にタイムアウトを掛ける + Stop チェック

`MFX_INFINITE` の代わりに数百 ms 〜数秒（例: `1000` ミリ秒）のタイムアウトを渡し、`MFX_WRN_IN_EXECUTION` を受けたら Worker 側で以下を行う。

1. `worker_rx.try_recv()` で `Stop` が来ていないか確認する
2. `Stop` が来ていれば SyncOperation を諦めて Worker を終了する（残りの pending はキャンセル通知する）
3. `Stop` が来ていなければ再度 SyncOperation を試みる

- 長所: Encoder / Decoder 双方に同じパターンを適用できる。Rust 側だけで完結し VPL の追加 API に依存しない。
- 短所: タイムアウト値を選ぶ必要がある（1000 ms なら最大 1 秒の shutdown 遅延）。

### 案 B: `Drop` で先に `MFXVideoENCODE_Close` / `MFXVideoDECODE_Close` を呼んで Sync を中断させる

`stop_worker()` の前に `mfx_video_encode_close(session)` を呼ぶ VPL 実装依存の対応。ドライバによっては進行中の SyncOperation を中断できる（要検証）。

- 長所: shutdown が最速。
- 短所: VPL 仕様上「Sync 中に Close するな」と読める記述もあり、実装依存の挙動を頼るのはリスクが高い。

推奨は **案 A**。案 A のタイムアウトを `100 ms` のような小さい値にすれば shutdown 遅延は許容範囲。

### タイムアウト値の目安

- 100 ms: shutdown 応答性は良いが、通常運用でも 100 ms 毎に SyncOperation を再開する CPU オーバーヘッド。
- 1000 ms: shutdown で最大 1 秒待つ。通常運用のオーバーヘッドは小さい。
- 実用的には 100 - 500 ms の範囲で選ぶ。

## 完了条件

以下すべてを満たす。

1. Encoder / Decoder 双方で SyncOperation が `MFX_INFINITE` を使わず、有限タイムアウトを渡すよう修正する。
2. `MFX_WRN_IN_EXECUTION` を受けたときに Worker が Stop コマンドを検知して抜けられる構造を実装する。
3. Stop 経由で抜けた場合、pending frame は `MFX_ERR_ABORTED` 相当のエラーとしてハンドラに通知する（既存のキャンセル通知経路を流用）。
4. `Encoder::drop` / `Decoder::drop` がタイムアウト内（例: 数秒以内）に返ることを確認する単体テストを追加する。実 GPU なしで検証できるよう、`MFXVideoCORE_SyncOperation` を経由しない Worker のみのテストを整備する（Encoder 側の既存 `worker_stop_returns_aborted_for_all_pending` を拡張、Decoder 側は 0008 と合わせて Worker 単体テストを新設）。
5. `CHANGES.md` の `## develop` に `[FIX]` として追記する。

## 影響範囲

- `src/encode.rs`（`sync_and_collect` の SyncOperation 呼び出し、Worker ループの Stop 検知）
- `src/decode.rs`（`sync_and_callback` / `sync_and_drain` の SyncOperation 呼び出し、Worker ループの Stop 検知）
- `src/vpl.rs`（`mfx_video_core_sync_operation` のラッパーが返すステータス値の扱いを Worker 側で分岐するために必要なら見直し）
- `CHANGES.md`

## 参考

- `/review-code` の致命的指摘 F5
- 関連 issue: 0008（Decoder の user_data 対応）、0009（DEVICE_BUSY 上限リトライ）
- VPL 仕様: `MFXVideoCORE_SyncOperation` の `wait` パラメータと `MFX_WRN_IN_EXECUTION` の返却条件
