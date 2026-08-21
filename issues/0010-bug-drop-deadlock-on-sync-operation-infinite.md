# Encoder / Decoder のデバイスエラー伝搬の検証・修正と Drop 経路の非同期契約の確認

- Priority: Medium
- Created: 2026-07-01
- Model: Opus 4.7
- Branch: feature/fix-device-error-propagation
- Polished: 2026-08-02

## 目的

GPU ハング・デバイス障害時に、`MFXVideoCORE_SyncOperation` が返すデバイスエラー（`MFX_ERR_GPU_HANG` / `MFX_ERR_DEVICE_LOST` / `MFX_ERR_DEVICE_FAILED`）がハンドラ経由でアプリに正しく通知され、アプリが一次資料（oneVPL 仕様）の規定どおり「関数クラスを close して再初期化」することで復旧できることを、一次資料と実装・実機に基づいて検証・修正する。

当初は「`MFX_INFINITE` を有限タイムアウト（500 ms）のポーリングに変更することでデバイスエラーを表面化する」方針だった。しかし一次資料（libmfx-gen の実装）の調査により、`MFXVideoCORE_SyncOperation` はタスクの結果（`pTask->opRes`）を `wait` 値に関係なく返すため、**有限タイムアウト化はエラー表面化に寄与しない**ことが判明した。また、ドライバがタスク結果をエラーにせず制御を返さない真の GPU ハングでは、有限タイムアウトのポーリングループも再試行を続けて終われず、Drop のデッドロックは解消しない（「スコープ外の明記」参照）。本 issue は有限タイムアウト化を廃案とし、一次資料と実装に基づいて達成可能な改善に絞る。

具体的には以下を行う。

- SyncOperation が返すデバイスエラーがハンドラ経由でアプリに通知される経路を検証し、あわせて既存の二重通知バグ（`sync_and_collect` の `Err` で pending frame が残留し、`Stop` ハンドラの `drain_all` で同一フレームが再通知される）を修正する。
- Drop 経路が oneVPL の非同期契約（Close 前に未完了の非同期操作をすべて同期する）どおり、未完了タスクを完了（またはエラー）まで同期してから Close する構造であることをコードレビューと実機テストで確認する。実機検証で、未完了タスクを残したまま Close すると libmfx-gen が SIGSEGV することを確認しており、この契約を厳守する。
- 真の GPU ハングで Drop を有限時間で返すことが仕様上達成不能であることを明文化する。

## 優先度根拠

Medium。以下による。

- **エラー経路の正しさがアプリの復旧に直結する**: デバイス障害時に SyncOperation が返すエラーがちょうど 1 回だけ通知されることは、アプリが「close + 再初期化」の復旧手順を正しく実行する前提である。現在は Sync エラー時に pending frame が残留して二重通知される潜在バグがあり、修正が必要。
- **実機で Close がクラッシュする**: 未完了タスクを残したまま `MFXVideoENCODE_Close` / `MFXClose` を呼ぶと libmfx-gen が SIGSEGV することを実機で確認した。非同期契約どおりにタスクを完了させてから Close する構造を Drop 経路で維持・検証しなければならない。
- **依存 issue の前提の確定**: 0008 / 0009 / 0012 / 0014 / 0026 が本 issue を依存先としている。本 issue の調査結果（有限タイムアウト化はエラー表面化に寄与しない）は依存側の前提にも影響するため、本 issue で確定させる。

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

`MFX_INFINITE`（= `0xFFFFFFFF` ms）で SyncOperation を待つ。`check_mfx` は `MFX_ERR_NONE` 以外を `Err` にするため、ドライバがタスク結果としてデバイスエラーを返せば、そのエラーは `Err` として伝搬され、`sync_worker` がハンドラ（`on_encoded`）経由でアプリに通知する（実装の詳細は「一次資料」参照）。なお `MFX_WRN_IN_EXECUTION` は `check_mfx` ではエラー扱いされるが、`MFX_INFINITE` では実質的に返らないため現状問題はない。

真の GPU ハング（ドライバがタスク結果をエラーにせず、SyncOperation が制御を返さない）の場合、Worker はブロックしたまま `Stop` コマンドを処理できず、`Drop`（`stop_worker` → `join`）がデッドロックする。これは仕様上達成不能であり（「スコープ外の明記」参照）、`MFX_INFINITE` を有限タイムアウトにしても解消しない。

### Decoder 側

- `src/decode.rs` の `sync_and_callback`: `MFX_INFINITE` で SyncOperation。エラーはハンドラ（`on_decoded`）経由で通知される。
- `src/decode.rs` の `sync_and_drain`: `MFX_INFINITE` で SyncOperation。エラーはすべて無視する（データ破棄が目的のため。issue 0014 の対象）。

