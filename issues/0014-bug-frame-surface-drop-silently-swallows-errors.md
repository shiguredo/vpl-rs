# Drop 経路で VPL API の Unmap / Release / Close 失敗が silent に潰されリソース破損を追跡できない

- Priority: High
- Created: 2026-07-01
- Model: Opus 4.7
- Branch: feature/fix-frame-surface-drop-silent-errors
- Polished: 2026-08-21

## 目的

`FrameSurface::Drop` が `mfxFrameSurfaceInterface::Unmap` / `Release` の失敗を完全に silent に潰しているため、VPL 内部の参照カウンタが壊れる、または map 状態のまま release して二重 free / use-after-free / GPU リソースリークが発生した場合に、原因追跡ができない。Drop 内のエラーを観測可能にし、致命的な状態遷移を検出できるようにする。同様のエラー黙殺は `Session::Drop`（`MFXClose`）、`Encoder::Drop` / `Decoder::Drop`（`mfx_video_encode_close` / `mfx_video_decode_close`）、および `Encoder::new` の失敗パスにも存在するため、合わせて対応する（`Encoder::new` の失敗パスは Drop 経路ではないが同一パターンのため含める。詳細は「現状」参照）。

## 優先度根拠

High。以下による。

- **リソース破損の隠蔽**: `Release` が失敗した瞬間に VPL 内部の参照カウンタが不整合になる可能性があり、後段で二重 free や use-after-free が起きても Rust 側から見ると「なぜかクラッシュした」以上の情報が得られない。
- **VPL 特有のリソース管理**: `mfxFrameSurfaceInterface::Release` は VPL のサーフェスプールの参照カウントを操作する API。ここが壊れると、以降のフレーム取得（`MFXMemory_GetSurfaceForEncode`）でプール枯渇や UB を招く。
- **過去の設計判断を上回る必要性**: closed issue 0006（unified surface wrapper）および 0007（Session RAII cleanup）では「Drop 内パニック回避のためにエラーを `let _ =` で破棄する」という設計判断がなされた。しかし本 issue はこの判断を覆し、観測性を優先する。stderr への書き込み失敗を握りつぶす形（`let _ = writeln!(std::io::stderr(), ...)`）であればパニックせず、Drop 内での安全性を損なわないため、0006/0007 の「パニック回避」という制約は満たしつつ観測性を向上できる。

## 現状

### FrameSurface の Drop 実装

`src/vpl.rs` の `FrameSurface::Drop`:

```rust
impl Drop for FrameSurface {
    fn drop(&mut self) {
        if self.mapped {
            let _ = Error::check_mfx(
                self.lib.mfx_frame_surface_unmap(self.as_ptr()),
                "mfxFrameSurfaceInterface::Unmap",
            );
        }
        let _ = Error::check_mfx(
            self.lib.mfx_frame_surface_release(self.as_ptr()),
            "mfxFrameSurfaceInterface::Release",
        );
    }
}
```

### 他のエラー黙殺（本 issue の対象）

同じパターンが以下にも存在する。うち `Encoder::new` の失敗パスは Drop 経路ではないが、同一のエラー黙殺パターンであるため本 issue の対象に含める:

- `src/vpl.rs` の `Session::Drop`: `let _ = self.lib.mfx_close(self.session);`（`mfx_unload` は戻り値が `()` のため対象外）
- `src/encode.rs` の `Encoder::Drop`: `let _ = self.session.lib().mfx_video_encode_close(...);`
- `src/decode.rs` の `Decoder::Drop`: `let _ = self.session.lib().mfx_video_decode_close(...);`（`if self.initialized` ガード内）
- `src/encode.rs` の `Encoder::new` の失敗パス 3 箇所（スレッド生成失敗パス、`mfx_video_encode_get_video_param` 失敗時の `inspect_err` 内、VP9 `WriteIVFHeaders` 不一致時のエラーパス）: `let _ = lib.mfx_video_encode_close(session_ptr);`（**Init 後のエラーリターン経路であり Drop 経路ではないが、同一関数の同一パターンであるため対象に含める**。完了条件 5 参照）

