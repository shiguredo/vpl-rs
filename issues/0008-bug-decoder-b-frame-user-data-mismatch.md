# Decoder で B フレーム有りのビットストリームで user_data の対応が壊れる

- Priority: High
- Created: 2026-07-01
- Completed: {YYYY-MM-DD}
- Model: Opus 4.7
- Branch: feature/fix-decoder-b-frame-user-data-mismatch
- Polished: 2026-08-21

## 目的

`Decoder::decode(bs, user_data)` に渡した `user_data` が、出力される `DecodedFrame::user_data()` で **正しい入力フレームに対応付けられない** バグを修正する。B フレームを含む H.264 / H.265 では出力順 (display order) と入力順 (decode order) が入れ替わるため、Decoder 側の FIFO 対応付けが破綻して user_data が誤ったフレームに紐付く。出力 Sync が user_data より多い場合は紐付かないままサイレント破棄される。

## 優先度根拠

High。以下の理由による。

- **サイレントなデータ破損**: エラーは発生せず、user_data だけが誤って紐付くか消失する。呼び出し側は「何かがずれている」と気付きにくい。
- **公開 API の契約違反**: `skills/shiguredo-vpl/SKILL.md` は「`user_data` は FIFO キュー (`VecDeque`) で `decode()` 呼び出し順に対応付く。ビットストリームがフレーム境界をまたいでも順序は保たれる」と明記しているが、B フレームによる表示順並び替えでこの契約が破綻する。
- **利用側への影響**: `user_data` に「対応する入力フレームの ID」を載せて出力フレームを追跡している利用者では、B フレーム有りのビットストリームで誤対応付けにより追跡が破綻する。

## 現状

### 対応付けの実装

`src/decode.rs` の `run_sync_worker` は以下の FIFO で user_data を出力に割り当てている。

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

`src/decode.rs` の `initialize` は `mfxVideoParam.mfx.DecodedOrder` を設定していない。VPL のデフォルト動作は AVC / HEVC で **display order で出力する**（`DecodedOrder` 未設定時のデフォルト。一次資料 `refs/oneVPL/api/vpl/mfxstructures.h` の `mfxInfoMFX::DecodedOrder` 説明は「For AVC and HEVC, used to instruct the decoder to return output frames in the decoded order.」であり、未設定（0）なら指示なし = decode order ではない出力となる。また同説明の続き「When enabled, correctness of `mfxFrameData::TimeStamp` and `FrameOrder` for output surface is not guaranteed, the application should ignore them.」は、DecodedOrder 有効時の出力 `surface.Data.TimeStamp` の破壊的影響を述べるものであり、未設定（本 design）ならこの影響を受けないことを示す。実機での確認は完了条件 1）。したがって Decoder は自動的に並び替えられた順で `Sync` を返してくる。

### 実例

入力（decode order）: `I0, P1, B2`
Decoder が返す出力（display order）: `I0, B2, P1`

FIFO で user_data を割り当てると:

| Sync 順 | 実 frame | 割り当てられた user_data |
|---|---|---|
| 1 | I0 | 0 (I0 の入力時) → 正しい |
| 2 | B2 | 1 (P1 の入力時) → **誤り** |
| 3 | P1 | 2 (B2 の入力時) → **誤り** |

なお、この表は「入力 I0 が即座に出力される」ことを仮定した例であり、実際の出力タイミングは VPL の内部バッファリングに依存する。誤対応付けが起こることは説明として十分であり、出力タイミングが異なる場合も「対応付けが入力順でなく出力順になる」点は変わらない。

### Encoder 側との非対称

Encoder 側 (`src/encode.rs`) は `frame_seq` を `mfxFrameSurface1.Data.TimeStamp` に載せ、`bitstream.TimeStamp == frame_seq` の完全一致で pending frame を引き当てる。B フレームでも順序が入れ替わっても正しく対応付く設計。Decoder 側だけがこの対称性を欠いている。

### テストの穴