### 二重エラー通知（既存の潜在バグ）

`sync_and_collect` が `Err` を返すと、`sync_and_build_frame` が early return し、`PendingFrameStore::take_by_frame_seq` が呼ばれず pending frame が残留する。その後 `Stop` ハンドラの `drain_all` で同一フレームに再度エラー通知される。ドライバがデバイスエラーを返した場合にこの経路が実行されるため、修正する。

## 一次資料（ドキュメント調査）

### Hardware Device Error Handling（`VPL_prg_err.rst`）

oneVPL 仕様（`oneVPL` リポジトリの `doc/spec/source/programming_guide/VPL_prg_err.rst`「Hardware Device Error Handling」）は、デバイス障害時の対処を以下のとおり規定する。

> VPL functions return `MFX_ERR_DEVICE_LOST` or `MFX_ERR_DEVICE_FAILED` to indicate that there is a complete failure in hardware acceleration. **The application must close and reinitialize the VPL function class.** If the application has provided a hardware acceleration device handle to VPL, the application must reset the device.

すなわち GPU ハング・デバイス障害時は、VPL 関数が `MFX_ERR_DEVICE_LOST` / `MFX_ERR_DEVICE_FAILED` を返し、**アプリケーションは関数クラスを close して再初期化する**ことが正しい復旧手順である。

### `MFX_ERR_GPU_HANG`

`oneVPL` リポジトリの `api/vpl/mfxdefs.h` は `MFX_ERR_GPU_HANG`（= -21）を「Device operation failure caused by GPU hang」と定義する。GPU ハングはデバイス障害の一種であり、VPL 関数がこのエラーを返すことを仕様が想定している。

### `MFXVideoCORE_SyncOperation` の実装（libmfx-gen）

`oneVPL-intel-gpu` リポジトリ（libmfx-gen、oneVPL GPU ランタイム）の実装では、`MFXVideoCORE_SyncOperation` は `MFX_ERR_GPU_HANG` 等のデバイスエラーを返し得る。このエラー伝搬は `wait` 値（`MFX_INFINITE` か有限値か）に依存しない。

- `MFXVideoCORE_SyncOperation` は `session->m_pScheduler->Synchronize(syncp, wait)` に委譲する（`_studio/mfx_lib/shared/src/libmfxsw_async.cpp:31`）。
- `mfxSchedulerCore::Synchronize` は条件変数 + 述語でタスク完了を待ち、`pTask->opRes` を返す（`_studio/mfx_lib/scheduler/linux/src/mfx_scheduler_core_ischeduler.cpp:256`）:

  ```cpp
  pTask->done.wait_for(guard, std::chrono::milliseconds(timeToWait), [pTask, handle] {
      return (pTask->jobID != handle.jobID) || (MFX_WRN_IN_EXECUTION != pTask->opRes);
  });

  if (pTask->jobID == handle.jobID) {
      return pTask->opRes;
  }
  ```

- タスク失敗時は scheduler が `pTask->opRes` をエラーに設定して `done.notify_all()` するため（`_studio/mfx_lib/scheduler/agnostic/src/mfx_scheduler_core_task_management.cpp:552`）、`wait` が起きて SyncOperation がデバイスエラーを返す。

一方、`MFX_INFINITE`（`0xFFFFFFFF` ms）でも述語が成立しなければ約 49.7 日待ち続ける。真の GPU ハング（タスク結果が `MFX_WRN_IN_EXECUTION` のまま変わらない）では、有限タイムアウトのポーリングも `MFX_WRN_IN_EXECUTION` を返し続けて再試行を繰り返すだけで制御は返らない。

### 非同期モデルと Close の契約

oneVPL の非同期モデルでは、`EncodeFrameAsync` / `DecodeFrameAsync` が返した Sync ポイントを `MFXVideoCORE_SyncOperation` で同期（完了待ち）してから、次の依存操作や Close を行う必要がある。未完了の非同期操作を残したまま Close することは契約違反であり、実機では libmfx-gen が SIGSEGV する（「実機検証結果」参照）。

## 実機検証結果

`libmfx-gen`（oneVPL GPU ランタイム）の実機（Intel GPU）で以下を確認した。