### Drop 外のエラー黙殺（本 issue の対象外）

- `src/decode.rs` の `sync_and_drain`: `let _ = lib.mfx_video_core_sync_operation(...);` および `let _ = frame_surface.map_read();`

**`sync_and_drain` は本 issue では対応しない**。理由は次のとおり:

- 依存 issue 0008 が `sync_and_drain` 自体を削除する（0008 の設計では引き当て失敗の破棄が `sync_and_callback` 内部で完結し、`Sync` アームからの呼び出しが不要になるため。0010 は `sync_and_drain` を変更しない。0010 の調査で有限タイムアウト化は廃案）。本 issue で stderr 出力に対応しても 0008 適用で消滅する。
- 0008 適用後の破棄経路では `FrameSurface::Drop`（本 issue の対象）が `Unmap` / `Release` 失敗を出力するため、観測性は確保される。
- なお、0008 の「依存 issue」セクションの本 issue の記述は、既に「`FrameSurface::Drop` のエラー処理を変更する。`sync_and_drain` は変更対象外（0008 の削除に委ねる）」に更新済みである（0008 の「依存 issue」セクション参照）。本 issue と 0008 の役割分担（`sync_and_drain` 自体の削除は 0008、Drop 経路の観測性は本 issue）は両者で一致している。0010 側の記述も「`sync_and_drain` のエラー無視は `FrameSurface::Drop` のエラー出力を扱う 0014、および `sync_and_drain` を削除する 0008 で対応する」であり、`FrameSurface::Drop` が対象のため陳腐化していない。

### 呼び出し元での安全対策

- `Encoder::encode` は `frame_surface.map_write()?` → データコピー → `frame_surface.unmap()?` を明示的に呼び、成功時は `mapped = false` にしてから Drop に任せる。
- `Decoder::sync_and_callback` は `frame_surface.map_read()?` → データ読み取り → `frame_surface` を Drop に任せる。

正常系では問題は顕在化していないが、異常系（GPU ハング後の cleanup 等）で検知手段がない。

### 想定される失敗シナリオ

- `Unmap` が失敗した状態のまま `Release` が呼ばれる → VPL 内部でどうなるかは実装依存
- すでに Release 済みの handle を再度 Release する → 現行コードでは `Release` は `FrameSurface::Drop` 内でのみ呼ばれ、同一 handle を包む複数の `FrameSurface` が同時に生存しないため、コード経路上は発生しない。想定シナリオとしての参考であり、VPL 内部の参照カウンタ異常（ゼロからの Release は `MFX_ERR_UNDEFINED_BEHAVIOR`。一次資料 `refs/oneVPL/api/vpl/mfxstructures.h` の `mfxFrameSurfaceInterface::Release` の @return 参照）の可能性を動機づけに使う

## 設計方針

**stderr への書き込みエラーを握りつぶす形でエラー情報を出力する。**

```rust
let _ = writeln!(
    std::io::stderr(),
    "[vpl-rs] {}",
    error, // Error の Display（function / status / message / status_name を含む）
);
```

`eprintln!` は stderr への書き込み失敗時に panic する（Rust 標準の仕様）ため使用しない。Drop 内で panic しない制約（0006/0007 の判断根拠）を守るために、`writeln!` の結果を `let _ =` で握りつぶす。stderr が閉じられている環境（デーモン化等）でも Drop は安全に進行する。

出力パターンは全 8 箇所（`FrameSurface::Drop` の Unmap / Release の 2 箇所、`Session::Drop` / `Encoder::Drop` / `Decoder::Drop` の 3 箇所、`Encoder::new` の `mfx_video_encode_close` 黙殺 3 箇所）で重複するため、`fn report_vpl_error(error: &Error)` のようなヘルパー関数を `src/vpl.rs`（または共通モジュール）に用意し、書式を一元化する（将来の出力先変更にも耐える）。ヘルパー名は Drop 経路だけでなく `Encoder::new` の失敗パスでも使うため、Drop に限定しない名前とする。