- `tests/test_roundtrip.rs` は `gop_ref_dist` を 2 以上に設定するテストが `test_drop_cancels_pending_callbacks` のみで、そのテストも 1 フレームしか投入しないため B フレームは実出力に至らない。
- 既存の全ラウンドトリップテスト (`test_roundtrip_h264_cbr`, `test_roundtrip_hevc_cbr`, `test_roundtrip_av1_cbr` など) および `roundtrip_colorbar` / `roundtrip_format` ヘルパー経由のテストはすべて `gop_ref_dist` 未指定 (デフォルト 1 = B フレームなし) のため B フレームは一切生成されない。
- `test_decode_user_data_callback` は B フレーム無しのため pass するが、このテストは「各 user_data がコールバックに出現した」ことのみを確認し、**どの出力フレームにどの user_data が紐付いたか**は検証していない。B フレーム有りでは全 user_data 出現の確認だけでは誤った対応付けを検出できない。
- `DecodedFrameInfo` 構造体には `user_data` フィールドがなく、`decode()` ヘルパーのコールバック内で `user_data` を破棄している。このため既存の全ラウンドトリップテストでは Decoder 出力の `user_data()` を一切検証できていない。
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

なお、`refs/oneVPL-intel-gpu` のデコーダ実装には `bs.TimeStamp` → `surface.Data.TimeStamp` の伝搬経路が全 4 コーデックで実装されている。入力側は `_studio/mfx_lib/shared/src/mfx_common_decode_int.cpp` の `MFXMediaDataAdapter::Load` が `SetTime(GetUmcTimeStamp(pBitstream->TimeStamp))` で `bs.TimeStamp` を UMC フレームへ取り込み、出力側は各コーデックのデコード処理が `surface_out->Data.TimeStamp` に書き戻す（H.264: `_studio/mfx_lib/decode/h264/src/mfx_h264_dec_decode.cpp`、H.265: `_studio/mfx_lib/decode/h265/src/mfx_h265_dec_decode.cpp`、VP9: `_studio/mfx_lib/decode/vp9/src/mfx_vp9_dec_decode_hw.cpp` の `(*surface_out)->Data.TimeStamp = bs->TimeStamp != MFX_TIMESTAMP_UNKNOWN ? bs->TimeStamp : ...`、AV1: `_studio/mfx_lib/decode/av1/src/mfx_av1_dec_decode.cpp`）。仕様文言上の保証ではないため実機確認は必須だが、確認の効率化に使える。

確認方法: `bs.TimeStamp` に既知値を設定して `DecodeFrameAsync` を呼び、**SyncOperation 完了後**に出力 `out_surface.Data.TimeStamp` の値を確認する（非同期デコードでは Sync 完了前の値は確定していないため、必ず Sync 後に読む）。B フレーム表示順出力で遅延出力されるフレームと `finish()` のドレイン期に出力されるフレームについても、TimeStamp が対応する入力フレームの `frame_seq` と一致することを確認する。

伝搬が確認できなければ、代替として `frame_seq` を `bs.Reserved[1]` などの予約フィールドに載せる方式を検討し、issue を再起票する。

**リスク B: 1 回の `decode()` で複数 Sync が発行されるケースと、フレーム境界をまたぐ分割入力のケース**

現在の `decode_bitstream()` はループ内で複数回 `Sync` を送り得るが、`QueueFrame` は `decode()` 呼び出し 1 回につき 1 つしか送られない。また `skills/shiguredo-vpl/SKILL.md` が保証する「ビットストリームがフレーム境界をまたいでも順序は保たれる」は、1 フレームが複数回の `decode()` 呼び出しに分割されて供給されるケースを含む。

複数 Sync の 2 つ目以降がどの `frame_seq` を持つかは実機確認まで確定しない。リスク A の確認手順で「出力フレームの TimeStamp が対応する入力フレームの `frame_seq` と一致する」ことを確認できれば、2 つ目以降の Sync も正しく引き当てられる。なお、この「正しく引き当てられる」が成立する前提は「1 回の `decode()` 呼び出し = 1 出力フレーム」である。1 回の `decode()` で複数フレームが出力された場合、`bs.TimeStamp` は呼び出しごとに 1 回しか設定されないため、複数出力が同一 `frame_seq` を共有し得る（この場合 2 つ目以降は引き当て失敗で drain 扱いとなる。現行 FIFO 方式でも同様に 1 呼び出し 1 `QueueFrame` のため、回帰ではない）。

