# Encoder / Decoder の SyncOperation を有限タイムアウト化してデバイスエラーを表面化する

- Priority: High
- Created: 2026-07-01
- Model: Opus 4.7
- Branch: feature/fix-drop-deadlock-on-sync-operation-infinite
- Polished: 2026-08-02

## 目的

Worker スレッドが `MFXVideoCORE_SyncOperation(session, syncp, MFX_INFINITE)` でブロックし、GPU ハング・デバイス障害時にドライバが返すデバイスエラーを正しく表面化できず、プロセスを復旧・終了できない問題を、一次資料（oneVPL 仕様）準拠の方法で解決する。

具体的には以下を行う。

- `MFX_INFINITE` を有限タイムアウト（500 ms）のポーリングに変更し、`MFX_ERR_GPU_HANG` / `MFX_ERR_DEVICE_LOST` / `MFX_ERR_DEVICE_FAILED` をハンドラ経由でアプリに通知する。アプリは一次資料の規定どおり「関数クラスを close して再初期化」することで復旧できる。
- Drop 経路は oneVPL の非同期契約（Close 前に未完了の非同期操作をすべて同期する）どおり、未完了タスクを完了（またはエラー）まで同期してから Close する。実機検証で、未完了タスクを残したまま Close すると libmfx-gen が SIGSEGV することを確認しており、この契約を厳守する。

なお、「ドライバがエラーを返さず制御を返さない真の GPU ハング」で Drop を有限時間で返すことは仕様上達成不能である（詳細は「スコープ外の明記」参照）。本 issue はドキュメント準拠の範囲で可能な改善を行う。

## 優先度根拠

High。以下による。

- **GPU ハング時にアプリが復旧できない**: ドライバがデバイスエラーを返しても、`MFX_INFINITE` 待機中の Worker がエラーを正しく伝えられないと、アプリが「close + 再初期化」の復旧手順を実行できない。一次資料が規定するデバイス障害時の対処（下記）を実現する必要がある。
- **実機で Close がクラッシュする**: 未完了タスクを残したまま `MFXVideoENCODE_Close` / `MFXClose` を呼ぶと libmfx-gen が SIGSEGV することを実機で確認した。非同期契約どおりにタスクを完了させてから Close しなければならない。
- **既存の `DEVICE_BUSY` リトライ上限（30 回）の意図との整合**: 上限リトライで途中エラーを返せるようにしても、Sync のブロックで止まるなら意味がない。

## 現状

### Encoder 側

`src/encode.rs` の `sync_and_collect`:

```rust
let status = lib.mfx_video_core_sync_operation(
    session_handle as sys::mfxSession,
    syncp,
    sys::MFX_INFINITE,
);
Error::check_mfx(status, "MFXVideoCORE_SyncOperation")?;
```

Worker はこのブロッキング呼び出しの中で待機する。`MFX_INFINITE` はドライバ実装によっては制御を返さない可能性があり、その場合 `Drop`（`stop_worker` → `join`）がデッドロックし、プロセスをシャットダウンできない。

### Decoder 側

同じ問題が Decoder にも存在する。

- `src/decode.rs` の `sync_and_callback`: `MFX_INFINITE` で SyncOperation
- `src/decode.rs` の `sync_and_drain`: `MFX_INFINITE` で SyncOperation

### 二重エラー通知（既存の潜在バグ）

`sync_and_collect` が `Err` を返すと、`sync_and_build_frame` が early return し、`PendingFrameStore::take_by_frame_seq` が呼ばれず pending frame が残留する。その後 `Stop` ハンドラの `drain_all` で同一フレームに再度エラー通知される。本 issue の有限タイムアウト化でデバイスエラー経路が通りやすくなるため、あわせて修正する。

## 一次資料（ドキュメント調査）

### Hardware Device Error Handling（`VPL_prg_err.rst`）

oneVPL 仕様（libvpl `doc/spec/source/programming_guide/VPL_prg_err.rst`「Hardware Device Error Handling」）は、デバイス障害時の対処を以下のとおり規定する。

> VPL functions return `MFX_ERR_DEVICE_LOST` or `MFX_ERR_DEVICE_FAILED` to indicate that there is a complete failure in hardware acceleration. **The application must close and reinitialize the VPL function class.** If the application has provided a hardware acceleration device handle to VPL, the application must reset the device.

すなわち GPU ハング・デバイス障害時は、VPL 関数が `MFX_ERR_DEVICE_LOST` / `MFX_ERR_DEVICE_FAILED` を返し、**アプリケーションは関数クラスを close して再初期化する**ことが正しい復旧手順である。

### `MFX_ERR_GPU_HANG`

