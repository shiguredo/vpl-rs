# Encoder::reconfigure が pending frame を drain しないため finish() で残留エラーが顕在化する

- Priority: High
- Created: 2026-07-01
- Model: Opus 4.7
- Branch: feature/fix-encoder-reconfigure-does-not-drain-pending
- Polished: 2026-08-21

## 目的

`Encoder::reconfigure` が呼ぶ `MFXVideoENCODE_Reset` は、パラメータ差分により VPL 内部の未出力フレームを破棄する場合がある（新シーケンス開始時）。しかし現在の実装は worker の `pending_store` を drain せず、Reset 後に pending が残留して `finish()` で「pending frames remained」エラーが顕在化する。ユーザーは reconfigure との因果関係が分からないままエラーに遭遇する。

なお、本 issue の前提「Reset が VPL 内部の未出力フレームを破棄する」は一次資料からの推論であり実機未確認である（「設計判断の根拠」参照）。実機での確認は完了条件 4 のテストで間接的に観測する（前提の真偽を pass/fail で確定する構造ではなく、観測結果に応じて完了条件の検証条件を調整する）。

## 優先度根拠

High。以下による。

- **`finish()` エラーの表出**: Reset で破棄されたフレームに対応する Sync が来ないまま pending_store に残り、`finish()` で drain されて「pending frames remained」の一括エラー通知になる。ユーザーには Reset との因果が分からない。
- **`Encoder::reconfigure` の doc に副作用の説明がない**: 現在の doc は「エンコーダパラメータを動的に変更する。`MFXVideoENCODE_Reset` を呼び出す。ビットレートやフレームレートの変更に使用する。」としか書いておらず、Reset の副作用（VPL 内部フレーム破棄）が説明されていない。
- **frame_seq の単調増加により誤対応付けは発生しないが、残留自体が異常**: frame_seq は u64 で `checked_add(1)` によりラップアラウンドしない（オーバーフロー時は Err）。Reset 後の Sync と誤って対応付くことはない。しかし pending_store に残るエントリ自体が設計上の欠陥であり、reconfigure の意味論として破綻している。

## 現状

### reconfigure の実装

`src/encode.rs` の `Encoder::reconfigure`:

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

`Encoder::encode` は `WorkerCommand::QueueFrame { frame_seq, pending_frame }` を Worker に送信し、worker の `run_sync_worker` の `QueueFrame` アームが `pending_store.insert(frame_seq, pending_frame)` で登録する。Sync 到着時に `sync_and_build_frame` が `take_by_frame_seq` で回収する。

Reset が発生すると、VPL は入力済みだが未出力のフレームを破棄する場合がある（新シーケンス開始時。詳細は「設計判断の根拠」参照）。しかし pending_store には frame_seq が残り、対応する Sync がもう来ないので、`finish()` の `WaitIdle` アームの `pending_store.drain_all()` でまとめてエラー通知される。

### エラーメッセージの例

`src/encode.rs` の `finish_pending_error`:

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

### frame_seq 衝突のリスク

frame_seq は Encoder のライフタイムを通じて単調増加する（`self.frame_count.checked_add(1)`）ので、Reset 後に新規入力しても既存の frame_seq とは衝突しない。`checked_add(1)` がオーバーフロー時に `Err` を返すためラップアラウンドは発生しない。

## 設計方針

**Reset の直前に pending_store を drain する。**

制御フロー:

1. パラメータバリデーション（`framerate_den` チェック）
2. `self.video_param` に `ReconfigureParams` を適用する（ExtParam のセットは行わない）
3. `WorkerCommand::DrainPending` を送信し、worker の drain 完了を `rx.recv()` で待つ
4. 0011 適用後: `video_param.ExtParam` に `ext_bufs` をセットする（**Reset 直前のこのタイミングで行う**。DrainPending 送信・受信の `?` による早期リターンで ExtParam がセットされたままにならないようにするため）
5. `MFXVideoENCODE_Reset` を呼ぶ
6. Reset の成否にかかわらず、関数の最後に `ExtParam = null` / `NumExtParam = 0` に戻す（0011 の不変条件の維持）

