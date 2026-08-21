# Decoder の DEVICE_BUSY / MORE_SURFACE リトライに上限がなくデバイス異常時にプロセスが固まる

- Priority: High
- Created: 2026-07-01
- Model: Opus 4.7
- Branch: feature/fix-decoder-device-busy-infinite-retry
- Polished: 2026-08-21

## 目的

`Decoder::decode` / `Decoder::finish` の内部ループが `MFX_WRN_DEVICE_BUSY` / `MFX_ERR_MORE_SURFACE` を返し続けた場合に **無限ループ / スピンループになるバグを修正する**。Encoder 側は `DEVICE_BUSY_MAX_RETRIES = 30` で上限を掛けているのに Decoder 側は上限なしのままになっており、デバイスが混雑し続ける異常状態で Decoder を触ったプロセスがそのまま固まる。本 issue が対象とするのは「デバイスは応答するが混雑し続け DEVICE_BUSY / MORE_SURFACE が返り続ける」ケースであり、真の GPU ハング（ドライバがタスク結果をエラーにせず SyncOperation が制御を返さない）は対象外（優先度根拠・完了条件 5 参照）。

## 優先度根拠

High。以下による。

- **プロセスが固まる**: デバイスが異常状態になると `Decoder::decode` を呼んだスレッドの呼び出しが戻らず、上位から検知できない。パニックも Error も返らない。
- **Encoder との実装非対称**: 同一ライブラリ内で Encoder は上限リトライ、Decoder は無限リトライ。同じ設計原則を採用しない理由がない。
- **`MORE_SURFACE` の CPU 100% スピン**: `sleep` すら入っていないため、`MORE_SURFACE` が返り続けると CPU 100% でスピンする。
- **本番運用でのシャットダウン阻害**: `Decoder::finish` は「コールバックが呼び出され終わるまでブロックする」契約のため、無限ループ内から抜けられず、シャットダウン処理に到達できない。なお真の GPU ハング（ドライバがタスク結果をエラーにせず SyncOperation が制御を返さない）時の Drop デッドロックは仕様上達成不能であり、0010 の調査でライブラリ側では解決できないことが確定している（0010 の「スコープ外の明記」参照）。本 issue でも対応しない。DEVICE_BUSY / MORE_SURFACE はデバイスが混雑している状態でありハングとは区別されるため、この状態では Worker の SyncOperation は完了（またはエラー）し、Drop の join は返る。

## 現状

### DEVICE_BUSY の無限リトライ（decode_bitstream）

`src/decode.rs` の `decode_bitstream` 内:

```rust
// DEVICE_BUSY: デバイスが混雑している。1ms 待って再試行
if status == sys::mfxStatus_MFX_WRN_DEVICE_BUSY {
    std::thread::sleep(std::time::Duration::from_millis(1));
    continue;
}
```

`while bs.DataLength > 0` ループの中で無限に `continue` する。リトライ上限なし。

### DEVICE_BUSY の無限リトライ（finish のドレイン）

`src/decode.rs` の `finish` の drain ループ:

```rust
if status == sys::mfxStatus_MFX_WRN_DEVICE_BUSY {
    std::thread::sleep(std::time::Duration::from_millis(1));
    continue;
}
```

`finish()` の drain ループも同じく上限なし。`Decoder::finish` は「コールバックが呼び出され終わるまでブロックする」契約なので、BUSY が返り続けると `finish` 呼び出しが永久にブロックする。

### MORE_SURFACE の CPU 100% スピン

`src/decode.rs` の `decode_bitstream` 内:

```rust
// MORE_SURFACE: 内部割り当てでは通常発生しないが、安全のため再試行
if status == sys::mfxStatus_MFX_ERR_MORE_SURFACE {
    continue;
}
```

`sleep` すら入っていないため、`MORE_SURFACE` が返り続けると CPU 100% でスピンする。libvpl 仕様（`mfxVideoParam.mfx` 内のデコードオプション構造体 `mfxInfoMFX` の `FilmGrain` 説明）では、AV1 デコードで `FilmGrain != 0` のとき「各フレームのデコードに 2 枚の出力サーフェスが必要になり、出力サーフェスが不足すると `MFXVideoDECODE_DecodeFrameAsync` が `MFX_ERR_MORE_SURFACE` を返す」と明記されている。ただし `surface_work=NULL`（VPL 内部割り当て）での発生有無は一次資料に明記がなく確認できていない。

