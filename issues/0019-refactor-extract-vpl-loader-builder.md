# VPL ローダー初期化シーケンスの 3 箇所重複を LoaderBuilder に集約する

- Priority: Medium
- Created: 2026-07-01
- Model: Opus 4.7
- Branch: feature/refactor-extract-vpl-loader-builder
- Polished: 2026-08-02

## 目的

`MFXLoad + MFXCreateConfig + MFXSetConfigFilterProperty` のシーケンスが `src/vpl.rs` の `VplLibrary::create_session` / `src/adapter.rs` の `list_adapters` / `src/codec_info.rs` の `supported_codecs` の 3 箇所で重複している。共通部分は「HW 実装フィルタの設定」で 3 箇所、DRM render node フィルタは `create_session` と `supported_codecs` の 2 箇所。プロパティ名のバイトリテラルまで同一で、libvpl 側の property 追加時に 3 箇所同期が必須になる。共通の `LoaderBuilder`（内部 API）に集約し、DRY を取る。

## 優先度根拠

Medium。以下による。

- **将来のバグ源**: 将来 property が追加された時、3 箇所同期を忘れると片方でしか有効にならない不整合が発生する。特に `adapter.rs` は `LoaderGuard` の RAII を使っているが `codec_info.rs` は生の `MFXUnload` を失敗パスごとに散在配置していて、panic 時の unwind 経路と将来の失敗パス追加時に解放漏れが発生しやすい構造になっている。
- **保守負荷**: 3 箇所のバイトリテラル・エラーハンドリング・cfg 分岐を目視で揃える負荷。
- **Priority は Medium**: 現状は 3 箇所が同じ動作をしているため直接的なバグはない。設計負債の返済として。

## 現状

### 重複箇所 3 つ

- **`src/vpl.rs` の `VplLibrary::create_session`**: `MFXLoad` → HW impl filter → DRM render node filter → `MFXCreateSession(loader, 0, ...)`。失敗パスで `MFXUnload` を明示呼び出し。
- **`src/adapter.rs` の `list_adapters`**: `MFXLoad` → HW impl filter のみ → `MFXEnumImplementations` loop。DRM フィルタは付けない。`LoaderGuard` の RAII で `MFXUnload` を管理。
- **`src/codec_info.rs` の `supported_codecs`**: `MFXLoad` → HW impl filter → DRM render node filter → `MFXEnumImplementations(0, ...)`。失敗パスで `MFXUnload` を明示呼び出し（RAII なし）。

### プロパティ名のバイトリテラル

以下の 2 つは、HW impl filter が 3 箇所、DRM フィルタが 2 箇所で完全一致。

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

- `adapter.rs` は `LoaderGuard` で RAII
- `codec_info.rs` は生の `MFXUnload` を各失敗パスに `unsafe { sys::MFXUnload(loader) };` と散在配置（`supported_codecs` 内の失敗パス全て。現在は全パスで呼び出されておりリークは発生していないが、構造的に漏れやすい）
- `vpl.rs` の `create_session` は成功時に `Session` の Drop で管理、失敗時は各パスで明示呼び出し

同じロジックで異なる管理方式は保守しにくく、失敗パスでリーク混入のリスクが高い。

## 設計方針

### 案 A: `LoaderBuilder`（内部 API）を新設する（推奨）

`src/vpl.rs` に共通の `LoaderBuilder` を導入する。`LoaderBuilder` は `mfxLoader` のライフタイムを管理し、Drop で `MFXUnload` を呼ぶ。

