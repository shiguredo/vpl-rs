# mfxFrameSurface1 の統一ラッパーを作成し lib.rs をモジュール分離する

Created: 2026-05-14
Completed: 2026-05-14
Model: deepseek-v4-pro

## 背景

現在 `encode.rs` に `SurfaceGuard`、`decode.rs` に `DecodedSurfaceGuard` という 2 つの RAII ガード構造体が存在する。また `encode.rs` と `decode.rs` には `CloseGuard` 構造体も重複して定義されている。さらに `lib.rs` には `VplLibrary` 構造体とその全メソッド、および `frame_type` / `gop_opt_flag` の `pub mod` が直接記述されており、クレートルートとしての責務を超えている。

具体的な問題:

1. **コード重複**: `SurfaceGuard` / `DecodedSurfaceGuard` の `Drop` 実装は同一（null チェック → mapped なら Unmap → Release）。一方は `map_write()` / `unmap()`、他方は `map_read()` のみを持つが、RAII ガードとしての本質は同じ。また `CloseGuard` も両ファイルで重複している
2. **生ポインタの露出**: `*mut sys::mfxFrameSurface1` が `encode_frame_async` の引数や `DecodeSyncData` のフィールドなど、`VplLibrary` の FFI 呼び出し以外の場所でも露出している
3. **モジュール構成の混在**: `lib.rs` に実装詳細（`VplLibrary`）と公開 API（`frame_type` / `gop_opt_flag`）が同居し、責務が不明瞭

## 要件

### 1. `NonNull` ベースの統一ラッパー `FrameSurface` を新規ファイル `src/vpl.rs` に作成する

#### 構造体

```rust
#[doc(hidden)]
pub struct FrameSurface {
    lib: VplLibrary,
    surface: std::ptr::NonNull<sys::mfxFrameSurface1>,
    mapped: bool,
}
```

- `lib`: `VplLibrary` を値として保持する
- `surface`: `NonNull` により null ポインタを型レベルで排除し、`Drop` 内の null チェックを不要にする
- `mapped`: 初期値 `false`
- `Send`: `VplLibrary` が `Copy` かつ `mfxFrameSurface1` が bindgen により `Send` なため、`FrameSurface` は自動的に `Send` になる（意図した動作）
- 可視性: `#[doc(hidden)]` 付きの `pub`

#### メソッド

- `new(lib: VplLibrary, surface: *mut sys::mfxFrameSurface1) -> Result<Self, Error>`
  - `surface` が null なら `Error` を返す
  - `NonNull::new(surface).ok_or_else(...)` で変換する
- `fn as_ptr(&self) -> *mut sys::mfxFrameSurface1`
  - `NonNull::as_ptr()` で生ポインタを返す。`VplLibrary` の FFI メソッドに渡す場合や、Map 済み期間中のフィールドアクセスに使用する
- `map_write(&mut self) -> Result<(), Error>`
  - 内部的に `mfxFrameSurfaceInterface::Map(MFX_MAP_WRITE)` を呼び、`mapped = true` にする
  - 既に `mapped == true` なら `Error`（`Error::new_custom` で二重呼び出しを示すメッセージ）を返す
- `map_read(&mut self) -> Result<(), Error>`
  - 内部的に `mfxFrameSurfaceInterface::Map(MFX_MAP_READ)` を呼び、`mapped = true` にする
  - 既に `mapped == true` なら `Error` を返す
- `unmap(&mut self) -> Result<(), Error>`
  - 内部的に `mfxFrameSurfaceInterface::Unmap` を呼び、`mapped = false` にする
  - 既に `mapped == false` なら `Error` を返す

#### Drop 実装

- `mapped == true` なら `Unmap` → 常に `Release` を実行する
- `Unmap` と `Release` のエラーは `let _ =` で破棄する（現行コードと同一方針。`Drop` 内パニックを避けるため）

#### 状態遷移