`mfxdefs.h` は `MFX_ERR_GPU_HANG`（= -21）を「Device operation failure caused by GPU hang」と定義する。GPU ハングはデバイス障害の一種であり、VPL 関数がこのエラーを返すことを仕様が想定している。

### 非同期モデルと Close の契約

oneVPL の非同期モデルでは、`EncodeFrameAsync` / `DecodeFrameAsync` が返した Sync ポイントを `MFXVideoCORE_SyncOperation` で同期（完了待ち）してから、次の依存操作や Close を行う必要がある。未完了の非同期操作を残したまま Close することは契約違反であり、実機では libmfx-gen が SIGSEGV する（「実機検証結果」参照）。

## 実機検証結果

`libmfx-gen`（oneVPL GPU ランタイム）の実機（Intel GPU）で以下を確認した。

- **未完了タスクを残したままの Close は SIGSEGV**: EncodeFrameAsync を 10 フレーム連続投入した直後に drop し、Sync 未完了のまま `MFXVideoENCODE_Close` / `MFXClose` を呼ぶと、libmfx-gen 内部ワーカースレッドが SIGSEGV（約 65% の確率で再現）。バックトレースでは drop スレッドが `MFXVideoENCODE_Close` 内の待機中に、libmfx-gen のワーカースレッドがクラッシュしていた。
- **タスク完了後なら Close は安全**: タスクが完了するまで待ってから drop するとクラッシュしない（10/10 pass）。
- **`MFXVideoENCODE_Close` をスキップしてもクラッシュ**: session Drop（`MFXClose`）だけにしても同様に SIGSEGV。
- **Decoder 側はクラッシュしない**: Decoder の drop 経路（`sync_and_callback` / `sync_and_drain` の Sync 中断）は同一条件下でクラッシュしないことを確認済み。

この結果から、「Sync を即時中断してタスクを in-flight のまま残し、Close する」設計は実機でクラッシュするため採用しない。

## 設計方針

**`MFX_INFINITE` を有限タイムアウトのポーリングに置き換え、一次資料に基づくデバイスエラー処理と非同期契約に従う。**

### 基本設計

1. **`sync_and_collect` / `sync_and_callback` / `sync_and_drain` の `MFX_INFINITE` を有限タイムアウトのポーリングに変更する**
   - タイムアウト値は **500 ms** とする（100 ms ではポーリング間隔が短すぎて再呼び出し頻度が増え、1000 ms ではシャットダウン時の待機が長い。500 ms はバランス値）。
   - 生の status 値で分岐するループ構造に変更する:
     ```rust
     loop {
         let status = lib.mfx_video_core_sync_operation(..., 500);
         if status == sys::mfxStatus_MFX_ERR_NONE {
             break; // 完了
         }
         if status == sys::mfxStatus_MFX_WRN_IN_EXECUTION {
             continue; // 未完了。再試行
         }
         return Err(Error::from_mfx(status, "MFXVideoCORE_SyncOperation"));
     }
     ```
   - `check_mfx` は `MFX_WRN_IN_EXECUTION`（正の警告値）をエラー扱いするため使用しない。
   - ポーリングにより、GPU ハング・デバイス障害時にドライバが返すデバイスエラー（`MFX_ERR_GPU_HANG` / `MFX_ERR_DEVICE_LOST` / `MFX_ERR_DEVICE_FAILED`）を確実に表面化する。

2. **デバイスエラーの表面化とドキュメント準拠の復旧**
   - Sync ループが返すエラーをハンドラ（`on_encoded` / `on_decoded`）経由でアプリに通知する。
   - アプリは一次資料（`VPL_prg_err.rst`）の規定どおり、関数クラスを close して再初期化する（デバイスハンドルを渡していた場合はデバイスもリセット）。ライブラリ側でデバイスエラーを握りつぶしたり、未文書化の復旧を行ったりしない。

3. **Drop 経路は非同期契約どおりに未完了タスクを完了させてから Close する**
   - Worker は `Stop` コマンド受信後も、キュー内の未完了 Sync を完了（またはエラー）まで同期してから終了する。healthy GPU では数 ms で完了するため Drop は高速に返る。
   - 未完了タスクを残したまま Close すると libmfx-gen が SIGSEGV する（実機検証結果）ため、「stopping フラグによる即時中断」や「Close スキップによるリーク」のようなタスクを in-flight のまま残す設計は採用しない。

4. **Encoder の Sync エラー時二重通知を防止する**
   - `SyncData` に `frame_seq: u64` を追加する（`encode()` 時点で frame_seq は既知）。
   - `sync_and_collect` が `Err` を返した場合、`sync_and_build_frame` は `SyncData.frame_seq` を使って `PendingFrameStore::take_by_frame_seq` を呼び、pending frame を消費してから `Err` を返す。これにより `Stop` ハンドラの `drain_all` での二重通知を防ぐ。
   - `finish()` のドレインループで生成される `SyncData` は、送信時点で対応する `frame_seq` を決定できない（SyncOperation 完了前は `bitstream.TimeStamp` が読めない）ため 0 のままとする。この場合 `take_by_frame_seq` が `None` を返し、残留エントリは `Stop` アームの一括 `MFX_ERR_ABORTED` 通知に委ねる。
   - 二重通知の防止対象は Sync エラー時のみとする（従来の挙動と同様）。