```rust
pub fn reconfigure(&mut self, params: ReconfigureParams) -> Result<(), Error> {
    if let Some(0) = params.framerate_den {
        return Err(Error::new_custom(
            "Encoder::reconfigure",
            "framerate_den must be non-zero",
        ));
    }

    // 現在の video_param をベースに変更を適用する（ExtParam のセットはしない）
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

    // 0011 適用後: ここ（Reset 直前）で ExtParam をセットする
    // ... video_param.ExtParam = self.ext_bufs.as_mut_ptr(); video_param.NumExtParam = ... ...

    // Reset の成否にかかわらず、関数の最後で ExtParam = null / NumExtParam = 0 に戻す（0011 の完了条件 3 参照）
    self.session
        .lib()
        .mfx_video_encode_reset(self.session.as_ptr(), &mut self.video_param)
    // ... ExtParam クリア（0011 の完了条件 3 参照）...
}
```

### WorkerCommand::DrainPending の追加

`WorkerCommand` enum に `DrainPending(mpsc::Sender<()>)` バリアントを新設する。Worker 側では:

- `pending_store.drain_all()` で全エントリを取り出す
- 各エントリに対して `handler.on_encoded(Err(reconfigure_canceled_error()))` を呼び出す（**`canceled_error()` は「Encoder::drop」コンテキストのため使用しない**。`"Encoder::reconfigure"` コンテキストの新エラー関数を用意する。理由は本 issue の目的が「ユーザーは reconfigure との因果関係が分からないままエラーに遭遇する」ことの解消であり、drop という誤コンテキストでは解消されないため）
- 全エントリ処理後に `reply_tx.send(())` で呼び出し元に完了を通知する

呼び出し元（`reconfigure`）は `rx.recv()` で完了を待ってから `Reset` を実行する。これにより pending_store の drain が Reset より先に完了することが保証される。

### ブロッキング挙動

本方式の `reconfigure` は `rx.recv()` で worker の drain 完了を待つ。worker は mpsc を FIFO で処理するため、`DrainPending` は先行する全 `QueueFrame` / `Sync` コマンドの処理後に到達する。つまり **reconfigure は「reconfigure 前に encode され syncp が発行されたフレームの Sync 完了と、drain 対象コールバックの完了」までブロックする**。`finish()` と異なる点は「EOS ドレインループ（null surface での全フレーム回収）」と「残留チェック（WaitIdle）」を行わないことである。

なお、0010 は SyncOperation を有限タイムアウト化しない（廃案）ため、このブロックの待機時間は 0010 の影響を受けない。

### Reset の挙動（シーケンス継続と新シーケンス開始）

VPL の `MFXVideoENCODE_Reset` の挙動はパラメータ差分に依存する。一次資料（`refs/oneVPL/doc/spec/source/programming_guide/VPL_prg_encoding.rst`（v2.17）の Configuration Change 節）は以下を規定している:

- 「パラメータ変更前後の差分によって、エンコーダは現在のシーケンスを継続するか、新しいシーケンスを開始する。新シーケンスを開始する場合、内部状態を完全にリセットし、IDR フレームで新しいシーケンスを開始する」
- 「アプリケーションは `mfxExtEncoderResetOption` 構造体を `mfxVideoParam` に付加することで、Reset 後に新しいシーケンスを開始するかどうかを制御できる」

本 issue の前提（drain + 破棄 + エラー通知）が機能するのは**新シーケンス開始時**である。シーケンス継続時は VPL 内部のフレームが保持される可能性があり（一次資料は継続時の内部フレーム保持を明記していないが、破棄は新シーケンス開始時のみ規定されている）、drain で削除・エラー通知済みの `frame_seq` の出力が reconfigure 後に遅延到着し、`sync_and_build_frame` の `take_by_frame_seq` が `None` になり `mismatched_timestamp_error` が発生し得る。この場合の設計判断は次のとおり:

- **`mfxExtEncoderResetOption` による新シーケンス開始の強制は採用しない**。Reset のたびに IDR が挿入され、帯域ジャンプと再構成遅延を伴う副作用があるため、動的パラメータ変更（ビットレート変更等）の用途に不相応。
- **シーケンス継続時の遅延到着による `mismatched_timestamp_error` は仕様として許容する**。エラー通知として表面化する（silent なデータ破損ではない）。
- なお、シーケンス継続時は「drain しない現状の実装でも、遅延到着する Sync が残留 pending を `take_by_frame_seq` で正常回収するため正常動作する」点に注意が必要である。つまり drain の導入は「新シーケンス開始時の `finish()` エラー」を修正する代わりに、「修正前は正常だったシーケンス継続時の動作」を「drain のエラー通知 + `mismatched` 追加エラー」へ劣化させるリスクを内包する。このため、実機確認（完了条件 4 のテスト）で「シーケンス継続時に drain 済み pending の遅延出力が発生するか」を観測し、発生が確認された場合は、シーケンス継続時に pending をエラー通知しない分岐（または drain 方式の再検討）を別 issue として対応する。