```rust
pub(crate) struct LoaderBuilder {
    lib: VplLibrary,
    loader: sys::mfxLoader,
    render_node: Option<u32>,
    consumed: bool,  // build 呼び出し後に二重解放を防ぐ
}

impl LoaderBuilder {
    pub(crate) fn new(lib: VplLibrary) -> Result<Self, Error> {
        // MFXLoad
    }

    pub(crate) fn require_hardware(&self) -> Result<(), Error> {
        // MFXCreateConfig + MFXSetConfigFilterProperty("mfxImplDescription.Impl", HARDWARE)
    }

    pub(crate) fn require_drm_render_node(&mut self, render_node: u32) -> Result<(), Error> {
        self.render_node = Some(render_node);
        // MFXCreateConfig + MFXSetConfigFilterProperty("mfxExtendedDeviceId.DRMRenderNodeNum", render_node)
    }

    pub(crate) fn build(mut self) -> Result<Session, Error> {
        // loader の所有権を Session に移す。成功したら consumed = true にして
        // LoaderBuilder::Drop で二重に MFXUnload しないようにする。
        // 失敗時は consumed = false のまま Drop に任せる。
        // MFX_ERR_NOT_FOUND のときは render_node フィールドを使って
        // "no Intel HW implementation found for DRM render node {render_node}" を返す
        // （現行の create_session のエラーメッセージ挙動を維持する。
        //  このメッセージは tests/test_adapter.rs の Encoder 経由テストが検証する公開挙動）。
        let session = /* MFXCreateSession(self.loader, 0, ...) */;
        self.consumed = true;
        Ok(Session {
            lib: self.lib,
            loader: self.loader,
            session,
        })
    }

    /// 生の loader を返す（unsafe）。呼び出し元は本オブジェクトの生存期間内でのみ
    /// 戻り値を使用すること（生存期間外の使用は use-after-free）。
    pub(crate) unsafe fn as_loader(&self) -> sys::mfxLoader {
        self.loader
    }
}

impl Drop for LoaderBuilder {
    fn drop(&mut self) {
        if !self.consumed {
            self.lib.mfx_unload(self.loader);
        }
    }
}
```

設計上の注意:

- `sys::mfxLoader` は `*mut`（Copy）のため、`LoaderBuilder`（Drop 実装型）から `Session` への loader の引き渡しはコピーで行える（部分ムーブ E0509 の問題は発生しない）。
- `Session` は既に `loader: sys::mfxLoader` フィールドを持ち、Drop で `MFXUnload` を実行するため、受け側の変更は不要。
- `LoaderBuilder::Drop` の `MFXUnload` は `self.lib.mfx_unload(self.loader)` 経由で呼ぶ（既存の VPL 関数呼び出しの閉じ込めパターンに統一する。`Session::Drop` と同じ方式）。
- `render_node: Option<u32>` は `require_drm_render_node` で設定される。`build()` の NOT_FOUND エラーメッセージでは `render_node` の値（未設定なら `"no Intel HW implementation found (DRM render node not set)"`）を使用する。現行の呼び出し元（`create_session` / `supported_codecs`）は常に先に `require_drm_render_node` を呼ぶ。
- `require_hardware()` / `require_drm_render_node()` は各 1 回のみ呼び出す契約とする（2 回目以降の呼び出しは `MFXCreateConfig` フィルタの重複設定になるため。複数回呼び出しの誤りはコンパイルでは検出できないため、実装時にガードを入れるかコメントで明記する）。
- `adapter.validate()`（`DrmRenderNode(0)` 拒否）は呼び出し元（`VplLibrary::create_session` と `supported_codecs`）で維持する（`tests/test_adapter.rs` が `err.function() == "AdapterSelector::validate"` を検証するテスト契約のため。`require_drm_render_node` は素の `u32` を受け取る）。

各呼び出し元の移行:

- **`create_session`**: `LoaderBuilder::new(*self)?` → `require_hardware()` → `require_drm_render_node(rn)` → `build()`（builder は consume）。`adapter.validate()` は呼び出し元で維持。
- **`list_adapters`**: `LoaderBuilder::new()?` → `require_hardware()` → `unsafe { as_loader() }` で loader を取得し、従来の `MFXEnumImplementations` ループを行う。`HdlGuard` は維持するが、loader フィールドの取得元を `as_loader()` に変更する（値コピー保持は現状のまま）。
  - `HdlGuard` の `MFXDispReleaseImplDescription(loader, hdl)` は `LoaderBuilder` Drop（`MFXUnload`）より前に呼ばれる必要がある。`HdlGuard` を `LoaderBuilder` より先に Drop させる（変数宣言順により逆順 Drop が保証される）ことで順序を保証する。
- **`supported_codecs`**: `LoaderBuilder::new(VplLibrary::load()?)?` → `require_hardware()` → `require_drm_render_node(rn)` → `unsafe { as_loader() }` で loader を取得し、従来の `MFXEnumImplementations` ループを行う。`MFXDispReleaseImplDescription` は従来どおり明示呼び出しで維持する（`HdlGuard` は adapter.rs の定義であり、codec_info.rs から参照するには移動が必要なため。明示呼び出しが最小変更）。**NOT_FOUND エラーメッセージ（render node 番号入り）は `supported_codecs` 内の `MFXEnumImplementations` エラーハンドリングで維持する**（`build()` は `Session` を返すため `supported_codecs` からは呼ばれない。render node 番号は関数ローカルの変数を使用する）。エラーメッセージのフォーマット文字列は `LoaderBuilder` に共通メソッド（例: `fn not_found_message(render_node: u32) -> String`）を用意し、`build()` と `supported_codecs` の 2 箇所で共有する（同期漏れ防止）。