なお、`FrameSurface::Drop` はデコーダの worker スレッドでも実行される。複数スレッドが同時に Drop を実行しても、`std::io::stderr()` は内部ロックを保持し `writeln!` は行単位で書き込むため、行内容の分断は実質発生しない（Rust 標準の実装。Drop がどのスレッドで実行されるかに依存しない）。

### 出力フォーマット

全箇所で統一したフォーマットを使用する。**crate 既存の `Error::Display` をそのまま利用する**（`function() failed[status={code}]: {message} ({status_name})` 形式）。数値のみの形式は status_name / status_message を捨て、原因追跡の情報が欠落するため。出力の接頭辞に `[vpl-rs] ` を付ける。

出力例（実際の `Error::Display` の出力。status_name / status_message は実際のエラーに応じて変わる）:

- `[vpl-rs] mfxFrameSurfaceInterface::Unmap() failed[status=-1]: Unknown error (MFX_ERR_UNKNOWN)`
- `[vpl-rs] mfxFrameSurfaceInterface::Release() failed[status=-1]: Unknown error (MFX_ERR_UNKNOWN)`
- `[vpl-rs] MFXClose() failed[status=-1]: Unknown error (MFX_ERR_UNKNOWN)`
- `[vpl-rs] MFXVideoENCODE_Close() failed[status=-1]: Unknown error (MFX_ERR_UNKNOWN)`
- `[vpl-rs] MFXVideoDECODE_Close() failed[status=-1]: Unknown error (MFX_ERR_UNKNOWN)`

### Unmap 失敗後も Release を続行する

`Unmap` が失敗しても `Release` を続行するのは意図的な設計である。Unmap 失敗の原因が VPL 内部の一時的なエラーである可能性があり、Release をスキップするとサーフェスリークが確実に発生する。一方、mapped のまま Release した場合の VPL 内部の挙動は実装依存であり不確実である。「確実なリーク」より「不確実な続行」を選ぶ判断は、両者のリスク比較（確実なリソースリーク vs 実装依存の挙動）に基づく。続行してエラーを観測可能にすることで、失敗の発生を検知しつつ、可能な限りリソースを解放する。

### Encoder::Drop / Decoder::Drop / Session::Drop でエラー後も続行することの妥当性

`mfx_video_encode_close` / `mfx_video_decode_close` 失敗後も `Session::Drop` で `MFXClose` + `MFXUnload` が走る。VPL の仕様上、Close 失敗後の MFXClose 呼び出しが安全かは明示されていないが、現行コードの挙動を変更するわけではなく、観測性を追加するだけであるため本 issue ではこの順序を維持する。

`Session::Drop` 内の `MFXClose` 失敗後も `MFXUnload` を続行するのも同様の判断である。`MFXUnload` をスキップすると loader ハンドルのリークが確実に発生するため、続行する（Unmap 失敗後の Release 続行と同じく「確実なリーク」を避ける）。

### 対象外の明記

- `src/adapter.rs` の `HdlGuard::Drop` の `MFXDispReleaseImplDescription` の戻り値無視: アダプタ列挙 API のクリーンアップであり、GPU ハング後の cleanup 経路ではないため本 issue の対象外とする。
- `src/codec_info.rs` の同型の戻り値無視（`MFXDispReleaseImplDescription` の扱い）: `adapter.rs` と同様にアダプタ列挙 API のクリーンアップであり、本 issue の対象外とする。
- `sync_and_drain`: 上記のとおり 0008 の削除に委ねる。

## 完了条件

以下すべてを満たす。