なお `finish` の drain ループは MORE_SURFACE を明示チェックせず `status < 0` の分岐で捕捉しており、**現状は即エラー**になっている（無限スピンはしない）。本修正で MORE_SURFACE に上限付きリトライを入れることは、`finish` 側では「即エラー → 30 回リトライ後のエラー」への挙動変更を伴う。

### Encoder 側との対比

`src/encode.rs` に定数 `DEVICE_BUSY_MAX_RETRIES`、`encode_frame_async` で明示的にリトライ回数を制限:

```rust
const DEVICE_BUSY_MAX_RETRIES: u32 = 30;

for _ in 0..DEVICE_BUSY_MAX_RETRIES {
    let status = self.session.lib().mfx_video_encode_frame_async(...);
    if status == sys::mfxStatus_MFX_WRN_DEVICE_BUSY {
        std::thread::sleep(std::time::Duration::from_millis(1));
        continue;
    }
    // ...
}
Err(Error::new_custom(
    "MFXVideoENCODE_EncodeFrameAsync",
    "device busy after max retries",
))
```

Decoder は同じ設計を採用すべき。なお Encoder 側の `encode_frame_async` は `MORE_DATA` を `Ok(None)` で分離し、それ以外の全ステータス（`MORE_SURFACE` を含む）を `status != MFX_ERR_NONE` で一括エラーにしている。本 issue の MORE_SURFACE の扱い（Decoder は上限付きリトライ、Encoder は即エラーのまま）を検討する際の参考になる。

### 過去の設計判断（closed issue 0005 の記録）

closed issue 0005（非同期コールバック化）は「`decode_bitstream()` では `DEVICE_BUSY` 時に 1ms スリープして再試行する。エンコーダと異なりリトライ回数の上限は設けていない（`while bs.DataLength > 0` ループが自然に終了するため）」と記録している。この判断の根拠（「ループが自然に終了する」）は GPU が正常に動作している間は成立するが、GPU 異常時（ハング等）には `DEVICE_BUSY` / `MORE_SURFACE` が返り続けて成立しない。本 issue はこの過去判断を覆す。

また 0005 は「`decode_bitstream()` がエラーを返した場合は `pending_values.clear()` で状態をリセットしている」と記録しているが、現行コードにはこの処理が存在しない（後続のリファクタで失われた）。そのため現行実装では、エラー時に送信済み `QueueFrame` が Worker の `pending_values` に残留する（「修正後のエラー復帰に関する注意」参照）。

### SKILL.md の記述との齟齬

`skills/shiguredo-vpl/SKILL.md` のエンコーダ節には「`MFX_ERR_MORE_DATA` と `MFX_WRN_DEVICE_BUSY` は内部で吸収される。`DEVICE_BUSY` は 1ms スリープで最大 30 回までリトライ (旧 10 回から拡張)。30 回を超えると致命的エラー。」と説明している。一方で **Decoder 側の DEVICE_BUSY / MORE_SURFACE の挙動は SKILL.md に一切記載されておらず**、Decoder の実装（上限なし）が規範文書に現れない状態になっている。

## 設計方針

- **DEVICE_BUSY**: 以下 2 箇所に Encoder と同じ上限リトライを導入する。上限は `30 回`、リトライ間隔は 1ms。定数は `src/decode.rs` 内で `const DEVICE_BUSY_MAX_RETRIES: u32 = 30;` を宣言する。Encoder 側の同名定数と同じ値だが、モジュール間の結合度を下げるために意図的に複製する（Encoder / Decoder は今後も独立に進化するため）。
  - `decode_bitstream` の DEVICE_BUSY 分岐
  - `finish` の drain ループの DEVICE_BUSY 分岐
- **MORE_SURFACE**: `surface_work=NULL`（VPL 内部割り当て）では発生しない可能性が高いが、libvpl 仕様では AV1 の `FilmGrain != 0` 時にサーフェス不足として返る可能性が明記されており、過渡的なサーフェス枯渇は Worker の SyncOperation 完了 → `Release` で解消し得る。即エラーにすると正常にデコードできていたストリームを壊しかねないため、DEVICE_BUSY と同じ上限付きリトライ（1ms sleep、30 回）にする。永続的に返り続ける場合のみ上限超過エラーになる。
  - リトライカウンタは「連続して BUSY / MORE_SURFACE が返った回数」を数える（`decode()` 呼び出し全体の累積ではない。Encoder の `encode_frame_async` と同じ API 呼び出し単位の意味論であり、ヘルパー内のローカルカウンタとして実装する）。BUSY / MORE_SURFACE 以外のステータスが返った場合は即返却する（カウンタは API 呼び出し単位で新規作成されるため、リセットという状態遷移は発生しない）。BUSY と MORE_SURFACE は同じ 1 つのカウンタを共有し、上限超過時のエラーメッセージは最後に返ったステータスで分岐する。
