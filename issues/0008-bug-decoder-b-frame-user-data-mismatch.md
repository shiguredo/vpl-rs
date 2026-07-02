# Decoder で B フレーム有りのビットストリームで user_data の対応が壊れる

- Priority: High
- Created: 2026-07-01
- Completed: {YYYY-MM-DD}
- Model: Opus 4.7
- Branch: feature/fix-decoder-b-frame-user-data-mismatch
- Polished: 2026-07-01

## 目的

`Decoder::decode(bs, user_data)` に渡した `user_data` が、出力される `DecodedFrame::user_data()` で **正しい入力フレームに対応付けられない** バグを修正する。B フレームを含む H.264 / H.265 では出力順 (display order) と入力順 (decode order) が入れ替わるため、Decoder 側の FIFO 対応付けが破綻して user_data が誤ったフレームに紐付く。出力 Sync が user_data より多い場合は紐付かないままサイレント破棄される。

## 優先度根拠

High。以下の理由による。

- **サイレントなデータ破損**: エラーは発生せず、user_data だけが誤って紐付くか消失する。呼び出し側は「何かがずれている」と気付きにくい。
- **公開 API の契約違反**: `skills/shiguredo-vpl/SKILL.md:305-306` は「`user_data` は FIFO キュー (`VecDeque`) で `decode()` 呼び出し順に対応付く。ビットストリームがフレーム境界をまたいでも順序は保たれる」と明記しているが、B フレームによる表示順並び替えでこの契約が破綻する。
- **利用側への影響**: Sora など Decoder を利用するダウンストリームで、user_data に「対応する入力フレームの ID」を載せて追跡している全ユースケースが壊れる。

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

`src/decode.rs:277-307` の `initialize` は `mfxVideoParam.mfx.DecodedOrder` を設定していない。VPL のデフォルト動作は AVC / HEVC で **display order で出力する**。したがって Decoder は自動的に並び替えられた順で `Sync` を返してくる。

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
- 既存の全ラウンドトリップテスト (`test_roundtrip_h264_cbr`, `test_roundtrip_hevc_cbr`, `test_roundtrip_av1_cbr` など) および `roundtrip_colorbar` / `roundtrip_format` ヘルパー経由のテストはすべて `gop_ref_dist` 未指定 (デフォルト 1 = B フレームなし) のため B フレームは一切生成されない。
- `test_decode_user_data_callback` (L508-567) は B フレーム無しのため pass するが、このテストは「各 user_data がコールバックに出現した」ことのみを確認し、**どの出力フレームにどの user_data が紐付いたか**は検証していない。B フレーム有りでは全 user_data 出現の確認だけでは誤った対応付けを検出できない。
- `DecodedFrameInfo` 構造体 (L29-34) には `user_data` フィールドがなく、`decode()` ヘルパー (L211-248) のコールバック内で `user_data` を破棄している。このため既存の全ラウンドトリップテストでは Decoder 出力の `user_data()` を一切検証できていない。
- 結果、CI では顕在化しない。

## 設計方針

**案 B: TimeStamp による完全一致対応付け（Encoder 対称化）で実装する。**

入力 `bs.TimeStamp` に連番 `frame_seq` を設定し、出力 `surface.Data.TimeStamp` を読み取って `HashMap<u64, user_data>` から引き当てる。

- 長所: display order でも decoded order でも正しく対応付く（VPL の出力順に依存しない）。Encoder との対称性が取れる。
- 短所: `mfxBitstream.TimeStamp` が出力 `surface.Data.TimeStamp` に伝搬されることは VPL 仕様で明示的に保証されていない。実装前に実機で伝搬を確認する必要がある。

なお、`DecodedOrder = 1` で decode order 出力にする案 A は、「AVC / HEVC 限定」「出力順が破壊的に変わる」「VP9 / AV1 では効かない」という問題があり不採用とした。

### 設計上のリスクと未検証項目

**リスク A: `bs.TimeStamp` → `surface.Data.TimeStamp` 伝搬が成立しない場合**

本設計は VPL が `mfxBitstream.TimeStamp` を出力 `mfxFrameSurface1.Data.TimeStamp` に伝搬することを前提とする。Encoder 側の伝搬（`surface.Data.TimeStamp` → `bitstream.TimeStamp`）は方向が逆であり、動作実績があるからといって Decoder 側も成立する保証はない。実装着手前に必ず実機で伝搬を確認する。

