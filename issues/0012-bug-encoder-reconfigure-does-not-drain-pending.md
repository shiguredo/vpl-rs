# Encoder::reconfigure が pending frame を drain しないため frame_seq 対応が破綻する

- Priority: High
- Created: 2026-07-01
- Model: Opus 4.7
- Branch: feature/fix-encoder-reconfigure-does-not-drain-pending
- Polished: 2026-07-01

## 目的

`Encoder::reconfigure` が呼ぶ `MFXVideoENCODE_Reset` は VPL 内部の未出力フレームを破棄する仕様。しかし現在の実装は `pending_store` を drain せず、Reset 後に pending が残留して `finish()` で「pending frames remained」エラーが顕在化する。ユーザーは reconfigure との因果関係が分からないままエラーに遭遇する。

## 優先度根拠

High。以下による。

- **`finish()` エラーの表出**: Reset で破棄されたフレームに対応する Sync が来ないまま pending_store に残り、`finish()` で drain されて「pending frames remained」の一括エラー通知になる。ユーザーには Reset との因果が分からない。
- **`Encoder::reconfigure` の doc に注意書きがない**: 現在の doc（`src/encode.rs:983-985`）は「エンコーダパラメータを動的に変更する」としか書いておらず、Reset の副作用（VPL 内部フレーム破棄）が説明されていない。
- **frame_seq の単調増加により誤対応付けは発生しないが、残留自体が異常**: frame_seq は u64 でラップアラウンドしないため Reset 後の Sync と誤って対応付くことはない。しかし pending_store に残るエントリ自体が設計上の欠陥であり、reconfigure の意味論として破綻している。

## 現状

### reconfigure の実装

`src/encode.rs:986-1022`:

```rust
pub fn reconfigure(&mut self, params: ReconfigureParams) -> Result<(), Error> {
    // ... バリデーション ...
    // 現在の video_param をベースに変更を適用する
    unsafe {
        // ... target_kbps / max_kbps / framerate を書き換え ...
    }
    self.session
        .lib()
        .mfx_video_encode_reset(self.session.as_ptr(), &mut self.video_param)
}
```

`Reset` を呼ぶだけで pending_store には一切触っていない。

### pending_store の状態

`Encoder::encode` は `pending_store.insert(frame_seq, pending_frame)` で入力ごとに登録し、Sync 到着時に `take_by_frame_seq` で回収する（`src/encode.rs:1341-1357`）。

Reset が発生すると、VPL は入力済みだが未出力のフレームを破棄する。しかし pending_store には frame_seq が残り、対応する Sync がもう来ないので、`finish()` の `pending_store.drain_all()` (`src/encode.rs:1312`) でまとめてエラー通知される。

### エラーメッセージの例

`src/encode.rs:1438-1445`:

```rust
fn finish_pending_error(frame_seq: u64, pending_count: usize) -> Error {
    Error::new_custom_owned(
        "Encoder::finish",
        format!(
            "pending frames remained after flush for frame sequence {frame_seq} (pending count: {pending_count})"
        ),
    )
}
```

ユーザーは「reconfigure が原因で pending が残った」と分からず、`finish()` の失敗として顕在化する。

### frame_seq 衝突のリスク

frame_seq は Encoder のライフタイムを通じて単調増加する（`self.frame_count.checked_add(1)`）ので、Reset 後に新規入力しても既存の frame_seq とは衝突しない。u64 のためラップアラウンドは現実的に発生しない。

## 設計方針

**Reset の直前に pending_store を drain する。**

制御フロー:

1. パラメータバリデーション（`framerate_den` チェック）
2. `self.video_param` に `ReconfigureParams` を適用する（0011 の ExtParam 再構築もここで行う）
3. `pending_store` を drain し、全エントリを `MFX_ERR_ABORTED` 相当のエラーとしてハンドラに通知する
4. `MFXVideoENCODE_Reset` を呼ぶ