なお、出力 TimeStamp の決定メカニズムはコーデックにより異なり、「同一 `frame_seq` の共有」の起こりやすさも変わる（いずれも実機確認で確定する）:
- **VP9**: 入力 `bs->TimeStamp` を直接コピーするため（`mfx_vp9_dec_decode_hw.cpp` の `(*surface_out)->Data.TimeStamp = bs->TimeStamp != MFX_TIMESTAMP_UNKNOWN ? bs->TimeStamp : ...`）、1 呼び出しで複数フレーム出力すると同一値の共有が起こり得る。
- **H.264 / H.265 / AV1**: 入力 `bs->TimeStamp` を各フレームの時刻（`m_dFrameTime` / `FrameTime`）に保存し、そのフレーム自身の時刻を出力に書き戻すため（H.264: `mfx_h264_dec_decode.cpp`、H.265: `mfx_h265_dec_decode.cpp`、AV1: `mfx_av1_dec_decode.cpp`）、同一入力から複数フレームが出力される場合も各フレームの時刻がそのまま伝搬する。
- また TimeStamp は H.264 / H.265 / AV1 で VPL 内部の u64 → double → u64 の往復変換（90kHz の単位換算、`GetUmcTimeStamp` / `GetMfxTimeStamp`）を経るため、「完全一致」が変換の丸め精度に依存する点にも注意する（この範囲はリスク A の実機確認で検証する。VP9 は変換を経ず直接コピー）。

分割入力では `bs.TimeStamp` が呼び出しごとに上書きされるため、出力フレームの TimeStamp がどの呼び出しの `frame_seq` になるか（先頭 / 途中 / 最後は VPL 実装依存）によって引き当てられる user_data が変わる。FIFO 方式では常に「フレームの先頭呼び出しの user_data」が付いた。分割入力時の対応付けは保証しないため、`skills/shiguredo-vpl/SKILL.md` の「フレーム境界をまたいでも順序は保たれる」の記述を、分割入力時の user_data 対応付けが保証されない旨に更新する。

対処: 2 つ目以降の Sync も TimeStamp 引き当てを試行し、引き当てに失敗した場合のみ drain 扱いで破棄する。特別な制御（first_sync フラグや `Drain` バリアント）は導入しない。

**リスク C: VP9 / AV1 での TimeStamp 伝搬未確認によるリグレッション**

本修正は全コーデックに `HashMap` + `TimeStamp` 一致方式を適用する。現在の FIFO 方式は B フレームを持たない VP9 / AV1 で正しく動作しているが、`bs.TimeStamp` → `surface.Data.TimeStamp` 伝搬が VP9 / AV1 で成立しない場合、全 user_data の引き当てが空振りになり、`on_decoded(Ok(...))` は通知されず、`pending_map` が残留して `finish()` の `WaitIdle` 残留チェック（項目 11）でエラー通知され、`finish()` が `Err` を返す。

対処: リスク A の伝搬確認を H.264 / H.265 に加えて VP9（可能なら AV1）でも実施する。伝搬が確認できないコーデックでは HashMap 方式を無効化して FIFO 方式を維持する分岐を実装する（分岐の配線は、`run_sync_worker` へコーデック情報を渡す形を実装時に検討する）。

### 変更の概要

#### `src/decode.rs`

1. **`Decoder` 構造体に `frame_count: u64` を追加する**
   - Encoder と同様に連番を管理する。
   - 初期値は 0 とする（Encoder の `frame_count` 初期値 0 と対称にする。0013 は closed で「`frame_count` 1 スタート化」は不採用のため、本 issue の初期値は 0013 に依存せず決定する）。