### 設計判断の根拠

**案 A（正規手順 / `finish()` のドレインで回収してから Reset）を採用しない理由**:

正規手順とは、VPL プログラミングガイド（一次資料: `refs/oneVPL/doc/spec/source/programming_guide/VPL_prg_encoding.rst` の Configuration Change 節）が規定する設定変更手順「NULL 入力で `EncodeFrameAsync` を呼び cached frames をすべて回収（`MFX_ERR_MORE_DATA` まで）→ `MFXVideoENCODE_Reset` → 成功すれば encode を継続」であり、フレームをロスなく回収できる。本 issue が drain（破棄 + エラー通知）方式を選ぶ理由は、この回収手順（および `finish()` のドレイン）では reconfigure が長時間ブロックし、しかも失敗時に Encoder が回復不能になるためである:

- **正規手順（全フレーム回収）**: すべての pending の Sync 完了 + コールバック完了を待つため、reconfigure が長時間ブロックする（推定数百 ms〜数秒。回収対象はルックアヘッド深さ分のフレームであり、正確な値は実機依存）。動的パラメータ変更として不相応な待機時間になる
- **`finish()` のドレイン経由**: `finish()` が Err を返した場合、EOS が部分的に送られた状態となり、Encoder が回復不能になる
- **`finish()` のドレイン経由（続き）**: `finish()` の成功後に Reset が失敗した場合も Encoder が動作不能になる（EOS 済み、Reset 失敗）が、このケースへの対処がない

つまり **フレームロスは「VPL 仕様で不可避」ではなく、本方式（破棄 + エラー通知）の設計上の選択である**。Reset 自体が VPL 内部のフレームを破棄するのは新シーケンス開始時のみと考えられる（一次資料 `VPL_prg_encoding.rst` は新シーケンス開始時に「completely resets internal state and begins a new sequence with the IDR frame」と規定するが、「未出力フレームの破棄」自体は明記しておらず、完全リセットからの推論である。実機確認は完了条件 4）。パラメータ差分によってはシーケンス継続となるため、シーケンス継続時の挙動は「Reset の挙動（シーケンス継続と新シーケンス開始）」の節を参照。

**本方式の特徴**:

- VPL に依存せず pending_store のみを操作するため、Reset 前後で Encoder の状態が破壊されない
- pending はエラー通知されるため、破棄されたフレームが呼び出し側に伝わる
- フレームロスを伴うが、これは本方式の設計上の選択（正規手順は回収だが長時間ブロックを伴う）

**Reset 失敗時の注意**: Reset が失敗した場合（`MFX_ERR_INCOMPATIBLE_VIDEO_PARAM` 等）、libvpl 仕様では「コンポーネントを閉じて再初期化する」ことが要求される場合がある。Reset 失敗時の Encoder の再利用可否はエラー種別に依存するため、失敗時は Drop して再作成することを前提とする（「再利用可能」とは断言しない）。

### 二重通知の可能性

先行する `Sync` アームで `sync_and_collect` が `Err` を返した場合（bitstream 範囲検証エラー等）、0010 の最終方針では「`SyncData` への `frame_seq` 追加と Sync エラー時の `take_by_frame_seq` による pending 消費は行わない」（0010 の設計方針 2）ため、`sync_and_build_frame` はそのまま `Err` を返し、対応する pending frame は消費されず残留する。この残留エントリは `DrainPending`（または `Stop`）で再通知され得る。すなわち **Sync エラー時は同一フレームが 2 回通知される二重通知が発生し得るが、これは 0010 の許容方針（非同期モデルの制約として二重通知を許容）に従い、本 issue でも許容する**（0010 の設計方針 2、および 0010 の「依存 issue」セクションの 0012 の項目参照）。`DrainPending` で通知されるのは Reset により VPL 内部で破棄されたフレームの pending と、Sync エラー時に消費されず残留した pending である（0010 の許容方針のため、二重通知を防ぐための追加対応は行わない）。

