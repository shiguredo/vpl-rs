# Decoder で B フレーム有りのビットストリームで user_data の対応が壊れる

- Priority: High
- Created: 2026-07-01
- Completed: {YYYY-MM-DD}
- Model: Opus 4.7
- Branch: feature/fix-decoder-b-frame-user-data-mismatch
- Polished: {YYYY-MM-DD}

## 目的

`Decoder::decode(bs, user_data)` に渡した `user_data` が、出力される `DecodedFrame::user_data()` で **正しい入力フレームに対応付けられない** バグを修正する。B フレームを含む H.264 / H.265 では出力順 (display order) と入力順 (decode order) が入れ替わるため、Decoder 側の FIFO 対応付けが破綻して user_data が全て誤ったフレームに紐付く。

## 優先度根拠

High。以下の理由による。

- **サイレントなデータ破損**: エラーは発生せず、user_data だけが誤って紐付く。呼び出し側は「何かがずれている」と気付きにくい。
- **公開 API の契約違反**: `skills/shiguredo-vpl/SKILL.md:305-306` は「`user_data` は FIFO キュー (`VecDeque`) で `decode()` 呼び出し順に対応付く。ビットストリームがフレーム境界をまたいでも順序は保たれる」と明記しているが、この「順序は保たれる」の意味を B フレームで解釈すると破綻する。
- **利用側への影響**: Sora など Decoder を利用するダウンストリームで、user_data に「対応する入力フレームの ID」を載せて追跡している全ユースケースが壊れる。
- **VP9 / AV1 でも表面化しうる**: B フレームがない構成でも、フレーム並び替え (frame reordering) を伴うコーデック設定で同じ問題が再現する可能性がある。

## 現状

### 対応付けの実装

`src/decode.rs:505-548` の `run_sync_worker` は以下の FIFO で user_data を出力に割り当てている。

```rust
let mut pending_values: VecDeque<H::UserData> = VecDeque::new();
while let Ok(command) = worker_rx.recv() {
    match command {
        WorkerCommand::QueueFrame(user_data) => {
            pending_values.push_back(user_data);
        }
        WorkerCommand::Sync { sync_data } => {
            if let Some(user_data) = pending_values.pop_front() {
                // ... user_data を出力フレームに紐付ける ...
            }
        }
        // ...
    }
}
```

`pending_values.push_back` / `pop_front` は「入力順 == 出力順」を前提とした対応付けで、B フレーム有りのストリームでは成立しない。

### VPL 側の出力順の仕様

`src/decode.rs:277-307` の `initialize` は `mfxInfoMFX.DecodedOrder` を設定していない。VPL 仕様上、`DecodedOrder = 0`（デフォルト）は AVC / HEVC で **display order で出力する** ことを意味する。したがって Decoder は自動的に並び替えられた順で `Sync` を返してくる。

### 実例

入力（decode order）: `I0, P1, B2`
Decoder が返す出力（display order）: `I0, B2, P1`

FIFO で user_data を割り当てると:

| Sync 順 | 実 frame | 割り当てられた user_data |
|---|---|---|
| 1 | I0 | 0 (I0 の入力時) → 正しい |
| 2 | B2 | 1 (P1 の入力時) → **誤り** |
| 3 | P1 | 2 (B2 の入力時) → **誤り** |

### Encoder 側との非対称

Encoder 側 (`src/encode.rs:1088-1160, 1341-1357`) は `frame_seq` を `mfxFrameSurface1.Data.TimeStamp` に載せ、`bitstream.TimeStamp == frame_seq` の完全一致で pending frame を引き当てる。B フレームでも順序が入れ替わっても正しく対応付く設計。Decoder 側だけがこの対称性を欠いている。

### テストの穴

- `tests/test_roundtrip.rs` は `gop_ref_dist` を 2 以上に設定するテストが `test_drop_cancels_pending_callbacks` (L588 の `gop_ref_dist = Some(3)`) のみで、そのテストも 1 フレームしか投入しないため B フレームは実出力に至らない。
- 通常のラウンドトリップテストは `gop_ref_dist` を明示せず、`EncoderConfig` のデフォルト (1 = B なし) を使う。
- 結果、CI では顕在化しない。

## 設計方針

以下のいずれかを採用する（要検討）。

### 案 A: `DecodedOrder = 1` で decoded order 出力にする

`initialize` で `mfx.DecodedOrder = 1` を設定し、VPL に「入力順で出力せよ」と指示する。

- 長所: Decoder 側の実装変更が最小で済む（FIFO 対応付けが再び正しくなる）。
- 短所:
  - `DecodedFrame` の実質的な表示順は呼び出し側の責任で並び替えることになる。
  - VPL 仕様上、`DecodedOrder = 1` は「AVC / HEVC 限定」であり、他コーデック（VP9 / AV1）での挙動を要検証。
  - `surface.Data.TimeStamp` / `FrameOrder` の意味が変わる可能性があり、公開 API に影響しないかを確認する必要がある。

### 案 B: TimeStamp による完全一致対応付け（Encoder 対称化）

Encoder 側と同じく、`bs.TimeStamp = frame_seq` を入力側に設定し、`read_decoded_surface` 内で `surface.Data.TimeStamp` を読んで `HashMap<TimeStamp, user_data>` から引き当てる。

- 長所:
  - display order でも decoded order でも正しく対応付く（VPL の出力順に依存しない）。
  - Encoder との対称性が取れる。設計負債の解消になる。
- 短所:
  - `mpsc::channel` に `QueueFrame(user_data)` を先送りする現行構造から、TimeStamp をキーにした `HashMap` 管理に変える必要がある。
  - `TimeStamp = 0` の扱い（VPL の一部ドライバで「未設定」扱いになる可能性）を要検証。encode.rs 側にも同種の懸念があり、共通の対応方針にすると良い。

現状の推奨は **案 B**。Encoder 側と対称的な実装にすることで長期的な保守性が改善するため。ただし案 A のほうが変更が小さいので、緊急パッチとしては案 A で先に修正し、次のマイナーで案 B に置き換える二段構えも検討可能。

## 完了条件

以下すべてを満たす。

1. B フレームを含むビットストリームで Decoder に `decode(bs, user_data)` を N 回呼んだあと、コールバックで受け取る `DecodedFrame::user_data()` が入力時の `user_data` と正しく対応付いている。
2. 上記を検証する統合テストが `tests/test_roundtrip.rs` に追加され、`gop_ref_dist = 3`（2 個の B フレーム）で N ≥ 15 フレームのラウンドトリップで pass する。
3. `skills/shiguredo-vpl/SKILL.md:305-306` の「FIFO で対応付く」記述を実装に合わせて更新する（案 A なら「decoded order 保証」、案 B なら「TimeStamp 一致で対応付く」）。
4. `CHANGES.md` の `## develop` に `[FIX]` として本修正を追記する。

## 影響範囲

- `src/decode.rs`（`initialize` / `run_sync_worker` / `read_decoded_surface` / `WorkerCommand` の対応付け経路）
- `tests/test_roundtrip.rs`（B フレーム込みのラウンドトリップテスト追加）
- `skills/shiguredo-vpl/SKILL.md`（対応付けの説明を実装に合わせる）
- `CHANGES.md`

## 参考

- `/review-code` の致命的指摘 F2
- Encoder 側の対応付け実装: `src/encode.rs:1088-1160, 1341-1357`
- `SKILL.md:305-306` の FIFO 説明
