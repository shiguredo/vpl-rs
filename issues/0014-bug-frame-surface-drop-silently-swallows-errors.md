# FrameSurface::Drop が Unmap / Release 失敗を silent に潰しリソース破損を追跡できない

- Priority: High
- Created: 2026-07-01
- Model: Opus 4.7
- Branch: feature/fix-frame-surface-drop-silent-errors
- Polished: 2026-07-02

## 目的

`FrameSurface::Drop` が `mfxFrameSurfaceInterface::Unmap` / `Release` の失敗を完全に silent に潰しているため、VPL 内部の参照カウンタが壊れる、または map 状態のまま release して二重 free / use-after-free / GPU リソースリークが発生した場合に、原因追跡ができない。Drop 内のエラーを観測可能にし、致命的な状態遷移を早期に検出できるようにする。

## 優先度根拠

High。以下による。

- **リソース破損の隠蔽**: `Release` が失敗した瞬間に VPL 内部の参照カウンタが不整合になる可能性があり、後段で二重 free や use-after-free が起きても Rust 側から見ると「なぜかクラッシュした」以上の情報が得られない。
- **VPL 特有のリソース管理**: `mfxFrameSurfaceInterface::Release` は VPL のサーフェスプールの参照カウントを操作する API。ここが壊れると、以降のフレーム取得（`MFXMemory_GetSurfaceForEncode`）でプール枯渇や UB を招く。
- **過去の設計判断を上回る必要性**: closed issue 0006（unified surface wrapper）および 0007（Session RAII cleanup）では「Drop 内パニック回避のためにエラーを `let _ =` で破棄する」という設計判断がなされた。しかし本 issue はこの判断を覆し、観測性を優先する。`eprintln!` による stderr 出力はパニックせず、Drop 内での安全性を損なわないため、0006/0007 の「パニック回避」という制約は満たしつつ観測性を向上できる。

## 現状

### FrameSurface の Drop 実装

`src/vpl.rs:393-406`:

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

### 他の Drop 経路のエラー黙殺

同じパターンが以下にも存在する:

- `src/vpl.rs:433-438` `Session::Drop`: `let _ = self.lib.mfx_close(self.session);`（`mfx_unload` は戻り値が `()` のため対象外）
- `src/encode.rs:1266-1269` `Encoder::Drop`: `let _ = self.session.lib().mfx_video_encode_close(...);`
- `src/decode.rs:488-491` `Decoder::Drop`: `let _ = self.session.lib().mfx_video_decode_close(...);`

### Drop 外のエラー黙殺

- `src/decode.rs:593-611` `sync_and_drain`: `let _ = lib.mfx_video_core_sync_operation(...);` および `let _ = frame_surface.map_read();`

### 呼び出し元での安全対策

- `Encoder::encode` (`src/encode.rs:1103-1120`) は `frame_surface.map_write()?` → データコピー → `frame_surface.unmap()?` を明示的に呼び、成功時は `mapped = false` にしてから Drop に任せる。
- `Decoder::sync_and_callback` (`src/decode.rs:554-587`) は `frame_surface.map_read()?` → データ読み取り → `frame_surface` を Drop に任せる。

正常系では問題は顕在化していないが、異常系（GPU ハング後の cleanup 等）で検知手段がない。

### 想定される失敗シナリオ

- `Unmap` が失敗した状態のまま `Release` が呼ばれる → VPL 内部でどうなるかは実装依存
- `Release` の内部参照カウンタが 0 未満になろうとするケース
- すでに Release 済みの handle を再度 Release する

## 設計方針

**eprintln! で失敗を stderr に出力する。**

Drop 内で panic しない制約（Rust 標準の非推奨事項、0006/0007 の判断根拠）を守りつつ、エラー情報を失わないために `eprintln!` を用いる。本 crate は `[dependencies]` が空であり、`log` crate や `tracing` の導入は別 issue で検討する（`shiguredo-rust` スキルは `tracing` の使用を推奨しているが、依存ゼロ方針とのトレードオフ判断が必要なため）。

### 出力フォーマット

全箇所で統一したフォーマットを使用する:

```
[vpl-rs] <関数名> failed: status=<ステータス値>
```

具体例:

- `[vpl-rs] mfxFrameSurfaceInterface::Unmap failed: status=-1`
- `[vpl-rs] mfxFrameSurfaceInterface::Release failed: status=-1`
- `[vpl-rs] MFXClose failed: status=-1`
- `[vpl-rs] MFXVideoENCODE_Close failed: status=-1`
- `[vpl-rs] MFXVideoDECODE_Close failed: status=-1`
- `[vpl-rs] MFXVideoCORE_SyncOperation failed: status=-1`（sync_and_drain 内）

### Unmap 失敗後も Release を続行する

`Unmap` が失敗しても `Release` を続行するのは意図的な設計である。Unmap 失敗の原因が VPL 内部の一時的なエラーである可能性があるため、Release をスキップするとサーフェスリークが確定的に発生する。続行してエラーを観測可能にすることで、失敗の発生を検知しつつ、可能な限りリソースを解放する。

### sync_and_drain のエラー黙殺対応

`src/decode.rs:603-610` の `sync_and_drain` 内の SyncOperation と Map のエラー黙殺も本 issue で対応する。同期関数内の `let _ =` であり Drop ではないが、同じ問題パターンであるため。

### Encoder::Drop / Decoder::Drop でエラー後も続行することの妥当性

`mfx_video_encode_close` / `mfx_video_decode_close` 失敗後も `Session::Drop` で `MFXClose` + `MFXUnload` が走る。VPL の仕様上、Close 失敗後の MFXClose 呼び出しが安全かは明示されていないが、現行コードの挙動を変更するわけではなく、観測性を追加するだけであるため本 issue ではこの順序を維持する。

## 完了条件

以下すべてを満たす。

1. `FrameSurface::Drop` の `Unmap` / `Release` 失敗を `eprintln!` で観測可能にする。フォーマットは `[vpl-rs] mfxFrameSurfaceInterface::Unmap failed: status={}` など。
2. `Session::Drop` の `MFXClose` 失敗を同様に `eprintln!` で観測可能にする。
3. `Encoder::Drop` の `mfx_video_encode_close` 失敗を同様に `eprintln!` で観測可能にする。
4. `Decoder::Drop` の `mfx_video_decode_close` 失敗を同様に `eprintln!` で観測可能にする。
5. `sync_and_drain`（`src/decode.rs:603-610`）の `SyncOperation` / `map_read` 失敗を同様に `eprintln!` で観測可能にする。
6. `CHANGES.md` の `## develop` に `[FIX]` として「Drop / drain 経路での VPL API エラー黙殺を eprintln! 出力に変更」を追記する（バグ修正）。

注: 異常系 Drop テスト（Release 失敗を強制するテスト）は、`AGENTS.md` の「モック禁止」下では実装不能のため実施しない。正常系（Drop が panic しないこと）は既存テストで担保されている。

## 影響範囲

- `src/vpl.rs`（`FrameSurface::Drop`: Unmap / Release の `let _ =` → `eprintln!`、`Session::Drop`: MFXClose の `let _ =` → `eprintln!`）
- `src/encode.rs`（`Encoder::Drop`: mfx_video_encode_close の `let _ =` → `eprintln!`）
- `src/decode.rs`（`Decoder::Drop`: mfx_video_decode_close の `let _ =` → `eprintln!`、`sync_and_drain`: SyncOperation / map_read の `let _ =` → `eprintln!`）
- `CHANGES.md`