```
new → (mapped=false)
 ├── map_read / map_write → Ok → mapped=true
 │    ├── map_read / map_write → Err (already mapped)
 │    ├── unmap → Ok → mapped=false → drop → Release
 │    │    └── unmap → Err (not mapped)
 │    └── drop → Unmap + Release
 ├── unmap → Err (not mapped)
 └── drop → Release
```

#### 型レベルの制約に関する注意

`FrameSurface` は null 非許容を型レベルで保証するが、Map 済みかどうかは実行時の `mapped: bool` に依存する。`map_read` / `map_write` による Map 後、`read_decoded_surface_inner` 等で生ポインタ経由のデータ読み取りが可能になるが、Map 済みであることの保証は呼び出し元の規律に委ねられる。

### 2. `encode.rs` と `decode.rs` で `*mut sys::mfxFrameSurface1` を `FrameSurface` に置き換える

**原則**: `VplLibrary` のメソッド内部（FFI 呼び出しの引数として生ポインタを渡す箇所）を除き、`*mut sys::mfxFrameSurface1` を直接扱わない。すべて `FrameSurface` 経由で操作する。ただし `encode()` 内の Map 済み期間中の `mfxFrameSurface1` 内部フィールドへの unsafe アクセス（`Data.TimeStamp` 設定、プレーンポインタへのデータコピー）は `frame_surface.as_ptr()` 経由で引き続き unsafe ブロック内で行う（これらは Map/Unmap が保証する安全な期間内の操作であるため）。

#### encode 側

- `SurfaceGuard` 構造体を削除し、`FrameSurface` に置き換える
- `encode()` メソッド内: `SurfaceGuard::new` → `FrameSurface::new`、`surface_guard.surface()` → `frame_surface.as_ptr()`
- `encode_frame_async` の `surface` 引数型を `*mut sys::mfxFrameSurface1` → `Option<&FrameSurface>` に変更する:
  - `encode()` からは `Some(&frame_surface)` で呼ぶ。メソッド内部で `frame_surface.as_ptr()` により生ポインタを取得する
  - `finish()` からは `None` で呼ぶ（ドレイン時は null surface が必要なため）。メソッド内部で `None` の場合は `std::ptr::null_mut()` を渡す
  - 呼び出し元からのポインタ取得は `encode_frame_async` 内部に閉じる

#### decode 側

`DecodeSyncData` は `FrameSurface` を所有する。`FrameSurface` の構築は main スレッド側（`decode_bitstream` / `finish`）で行い、Worker スレッド側（`sync_and_callback` / `sync_and_drain`）は構築済みの `FrameSurface` を受け取る。`sync_and_drain` 内の空 `DecodeSyncData`（null syncp / null surface）は発生しなくなるため削除する（null の場合は事前にフィルタされる）。

- `DecodedSurfaceGuard` 構造体を削除し、`FrameSurface` に置き換える
- `DecodeSyncData.out_surface: *mut sys::mfxFrameSurface1` → `frame_surface: FrameSurface` に変更する。これに伴い `DecodeSyncData` の `unsafe impl Send` を削除する（`FrameSurface` が自動的に `Send` なため不要になる）
- `DecodeSyncData` 構築箇所（`decode_bitstream`、`finish`）:
  - `FrameSurface::new(self.lib, out_surface)` で構築し、エラー時は呼び出し元で処理する
  - `decode_bitstream` 側: `out_surface` が null なら `FrameSurface::new` が `Err` を返すため、`?` で伝播する
  - `finish()` のドレインループ側: `FrameSurface::new` が `Err` を返した場合は `continue` で無視する（ドレイン時に `syncp` 非 null かつ `out_surface` が null になる異常系は破棄して次のフレームへ）
