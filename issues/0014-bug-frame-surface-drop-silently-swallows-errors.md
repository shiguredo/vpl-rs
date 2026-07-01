# FrameSurface::Drop が Unmap / Release 失敗を silent に潰しリソース破損を追跡できない

- Priority: High
- Created: 2026-07-01
- Completed: {YYYY-MM-DD}
- Model: Opus 4.7
- Branch: feature/fix-frame-surface-drop-silent-errors
- Polished: {YYYY-MM-DD}

## 目的

`FrameSurface::Drop` が `mfxFrameSurfaceInterface::Unmap` / `Release` の失敗を完全に silent に潰しているため、VPL 内部の参照カウンタが壊れる、または map 状態のまま release して二重 free / use-after-free / GPU リソースリークが発生した場合に、原因追跡ができない。少なくとも観測可能にして、致命的な状態遷移を早期に検出できるようにする。

## 優先度根拠

High。以下による。

- **リソース破損の隠蔽**: `Release` が失敗した瞬間に VPL 内部の参照カウンタが不整合になる可能性があり、後段で二重 free や use-after-free が起きても Rust 側から見ると「なぜかクラッシュした」以上の情報が得られない。
- **VPL 特有のリソース管理**: `mfxFrameSurfaceInterface::Release` は VPL のサーフェスプールの参照カウントを操作する API。ここが壊れると、以降のフレーム取得（`MFXMemory_GetSurfaceForEncode`）でプール枯渇や UB を招く。
- **開発時の検出手段が皆無**: 現在 `debug_assert!` や `eprintln!` すらないため、開発 / CI でも失敗が起きていることが分からない。

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

`let _ = ...` で戻り値を明示的に捨てている。エラーが発生してもログすら出ない。

### 想定される失敗シナリオ

- `Unmap` が失敗した状態のまま `Release` が呼ばれる → VPL 内部でどうなるかは実装依存
- `Release` の内部参照カウンタが 0 未満になろうとするケース（複数箇所から Release が呼ばれた実装バグ）
- サーフェスの内部状態が想定外（例: すでに Release 済みの handle を再度 Release する）

### Session / Drop 経路との比較

`src/vpl.rs:433-438` の `Session::Drop` も `let _ = self.lib.mfx_close(self.session);` で silent。同じ問題を持つが、Session は Encoder / Decoder のライフタイム末尾で 1 回だけ Drop されるため頻度は低い。FrameSurface は 1 フレームごとに Drop されるので、失敗の影響が広範囲。

### 呼び出し元での安全対策

- `Encoder::encode` (`src/encode.rs:1103-1120`) は `frame_surface.map_write()?` → データコピー → `frame_surface.unmap()?` を明示的に呼び、成功時は `mapped = false` にしてから Drop に任せる（Release のみ実行される）。
- `Decoder::sync_and_callback` (`src/decode.rs:554-587`) は `frame_surface.map_read()?` → データ読み取り → `frame_surface` を Drop に任せる（Unmap + Release が実行される）。

正常系はテストで pass しているので、当面の実運用で問題にはなっていない可能性がある。しかし異常系（例: GPU ハング後の cleanup）で問題が起きても検知できない。

## 設計方針

### 案 A: eprintln! でログ出力（推奨、簡易）

Drop で失敗したら `eprintln!("mfxFrameSurfaceInterface::Release failed: status={}", status)` を出す。

- 長所: 実装コスト最小。テストや実運用で発生したら stderr で観測できる。
- 短所: `log` crate を使っていないため、アプリ側でログルーティングを制御できない。ただし本 crate は `[dependencies]` が空で、外部ログ crate を導入する方針かどうかは要検討。

### 案 B: debug_assert! で開発時検出

`debug_assert!(status == sys::mfxStatus_MFX_ERR_NONE, ...)` で開発時のみ panic させる。

- 長所: リリースビルドではオーバーヘッドなし。
- 短所: リリースビルド（本番）では検出不能。

### 案 C: FrameSurface に `debug_check` の外部 API を追加

`FrameSurface::release(&mut self) -> Result<(), Error>` のような明示的な release API を追加し、呼び出し側でエラーを判定させる。Drop は最後の安全網に留める。

- 長所: 正常経路では確実にエラーを検出できる。
- 短所: 既存 API の変更が必要。テストコードも修正。

推奨は **案 A + 案 B の併用**。実装コストが小さく、開発時と本番の両方で最低限の観測性が得られる。将来的に `log` crate を導入するなら `log::warn!` に切り替える。

### 併せて対応: Session::Drop も同じ扱い

`src/vpl.rs:433-438` の `Session::Drop` の `MFXClose` 失敗も同じパターンで観測可能にする。

### 併せて対応: Encoder::Drop / Decoder::Drop も同じ扱い

`src/encode.rs:1266-1269` の `mfx_video_encode_close` 失敗、`src/decode.rs:488-491` の `mfx_video_decode_close` 失敗も同じ扱い。

## 完了条件

以下すべてを満たす。

1. `FrameSurface::Drop` の `Unmap` / `Release` 失敗が `eprintln!` などで観測可能になる。
2. `debug_assert!` を併用し、開発ビルドでは失敗が即座に panic として検出される。
3. `Session::Drop` / `Encoder::Drop` / `Decoder::Drop` も同じ扱いに揃える。
4. `#[cfg(test)] mod tests` に「Drop 経路の失敗ログが観測されるユニットテスト」を追加する（`FrameSurface` を強制的に不正な状態にして Drop するテストなど、モック不使用で実現可能なもの）。
5. `CHANGES.md` の `## develop` に `[UPDATE]` として追記する（バグ修正ではなく観測性改善）。

## 影響範囲

- `src/vpl.rs`（`FrameSurface::Drop` / `Session::Drop`）
- `src/encode.rs`（`Encoder::Drop`）
- `src/decode.rs`（`Decoder::Drop`）
- `CHANGES.md`

## 参考

- `/review-code` の致命的指摘 F7
- 関連: 将来的な `log` crate 導入の是非は別 issue で議論
