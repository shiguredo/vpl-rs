# Encoder が frame_seq=0 を TimeStamp として使うため一部 VPL ドライバで対応付けが破綻する可能性がある

- Priority: High
- Created: 2026-07-01
- Model: Opus 4.7
- Branch: feature/fix-encoder-frame-seq-zero-timestamp-collision
- Polished: 2026-07-01

## 目的

`Encoder::encode` の 1 フレーム目は `frame_seq = 0` を `mfxFrameSurface1.Data.TimeStamp` として渡す。VPL の一部ドライバ実装では TimeStamp = 0 を「未設定」として扱い、`bitstream.TimeStamp` に `MFX_TIMESTAMP_UNKNOWN` (0xFFFF_FFFF_FFFF_FFFF) や別の値を書き戻す挙動が観測されている。この場合 `pending_store.take_by_frame_seq(bitstream.TimeStamp)` が空振りし、1 フレーム目が「no pending frame for bitstream timestamp」で必ずエラーになる。

現状のテストで顕在化していないが、別世代/別実装の GPU で当たる可能性がある。修正コストが `frame_count` 初期値の 1 行変更のみであるため、予防的に対処する。

## 優先度根拠

High。以下による。

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

**frame_count を 1 スタートにする。**

- `Encoder::new` で `frame_count: 1` に初期化する（現在は 0、`src/encode.rs:955`）。
- `pending_store` の登録キーと `Data.TimeStamp` の両方が 1 以上になるため、VPL ドライバが TimeStamp = 0 を「未設定」扱いしても衝突しない。
- frame_seq は内部の連番で公開 API に直接露出しないが、`EncodedFrame::timestamp()` の値は `frame_seq * framerate_den` で計算されるため影響を受ける。修正後は最初のフレームの `timestamp()` が 0 ではなく `framerate_den` になる。この変更は破壊的ではないが、timestamp の開始値に依存するコードは修正が必要。
- 修正は初期化 1 行の変更のみで、`frame_count.checked_add(1)` のロジックはそのまま利用できる。

`TimeStamp` と `frame_seq` を分離する案や、`Data.TimeStamp = frame_seq + 1` とする案は、実装変更が増えるわりに本質的な利点がないため不採用。

## 完了条件

以下すべてを満たす。

1. `Encoder::new` で `frame_count: 1` に初期化する（`src/encode.rs:955`、現在は `frame_count: 0`）。
2. 既存の全ラウンドトリップテストが pass することを確認する。`EncodedFrame::timestamp()` の値が 1 フレーム目から `framerate_den` になることをテストで確認する必要はないが、変更を認識した上で pass すること。
3. `CHANGES.md` の `## develop` に `[FIX]` として追記する。`timestamp()` の開始値が 0 から `framerate_den` に変わることに言及する。

## 影響範囲

- `src/encode.rs`（`Encoder::new` の `frame_count` 初期化、L955 の 1 行）
- `CHANGES.md`

## 参考

- `src/encode.rs:955` の `frame_count: 0`（修正対象）
- `mfxdefs.h` に `MFX_TIMESTAMP_UNKNOWN` (0xFFFF_FFFF_FFFF_FFFF) が定義されている