確認方法: `bs.TimeStamp` に既知値を設定して `DecodeFrameAsync` を呼び、出力 `out_surface.Data.TimeStamp` の値を確認する。また `DecodeHeader`（`initialize()` から呼ばれる）が `bs.TimeStamp` を上書きしないことも併せて確認する。

伝搬が確認できなければ、代替として `frame_seq` を `bs.Reserved[1]` などの予約フィールドに載せる方式、または VPL に依存せず Worker 側で出力出現順を管理する方式を検討し、issue を再起票する。また、H.264 / H.265 以外のコーデック（VP9 / AV1）で伝搬が確認できない場合は、該当コーデックでは FIFO 方式を維持する分岐を実装する（リスク C 参照）。

**リスク B: `decode_bitstream()` が 1 回の `decode()` で複数 Sync を発行するケース**

現在の `decode_bitstream()` (`decode.rs:351-397`) はループ内で複数回 `Sync` を送り得るが、`QueueFrame` は `decode()` 呼び出し 1 回につき 1 つしか送られない。これは FIFO 方式でも存在する既存の限界だが、HashMap 方式では 2 つ目以降の Sync が同一 `frame_seq` を持つため timestamp 不一致エラーになる。

対処: `decode_bitstream` 内に `bool first_sync = true` フラグを導入し、最初の Sync のみ `WorkerCommand::Sync`、2 つ目以降は `WorkerCommand::Drain` で処理する。2 つ目以降のフレームデータはハンドラに通知されず破棄されるが、この状況が現実にどれだけ発生するかは実装後に実機で観測し、必要に応じて別 issue で対応する。

通常の利用（1 回の `decode()` に 1 フレーム分のビットストリームを渡す）では複数 Sync は発生せず、この制限が問題になることはない。

**リスク C: VP9 / AV1 での TimeStamp 伝搬未確認によるリグレッション**

本修正は全コーデックに `HashMap` + `TimeStamp` 一致方式を適用する。現在の FIFO 方式は B フレームを持たない VP9 / AV1 で正しく動作しているが、`bs.TimeStamp` → `surface.Data.TimeStamp` 伝搬が VP9 / AV1 で成立しない場合、全 user_data の引き当てが空振りになり、ハンドラにはエラーのみが通知される。

対処: リスク A の伝搬確認を H.264 / H.265 に加えて VP9（可能なら AV1）でも実施し、少なくとも「伝搬しないコーデックでは HashMap 方式を無効化する（FIFO にフォールバックする）」分岐を実装するか、全コーデックで伝搬が確認できるまで実装を保留する。

### 変更の概要

#### `src/decode.rs`

1. **`Decoder` 構造体に `frame_count: u64` を追加する**
   - `Encoder` (`encode.rs:672`) と同様に連番を管理する。
   - 初期値は依存 issue 0013 と整合させる。詳細は「依存 issue」セクションを参照。

2. **`decode()` 内で `bs.TimeStamp` に `frame_seq` を設定する**
   - 現在の `decode.rs:319` では `bs` を `zeroed()` で初期化し `TimeStamp = 0` のままである。
   - 以下の処理順序で実装する:
     1. `let frame_seq = self.frame_count` で現在の連番を取得
     2. `bs.TimeStamp = frame_seq` でビットストリームに設定（`initialize()` の `DecodeHeader` より前に設定しても `TimeStamp` は上書きされない前提）
     3. `self.send_worker_command("Decoder::decode", WorkerCommand::QueueFrame { frame_seq, user_data })` で Worker に転送
     4. `self.decode_bitstream(&mut bs)` でデコード実行
     5. `self.frame_count = self.frame_count.checked_add(1).ok_or_else(|| ...)?` でインクリメント

3. **`WorkerCommand` の `QueueFrame` に `frame_seq: u64` を追加する**
   - 現在: `WorkerCommand::QueueFrame(T)`
   - 変更後: `WorkerCommand::QueueFrame { frame_seq: u64, user_data: T }`

4. **`run_sync_worker` で `VecDeque<T>` を `HashMap<u64, T>` に置き換える**
   - `pending_values: VecDeque<H::UserData>` → `pending_map: HashMap<u64, H::UserData>`
   - `QueueFrame` 受信時: `pending_map.insert(frame_seq, user_data)`。重複キー検出時は `on_decoded(Err(...))` でエラー通知（`encode.rs:1289-1295` と同様）。
   - `Sync` 受信時: `sync_and_callback` で `surface.Data.TimeStamp` から timestamp を読み取り、`pending_map.remove(&timestamp)` で引き当てる。