- **未完了タスクを残したままの Close は SIGSEGV**: EncodeFrameAsync を 10 フレーム連続投入した直後に drop し、Sync 未完了のまま `MFXVideoENCODE_Close` / `MFXClose` を呼ぶと、libmfx-gen 内部ワーカースレッドが SIGSEGV（約 65% の確率で再現）。バックトレースでは drop スレッドが `MFXVideoENCODE_Close` 内の待機中に、libmfx-gen のワーカースレッドがクラッシュしていた。
- **タスク完了後なら Close は安全**: タスクが完了するまで待ってから drop するとクラッシュしない（10/10 pass）。
- **`MFXVideoENCODE_Close` をスキップしてもクラッシュ**: session Drop（`MFXClose`）だけにしても同様に SIGSEGV。
- **Decoder 側はクラッシュしない**: Decoder の drop 経路（`sync_and_callback` / `sync_and_drain` の Sync 中断）は同一条件下でクラッシュしないことを確認済み。

この結果から、「Sync を即時中断してタスクを in-flight のまま残し、Close する」設計は実機でクラッシュするため採用しない。

なお、デバイスエラー（`MFX_ERR_GPU_HANG` 等）の強制は実機ではできないため、エラー伝搬の確認はコードレビュー（libmfx-gen 実装）と、エラー経路を注入するユニットテストで行う。

## 設計方針

**`MFX_INFINITE` は維持し、有限タイムアウト化は行わない。** 一次資料（libmfx-gen 実装）により、SyncOperation は `wait` 値に関係なくタスク結果（デバイスエラー含む）を返すため、有限タイムアウト化はエラー表面化に寄与しない。本 issue は、デバイスエラー伝搬の保証と Drop 経路の非同期契約の検証に絞る。

### 基本設計

1. **デバイスエラー伝搬の保証（検証対象）**
   - SyncOperation が返すデバイスエラーは `check_mfx` → `Err` → ハンドラ（`on_encoded` / `on_decoded`）経由でアプリに通知される。アプリは一次資料（`VPL_prg_err.rst`）の規定どおり、関数クラスを close して再初期化する（デバイスハンドルを渡していた場合はデバイスもリセット）。ライブラリ側でデバイスエラーを握りつぶしたり、未文書化の復旧を行ったりしない。
   - 通知が「ちょうど 1 回」であることを保証するため、項目 2 の二重通知修正を行う。

2. **Encoder の Sync エラー時二重通知を防止する**
   - `SyncData` に `frame_seq: u64` を追加する（`encode()` 時点で frame_seq は既知）。
   - `sync_and_collect` が `Err` を返した場合、`sync_and_build_frame` は `SyncData.frame_seq` を使って `PendingFrameStore::take_by_frame_seq` を呼び、pending frame を消費してから `Err` を返す。これにより `Stop` ハンドラの `drain_all` での二重通知を防ぐ。
   - `finish()` のドレインループで生成される `SyncData` は、送信時点で対応する `frame_seq` を決定できない（SyncOperation 完了前は `bitstream.TimeStamp` が読めない）ため 0 のままとする。この場合 `take_by_frame_seq` が `None` を返し、残留エントリは `Stop` アームの一括 `MFX_ERR_ABORTED` 通知に委ねる。
   - 二重通知の防止対象は Sync エラー時のみとする（従来の挙動と同様）。

3. **Drop 経路は非同期契約どおりに未完了タスクを完了させてから Close する**
   - worker はコマンドを逐次処理するため、`Stop` を受信する時点で、それ以前に送信された `Sync` コマンドはすべて完了（またはエラー）済みである。`Drop`（`stop_worker` → `join`）→ Close は未完了タスクを残さない構造（現状のまま）。healthy GPU ではタスクは数 ms で完了するため Drop は高速に返る。
   - 未完了タスクを残したまま Close すると libmfx-gen が SIGSEGV する（実機検証結果）ため、「stopping フラグによる即時中断」や「Close スキップによるリーク」のようなタスクを in-flight のまま残す設計は採用しない。
   - この構造をコードレビューと GPU テスト（完了条件 3）で確認する。

### スコープ外の明記

- **「真の GPU ハングで Drop を有限時間で返す」ことは仕様上達成不能**: 一次資料の枠組みは、ドライバがタスク結果として `MFX_ERR_GPU_HANG` / `MFX_ERR_DEVICE_LOST` / `MFX_ERR_DEVICE_FAILED` を返す前提であり、アプリが close + 再初期化で復旧する。ドライバがタスク結果をエラーにせず SyncOperation が制御を返さない場合はドライバ起因であり、ライブラリ側では解決できない（`MFX_INFINITE` を有限タイムアウトにしても、ポーリングループが再試行を続けるだけで制御は返らない）。このため、stopping フラグによる Sync の即時中断や `gpu_unresponsive` フラグ、Close スキップのような未文書化の機構は導入しない。当初の「有限タイムアウト化」案もこの理由で廃案とした。
- **`finish()` のブロック**: `finish()` の GPU ハング時のブロックは仕様上達成不能であり、issue 0026 は `issues/pending/` に移動済み。ドライバがタスク結果としてデバイスエラーを返す限り、SyncOperation がエラーで制御を返し worker が `WaitIdle` に応答するため、`finish()` は現状の実装で既に有限時間でエラーを返す（修正不要）。真のハング（ドライバがエラーを返さない）で `finish()` を有限時間で返すには、タスクを in-flight のまま残す（中断後の Drop → Close が未完了タスクで SIGSEGV する。本 issue の実機検証結果）か、Close をスキップしてリークするしかなく、どちらも採用しない。本 issue の対象外とする。
- **`sync_and_drain` のエラー無視**: `sync_and_drain` はエラーを無視するが、本 issue では変更しない。ドレインフレーム破棄時の観測性は、`FrameSurface::Drop` のエラー出力を扱う issue 0014、および `sync_and_drain` 自体を削除する issue 0008 で対応する。