- `sync_and_callback`:
  - `DecodedSurfaceGuard::new(lib, out_surface)` の行を削除する（`frame_surface` は `DecodeSyncData` から取得済み）
  - `out_surface.is_null()` のチェックを削除する（`FrameSurface::new` により null は構築時に弾かれる）
  - `surface_guard.map_read()` → `frame_surface.map_read()`、`surface_guard.surface()` → `frame_surface.as_ptr()`
  - `read_decoded_surface` の引数を `&surface_guard` → `&frame_surface` に変更
- `read_decoded_surface`: 引数型を `&DecodedSurfaceGuard` → `&FrameSurface` に変更し、内部の `surface_guard.surface()` → `frame_surface.as_ptr()` に変更する
- `read_decoded_surface_inner`: 引数型を `*mut sys::mfxFrameSurface1` → `&FrameSurface` に変更し、関数冒頭で `unsafe { &*frame_surface.as_ptr() }` により参照を取得する。`# Safety` コメントを更新し、「`out_surface` が null でないことは型レベルで保証される。Map 済みであることと、戻り値の `DecodedFrame` が生存している間サーフェスデータが有効であることの保証は呼び出し元の責任」とする

### 3. `lib.rs` から `vpl.rs` へ VPL 関連コードを移動する

#### `src/vpl.rs`（新規作成）に含めるもの

- `VplLibrary` 構造体とその全 `impl` ブロック（`lib.rs` から移動）
- `frame_type` モジュールと `gop_opt_flag` モジュール（`lib.rs` から移動）
- `FrameSurface` 構造体とその全 `impl` ブロック（新規）
- `CloseGuard` 構造体（encode 版・decode 版を統合し `close_encoder: bool` で切り替える）

#### `src/lib.rs` の変更後

- クレートドキュメント（`//!` コメント、doctest）はそのまま残す
- `mod vpl;` 宣言を追加
- `VplLibrary` / `frame_type` / `gop_opt_flag` の定義を削除する
- `#[doc(hidden)] pub mod ffi` はそのまま残す
- `BUILD_VERSION` 定数は `sys` への参照のみを含むため `lib.rs` に残す
- 再エクスポート:
  - 公開 API: `pub use vpl::frame_type;`、`pub use vpl::gop_opt_flag;`（外部クレートからのアクセスパスを維持するため `[UPDATE]` 扱い）
  - クレート内: `pub(crate) use vpl::VplLibrary;`、`pub(crate) use vpl::CloseGuard;`（既存の import パスを維持）
  - `FrameSurface` は `lib.rs` で再エクスポートせず、各モジュールが `use crate::vpl::FrameSurface;` で直接インポートする

#### `encode.rs` / `decode.rs` の import 変更

- `lib.rs` に `pub(crate) use vpl::VplLibrary;` を置くため、既存の `use crate::VplLibrary` は変更不要
- 追加: `use crate::vpl::FrameSurface;`

#### `codec_info.rs` について

`codec_info.rs` は `use crate::{AdapterSelector, Error, VplLibrary, sys};` で `VplLibrary` を import している。`lib.rs` の `pub(crate) use vpl::VplLibrary;` により import パスは維持されるため編集不要。影響範囲表ではこの事実を明記する。

### 4. `CloseGuard` を `vpl.rs` に統一する

`encode.rs` と `decode.rs` にそれぞれ定義されている `CloseGuard` を `vpl.rs` に 1 つだけ定義する。差分（encode 版の `close_encoder: bool` フィールド）は統合版に吸収する。

`encode.rs` と `decode.rs` の `CloseGuard` 定義を削除し、`use crate::CloseGuard;` でインポートする（`lib.rs` の `pub(crate) use` により利用可能）。

既存の `Encoder::new` では `session_guard`（session+loader の CloseGuard）と `encoder_guard`（encoder+session+loader の CloseGuard）の 2 つが同じ `session`/`loader` に対して作成されており、エラーパスでは `mfx_close` + `mfx_unload` が二重に呼ばれる可能性がある。この動作は既存コードから変わらず、本 issue のスコープ外とする。

## テスト戦略

### 単体テスト（`src/vpl.rs` の `#[cfg(test)] mod tests`）

