# Encoder が frame_seq=0 を TimeStamp として使うため一部 VPL ドライバで対応付けが破綻する可能性がある

- Priority: High
- Created: 2026-07-01
- Completed: {YYYY-MM-DD}
- Model: Opus 4.7
- Branch: feature/fix-encoder-frame-seq-zero-timestamp-collision
- Polished: {YYYY-MM-DD}

## 目的

`Encoder::encode` の 1 フレーム目は `frame_seq = 0` を `mfxFrameSurface1.Data.TimeStamp` として渡す。VPL の一部ドライバ実装では TimeStamp = 0 を「未設定」として扱い、`bitstream.TimeStamp` に `MFX_TIMESTAMP_UNKNOWN` (0xFFFF_FFFF_FFFF_FFFF) や別の値を書き戻す挙動が観測されている。この場合 `pending_store.take_by_frame_seq(bitstream.TimeStamp)` が空振りし、1 フレーム目が「no pending frame for bitstream timestamp」で必ずエラーになる。

現状のテストで顕在化していないため、本 issue はまず **現実に発生するかの検証** を先に行い、発生するなら修正、しないならドキュメント化する。

## 優先度根拠

High（要検証）。以下による。

- **1 フレーム目が必ず失敗する可能性**: もし該当ドライバに当たると、Encoder を起動して最初の入力が **確実に失敗する**。回避不能で、原因は表面的にはコード変更なしでの環境依存となる。
- **対応方針が単純**: `frame_count` を 1 スタートにするだけで回避可能。修正コスト極小。
- **CI 検証がない**: 現在の実 GPU CI（`test-intel-vpl`）は特定の 1 台のハードウェアで動くので、他ドライバでの再現性は未検証。

## 現状

### frame_seq = 0 の Data.TimeStamp

`src/encode.rs:1088, 1110`:

```rust
let frame_seq = self.frame_count; // frame_count は new() で 0 初期化
// ...
unsafe {
    (*frame_surface.as_ptr()).Data.TimeStamp = frame_seq;
    // ...
}
```

`Encoder::new` は `frame_count: 0` で初期化するため、最初の `encode()` 呼び出しでは `frame_seq = 0` が `Data.TimeStamp` に載る。

### take_by_frame_seq での引き当て

`src/encode.rs:1341-1357` の `sync_and_build_frame`:

```rust
fn sync_and_build_frame<T>(
    lib: VplLibrary,
    session_handle: usize,
    sync_data: SyncData,
    pending_store: &mut PendingFrameStore<T>,
) -> Result<EncodedFrame<T>, Error> {
    let synced = sync_and_collect(lib, session_handle, sync_data)?;
    let pending = pending_store
        .take_by_frame_seq(synced.frame_seq)
        .ok_or_else(|| mismatched_timestamp_error(synced.frame_seq, pending_store.len()))?;
    // ...
}
```

`synced.frame_seq = bitstream.TimeStamp`（`src/encode.rs:1396, 1424`）で、pending_store の登録キーと完全一致で引き当てる。もし VPL 側が TimeStamp を書き換えたら空振りする。

### VPL 側の TimeStamp 扱いの参考

- `mfxdefs.h` に `MFX_TIMESTAMP_UNKNOWN` (0xFFFF_FFFF_FFFF_FFFF) が定義されている
- libvpl の一部リファレンス実装では、`Data.TimeStamp == 0` を「未設定」と解釈して bitstream 側に `MFX_TIMESTAMP_UNKNOWN` を出力する挙動が報告されている
- ただし全ドライバがこの挙動をするかは Intel の公式ドキュメントでは明確でない

### テスト状況

- `tests/test_roundtrip.rs` の全ラウンドトリップテストは 30fps × 8〜30 フレームで、frame_seq = 0 を含むが、テスト環境の GPU（CI の Arc / Xe） では 0 を有効な値として扱っているようで問題が顕在化していない。
- 別世代 / 別実装の GPU で当たる可能性がある。

## 設計方針

### 案 A: frame_count を 1 スタートにする（推奨）

`Encoder::new` で `frame_count: 1` に初期化し、pending_store の登録キーと `Data.TimeStamp` の両方が 1 以上になるようにする。

- 長所: 実装変更 1 行。既存の frame_seq 意味論（単調増加）を壊さない。
- 短所: 特になし（frame_seq を「入力連番」として使う内部の他コードも 1 スタートに切り替わるが、公開 API には露出しない）。

### 案 B: TimeStamp と frame_seq を分離する

`Data.TimeStamp` には別の値（例: `frame_count + 1` や `presentation_timestamp`）を使い、pending_store のキーには内部で管理する frame_seq を使う。VPL が TimeStamp を書き戻す仕様に依存しない実装にする。

- 長所: VPL の TimeStamp 挙動の変化に強い。
- 短所: 実装が複雑になる。frame_seq と TimeStamp の対応表を持つ必要がある。

### 案 C: 現状維持 + doc に注意書き

「一部 VPL ドライバで frame_seq = 0 が誤動作する可能性」を doc に記載し、対応は保留。

- 短所: 実装者がドライバ依存で当たる。

推奨は **案 A**。案 B は将来の設計改善で余裕があれば検討。

## 完了条件

以下すべてを満たす。

1. **検証を先行する**: 手元で入手可能な複数世代の Intel GPU（例: iGPU + dGPU）で `frame_seq = 0` を `Data.TimeStamp` に載せて encode し、`bitstream.TimeStamp` に 0 がそのまま返るかを確認する。少なくとも 2 種類以上の GPU で検証する。
2. 1 で発生することが確認できたら案 A を採用して `Encoder::new` で `frame_count: 1` に初期化する。発生しなくても、他ドライバでの潜在的リスクを避けるため案 A で対処する（実装コストが極小なので）。
3. `Encoder` の frame_seq が 1 スタートになったことを確認する単体テスト、または pending_frame の内部状態を検証するテストを追加する。
4. `CHANGES.md` の `## develop` に `[FIX]` として追記する（発生する場合）または `[CHANGE]` として追記する（予防的修正の場合）。

## 影響範囲

- `src/encode.rs`（`Encoder::new` の `frame_count` 初期化のみ）
- `CHANGES.md`
- 検証時のみ実 GPU テスト環境

## 参考

- `/review-code` の致命的指摘 F6
- 関連コード: `src/encode.rs:669, 954, 1088, 1110, 1157-1160, 1341-1357, 1424`