2. **`decode()` 内で `bs.TimeStamp` に `frame_seq` を設定する**
   - 現在の `decode()` では `bs` を `zeroed()` で初期化し `TimeStamp = 0` のままである。
   - 以下の処理順序で実装する:
     1. 未初期化の場合は `initialize()` を呼ぶ（`initialize()` は手順 3 より前に呼ばれるため、`bs.TimeStamp` の設定順に影響しない）
     2. `let frame_seq = self.frame_count` で現在の連番を取得
     3. `bs.TimeStamp = frame_seq` でビットストリームに設定
     4. `self.send_worker_command("Decoder::decode", WorkerCommand::QueueFrame { frame_seq, user_data })` で Worker に転送
     5. `self.frame_count = self.frame_count.checked_add(1).ok_or_else(|| ...)?` でインクリメント
     6. `self.decode_bitstream(&mut bs)` でデコード実行
   - 注意: 手順 5 のインクリメントをデコード実行（手順 6）より前に置くことで、`decode_bitstream` がエラーを返しても送信済み `QueueFrame` の `frame_seq` が次回 `decode()` で再利用されて重複キー検出エラーになったり、旧 `user_data` が上書き喪失したりしない。エラーで出力に紐付かなかった `user_data` は `pending_map` に残留したままなので、`finish()` の残留チェック（項目 11）または `Drop` 時の `Stop` アーム（項目 10）でエラーとして通知される（0009 の「エラー後の `frame_count` の扱いを整合させる」要求をこの順序で満たす）。

3. **`WorkerCommand` の `QueueFrame` に `frame_seq: u64` を追加する**
   - 現在: `WorkerCommand::QueueFrame(T)`
   - 変更後: `WorkerCommand::QueueFrame { frame_seq: u64, user_data: T }`

4. **`run_sync_worker` で `VecDeque<T>` を `HashMap<u64, T>` に置き換える**
   - `pending_values: VecDeque<H::UserData>` → `pending_map: HashMap<u64, H::UserData>`
   - `QueueFrame` 受信時: `pending_map.insert(frame_seq, user_data)`。重複キー検出時は `on_decoded(Err(...))` でエラー通知（Encoder 側の `PendingFrameStore::insert` の重複検出と同様）。
   - `Sync` 受信時: `sync_and_callback` を呼び、`pending_map.remove(&timestamp)` で引き当てる。引き当て失敗時は `sync_and_callback` 内部で破棄され（項目 6 参照）、`Sync` アームでの分岐は不要。

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
     5. 引き当て失敗: エラー通知は行わず、そのまま `Ok(())` を返す（`Sync` アームでの分岐は不要。Map 済みの `frame_surface` は Drop 時に `Unmap + Release` が自動実行されるため、解放のみが自動で行われる）
   - `sync_and_callback` の `Err` はデータ破損系（`SyncOperation` 失敗・`map_read` 失敗・`read_decoded_surface` の検証エラー）のみとする。**引き当て失敗は `Err` にしない**（ドレインの空フレームや異常出力が原因の正常な破棄として扱う）。
   - `SyncOperation` 失敗（デバイスエラー等）時は `Err` を返し、Worker の `Sync` アームが `handler.on_decoded(Err(...))` で通知する。このとき対応する `pending_map` エントリは消費できない（`SyncOperation` 完了前に出力 timestamp が確定せず、どのエントリか特定できないため）。残留エントリは `WaitIdle` アームの残留チェック（項目 11）で通知され、`finish()` が `Err` を返す。`finish()` を呼ばずに `Drop` した場合は `Stop` アームの一括 `MFX_ERR_ABORTED` 通知に委ねる。デバイスエラー時は同一 user_data が 2 回通知（Sync エラー + 残留処理）され得るが、これは TimeStamp 方式の制約であり許容する（0010 の Encoder 側も二重通知を許容する方針に変更された。0010 の設計方針 2 参照）。
   - 引き当て失敗の破棄は `sync_and_callback` 内部で完結するため、`Sync` アームからの `sync_and_drain` 呼び出しは不要になり、**`sync_and_drain` は呼び出し元が消滅するため削除する**（0010 は `sync_and_drain` を変更せず、0014 は変更対象外としている。削除は 0008 の変更に含める）。