### reconfigure の doc 更新

以下の内容を reconfigure の doc に追記する（完了条件 3 に対応）:

- `MFXVideoENCODE_Reset` が VPL 内部の未出力フレームを破棄すること
- 破棄されたフレームがある場合、エラー（`MFX_ERR_ABORTED`）が通知される。**どのフレームが破棄されたか（user_data）は通知されない**（`on_encoded(Err(...))` は user_data を運ばないため）
- frame_seq は reconfigure 後も継続しリセットされないこと
- reconfigure が先行フレームの Sync 完了までブロックすること（「ブロッキング挙動」の節）
- シーケンス継続時でも pending が残っていればエラー通知されること（「Reset の挙動」の節）

## 完了条件

以下すべてを満たす。

1. `WorkerCommand` enum に `DrainPending` バリアントを追加し、Worker 側で `pending_store.drain_all()` + 全エントリのエラー通知（`"Encoder::reconfigure"` コンテキストの `MFX_ERR_ABORTED` エラー。`canceled_error()` は使わない）を行う。
2. `Encoder::reconfigure` が Reset 直前に `WorkerCommand::DrainPending` を送信し、worker の drain 完了を待ってから `MFXVideoENCODE_Reset` を呼ぶよう修正する。`video_param.ExtParam` のセットは **DrainPending 完了後（Reset 直前）に限定し**、クリアは関数の最後（エラー経路を含む）で行う（0011 の不変条件を維持する。DrainPending 送信の `?` による早期リターンで ExtParam がセットされたままにならないようにする）。
3. reconfigure の doc に、Reset の副作用（VPL 内部フレーム破棄、エラー通知、frame_seq 継続。user_data は通知されないこと）、reconfigure が先行フレームの Sync 完了までブロックすること、シーケンス継続時でも pending が残っていればエラー通知されることを明記する。
4. reconfigure の前後で encode したフレームを検証するテストを `tests/test_roundtrip.rs` に追加する。テストは encode → reconfigure → encode → finish のシーケンスで検証し、以下を確認する:
   - Ok 通知で返る user_data の集合が **reconfigure 後に encode したフレームの user_data をすべて含む**（重複なし）。reconfigure 前に encode したフレームの user_data は、Sync 完了済みなら Ok 集合に含まれ、drain されたなら含まれない（Err 通知は user_data を運ばないため「0 回通知」となる。「全 user_data がちょうど 1 回通知される」は検証できない命題であり、検証対象は Ok 通知の user_data 集合 + 通知総数に限定する）
   - 通知総数が encode 総数（reconfigure 前後の合計）以上であること（Sync エラー時は二重通知が発生し得るため、0010 の許容方針により総数は encode 総数と一致するとは限らない。一致するのは「二重通知が発生しない」場合のみであり、**「一致すること」を無条件 assert にしない**。「以上」を下限として検証する）。シーケンス継続時に遅延出力（`mismatched_timestamp_error`）が発生した場合はさらに通知が増えるため、この検証は「少なくとも encode 総数分の通知がある」ことの確認に留める
   - `finish()` が `Ok` を返すこと（drain 未実装かつ reconfigure 時点で pending が存在する場合、残留 pending は `finish()` の `WaitIdle` で `finish_pending_error` 通知 + `Err` になるため、`finish()` の Ok assert により drain 漏れを確実に検出できる）
   - reconfigure 後に encode したフレームはすべて正常通知（Ok）
   - エラー通知が発生した場合のみ、その内容を検証する。**検証対象は reconfigure-canceled エラー（`function() == "Encoder::reconfigure"`、`status_code() == MFX_ERR_ABORTED`）に限定し**、それ以外のエラー種別（Sync エラー時の残留通知、シーケンス継続時の `mismatched_timestamp_error`（`"Encoder::sync_worker"` コンテキスト・非 ABORTED））は観測対象として区別する（エラー通知数が 0 でも pass する条件付き検証。エラー通知数 0 のケースを pass させることで「drain 未実装でも pass する」silent pass 構造を残すため、エラー発生時にのみ内容を検証する）
   - **0011 からの依頼として、reconfigure 後も LookAheadDepth / QVBRQuality が保持されることの検証を含める**（実機確認は 0011 の完了条件 6、テストは本 issue の reconfigure テストに含める。検証に公開 API の拡張（`Encoder::get_video_param()` に `ExtParam` を渡す手段）が必要になる場合は、**issue 0027（`0027-add-get-video-param-ext-param`）が対応する**。0027 は本 issue のテストでの利用方法に合わせて設計を確定するとしているため、0012 と 0027 を相互に参照して実装する）
   - 注: エラー通知数はタイミング依存のため、`gop_ref_dist = Some(3)` 等の MORE_DATA が発生する設定で pending 残存を作り出すこと。またシーケンス継続時の遅延出力（`mismatched_timestamp_error`）が発生するかもこのテストで観測し、「Reset の挙動」の節に従って対処する。観測結果として遅延出力が確認された場合は、本完了条件の検証条件を「遅延出力分の通知を許容する形」に調整する（本 issue の設計方針の「シーケンス継続時」の節に従う）。