- `FrameSurface::new(lib, std::ptr::null_mut())` が `Error` を返すこと
- `map_write()` を二重に呼んだ場合に `Error` が返ること
- `map_read()` を二重に呼んだ場合に `Error` が返ること
- `unmap()` を二重に呼んだ場合に `Error` が返ること
- 正常系の `new → map_write → unmap → drop` の流れ（実機依存: 有効な `*mut mfxFrameSurface1` の取得には VPL セッションが必要なため、`list_adapters()` が空なら `return` で early exit する。Intel GPU がない CI 環境では正常系テストはスキップされる）

`#[cfg(test)]` 内で `VplLibrary` の unit struct 値はそのまま使える。一方 `encode.rs` の `#[cfg(test)]` 内でも `VplLibrary` 値を直接使用している箇所があるが、`lib.rs` の `pub(crate) use` により import パスは維持されるため変更不要。

### 既存テスト

- `tests/test_roundtrip.rs` は `Encoder` / `Decoder` の公開 API のみを使用しており、内部の `SurfaceGuard` / `DecodedSurfaceGuard` には直接依存していない。今回の変更後も全テストが通過すること

## 影響範囲

| ファイル | 操作 | 内容 |
|---|---|---|
| `src/vpl.rs` | 新規作成 | `VplLibrary`, `FrameSurface`, `CloseGuard`, `frame_type` モジュール, `gop_opt_flag` モジュール、`#[cfg(test)]` |
| `src/lib.rs` | 編集 | `VplLibrary` / `frame_type` / `gop_opt_flag` 定義を削除、`mod vpl;` と再エクスポートを追加 |
| `src/encode.rs` | 編集 | `SurfaceGuard` / `CloseGuard` 定義を削除、import 変更、`FrameSurface` / `CloseGuard` 使用へ移行、`encode_frame_async` の `surface` 引数型を `Option<&FrameSurface>` に変更 |
| `src/decode.rs` | 編集 | `DecodedSurfaceGuard` / `CloseGuard` 定義を削除、import 変更、`FrameSurface` / `CloseGuard` 使用へ移行、`DecodeSyncData.out_surface` を `FrameSurface` に変更、`DecodeSyncData` の `unsafe impl Send` を削除、`read_decoded_surface` / `read_decoded_surface_inner` の引数型変更、`sync_and_drain` の null surface チェックを削除 |
| `src/codec_info.rs` | 編集不要 | `VplLibrary` に依存しているが、`lib.rs` の `pub(crate) use vpl::VplLibrary` により import パスは維持される |

## 後方互換性

- `SurfaceGuard` と `DecodedSurfaceGuard` は `pub` ではない内部型のため、クレート外部への影響はない
- `frame_type` と `gop_opt_flag` は `lib.rs` で再エクスポートするため、外部クレートからのアクセスパスは変更されない
- `Encoder::encode` / `Decoder::decode` の公開 API シグネチャは変更なし
- `CHANGES.md` への追記: `## develop` セクションの `### misc` 配下に以下を追加する（エントリ末尾に担当者行を含めること）
  - `[UPDATE]` `SurfaceGuard` / `DecodedSurfaceGuard` を `FrameSurface` に統合する
    - `- @実装者の GitHub ユーザー名`
  - `[UPDATE]` `CloseGuard` を `src/vpl.rs` に統一する
    - `- @実装者の GitHub ユーザー名`
  - `[UPDATE]` `VplLibrary` / `frame_type` / `gop_opt_flag` を `src/vpl.rs` に移動する
    - `- @実装者の GitHub ユーザー名`

## 解決方法

### src/vpl.rs を新規作成