- **上限超過時**: 上限超過の原因に応じてエラーメッセージを分ける。`MFX_WRN_DEVICE_BUSY` による上限超過は `Error::new_custom("MFXVideoDECODE_DecodeFrameAsync", "device busy after max retries")`、`MFX_ERR_MORE_SURFACE` による上限超過は `Error::new_custom("MFXVideoDECODE_DecodeFrameAsync", "more surface after max retries")` を返す（Encoder 側のメッセージ形式に合わせる）。
- **`finish` の drain ループ**: `decode_bitstream` と同じく DEVICE_BUSY / MORE_SURFACE の上限付きリトライを入れる（両方明示で一貫させる）。あわせて、`finish` の drain ループ内で `FrameSurface::new` が `syncp.is_null()` チェックより先に呼ばれている呼び出し順序を `decode_bitstream` と揃えて統一する（`decode_bitstream` は `syncp` 非 null 確認後に `FrameSurface::new` を呼ぶのに対し、`finish` は先に `FrameSurface::new` を呼ぶ。`FrameSurface::new` は surface が null だと「surface pointer is null」エラーを返すため、`MFX_ERR_NONE` 以外で `surface_out` が null になり得るケース（正の警告 `MFX_WRN_VIDEO_PARAM_CHANGED` 等が `status < 0` 分岐を素通りした場合等）に spurious エラーとなる余地がある。ステータス処理の一貫化の一環として本 issue で行う。完了条件 6 参照）。
- **リトライ処理の実装**: DEVICE_BUSY / MORE_SURFACE の両方を同じリトライループで処理する（1ms sleep は両方に共通）。リトライロジックは 1 回の `DecodeFrameAsync` 呼び出しをラップするヘルパー関数（例: `call_decode_frame_async_with_retry`）に集約し、`decode_bitstream` と `finish` の両方から使用する（リトライ上限・sleep 時間・エラーメッセージの二重管理を避けるため）。
  - ヘルパーの戻り値設計: 呼び出し元ごとに解釈が異なるため、返り値で「ステータス種別」を判定可能にする。具体的には `MFX_ERR_MORE_DATA`（入力不足 / ドレイン完了）と「`MFX_ERR_NONE` かつ `syncp == null`」（出力なし）の両方を `Ok` で表現しつつ区別できる形にする（Encoder の `encode_frame_async` が `MORE_DATA` を `Ok(None)` で分離している方式を参考に、MORE_DATA と syncp null の区別が失われない設計にする）。詳細な型（enum か `Ok(Option<...>)` か）は実装時に決定してよいが、両呼び出し元の分岐（`decode_bitstream` は MORE_DATA で `Ok(())`、`finish` は MORE_DATA で break）が実現できる契約であること。DEVICE_BUSY / MORE_SURFACE 以外のステータス（正の警告 `MFX_WRN_VIDEO_PARAM_CHANGED` / `MFX_WRN_ALLOC_TIMEOUT_EXPIRED` 等）はリトライ対象外とし、現行どおり `syncp` 非 null チェックへ流す（判定の純粋関数がこれらのステータスをどう分類するかを明示する）。

### 仕様由来の挙動の根拠資料コメント

MORE_SURFACE のリトライ化は libvpl 仕様（`mfxVideoParam.mfx` 内のデコードオプション構造体 `mfxInfoMFX` の `FilmGrain` 説明）由来の判断である。一次資料（`refs/oneVPL/api/vpl/mfxstructures.h` の `mfxInfoMFX::FilmGrain` 説明）の該当文面:

> If FilmGrain is nonzero decoding of each frame require two output surfaces (one for reconstructed frame and one for output frame with film grain applied). The decoder returns `MFX_ERR_MORE_SURFACE` from `MFXVideoDECODE_DecodeFrameAsync` if it has insufficient output surfaces to decode frame.