7. **`run_sync_worker` のマッチアームを書き換える**
   - `QueueFrame`: `pending_map` に登録
   - `Sync`: `sync_and_callback` を呼び、`Err` 時のみ `handler.on_decoded(Err(...))` で通知（`Ok` 時は引き当て成功なら `on_decoded(Ok)` 済み、引き当て失敗なら破棄済みのため何もしない）
   - `WaitIdle`: 残留チェック + 応答
   - `Stop`: `pending_map.drain()` で全エントリに `MFX_ERR_ABORTED` 通知
   - `WorkerCommand::Drain` バリアントは**導入しない**。`finish()` のドレインフレームも `Sync` として送り、TimeStamp 引き当て成功時は通知、失敗時は drain 扱いで破棄する。

8. **`finish()` のドレイン時も `WorkerCommand::Sync` を送信する（現行のまま）**
   - ドレイン出力は B フレーム表示順出力の末尾フレーム（対応する user_data が存在する実フレーム）を含むため、`Sync` として送り TimeStamp 引き当てで対応付ける。引き当て失敗時のみ drain 扱いで破棄する。

9. **`decode.rs` の doc comment を更新する**
   - `decode()` の doc comment（「FIFO キュー」→「`TimeStamp` 一致」）
   - `decode()` 内のコードコメント（「FIFO 対応付けられる」→「`TimeStamp` 一致で対応付けられる」）
   - `decode_bitstream()` の doc comment（「FIFO キュー」→「`TimeStamp` 一致」）
   - `finish()` の doc comment（「FIFO キュー」→「`TimeStamp` 一致」）
   - `run_sync_worker()` の doc comment（「FIFO キュー」→「`TimeStamp` 一致」）
   - `stop_worker` のコードコメント（「`pending_values`」→「`pending_map`」）
   - `WorkerCommand::Stop` の doc comment（「`pending_values`」→「`pending_map`」）

10. **`Stop` 時の pending drain 処理を `HashMap` 対応に変更する**
    - 現在: `for _user_data in pending_values.drain(..)`
    - 変更後: `for (_, user_data) in pending_map.drain()`
    - `HashMap::drain()` の順序は `VecDeque::drain()` と異なり不定だが、全エントリに同一の `MFX_ERR_ABORTED` を通知するため順序は問題にならない。

11. **`WaitIdle` アームで `pending_map` の残留チェックを追加する**
    - Encoder 側の `run_sync_worker` の `WaitIdle` アームを移植する。
    - 残留エントリがある場合は、全エントリを drain して各エントリに「`decode pending frame seq={frame_seq}`」を含むエラーメッセージでハンドラに通知し、`reply_tx` にエラーを返す。
    - このエラーメッセージはテスト可能であること。
    - 残留の処理担当の整理: 通常フロー（`finish()` を呼ぶ）では、Sync 失敗時に消費されなかった `pending_map` エントリはここ（`WaitIdle`）で drain され、`finish()` が `Err` を返す。`finish()` を呼ばずに `Drop` した場合のみ `Stop` アーム（項目 10）が一括 `MFX_ERR_ABORTED` で通知する（項目 6 参照）。「Stop に委ねる」は `finish()` を呼ばない Drop 経路の話であり、通常フローでは `WaitIdle` が先に処理するため、項目 6 と矛盾しない。
    - B フレーム表示順出力では `finish()` のドレインで末尾フレームが出力されるため、1 フレーム = 1 `decode()` 呼び出しの通常利用では残留は発生しない。残留が発生するのは「入力フレームが出力されない」異常系（Sync 失敗の連続等）と、フレーム境界をまたぐ分割入力（リスク B 参照。分割入力では複数の `frame_seq` のうち出力に消費されないものが残留する）。
    - 既知の制約: 正当なストリームでも出力が発生しないフレームが存在すると残留し、`finish()` が `Err` になる。代表例は VP9 temporal scalability の層切り替えフレーム（エンコーダ側 `refs/oneVPL-intel-gpu/_studio/mfx_lib/encode_hw/vp9/src/mfx_vp9_encode_hw_utils.cpp` で `showFrame = 0` が設定され、デコーダ側 `mfx_vp9_dec_decode_hw.cpp` は showFrame 時のみ出力する）。本 crate は VP9 temporal layer 設定を公開していないため自前エンコードでは発生しないが、第三者の temporal-layer VP9 ビットストリームでは発生し得る。対応は本 issue のスコープ外として別途検討する（レビュー指摘 4-2 の記録）。