### スコープ外の明記

- **「真の GPU ハングで Drop を有限時間で返す」ことは仕様上達成不能**: 一次資料の枠組みは、ドライバが `MFX_ERR_GPU_HANG` / `MFX_ERR_DEVICE_LOST` / `MFX_ERR_DEVICE_FAILED` を返す前提であり、アプリが close + 再初期化で復旧する。ドライバがエラーを返さず制御を返さないケースはドライバ起因であり、ライブラリ側では解決できない（issue 0026 も同様の認識）。このため、stopping フラグによる Sync の即時中断や `gpu_unresponsive` フラグ、Close スキップのような未文書化の機構は導入しない。
- **`finish()` のブロック**: `finish()` の GPU ハング時のブロックは別 issue（0026）で対応する。本 issue は SyncOperation の有限タイムアウト化が目的であり、`finish()` の挙動変更は含まない。
- 修正の有効範囲は「SyncOperation が wait ミリ秒で必ず制御を返す」という VPL の契約に依存する。ドライバレベルでハングして SyncOperation 自体が返らない場合、有限タイムアウトでもブロックは解消しない。

## 完了条件

以下すべてを満たす。

1. `sync_and_collect` / `sync_and_callback` / `sync_and_drain` が `MFX_INFINITE` の代わりに有限タイムアウト（500 ms）のポーリングで SyncOperation を呼び、`MFX_ERR_NONE` で完了、`MFX_WRN_IN_EXECUTION` で再試行、それ以外（デバイスエラー含む）はエラーとして返すループ構造に修正する。
2. Sync ループが返すエラー（`MFX_ERR_GPU_HANG` / `MFX_ERR_DEVICE_LOST` / `MFX_ERR_DEVICE_FAILED` 等）がハンドラ経由でアプリに通知される。
3. Drop 経路（`stop_worker` → `join` → Close）が、未完了タスクを完了（またはエラー）まで同期してから Close する構造であることをコードレビューで確認する。未完了タスクを残したまま Close しないこと（実機で SIGSEGV するため）。
4. `SyncData` に `frame_seq: u64` を追加し、`sync_and_build_frame` で Sync エラー時に `PendingFrameStore::take_by_frame_seq` を呼んで pending frame を消費してからエラーを返し、二重通知を防止する。
5. Drop 経路の GPU テスト（`tests/test_roundtrip.rs` に追記）で、encode / decode 直後の drop がクラッシュせず、各フレームがちょうど 1 回だけ通知されることを確認する。
6. `CHANGES.md` の `## develop` に `[FIX]` として追記する。

## 影響範囲

- `src/encode.rs`: `SyncData` (`frame_seq` 追加)、`sync_and_collect` (タイムアウト + 生 status 分岐 + シグネチャ変更)、`sync_and_build_frame` (エラー時の pending 消費 + シグネチャ変更)、`encode()` (`SyncData` への `frame_seq` 設定)、`finish()` (ドレイン由来 `SyncData` の `frame_seq` は 0)
- `src/decode.rs`: `sync_and_callback` (タイムアウト + 生 status 分岐 + シグネチャ変更)、`sync_and_drain` (タイムアウト + 生 status 分岐 + シグネチャ変更)
- `tests/test_roundtrip.rs` (Drop 経路の GPU テスト追加)
- `CHANGES.md`

## 依存 issue

- **issue 0008** (`0008-bug-decoder-b-frame-user-data-mismatch`): 0008 は本 issue を依存先として「0010 を先に適用し、その差分の上に 0008 の変更を重ねること」と明記している。0008 は Decoder 側の user_data 対応付けを HashMap + TimeStamp 方式に変更するため、`sync_and_callback` のシグネチャ変更が 0008 の設計前提となる。
- **issue 0009** (`0009-bug-decoder-device-busy-infinite-retry`): 0009 は本 issue を依存先として「0010 を先に適用すること」と明記している。
- **issue 0014** (`0014-bug-frame-surface-drop-silently-swallows-errors`): `sync_and_drain` 内の `FrameSurface::Drop` のエラー処理を変更する。本 issue の後に適用する。
- **issue 0026** (`0026-bug-finish-block-on-gpu-hang`): `finish()` の GPU ハング時のブロックを扱う。本 issue の有限タイムアウト化を前提とする。
