# VPL ローダー初期化シーケンスの 3 箇所重複を LoaderBuilder に集約する

- Priority: Medium
- Created: 2026-07-01
- Model: Opus 4.7
- Branch: feature/refactor-extract-vpl-loader-builder
- Polished: 2026-07-01

## 目的

`MFXLoad + MFXCreateConfig × 2 + MFXSetConfigFilterProperty × 2` のシーケンスが `src/vpl.rs` / `src/adapter.rs` / `src/codec_info.rs` の 3 箇所で完全に重複している。プロパティ名のバイトリテラルまで同一で、libvpl 側の property 追加時に 3 箇所同期が必須になる。共通の `LoaderBuilder`（内部 API）に集約し、DRY を取る。

## 優先度根拠

Medium。以下による。

- **将来のバグ源**: libvpl 2.18 以降で property が追加された時、3 箇所同期を忘れると片方でしか有効にならない不整合が発生する。特に `adapter.rs` は `LoaderGuard` の RAII を使っているが `codec_info.rs` は生の `MFXUnload` を失敗パスごとに散在配置していて、失敗パスでリソースリークが発生する可能性がある（現状も一部リスク）。
- **保守負荷**: 3 箇所のバイトリテラル・エラーハンドリング・cfg 分岐を目視で揃える負荷。
- **Priority は Medium**: 現状は 3 箇所が同じ動作をしているため直接的なバグはない。設計負債の返済として。

## 現状

### 重複箇所 3 つ

- **`src/vpl.rs:67-103` (`VplLibrary::create_session`)**: `MFXLoad` → HW impl filter → DRM render node filter → `MFXCreateSession(loader, 0, ...)`。失敗パスで `MFXUnload` を明示呼び出し。
- **`src/adapter.rs:106-124` (`list_adapters`)**: `MFXLoad` → HW impl filter のみ → `MFXEnumImplementations` loop。DRM フィルタは付けない。`LoaderGuard` の RAII で `MFXUnload` を管理。
- **`src/codec_info.rs:152-188` (`supported_codecs`)**: `MFXLoad` → HW impl filter → DRM render node filter → `MFXEnumImplementations(0, ...)`。失敗パスで `MFXUnload` を明示呼び出し（RAII なし）。

### プロパティ名のバイトリテラル

以下の 2 つが 3 箇所（HW impl filter は 3 箇所、DRM フィルタは 2 箇所）で完全一致。

```rust
let name = b"mfxImplDescription.Impl\0";
let drm_name = b"mfxExtendedDeviceId.DRMRenderNodeNum\0";
```

### エラーハンドリング

- `Error::new_custom("MFXLoad", "returned null")`
- `Error::new_custom("MFXCreateConfig", "returned null")`
- `Error::from_mfx(status, "MFXSetConfigFilterProperty")`

これらのメッセージも 3 箇所で完全一致。

### リソース解放の不統一

- `adapter.rs` は `LoaderGuard` で RAII（`src/adapter.rs:253-263`）
- `codec_info.rs` は生の `MFXUnload` を各失敗パスに `unsafe { sys::MFXUnload(loader) };` と散在配置（`src/codec_info.rs:161, 169, 186, 200-208, 213-217, 221-225`）
- `vpl.rs` の `create_session` は成功時に `Session` の Drop で管理、失敗時は各パスで明示呼び出し

同じロジックで異なる管理方式は保守しにくく、失敗パスでリーク混入のリスクが高い。

## 設計方針

### 案 A: `LoaderBuilder`（内部 API）を新設する（推奨）

`src/vpl.rs` に共通の `LoaderBuilder` を導入する。`LoaderBuilder` は `mfxLoader` のライフタイムを管理し、Drop で `MFXUnload` を呼ぶ。