12. **`use std::collections::HashMap;` を import に追加する**
    - `VecDeque` は `decode.rs` 内で `pending_values` のみで使用されていたため削除し、`HashMap` に置き換える。

#### `tests/test_roundtrip.rs`

1. **`DecodedFrameInfo` に `user_data: usize` フィールドを追加する**

2. **`decode()` ヘルパーを拡張する**
   - コールバックで `DecodedFrame::user_data()` を取得し、`DecodedFrameInfo` に保存する。
   - B フレーム有りテスト用に、`EncodedFrame` のリストを受け取り `EncodedFrame::user_data()` を Decoder の `decode()` に渡す新しいヘルパーを追加する。既存の `bitstreams: &[Vec<u8>]` ベースのヘルパーは変更しない。

3. **B フレーム有効時の統合テストを追加する**
   - `gop_ref_dist = Some(3)` (2 B フレーム) で N >= 15 フレームのラウンドトリップ。
   - H.264 は profile に Main 以上を指定する（Baseline プロファイルは B スライスを符号化しないという H.264 規格の制約による。一次資料（`refs/`）で直接確認できないため、テスト項目の「B ピクチャが含まれることを確認する」で裏付ける）。H.265 は Main を指定する。`gop_pic_size` も指定して GOP 構造を確定させる。
   - Encoder で入力フレームごとに `user_data = frame_index` (0..N-1) を渡し、Encoder 出力の `EncodedFrame::user_data()` を Decoder 入力の `user_data` に転送する。
   - Encoder 出力のうちドレイン期の空データフレーム（`data()` が空、`DataLength == 0`）は Decoder に渡さず除外する（空フレームを `decode()` に渡すと `QueueFrame` が 1 つ消費されるだけでフレームが出力されず、`pending_map` に残留して `finish()` の残留チェックでエラーになるため）。
   - なお、エンコーダのドレイン空フレームは `TimeStamp = 0` のまま生成されるため（`create_bitstream()` が `zeroed()` で初期化する）、ドレイン空フレームは `frame_seq = 0` の pending が残っていればそれを誤消費し（この場合エラーは実フレーム 0 の出力時に顕在化）、残っていなければ「no pending frame for bitstream timestamp 0」の `Err` 通知になる（エンコーダ側の既存の課題であり、issue 0025 で対応する。0013 は closed で `frame_count` 1 スタート化は不採用のため、この前提は 0013 に依存しない）。テストヘルパーではこの `Err` をドレイン期の空フレームに限って許容する（issue 0025 の完了条件 2 参照）。
   - Decoder の全コールバック出力を収集し、以下を検証する:
     1. `DecodedFrame::user_data()` の集合が `{0..N-1}` と一致（過不足なし）
     2. 各値がちょうど 1 回出現（重複なし）
     3. `user_data == i` の出力フレームの Y プレーンが入力フレーム i の内容と一致する（対応付けの正しさの直接検証）。非可逆圧縮のため完全一致はしないので、既存の `psnr_y` ヘルパーで PSNR を計算し、入力フレーム i との PSNR が閾値（既存テストと同じ 25.0 dB を目安）以上であること、かつ入力フレーム j (j ≠ i) との PSNR より十分高いことを確認する。**注意**: 既存の `generate_dummy_nv12` は `(x + y + frame_index * 7) % 256` で画素値を生成するため、隣接フレーム間の PSNR は mod-256 のラップアラウンドにより約 15.8 dB となる（全画素の差が 7 と仮定した約 31 dB ではない。数値は 320x240 での計算値であり、解像度が異なると変わり得る）。したがって誤対応フレームとの PSNR は 25.0 dB を下回り、単独閾値でも誤対応は検出可能だが、「正しいフレームとの PSNR が誤対応フレームとの PSNR より十分高い」ことを主判定にする（対応付けの正しさを直接検証するため）。
   - 検証 3 が回帰検出能力を持つこと（修正前の FIFO 方式では失敗すること）を、新テストヘルパーと修正前の `decode.rs` のロジックの組合せで実装時に確認する。
   - Encoder 出力に B ピクチャが含まれることを確認する（`EncodedFrame::picture_type()` が `PictureType::B` のフレームが 1 つ以上あること。B フレームが実際に生成されたことの検証）。