- `VplLibrary` 構造体と全 `impl` ブロックを `lib.rs` から移動
- `frame_type` / `gop_opt_flag` モジュールを `lib.rs` から移動
- `FrameSurface` 構造体を新規実装（`NonNull` ベース、`map_write`/`map_read`/`unmap` の二重呼び出し検知付き）
- `CloseGuard` を `encode.rs` / `decode.rs` から統合（`close_encoder: bool` フラグで切り替え）
- `#[cfg(test)]` で null 拒否テストと GPU 依存の正常系・二重呼び出しテストを追加

### src/lib.rs を編集

- `mod vpl;` を追加
- `VplLibrary` / `frame_type` / `gop_opt_flag` 定義を削除
- `pub(crate) use vpl::{CloseGuard, VplLibrary};` と `pub use vpl::{frame_type, gop_opt_flag};` を追加

### src/encode.rs を編集

- `use crate::vpl::FrameSurface;` を追加
- `SurfaceGuard` 構造体と `CloseGuard` 構造体を削除
- `encode()` 内で `SurfaceGuard` → `FrameSurface` に置換
- `encode_frame_async` の `surface` 引数を `*mut sys::mfxFrameSurface1` → `Option<&FrameSurface>` に変更
- `encode()` から `Some(&frame_surface)`、`finish()` から `None` で呼び出し

### src/decode.rs を編集

- `use crate::vpl::FrameSurface;` を追加
- `DecodedSurfaceGuard` 構造体と `CloseGuard` 構造体を削除
- `DecodeSyncData.out_surface: *mut sys::mfxFrameSurface1` → `frame_surface: FrameSurface` に変更
- `unsafe impl Send for DecodeSyncData` を削除（`FrameSurface` が自動的に `Send` なため不要）
- `decode_bitstream` / `finish` で `FrameSurface::new` を呼ぶように変更
- `sync_and_callback` / `sync_and_drain` / `read_decoded_surface` / `read_decoded_surface_inner` の引数型を `FrameSurface` に変更

### CHANGES.md を編集

- `### misc` に 3 件の `[UPDATE]` エントリを追加

## 完了条件

- [ ] `src/vpl.rs` を新規作成し `VplLibrary`, `FrameSurface`, `CloseGuard`, `frame_type` モジュール, `gop_opt_flag` モジュール、`#[cfg(test)]` を定義した
- [ ] `src/lib.rs` から `VplLibrary` / `frame_type` / `gop_opt_flag` 定義を削除し、`mod vpl;` と再エクスポートを追加した
- [ ] `src/encode.rs` の `SurfaceGuard` / `CloseGuard` 定義を削除し `FrameSurface` / `CloseGuard` に置き換えた
- [ ] `encode_frame_async` の `surface` 引数型を `Option<&FrameSurface>` に変更し、`encode()` からは `Some(&)` で、`finish()` からは `None` で呼ぶようにした
- [ ] `src/decode.rs` の `DecodedSurfaceGuard` / `CloseGuard` 定義を削除し `FrameSurface` / `CloseGuard` に置き換えた（`sync_and_drain` 含む）
- [ ] `DecodeSyncData.out_surface` を `FrameSurface` に変更し、`unsafe impl Send` を削除した
- [ ] `DecodeSyncData` 構築箇所（`decode_bitstream`、`finish`）で `FrameSurface::new` を呼ぶようにした
- [ ] `read_decoded_surface` / `read_decoded_surface_inner` の引数型を変更し、`# Safety` コメントを更新した
- [ ] `src/vpl.rs` の `#[cfg(test)]` に `FrameSurface` の単体テスト（null 拒否、二重呼び出しエラー）を追加した
- [ ] 既存の `tests/test_roundtrip.rs` が変更後も全て通過することを確認した
- [ ] `CHANGES.md` の `## develop` → `### misc` に 3 件の `[UPDATE]` エントリを担当者行付きで追加した
- [ ] `feature/add-unified-frame-surface` ブランチで作業すること
- [ ] `cargo test` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --check` が全て通過すること
- [ ] issue ファイルに `Completed: YYYY-MM-DD` を追記し、`issues/closed/` へ `git mv` すること