5. **`read_surface_timestamp` ヘルパー関数を追加する（新設）**
   - `read_decoded_surface` は既存の借用（ゼロコピー）のまま維持する。
   - 新たに `fn read_surface_timestamp(surface: &FrameSurface) -> u64` を追加し、`unsafe { (*surface.as_ptr()).Data.TimeStamp }` で timestamp を読み取る。
   - このアプローチにより、既存の `DecodedFrame<'a, T>` の借用セマンティクスとライフタイムパラメータを一切変更しない。
   - 安全性: `Data.TimeStamp` は VPL が出力サーフェスに設定するメタデータであり、Map 後は読み取り専用。`read_surface_timestamp` と `read_decoded_surface` は同一サーフェス上の異なるフィールド（`TimeStamp` と `Y`/`UV` プレーン）を読み取るため、aliasing rules に違反しない。

6. **`sync_and_callback` のシグネチャと実装を変更する**
   - 現在のシグネチャ: `(lib, session_handle, sync_data, user_data: H::UserData, handler: &mut H) -> Result<()>`
   - 変更後: `(lib, session_handle, sync_data, pending_map: &mut HashMap<u64, H::UserData>, handler: &mut H) -> Result<()>`
   - 内部処理:
     1. `SyncOperation` 完了後、`frame_surface.map_read()` で Map
     2. `read_surface_timestamp(&frame_surface)` で `timestamp` を取得
     3. `pending_map.remove(&timestamp)` で user_data を引き当てる
     4. 引き当て成功: 既存の `read_decoded_surface(&frame_surface, user_data)` を呼び、`DecodedFrame` を構築して `handler.on_decoded(Ok(frame))`
     5. 引き当て失敗: `Err` を返す。エラー通知は呼び出し元（`Sync` アーム）で `handler.on_decoded(Err(...))` として一元化する
   - Map 済みの `frame_surface` は、Err 時も含めて Drop 時に `Unmap + Release` が自動実行されるため明示的な解放は不要。

7. **`WorkerCommand` に `Drain` バリアントを追加する**
   - `finish()` から送られる null bitstream 由来の Sync フレームには user_data が存在しないため、`Sync` とは別に `Drain { sync_data }` を追加する。
   - Encoder 側は drain 時も `Sync` で `bitstream.TimeStamp` 引き当てを行うが、Decoder では null bitstream での `TimeStamp` 伝搬が保証されないため別バリアントに分離する。

8. **`run_sync_worker` のマッチアームを書き換える**
   - `QueueFrame`: `pending_map` に登録
   - `Sync`: `sync_and_callback` → 成功時 `Ok`、失敗時は `handler.on_decoded(Err(...))`
   - `Drain`: `sync_and_drain` を呼び出し、データは読み取らず解放のみ
   - `WaitIdle`: 残留チェック + 応答
   - `Stop`: `pending_map.drain()` で全エントリに `MFX_ERR_ABORTED` 通知

9. **`finish()` のドレイン時に `WorkerCommand::Drain` を送信する**
   - 現在の `decode.rs:442` の `WorkerCommand::Sync` を `WorkerCommand::Drain` に変更する。
   - `sync_and_drain` 関数本体の変更は不要（呼び出し元のみ変更）。

10. **`decode.rs` の doc comment を更新する**
    - L317: `decode()` の doc comment（「FIFO キュー」→「`TimeStamp` 一致」）
    - L337-338: `decode()` 内のコードコメント（「FIFO 対応付けられる」→「`TimeStamp` 一致で対応付けられる」）
    - L344-345: `decode_bitstream()` の doc comment（「FIFO キュー」→「`TimeStamp` 一致」）
    - L401-402: `finish()` の doc comment（「FIFO キュー」→「`TimeStamp` 一致」）
    - L502-504: `run_sync_worker()` の doc comment（「FIFO キュー」→「`TimeStamp` 一致」）
    - L477: `stop_worker` のコードコメント（「`pending_values`」→「`pending_map`」）
    - L142: `WorkerCommand::Stop` の doc comment（「`pending_values`」→「`pending_map`」）

11. **`Stop` 時の pending drain 処理を `HashMap` 対応に変更する**
    - 現在: `for _user_data in pending_values.drain(..)`
    - 変更後: `for (_, user_data) in pending_map.drain()`
    - `HashMap::drain()` の順序は `VecDeque::drain()` と異なり不定だが、全エントリに同一の `MFX_ERR_ABORTED` を通知するため順序は問題にならない。