4. **テストのコーデック範囲**: H.264 と H.265 の B フレーム有りテストは必須。VP9 / AV1 は B フレーム概念がなく新テストの対象外だが、HashMap 方式を適用したコーデック（伝搬確認済みの全コーデック）で既存の VP9 / AV1 ラウンドトリップテストが引き続き pass することを確認する（伝搬が確認できず FIFO 方式を維持するコーデックは従来の挙動のまま）。

#### `skills/shiguredo-vpl/SKILL.md`

- Decoder の user_data 対応付けに関する「FIFO キュー」「VecDeque」「キュー」の全記述を `TimeStamp` 一致方式に更新する。
- `DecodedFrame<'_, T>` のライフタイム説明は借用モデルを維持するため変更不要。
- 「decode 呼び出しごとに user_data を 1 つ供給する」の項目は、供給過多の残余の扱いが「finish 後の Drop までキューに残る」から「finish() の残留チェックでエラーになる」に変わることを正確に記述する。
- 「ビットストリームがフレーム境界をまたいでも順序は保たれる」の記述を、分割入力時の user_data 対応付けが保証されない旨と、分割入力で残留エントリが発生して `finish()` が残留チェックエラーを返す挙動変化の旨に更新する（リスク B 参照）。

## 完了条件

以下すべてを満たす。

1. 実装着手前に、H.264 / H.265 / VP9（可能なら AV1）で `bs.TimeStamp` → `surface.Data.TimeStamp` の伝搬が成立することを実機で確認する。確認手順はリスク A に従う（SyncOperation 完了後の読み取り、遅延出力フレームとドレイン期出力フレームの TimeStamp の確認を含む）。伝搬が確認できたコーデックにのみ HashMap 方式を適用する。確認できなかったコーデックがある場合は、該当コーデックでは FIFO 方式を維持する分岐を実装する（リスク C 参照）。
2. B フレームを含むビットストリームで Decoder に `decode(bs, user_data)` を N 回呼んだあと、コールバックで受け取る `DecodedFrame::user_data()` が入力時の `user_data` と正しく対応付いている。
3. 上記を検証する統合テストが `tests/test_roundtrip.rs` に追加され、`gop_ref_dist = 3`（2 個の B フレーム）で N >= 15 フレームのラウンドトリップで以下を検証して pass する:
   - 全 `DecodedFrame::user_data()` の集合が入力時の値と過不足なく一致する
   - 各 `user_data` 値がちょうど 1 回出現する（重複なし）
   - `user_data == i` の出力フレームの Y プレーンが入力フレーム i の内容と一致する（PSNR 閾値方式。テスト項目 3 参照）
   - B ピクチャが実際に生成されていること
4. `skills/shiguredo-vpl/SKILL.md` 内の全「FIFO キュー」関連記述が `TimeStamp` 一致方式に更新されている（供給過多の残余の扱いと分割入力の扱いの挙動変化を含む）。
5. `CHANGES.md` の `## develop` に `[FIX]` として本修正を追記する。
6. `finish()` がエラーなく完了し、ドレインフレームが `Sync` 経由で正しく処理されること（TimeStamp 引き当て成功時は通知、失敗時は drain 扱いで破棄）。
7. 既存の VP9 / AV1 ラウンドトリップテストが引き続き pass することを確認する。

## 影響範囲

- `src/decode.rs`: `Decoder` 構造体 (`frame_count` 追加)、`decode()` (`bs.TimeStamp` 設定、`QueueFrame` への `frame_seq` 追加)、`WorkerCommand` (`QueueFrame` の変更)、`read_surface_timestamp` (新設)、`sync_and_callback` (シグネチャ変更と TimeStamp 引き当て)、`sync_and_drain` (削除)、`run_sync_worker` (`VecDeque` → `HashMap`、残留チェック)、`finish()` (ドレインの Sync 維持)、`Stop` / `WaitIdle` アーム、doc comment 群、import 追加・削除
- `tests/test_roundtrip.rs`: `DecodedFrameInfo` 構造体 (`user_data` フィールド追加)、`decode()` ヘルパー (`user_data` 保持)、B フレーム用 Encoder user_data 転送ヘルパー（新設）、B フレーム込み統合テスト追加
- `skills/shiguredo-vpl/SKILL.md`（全 FIFO 関連記述を TimeStamp 一致に更新）
- `CHANGES.md`