```rust
pub fn reconfigure(&mut self, params: ReconfigureParams) -> Result<(), Error> {
    if let Some(0) = params.framerate_den {
        return Err(Error::new_custom(
            "Encoder::reconfigure",
            "framerate_den must be non-zero",
        ));
    }

    // 現在の video_param をベースに変更を適用する（0011 の ExtParam 再構築を含む）
    unsafe {
        // ... target_kbps / max_kbps / framerate を書き換え ...
    }

    // Reset により VPL 内部で破棄されるフレームに対応する pending を事前に drain する
    // ワーカー側の drain 完了を同期してから Reset を呼ぶ（逆順だと VPL 内部フレームが先に破棄される）
    let (tx, rx) = mpsc::channel();
    self.send_worker_command(
        "Encoder::reconfigure",
        WorkerCommand::DrainPending(tx),
    )?;
    rx.recv().map_err(|_| {
        Error::new_custom("Encoder::reconfigure", "sync worker thread stopped unexpectedly")
    })?;

    self.session
        .lib()
        .mfx_video_encode_reset(self.session.as_ptr(), &mut self.video_param)
}
```

### WorkerCommand::DrainPending の追加

`WorkerCommand` enum に `DrainPending(mpsc::Sender<()>)` バリアントを新設する。Worker 側では:

- `pending_store.drain_all()` で全エントリを取り出す
- 各エントリに対して `handler.on_encoded(Err(canceled_error()))` を呼び出す
- 全エントリ処理後に `reply_tx.send(())` で呼び出し元に完了を通知する

呼び出し元（`reconfigure`）は `rx.recv()` で完了を待ってから `Reset` を実行する。これにより pending_store の drain が Reset より先に完了することが保証される。

### 設計判断の根拠

**案 A（finish() で drain）を採用しない理由**:

- `finish()` は EOS 信号を VPL に送る。EOS 後の Reset → encode() 再開は VPL 仕様で保証されていない（未検証の VPL 挙動に依存する）
- `finish()` が Err を返した場合、EOS が部分的に送られた状態で Reset を呼べず、Encoder が回復不能になる
- `finish()` は全 pending の Sync 完了 + コールバック完了を待つため、reconfigure が長時間ブロックする（数百 ms〜数秒）。動的パラメータ変更として不相応な待機時間になる
- `finish()` の成功後に Reset が失敗した場合も Encoder が動作不能になる（EOS 済み、Reset 失敗）が、このケースへの対処がない

**本方式の特徴**:

- VPL に依存せず pending_store のみを操作するため、Reset 前後で Encoder の状態が破壊されない
- Reset 失敗時も pending は drain 済みだが Encoder 自体は再利用可能（Drop + 再作成は不要）
- pending はエラー通知されるため、データ消失が呼び出し側に伝わる
- フレームロスを伴うが、`MFXVideoENCODE_Reset` 自体が VPL 内部フレームを破棄する仕様である以上、フレームロスは不可避

### ブロッキング挙動

本方式では `finish()` を呼ばないため、reconfigure は Sync 完了を待たずに即座にリターンする。既存の挙動と一致する。

### reconfigure の doc 更新

以下の内容を reconfigure の doc に追記する:

- `MFXVideoENCODE_Reset` が VPL 内部の未出力フレームを破棄すること
- 破棄されたフレームに対応する user_data は `MFX_ERR_ABORTED` としてハンドラに通知されること
- frame_seq は reconfigure 後も継続しリセットされないこと

## 完了条件

以下すべてを満たす。

1. `WorkerCommand` enum に `DrainPending` バリアントを追加し、Worker 側で `pending_store.drain_all()` + 全エントリ `canceled_error()` 通知を行う。
2. `Encoder::reconfigure` が Reset 直前に `WorkerCommand::DrainPending` を送信するよう修正する。
3. reconfigure の doc に、Reset の副作用（VPL 内部フレーム破棄、対応する user_data のエラー通知、frame_seq 継続）を明記する。
4. reconfigure の前後で encode した全フレームの user_data が、reconfigure 前のフレームはエラー通知、reconfigure 後のフレームは正常通知されることを検証するテストを `tests/test_roundtrip.rs` に追加する。テストは encode → reconfigure → encode → finish のシーケンスで検証する。
5. `CHANGES.md` の `## develop` に `[FIX]` として追記する。

## 影響範囲

- `src/encode.rs`（`WorkerCommand` enum に `DrainPending` 追加、`run_sync_worker` の `DrainPending` アーム追加、`Encoder::reconfigure` に `DrainPending` 送信追加、reconfigure の doc 更新）
- `tests/test_roundtrip.rs`（reconfigure 前後の user_data 検証テスト追加、実 GPU 依存）
- `CHANGES.md`

## 参考

- 関連 issue: 0011（reconfigure が ExtParam を送らない別問題。reconfigure の修正順序に注意）
- `canceled_error()`: `src/encode.rs:1429-1436`