12. **`WaitIdle` アームで `pending_map` の残留チェックを追加する**
    - Encoder 側 (`encode.rs:1304-1322`) を移植する。
    - 残留エントリがある場合は、全エントリを drain して各エントリに「`decode pending frame seq={frame_seq}`」を含むエラーメッセージでハンドラに通知し、`reply_tx` にエラーを返す。
    - このエラーメッセージはテスト可能であること。

13. **`use std::collections::HashMap;` を import に追加する**
    - `VecDeque` は `decode.rs` 内で `pending_values` (L511) のみで使用されていたため削除し、`HashMap` に置き換える。

14. **`decode_bitstream` 内の複数 Sync 発行時に対応する**
    - 関数内に `bool first_sync = true` フラグを導入する（`decode_bitstream` のローカル変数）。
    - `first_sync == true` なら `WorkerCommand::Sync` を送信し `first_sync = false` に設定。
    - 2 つ目以降（`first_sync == false`）なら `WorkerCommand::Drain` を送信する。

#### `tests/test_roundtrip.rs`

1. **`DecodedFrameInfo` に `user_data: usize` フィールドを追加する**

2. **`decode()` ヘルパーを拡張する**
   - コールバックで `DecodedFrame::user_data()` を取得し、`DecodedFrameInfo` に保存する。
   - B フレーム有りテスト用に、`EncodedFrame` のリストを受け取り `EncodedFrame::user_data()` を Decoder の `decode()` に渡す新しいヘルパーを追加する。既存の `bitstreams: &[Vec<u8>]` ベースのヘルパーは変更しない。

3. **B フレーム有効時の統合テストを追加する**
   - `gop_ref_dist = Some(3)` (2 B フレーム) で N >= 15 フレームのラウンドトリップ。
   - Encoder で入力フレームごとに `user_data = frame_index` (0..N-1) を渡し、Encoder 出力の `EncodedFrame::user_data()` を Decoder 入力の `user_data` に転送する。
   - Decoder の全コールバック出力を収集し、以下を検証する:
     1. `DecodedFrame::user_data()` の集合が `{0..N-1}` と一致（過不足なし）
     2. 各値がちょうど 1 回出現（重複なし）
   - 検証の妥当性: 修正後の `DecodedFrame::user_data()` は `HashMap<frame_seq, user_data>` を `surface.Data.TimeStamp`（= `frame_seq`）で引き当てた結果である。`frame_seq` は入力時の連番であり、`TimeStamp` 伝搬が成立すれば常に正しい `user_data` が返る。集合の過不足検査はデータ喪失や重複がないことの検証であり、対応付けの正しさは修正機構自体が保証する。
   - `test_decode_user_data_callback` にも `gop_ref_dist = Some(3)` 版を追加する。

4. **テストのコーデック範囲**: H.264 と H.265 の B フレーム有りテストは必須。VP9 / AV1 は B フレーム概念がなく新テストの対象外だが、修正が全コーデックに適用されるため、既存の VP9 / AV1 ラウンドトリップテストが引き続き pass することを確認する。

#### `skills/shiguredo-vpl/SKILL.md`

- L305-306: 「FIFO キュー (`VecDeque`)」→「`TimeStamp` 一致」に更新。「キューに残る」→「pending_map に残る」に更新。
- L310-327: `DecodedFrame<'_, T>` のライフタイム説明は借用モデルを維持するため変更不要。
- L367-368: Worker 内キューの枯渇と順序保証に関する記述を `HashMap` 方式に合わせて更新。
- 上記に加え、SKILL.md 内の「FIFO」「キュー」「VecDeque」を全検索し、Decoder の user_data 対応付けに関する全記述を更新する。

## 完了条件

以下すべてを満たす。

1. 実装着手前に、H.264 / H.265 / VP9（可能なら AV1）で `bs.TimeStamp` → `surface.Data.TimeStamp` の伝搬が成立することを実機で確認する。伝搬が確認できたコーデックにのみ HashMap 方式を適用する。
2. B フレームを含むビットストリームで Decoder に `decode(bs, user_data)` を N 回呼んだあと、コールバックで受け取る `DecodedFrame::user_data()` が入力時の `user_data` と正しく対応付いている。
3. 上記を検証する統合テストが `tests/test_roundtrip.rs` に追加され、`gop_ref_dist = 3`（2 個の B フレーム）で N >= 15 フレームのラウンドトリップで以下を検証して pass する:
   - 全 `DecodedFrame::user_data()` の集合が入力時の値と過不足なく一致する
   - 各 `user_data` 値がちょうど 1 回出現する（重複なし）