5. `run_sync_worker` の `DrainPending` アームの単体テストを `src/encode.rs` 内の `#[cfg(test)]` モジュールに追加する（既存の `worker_wait_idle_returns_error_when_pending_remains` と同型の GPU 不要テスト）。エラー関数のコンテキストが `"Encoder::reconfigure"` であること（`error.function() == "Encoder::reconfigure"`）も assert する（`canceled_error()` の「Encoder::drop」コンテキスト混入の回帰を防ぐため。`status_code()` だけでは `MFX_ERR_ABORTED` が同一のため検出できない）。
6. `skills/shiguredo-vpl/SKILL.md` の「動的再構成 (reconfigure)」節に、Reset の副作用（VPL 内部フレーム破棄、エラー通知、frame_seq 継続）を追記する。**0010 が closed で残余として残した「デバイスエラー時に同一フレームが 2 回通知され得る（二重通知を許容）」の明文化（0010 の完了条件 2）は、本 issue では扱わない（スコープ外）**。0010 の残余は、0010 の解決方法のとおり「必要になった場合は別 issue として切り出す」方針に従う。
7. `CHANGES.md` の `## develop` に `[FIX]` として追記する。

## 影響範囲

- `src/encode.rs`（`WorkerCommand` enum に `DrainPending` 追加、`run_sync_worker` の `DrainPending` アーム追加、`Encoder::reconfigure` に `DrainPending` 送信追加、reconfigure 用のエラー関数新設、reconfigure の doc 更新、`#[cfg(test)]` モジュールに単体テスト追加）
- `tests/test_roundtrip.rs`（reconfigure 前後の user_data 検証テスト追加、実 GPU 依存。LookAheadDepth / QVBRQuality 保持の検証には issue 0027 の公開 API 拡張を利用する）
- `skills/shiguredo-vpl/SKILL.md`（動的再構成節に Reset の副作用を追記）
- `CHANGES.md`

## 参考

- 関連 issue: 0011（reconfigure が ExtParam を送らない別問題。**適用順序は 0011 を先に適用し、その差分の上に本 issue の変更を重ねる**。0011 の完了条件 6 からの検証依頼（LookAheadDepth / QVBRQuality 保持）を完了条件 4 に反映済み）
- 関連 issue: 0010（デバイスエラー伝搬の検証と二重通知の扱いの確定。`SyncData` への `frame_seq` 追加・Sync エラー時の `take_by_frame_seq` による pending 消費・`stopping` フラグ・500 ms タイムアウトはすべて廃案。二重通知（Sync エラー通知 + Drop 時の `MFX_ERR_ABORTED`）は非同期モデルの制約として許容する方針。0010 はプロダクションコード変更なしで closed 済みのため、本 issue は 0010 の調査結果・方針を参照するのみで適用差分はない）
- 関連 issue: 0020（encode.rs のサブモジュール分割。`WorkerCommand` / `run_sync_worker` を移動するため、0020 は本 issue 適用後に調整する）
- 関連 issue: 0027（`Encoder::get_video_param()` に `ExtParam` を渡す手段の追加。本 issue の reconfigure テストでの LookAheadDepth / QVBRQuality 保持検証に利用する。0027 は本 issue のテスト実装での利用方法に合わせて設計を確定するため、相互に参照して実装する）
- `finish_pending_error` / `mismatched_timestamp_error` / `canceled_error` は `src/encode.rs` の worker 関連のエラー関数