### 後方互換性

- 公開 API のシグネチャ (`Decoder::decode()`, `DecodedFrame::user_data()`) は変更なし
- `DecodedFrame<'a, T>` の型パラメータ・ライフタイムは変更なし（借用モデルを維持）
- `enumerate()` で user_data を渡している既存利用者コードは、B フレームがなければ修正前後で挙動が変わらない
- B フレーム有りの利用者にとっては **バグ修正** であり、破壊的変更ではない
- 注意: 入力フレームが出力されない異常系では残留エントリの扱いが変わる。FIFO 方式では Drop 時に `MFX_ERR_ABORTED` 通知のみだったが、TimeStamp 方式では `finish()` の `WaitIdle` 残留チェックでエラーになる（残留自体が「出力されないフレームが存在する」異常系の検出であり、1 フレーム = 1 `decode()` 呼び出しの正常利用では発生しない）。フレーム境界をまたぐ分割入力を利用している場合は、分割入力で残留が発生し `finish()` がエラーを返す挙動変化がある（リスク B 参照）。

## 依存 issue

- **issue 0013** (`0013-bug-encoder-frame-seq-zero-timestamp-collision`): 当初は「Encoder 側の `frame_count` 初期値を 1 に変更する修正」として本 issue の初期値を 1 と整合させる前提だったが、**0013 は closed で `frame_count` 1 スタート化は不採用**（`src/encode.rs` の `Encoder::new` は `frame_count: 0` のまま）。本 issue の `frame_count` 初期値は 0 とし、0013 に依存しない（変更概要 1 参照）。
- **issue 0010** (`0010-bug-drop-deadlock-on-sync-operation-infinite`): デバイスエラー伝搬の検証と二重通知の扱いの確定を行う。0010 は「`SyncData` への `frame_seq` 追加と Sync エラー時の `take_by_frame_seq` による pending 消費」を**廃案**にし、Encoder 側も二重通知を許容する方針に確定した（0010 の設計方針 2）。本 issue の Decoder 側方針（Sync 失敗時は pending を消費せず二重通知を許容）は 0010 の最終方針と同じであり、対比の必要はなくなった。`sync_and_drain` / `sync_and_callback` の `MFX_INFINITE` は変更しない（有限タイムアウト化は廃案）。適用順序は 0010 → 0008（0010 はプロダクションコード変更なしで closed 済みのため、実質の依存は情報参照のみ）。
- **issue 0009** (`0009-bug-decoder-device-busy-infinite-retry`): `decode_bitstream` / `finish` の DEVICE_BUSY 無限再試行を上限付きにする修正。当初は「`decode_bitstream` がエラーを返した場合、送信済み `QueueFrame` の `frame_seq` はインクリメントされないため、次回 `decode()` で同一 `frame_seq` となり重複キーエラーになる。0009 を先に適用するか、エラー後の `frame_count` の扱いを 0009 の実装と整合させること」としていた。0009（リトライ上限追加）は適用済みで、エラー後の `frame_count` の整合は本 issue の変更概要 2 の「デコード実行前インクリメント」順序で満たす（0009 は `frame_count` 自体には触れない）。
- **issue 0014** (`0014-bug-frame-surface-drop-silently-swallows-errors`): `FrameSurface::Drop` のエラー処理を stderr 出力に変更する issue。`sync_and_drain` は変更対象外（0008 の削除に委ねる）。**0014 はログライブラリ（tracing 等）未導入のため `issues/pending/` に移動済み（pending）**。0008 は 0014 の変更（`src/vpl.rs` の Drop とヘルパー追加）と競合しないため、0014 の適用を待たずに実装する。0014 が将来実装された際は、0008 適用後の `FrameSurface::Drop`（Unmap / Release）の失敗が観測可能になる。