1. `FrameSurface::Drop` の `Unmap` / `Release` 失敗を `let _ = writeln!(std::io::stderr(), "[vpl-rs] {}", error)` で観測可能にする（`eprintln!` は使わない。stderr 書き込み失敗時に panic するため）。
2. `Session::Drop` の `MFXClose` 失敗を同様に観測可能にする。
3. `Encoder::Drop` の `mfx_video_encode_close` 失敗を同様に観測可能にする。
4. `Decoder::Drop` の `mfx_video_decode_close` 失敗を同様に観測可能にする。
5. `Encoder::new` の `mfx_video_encode_close` 失敗の黙殺 3 箇所（スレッド生成失敗パス、`mfx_video_encode_get_video_param` 失敗時の `inspect_err` 内、VP9 `WriteIVFHeaders` 不一致時のエラーパス）を同様に観測可能にする。
6. 出力フォーマットは `[vpl-rs] ` + `Error::Display`（`function() failed[status={code}]: {message} ({status_name})`）に統一する。
7. `skills/shiguredo-vpl/SKILL.md` のリソース管理の説明に、Drop 経路の失敗が stderr に出力される旨を追記する。
8. `cargo test` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --check` がすべて通過すること（正常系の Drop が panic しないことは、実機がある環境での既存テストで担保される。なお、既存の `frame_surface_gpu_required` は issue 0023 が指摘する silent early-return を含むため、0023 適用後（`#[cfg(intel_vpl)]` 付きの分割テストへの再構成）を前提とし、テスト名に依存しない表現で確認する）。
9. `CHANGES.md` の `## develop` に `[FIX]` として「Drop 経路および `Encoder::new` 失敗パスでの VPL API エラー黙殺を stderr 出力に変更する」を追記する。

注: 異常系 Drop テスト（Release 失敗を強制するテスト）は、結果が VPL 実装依存で非決定になるため検証として弱く、実施しない（`AGENTS.md` の「モック禁止」にも従う）。

## 影響範囲

- `src/vpl.rs`（`FrameSurface::Drop`: Unmap / Release の `let _ =` → stderr 出力、`Session::Drop`: MFXClose の `let _ =` → stderr 出力）
- `src/encode.rs`（`Encoder::Drop`: mfx_video_encode_close の `let _ =` → stderr 出力、`Encoder::new` の `mfx_video_encode_close` 黙殺 3 箇所）
- `src/decode.rs`（`Decoder::Drop`: mfx_video_decode_close の `let _ =` → stderr 出力）
- `skills/shiguredo-vpl/SKILL.md`（Drop 経路の失敗出力の追記）
- `CHANGES.md`

## 依存 issue

- **issue 0010** (`0010-bug-drop-deadlock-on-sync-operation-infinite`): `sync_and_drain` は変更しない（0010 の調査で有限タイムアウト化は廃案）。0010 は「`SyncData.frame_seq` 追加・Sync エラー時の pending 消費」を廃案とし、プロダクションコード変更なしで closed 済みである（0010 の解決方法・影響範囲参照）。したがって本 issue との差分の重なりはなく、適用順序の依存はない（0010 の調査結果・方針を参照するのみ）。本 issue の変更対象（`src/vpl.rs` の `FrameSurface::Drop` 等）と 0010 は変更ファイルが重ならない。
- **issue 0008** (`0008-bug-decoder-b-frame-user-data-mismatch`): `sync_and_drain` を削除する（引き当て失敗の破棄が `sync_and_callback` 内部で完結するため）。本 issue は `sync_and_drain` を対象外とし、0008 の削除に委ねる（「Drop 外のエラー黙殺（本 issue の対象外）」参照）。**適用順序は本 issue を 0008 より先に適用する**（0008 側も「0014 を 0008 より先に適用してから本 issue の変更を重ねること」と明記している）。0008 適用後の破棄経路では本 issue の `FrameSurface::Drop` 対応が観測性を確保する。
- **issue 0023** (`0023-test-fix-silent-pass-tests`): `frame_surface_gpu_required` の silent early-return を修正する。完了条件 8 の既存テスト担保は 0023 適用後を前提とするため、**適用順序は 0023 を先に適用する**（0023 側も本 issue を前提として相互参照している）。
- **issue 0019** (`0019-refactor-extract-vpl-loader-builder`): `src/vpl.rs` に `LoaderBuilder` を導入し `create_session` を書き換える。本 issue の変更箇所（`FrameSurface::Drop` / `Session::Drop` / ヘルパー追加）とは関数が重ならず競合なし（0019 側も「競合なし」と明記）。
- **issue 0020** (`0020-refactor-split-encode-module`): `Encoder::Drop` を `src/encode/encoder.rs` へ移動する。本 issue の `Encoder::Drop` 変更は 0020 の分割に独立に適用可能（0020 側も「独立に適用可能」と明記）。
