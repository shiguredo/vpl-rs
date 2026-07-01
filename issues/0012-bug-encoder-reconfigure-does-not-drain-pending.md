# Encoder::reconfigure が pending frame を drain しないため frame_seq 対応が破綻する

- Priority: High
- Created: 2026-07-01
- Completed: {YYYY-MM-DD}
- Model: Opus 4.7
- Branch: feature/fix-encoder-reconfigure-does-not-drain-pending
- Polished: {YYYY-MM-DD}

## 目的

`Encoder::reconfigure` が呼ぶ `MFXVideoENCODE_Reset` は VPL 内部の未出力フレームを破棄する仕様。しかし現在の実装は `pending_store` を drain せず、Reset 前後で `frame_seq` の対応付けが破綻する。ユーザーは意図せず「以前入力したフレームの user_data」が「Reset 後に入力したフレームの出力」に紐付く、あるいは `finish()` で「pending frames remained」エラーとして未処理フレームが顕在化する。

## 優先度根拠

High。以下による。

- **サイレントなデータ破損**: `Reset` が VPL 内部フレームを破棄しても pending_store 側の frame_seq は残るため、後続の Sync 出力と誤って対応付けが成立するリスクがある（frame_seq が衝突しない限り気付けない）。
- **`finish()` エラーの表出**: 対応する Sync が来ないまま pending_store に残り、`finish()` で drain されて「pending frames remained」の一括エラー通知になる。ユーザーには Reset との因果が分からない。
- **`Encoder::reconfigure` の doc に注意書きがない**: 現在の doc（`src/encode.rs:983-985`）は「エンコーダパラメータを動的に変更する」としか書いておらず、Reset の副作用（VPL 内部フレーム破棄）が説明されていない。

## 現状

### reconfigure の実装

`src/encode.rs:986-1022`:

```rust
pub fn reconfigure(&mut self, params: ReconfigureParams) -> Result<(), Error> {
    if let Some(0) = params.framerate_den {
        return Err(Error::new_custom(
            "Encoder::reconfigure",
            "framerate_den must be non-zero",
        ));
    }

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

`Encoder::encode` は `pending_store.insert(frame_seq, pending_frame)` で入力ごとに登録し、Sync 到着時に `take_by_frame_seq` で回収する（`src/encode.rs:1341-1357`）。B フレーム並び替えを含めて frame_seq の完全一致で対応付ける設計。

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

frame_seq は Encoder のライフタイムを通じて単調増加する（`self.frame_count.checked_add(1)`）ので、Reset 後に新規入力しても既存の frame_seq とは衝突しない。したがって「誤って対応付けが成立する」ケースは frame_seq がラップアラウンドしない限り発生しない（u64 なので現実的にありえない）。

ただし、Reset 前の未回収 pending が「消えることなく残る」設計自体が、`finish()` エラーとして顕在化する。エラーで気付けるのは救いだが、reconfigure の意味論としては破損している。

## 設計方針

以下のいずれか。

### 案 A: reconfigure() の中で finish() を呼ぶ（推奨）

`reconfigure()` の直前に既存 pending を全てフラッシュする。

```rust
pub fn reconfigure(&mut self, params: ReconfigureParams) -> Result<(), Error> {
    // pending をフラッシュしてから Reset する
    self.finish()?;
    // ... 既存の Reset 処理 ...
}
```

- 長所: reconfigure の意味論が明確になる（「既存フレームは全て処理してから設定変更」）。
- 短所: reconfigure がブロッキングになる（既存フレームの Sync 完了まで待つ）。ただしこれは reconfigure の正しい意味論。

### 案 B: pending_store が非空なら Err を返す

呼び出し側の責任で `finish()` を先に呼ばせる。pending が残っている状態で reconfigure を呼ぶとエラー。

- 長所: reconfigure のブロッキング時間が読める。
- 短所: 呼び出し側の負担が増える。「なぜエラーになったか」の理解が必要。

### 案 C: reconfigure() で pending_store を drain（明示エラー通知）

Reset を呼ぶ前に `pending_store` を drain し、各 user_data に対して `MFX_ERR_ABORTED` 相当のエラーを通知する。

- 長所: 待たずに reconfigure できる。
- 短所: 「reconfigure したら未処理フレームが失われる」挙動が非直感的。

推奨は **案 A**。ユーザーが reconfigure を呼ぶユースケース（ビットレート動的変更）では、既存フレームの完了を待つのが自然。

## 完了条件

以下すべてを満たす。

1. `Encoder::reconfigure` の実装が上記いずれかの案で修正される。案 A の場合、reconfigure の doc に「既存 pending が全て完了してから Reset を実行する」旨を明記する。
2. reconfigure 後に `encode()` した結果の user_data が、reconfigure 前の frame_seq と衝突せず正しく対応付くことを検証するテストを追加する（実 GPU 依存で `tests/test_roundtrip.rs`）。
3. `finish()` 経由の「pending frames remained」エラーが reconfigure 起因で発生しないことを確認する。
4. `CHANGES.md` の `## develop` に `[FIX]` として追記する。

## 影響範囲

- `src/encode.rs`（`Encoder::reconfigure`）
- `tests/test_roundtrip.rs`（reconfigure のラウンドトリップテスト。実 GPU 依存）
- `CHANGES.md`

## 参考

- `/review-code` の致命的指摘 F4
- 関連 issue: 0011（reconfigure が ExtParam を送らない別問題）