shiguredo-rust 規約に従い、置き換え後のコードコメントに根拠資料名・節番号（libvpl の `mfxstructures.h` の `mfxInfoMFX.FilmGrain` 説明）と「`surface_work=NULL`（内部割り当て）での発生有無は未確認であり、将来変更される可能性がある」ことを明記する。なお「過渡的なサーフェス枯渇は再試行で解消し得る」ことを支持する根拠として、`refs/oneVPL/api/vpl/mfxvideo.h` の `MFXVideoDECODE_DecodeFrameAsync` の返り値説明がある（サーフェス不足が過渡的であることを示すものであり、SyncOperation 完了 → Release による解消はライブラリの `FrameSurface` Drop 実装（`src/vpl.rs`）由来の推論）:
- `MFX_ERR_MORE_SURFACE` の説明「The function requires more frame surface at output before decoding can proceed.」（出力サーフェス不足を示す）
- `MFX_WRN_ALLOC_TIMEOUT_EXPIRED` の説明「Timeout expired for internal output frame allocation (if set with mfxExtAllocationHints and NULL passed as surface_work). Repeat the call in a few milliseconds or re-initialize decoder with higher surface limit.」（`mfxExtAllocationHints` 設定時かつ `surface_work=NULL` の内部割り当てで返り得る。本ライブラリは `Decoder::initialize` で `mfxExtAllocationHints` を設定しないため、ALLOC_TIMEOUT_EXPIRED も通常は返らない。この一次資料の条件と自実装を合わせると「内部割り当てでは MORE_SURFACE が発生する余地はさらに小さい」ことになり、設計方針の「発生しない可能性が高い」を補強する）

### 修正後のエラー復帰に関する注意

`Decoder::decode` は `QueueFrame` を Worker に送信した後で `decode_bitstream` を実行する。本修正により `decode_bitstream` が上限超過で `Err` を返した場合、以下の 2 段階の通知が発生する:

1. ループ途中で既に送信済みの `Sync`（デコード済みフレーム）は Worker で処理され、`on_decoded(Ok(...))` または `on_decoded(Err(...))` が通知される（BUSY / MORE_SURFACE が解消して SyncOperation が成功すれば `Ok`、デバイスエラーが返れば `Err`）。
2. 送信済みの `QueueFrame`（出力に至らなかった分）は Worker の `pending_values` に残留し、Drop 時の `Stop` で `MFX_ERR_ABORTED` として通知される。

呼び出し側は「`decode()` が `Err` を返した後も正常なコールバックが届き、その後 Drop で残留分が `ABORTED` 通知される」ことを前提とする。エラー後も Decoder を再利用することはサポート対象外とし、エラー後は Drop することを前提とする。この残留問題の扱いは依存 issue 0008（Decoder の user_data 対応付けを HashMap 方式に変更する issue）と関係するため、「依存 issue」セクションを参照。

## 完了条件

以下すべてを満たす。

