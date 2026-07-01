# VPL ローダー初期化シーケンスの 3 箇所重複を LoaderBuilder に集約する

- Priority: Medium
- Created: 2026-07-01
- Completed: {YYYY-MM-DD}
- Model: Opus 4.7
- Branch: feature/refactor-extract-vpl-loader-builder
- Polished: {YYYY-MM-DD}

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

`src/vpl.rs` に共通の `LoaderBuilder` を導入する。

```rust
pub(crate) struct LoaderBuilder {
    loader: sys::mfxLoader,
    // MFXCreateConfig は複数保持可能だが VPL 側で loader Drop 時に一括破棄されるので追跡不要
}

impl LoaderBuilder {
    pub(crate) fn new() -> Result<Self, Error> {
        // MFXLoad
    }

    pub(crate) fn require_hardware(&self) -> Result<(), Error> {
        // MFXCreateConfig + MFXSetConfigFilterProperty("mfxImplDescription.Impl", HARDWARE)
    }

    pub(crate) fn require_drm_render_node(&self, render_node: u32) -> Result<(), Error> {
        // MFXCreateConfig + MFXSetConfigFilterProperty("mfxExtendedDeviceId.DRMRenderNodeNum", render_node)
    }

    pub(crate) fn create_session(self) -> Result<Session, Error> {
        // MFXCreateSession(loader, 0, ...) → Session を返す。self は consume される
    }

    pub(crate) fn enumerate_impls<F>(&self, callback: F) -> Result<(), Error>
    where
        F: FnMut(usize) -> Result<(), Error>,
    {
        // MFXEnumImplementations loop
    }

    pub(crate) fn as_loader(&self) -> sys::mfxLoader {
        self.loader
    }
}

impl Drop for LoaderBuilder {
    fn drop(&mut self) {
        // MFXUnload
    }
}
```

以下のように各呼び出し元をリファクタする。

- `create_session`:

```rust
let builder = LoaderBuilder::new()?;
builder.require_hardware()?;
builder.require_drm_render_node(render_node)?;
builder.create_session()  // Session を返す（builder は consume）
```

- `list_adapters`:

```rust
let builder = LoaderBuilder::new()?;
builder.require_hardware()?;
builder.enumerate_impls(|i| { /* per impl 処理 */ })?;
// builder は関数スコープの Drop で MFXUnload
```

- `supported_codecs`:

```rust
let builder = LoaderBuilder::new()?;
builder.require_hardware()?;
builder.require_drm_render_node(render_node)?;
builder.enumerate_impls(|i| { /* per impl 処理 */ })?;
```

- 長所: 3 箇所を 1 箇所に集約。RAII で失敗パスのリーク防止。
- 短所: 内部 API の増設。既存の `LoaderGuard` / `HdlGuard` との整合を取る必要がある。

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

- `/review-code` の致命的指摘 F12
- 関連コード: `src/adapter.rs:253-277` の `LoaderGuard` / `HdlGuard`（既存 RAII）