4. `skills/shiguredo-vpl/SKILL.md` 内の全「FIFO キュー」関連記述が `TimeStamp` 一致方式に更新されている。
5. `CHANGES.md` の `## develop` に `[FIX]` として本修正を追記する。
6. `finish()` がエラーなく完了し、ドレインフレームが `WorkerCommand::Drain` 経由で正しく処理されること。
7. 既存の VP9 / AV1 ラウンドトリップテストが引き続き pass することを確認する。伝搬が確認できないコーデックについては、`DecodedFrameInfo` に `user_data` フィールドを追加した状態で既存テストの pass をもってリグレッションなしと判定する。

## 影響範囲

- `src/decode.rs`: `Decoder` 構造体 (`frame_count` 追加)、`decode()` (`bs.TimeStamp` 設定, オーバーフローチェック, QueueFrame に frame_seq 追加, コードコメント更新)、`decode()` の doc comment (L317)、`decode_bitstream()` の doc comment (L344-345)、`finish()` の doc comment (L401-402)、`run_sync_worker()` の doc comment (L502-504)、`stop_worker` のコードコメント (L477)、`WorkerCommand::Stop` の doc comment (L142)、`decode_bitstream` (first_sync フラグと Drain バリアント制御)、`run_sync_worker` (VecDeque → HashMap, 重複キー検出追加)、`WorkerCommand` (QueueFrame に frame_seq 追加, Drain バリアント新設)、`read_surface_timestamp` (新設)、`read_decoded_surface` (変更なし)、`sync_and_callback` (シグネチャ変更, timestamp 読み取りと一致引き当て, エラー通知は呼び出し元に一元化)、`sync_and_drain` (関数本体変更不要、呼び出し元を Sync アームから Drain アームに変更)、`finish()` (Drain バリアント送信), `WaitIdle` アーム (残留 pending_map チェックとエラー通知追加)、`Drop`/`Stop` (HashMap drain 対応)、`std::collections::HashMap` import 追加, `std::collections::VecDeque` import 削除
- `tests/test_roundtrip.rs`: `DecodedFrameInfo` 構造体 (`user_data` フィールド追加)、`decode()` ヘルパー (`user_data` 保持)、B フレーム用 Encoder user_data 転送ヘルパー（新設）、B フレーム込み統合テスト追加、`test_decode_user_data_callback` の `gop_ref_dist` 版追加
- `skills/shiguredo-vpl/SKILL.md`（全 FIFO 関連記述を TimeStamp 一致に更新）
- `CHANGES.md`

### 後方互換性

- 公開 API のシグネチャ (`Decoder::decode()`, `DecodedFrame::user_data()`) は変更なし
- `DecodedFrame<'a, T>` の型パラメータ・ライフタイムは変更なし（借用モデルを維持）
- `enumerate()` で user_data を渡している既存利用者コードは、B フレームがなければ修正前後で挙動が変わらない
- B フレーム有りの利用者にとっては **バグ修正** であり、破壊的変更ではない
- 注意: VPL の内部バッファリングにより `decode()` 呼び出し順と出力順が 1:1 でないケース（`MFX_ERR_MORE_DATA` で Sync が遅延するケース）では、FIFO 方式では遅延した分だけ user_data の割り当てがずれる。TimeStamp 一致方式では遅延の有無に関わらず `frame_seq` と `surface.Data.TimeStamp` の一致で正しく引き当てられるため、このケースも改善される。

## 依存 issue

- **issue 0013** (`0013-bug-encoder-frame-seq-zero-timestamp-collision`): Encoder 側の `frame_count` 初期値を 1 に変更する修正。本 issue の `frame_count` 初期値は 0013 と整合させる。0013 より先に 0008 を実装する場合は暫定的に 1 スタートとし、0013 実装後に揃える。
- **issue 0010** (`0010-bug-drop-deadlock-on-sync-operation-infinite`): `sync_and_drain` および `sync_and_callback` 内の `MFX_INFINITE` を修正する。本 issue で `sync_and_callback` のシグネチャを変更するため、0010 を先に適用し、その差分の上に 0008 の変更を重ねること。`WorkerCommand::Drain` 経由の `sync_and_drain` 呼び出しも 0010 の修正後のコードを経由する。