1. `src/decode.rs` の `decode_bitstream` / `finish` で `MFX_WRN_DEVICE_BUSY` / `MFX_ERR_MORE_SURFACE` のリトライに上限 (30 回) が導入され、上限超過時は `Error::new_custom(...)` を返す。
2. `src/decode.rs` の `decode_bitstream` の `MFX_ERR_MORE_SURFACE` の `continue`（sleep なしスピン）を除去し、DEVICE_BUSY と同じ上限付きリトライに置き換える。
3. `src/decode.rs` の `finish` の drain ループに `MFX_ERR_MORE_SURFACE` の明示的な上限付きリトライを追加し、`decode_bitstream` と一貫したステータス処理にする。
4. `src/decode.rs` に `const DEVICE_BUSY_MAX_RETRIES: u32 = 30;` を宣言する。Encoder 側の同名定数と同じ値であること（値変更時は両方の意図を確認して同時に更新する）。この定数は DEVICE_BUSY と MORE_SURFACE の両方のリトライ上限に使用するため、宣言時にその旨をコメントで明記する。
5. `decode_bitstream` / `finish` のいずれかが上限超過エラーで抜けた後、`Decoder` を Drop した際に `stop_worker` が Worker スレッドを正常に join できること。DEVICE_BUSY / MORE_SURFACE はデバイスが混雑しているだけでタスクは実行可能な状態であり、上限超過エラー後の Drop では Worker が残りの Sync を完了（またはエラー）させた後に `Stop` を受けて終了するため、join は正常に返る。真の GPU ハング（SyncOperation が制御を返さない）時の join ブロックは仕様上達成不能であり検証対象外（0010 の「スコープ外の明記」参照）。検証方法はコードレビューで Drop 経路が返る構造であることを確認する（実機では DEVICE_BUSY を強制できないため）。
6. `src/decode.rs` の `finish` の drain ループ内で `FrameSurface::new` が `syncp.is_null()` チェックより先に呼ばれている呼び出し順序を、`decode_bitstream` と揃えて `syncp` 非 null チェック後に `FrameSurface::new` を呼ぶ形に統一する（ステータス処理の一貫化の一環）。
7. `skills/shiguredo-vpl/SKILL.md` のエンコーダ節の DEVICE_BUSY / MORE_SURFACE の説明に Decoder 側の挙動を追記する。**Encoder は MORE_SURFACE を即エラーにしている実態があるため**、「DEVICE_BUSY は Encoder / Decoder ともに最大 30 回リトライ」「MORE_SURFACE は Decoder のみ最大 30 回リトライ（Encoder は即エラー）」「上限超過時はエラー」と実態に合わせて明記する。
8. リトライ処理のコードコメントに根拠資料（libvpl の `mfxstructures.h` の `mfxInfoMFX.FilmGrain` 説明）と将来変更可能性（`surface_work=NULL` での発生有無は未確認）を明記する。
9. リトライヘルパーの「status → エラーメッセージ分岐」ロジックを、`src/decode.rs` 内の `#[cfg(test)]` モジュールの単体テストで検証する（実機での DEVICE_BUSY 強制はできないため、ヘルパーを「実際の `mfx_video_decode_frame_async` 呼び出し」と「status を受け取って判定する純粋関数」に分割し、後者をテストする。これによりモックやスタブを使わず（AGENTS.md 規約）status 分岐のロジックを検証できる）。テストでは「最後に返った status でエラーメッセージが分岐すること」（BUSY 上限超過 → "device busy after max retries"、MORE_SURFACE 上限超過 → "more surface after max retries"）と「DEVICE_BUSY / MORE_SURFACE 以外のステータスがリトライ対象外として分類されること」を検証する。
10. 既存のラウンドトリップテスト（`tests/test_roundtrip.rs` の全テスト）が引き続き pass することを確認する（ヘルパー化により `decode_bitstream` / `finish` の正常系ループに触れるため、回帰がないことを確認する）。
11. `CHANGES.md` の `## develop` に `[FIX]` として本修正を追記する。

## 影響範囲

- `src/decode.rs`（`decode_bitstream`: DEVICE_BUSY / MORE_SURFACE の上限付きリトライ、`finish`: DEVICE_BUSY / MORE_SURFACE の上限付きリトライ、`DEVICE_BUSY_MAX_RETRIES` 定数宣言、リトライ処理のコードコメント更新、リトライヘルパー（例: `call_decode_frame_async_with_retry`）の新設と「status → 判定」の純粋関数分割。`finish` の drain ループ内で `FrameSurface::new` が `syncp.is_null()` チェックより先に呼ばれている呼び出し順序も、`decode_bitstream` と揃えて `syncp` 非 null チェック後に `FrameSurface::new` を呼ぶ形に統一する。`#[cfg(test)]` モジュールの新設と status 分岐の単体テスト追加）
- `skills/shiguredo-vpl/SKILL.md`（エンコーダ節の DEVICE_BUSY / MORE_SURFACE 説明に Decoder 側の挙動を追記）
- `CHANGES.md`

## 依存 issue

- **issue 0010** (`0010-bug-drop-deadlock-on-sync-operation-infinite`): 調査により、`MFXVideoCORE_SyncOperation` はタスク結果を `wait` 値に関係なく返すため有限タイムアウト化はエラー表面化に寄与せず、`MFX_INFINITE` は維持される（廃案）。また真の GPU ハングでの Drop デッドロックは仕様上達成不能であることが確定した。本 issue の完了条件 5 は 0010 への依存を外し、「DEVICE_BUSY / MORE_SURFACE 状態（真のハングではない）での join 保証」に限定する。
- **issue 0008** (`0008-bug-decoder-b-frame-user-data-mismatch`): Decoder の user_data 対応付けを FIFO から HashMap + TimeStamp 方式に変更する issue。**適用順序は本 issue (0009) を先に適用する**（0008 は本 issue のエラー経路を前提として「エラー後の `frame_count` の扱い」を 0008 の設計に組み込んでいる。0008 の設計では `decode_bitstream` がエラーを返した場合 `frame_count` がインクリメントされず、次回 `decode()` で同一 `frame_seq` となり重複キーエラーになる）。本 issue のエラー復帰に関する注意（QueueFrame 残留）は 0008 の HashMap 方式導入後は残留エントリの扱いが変わるため、0008 の実装と整合を取ること。