```rust
pub(crate) struct LoaderBuilder {
    loader: sys::mfxLoader,
    render_node: u32,
    consumed: bool,  // create_session 呼び出し後に二重解放を防ぐ
}

impl LoaderBuilder {
    pub(crate) fn new() -> Result<Self, Error> {
        // MFXLoad
    }

    pub(crate) fn require_hardware(&self) -> Result<(), Error> {
        // MFXCreateConfig + MFXSetConfigFilterProperty("mfxImplDescription.Impl", HARDWARE)
    }

    pub(crate) fn require_drm_render_node(&mut self, render_node: u32) -> Result<(), Error> {
        self.render_node = render_node;
        // MFXCreateConfig + MFXSetConfigFilterProperty("mfxExtendedDeviceId.DRMRenderNodeNum", render_node)
    }

    pub(crate) fn create_session(mut self) -> Result<Session, Error> {
        // loader の所有権を Session に移す。成功したら consumed = true にして
        // LoaderBuilder::Drop で二重に MFXUnload しないようにする。
        // 失敗時は consumed = false のまま Drop に任せる。
        let session = /* MFXCreateSession(self.loader, 0, ...) */;
        self.consumed = true;
        Ok(Session { lib: ..., loader: self.loader, session })
    }

    pub(crate) fn as_loader(&self) -> sys::mfxLoader {
        self.loader
    }
}

impl Drop for LoaderBuilder {
    fn drop(&mut self) {
        if !self.consumed {
            unsafe { sys::MFXUnload(self.loader); }
        }
    }
}
```

各呼び出し元の移行:

- **`create_session`**: `LoaderBuilder::new()?` → `require_hardware()` → `require_drm_render_node(rn)` → `create_session()`（builder は consume）
- **`list_adapters`**: `LoaderBuilder::new()?` → `require_hardware()` → `as_loader()` で loader を取得し、従来の `MFXEnumImplementations` ループを行う。`HdlGuard` は維持するが、`loader` フィールドを `sys::mfxLoader` の値コピーに変更する（`as_loader()` から取得）。
  - `HdlGuard` の `MFXDispReleaseImplDescription(loader, hdl)` は `LoaderBuilder` Drop（`MFXUnload`）より前に呼ばれる必要がある。`HdlGuard` を `LoaderBuilder` より先に Drop させることで順序を保証する。
- **`supported_codecs`**: `create_session` と同様のパターン。`MFXDispReleaseImplDescription` は `LoaderBuilder` Drop の前に完了させる。

`enumerate_impls` メソッドは `list_adapters` が index 1 つにつき `IMPLDESCSTRUCTURE` と `DEVICE_ID_EXTENDED` の 2 回の `MFXEnumImplementations` を必要とするため、単純なコールバックでは置き換えられない。`as_loader()` 経由で各呼び出し元が直接ループを書く。

注意: `as_loader()` は生の `sys::mfxLoader` を返す。`LoaderBuilder` が Drop されてからこのポインタを使うと use-after-free になるため、呼び出し元は `LoaderBuilder` の生存期間中にのみ `as_loader()` の戻り値を使用すること。

- 長所: 3 箇所の `MFXLoad` / `MFXCreateConfig` / `MFXSetConfigFilterProperty` を 1 箇所に集約。RAII で失敗パスのリーク防止。
- 短所: 内部 API の増設。既存の `LoaderGuard`（adapter.rs）は削除、`HdlGuard` は loader 所有権を除去して維持。

### 案 B: 現状維持 + コメントで注意喚起

3 箇所を残し、コメントで「他の 2 箇所と同期を取ること」と注意書き。

- 短所: 実際に同期漏れがいずれ発生する。

推奨は **案 A**。

## 完了条件

以下すべてを満たす。

1. `src/vpl.rs` に `LoaderBuilder`（あるいは同等の内部 API）が追加される。
2. `src/vpl.rs::create_session` / `src/adapter.rs::list_adapters` / `src/codec_info.rs::supported_codecs` の 3 箇所が `LoaderBuilder` を使う実装に置き換わる。
3. `adapter.rs` の `LoaderGuard` / `HdlGuard` は削除して `LoaderBuilder` に統合するか、両立可能なら残す（`HdlGuard` は `MFXDispReleaseImplDescription` 用なので残す）。
4. `codec_info.rs` の失敗パスに散在していた `MFXUnload` 呼び出しが `LoaderBuilder` の Drop に集約される。
5. 既存のラウンドトリップテスト / adapter テストが全て pass する。
6. `CHANGES.md` の `## develop` に `[UPDATE]` として追記する（内部リファクタで公開 API 変更なし）。

## 影響範囲

- `src/vpl.rs`（`LoaderBuilder` 追加、`create_session` 書き換え）
- `src/adapter.rs`（`list_adapters` 書き換え、`LoaderGuard` の扱い）
- `src/codec_info.rs`（`supported_codecs` 書き換え、生の `MFXUnload` 削除）
- `CHANGES.md`

## 参考

- 関連コード: `src/adapter.rs:253-277` の `LoaderGuard` / `HdlGuard`（既存 RAII）