## 完了条件

以下すべてを満たす。

1. SyncOperation が返すデバイスエラー（`MFX_ERR_GPU_HANG` / `MFX_ERR_DEVICE_LOST` / `MFX_ERR_DEVICE_FAILED` 等）が `check_mfx` → `Err` → ハンドラ経由でアプリに通知されることを、コードレビュー（libmfx-gen 実装に基づく）とテストで確認する。`MFX_INFINITE` は維持する。
2. `SyncData` に `frame_seq: u64` を追加し、`sync_and_build_frame` で Sync エラー時に `PendingFrameStore::take_by_frame_seq` を呼んで pending frame を消費してからエラーを返し、二重通知を防止する。
3. Drop 経路の GPU テスト（`tests/test_roundtrip.rs` に追記）で、encode / decode 直後の drop がクラッシュせず、各フレームがちょうど 1 回だけ通知されることを確認する。
4. `CHANGES.md` の `## develop` に `[FIX]` として追記する。

## 影響範囲

- `src/encode.rs`: `SyncData` (`frame_seq` 追加)、`sync_and_build_frame` (エラー時の pending 消費 + シグネチャ変更)、`encode()` (`SyncData` への `frame_seq` 設定)、`finish()` (ドレイン由来 `SyncData` の `frame_seq` は 0)
- `src/decode.rs`: 変更なし（`MFX_INFINITE` 維持。エラー伝搬は現状どおり）
- `tests/test_roundtrip.rs` (Drop 経路の GPU テスト追加)
- `CHANGES.md`

## 依存 issue

本 issue の調査結果（有限タイムアウト化はエラー表面化に寄与しない、真の GPU ハングでの Drop の有限時間化は仕様上達成不能）は、本 issue を前提としていた各 issue の記述に影響した。各 issue の「依存 issue」セクションは本 issue の現状（`MFX_INFINITE` 維持、stopping フラグ・500 ms タイムアウト廃案）に合わせて修正済みであり、issue 0026 は仕様上達成不能として `issues/pending/` に移動済みである。

- **issue 0008** (`0008-bug-decoder-b-frame-user-data-mismatch`): Encoder 側の二重通知修正（`SyncData.frame_seq`）と Decoder 側の TimeStamp 対応付けは独立。0008 の Sync 失敗時の残留エントリの扱いは、0010 の Encoder 側方針（Sync エラー時に pending を消費）とは異なり、Decoder 側はエントリを消費できないため二重通知を許容する方針に修正済み（0008 の項目 6 参照）。適用順序は 0010 → 0008。
- **issue 0009** (`0009-bug-decoder-device-busy-infinite-retry`): 完了条件 5 は 0010 への依存を外し、「DEVICE_BUSY / MORE_SURFACE 状態（真のハングではない）での join 保証」に限定する形に修正済み。
- **issue 0012** (`0012-bug-encoder-reconfigure-does-not-drain-pending`): 0010 の 500 ms タイムアウト前提と、Sync エラー時の二重通知の記述を修正済み（0010 適用後は `take_by_frame_seq` で pending が消費されるため二重通知は発生しない）。適用順序は 0010 → 0012。
- **issue 0014** (`0014-bug-frame-surface-drop-silently-swallows-errors`): 0010 が `sync_and_drain` を変更しないことに合わせて修正済み。適用順序は 0010 → 0014。
- **issue 0020** (`0020-refactor-split-encode-module`): 0010 の `stopping` 引数前提を修正済み。
- **issue 0026** (`0026-bug-finish-block-on-gpu-hang`): `finish()` の GPU ハング時ブロックは、ドライバがエラーを返す限り現状の実装で有限時間で返る（修正不要）ため、また真のハングでは制約内で達成不可能（Close スキップのリーク設計が必要になるため）であることから、**`issues/pending/` に移動済み**。本 issue（0010）の調査結果に基づく。