`as_loader()` 経由で各呼び出し元が `MFXEnumImplementations` ループを直接書く（`list_adapters` は index 1 つにつき `IMPLDESCSTRUCTURE` と `DEVICE_ID_EXTENDED` の 2 回の `MFXEnumImplementations` を必要とするため、単純なコールバックでは置き換えられない）。

- 長所: 3 箇所の `MFXLoad` / `MFXCreateConfig` / `MFXSetConfigFilterProperty` を 1 箇所に集約。RAII で失敗パスのリーク防止。
- 短所: 内部 API の増設。既存の `LoaderGuard`（adapter.rs）は削除し、`HdlGuard` は loader の値コピー保持を維持（取得元を `as_loader()` に変更）。

### 案 B（却下）: 現状維持 + コメントで注意喚起

3 箇所を残し、コメントで「他の 2 箇所と同期を取ること」と注意書き。

- 短所: 実際に同期漏れがいずれ発生する。

推奨は **案 A**。

### 適用順序（他 issue との関係）

- **issue 0015**（非 Linux ガード）: 同じ `src/adapter.rs` を書き換える（0015 は非 Linux 版 `list_adapters` 削除と cfg 分岐）。**適用順序は 0015 を先に適用する**（0015 適用後は非 Linux が `compile_error!` でビルド拒否されるため、`LoaderBuilder` に cfg 分岐は不要になる）。
- **issue 0018**（型統合）: 同じ `src/codec_info.rs` を編集する（0018 は `query_encoding_profiles` / `match_profiles`、本 issue は `supported_codecs`）。関数単位で重ならないため、適用順序の競合は限定的。
- **issue 0014**（Drop 経路のエラー観測）: 同じ `src/vpl.rs` を編集する（0014 は `Session::Drop` の `MFXClose`）。`Session` 構造体の変更は本 issue にはないため競合なし。

## 完了条件

以下すべてを満たす。

1. `src/vpl.rs` に `LoaderBuilder` が追加される。
2. `src/vpl.rs` の `create_session` / `src/adapter.rs` の `list_adapters` / `src/codec_info.rs` の `supported_codecs` の 3 箇所が `LoaderBuilder` を使う実装に置き換わる。`adapter.validate()` の呼び出し位置と NOT_FOUND エラーメッセージの挙動は維持する。
3. `adapter.rs` の `LoaderGuard` は削除して `LoaderBuilder` に統合する。`HdlGuard` は `MFXDispReleaseImplDescription` 用のため維持する（loader フィールドの取得元を `as_loader()` に変更）。
4. `codec_info.rs` の失敗パスに散在していた `MFXUnload` 呼び出しが `LoaderBuilder` の Drop に集約される。
5. 既存のラウンドトリップテスト / adapter テストが全て pass する。なお、現状のテストは issue 0023 が指摘する silent pass の構造を含むため、0023 適用後（テストの検証力向上後）も pass することを確認する。
6. `CHANGES.md` の `## develop` の `### misc` サブセクションに `[UPDATE]` として追記する（内部リファクタで公開 API 変更なし。機能に直接影響しない変更のため `### misc` に記載する）。

## 影響範囲

- `src/vpl.rs`（`LoaderBuilder` 追加、`create_session` 書き換え）
- `src/adapter.rs`（`list_adapters` 書き換え、`LoaderGuard` 削除、`HdlGuard` の loader 取得元変更）
- `src/codec_info.rs`（`supported_codecs` 書き換え、生の `MFXUnload` 削除）
- `CHANGES.md`

## 参考

- 関連 issue: 0015（非 Linux ガード。同じ `src/adapter.rs` を書き換えるため、**適用順序は 0015 を先に適用する**）
- 関連 issue: 0018（型統合。同じ `src/codec_info.rs` を編集するため、適用順序に注意）
- 関連 issue: 0014（Drop 経路のエラー観測。同じ `src/vpl.rs` を編集する）
- 関連 issue: 0023（silent pass テストの修正。完了条件 5 の検証力に関連）
