# Session 型を新設し create_session で返すようにする

Created: 2026-05-14
Completed: 2026-05-14
Model: deepseek-v4-pro

## 背景

現在の `VplLibrary::create_session` は生の `(sys::mfxLoader, sys::mfxSession)` タプルを返し、呼び出し元（`Encoder::new` / `Decoder::new`）が個別に `lib`, `loader`, `session` の 3 つのフィールドを管理している。またエラーパスでの後始末は `CloseGuard` により `session_guard.cancel()` / `encoder_guard.cancel()` のパターンで行われており、正常系と異常系でリソースの所有権移管が複雑になっている。

具体的な問題:

1. **リソース管理の分散**: `lib`, `loader`, `session` の 3 つが常にセットでライフサイクルを共有するにもかかわらず、それぞれ独立したフィールドとして管理されている
2. **`CloseGuard` の複雑さと重複**: `CloseGuard::session` と `CloseGuard::encoder` の 2 種があり、`Encoder::new` では両方が同じ `loader`/`session` に対して作成されるため、エラーパスで `mfx_close` + `mfx_unload` が二重に呼ばれる可能性がある（既知の設計上の問題、issue 0006 参照）。また `.cancel()` による正常系と異常系の切り替えパターンが直感的でない
3. **所有権の不明瞭さ**: `create_session` が生のハンドルを返すため、戻り値の所有権が呼び出し元に分散している

## 要件

### 1. `Session` 型を `src/vpl.rs` に新設する

```rust
pub(crate) struct Session {
    lib: VplLibrary,
    loader: sys::mfxLoader,
    session: sys::mfxSession,
}
```

#### フィールド

- `lib: VplLibrary` — VPL ライブラリのコピー（`Copy` のため値で保持する）
- `loader: sys::mfxLoader` — MFXLoad で取得したローダーハンドル（Drop 時の MFXUnload で必要）
- `session: sys::mfxSession` — MFXCreateSession で取得したセッションハンドル

#### Send 実装

`VplLibrary` が `Copy`、`mfxLoader` と `mfxSession` が生ポインタ型のため、`Session` は自動的に `!Send` になる。実用上 `Send` であっても問題ないため、`unsafe impl Send` を付与する。`Sync` は実装しない（`mfxLoader` / `mfxSession` が生ポインタのため自動的に `!Sync` になる）。これにより同時アクセスは型レベルで防止される。

```rust
// Safety: VPL 仕様上、mfxSession にスレッドアフィニティの制約は課されていない。
// Session は !Sync であるため、複数スレッドからの同時操作は型レベルで防止される。
// mfxSession / mfxLoader は単一スレッドからの逐次アクセスであれば安全に扱える。
unsafe impl Send for Session {}
```

`Encoder` / `Decoder` の `unsafe impl Send` は既存のまま維持する。`mfxVideoParam` / `mfxFrameInfo` 等の他の生ポインタ含有フィールドが存在するため。

#### メソッド

- `fn lib(&self) -> VplLibrary`
  - `VplLibrary` は `Copy` なので値で返す
- `fn as_ptr(&self) -> sys::mfxSession`
  - セッションハンドルを返す。VPL の FFI メソッドにセッションを渡す場合や、ワーカースレッドに `as usize` で渡す場合に使用する

`loader` の getter は不要。`mfxLoader` は `Session::Drop` 内の `MFXUnload` でのみ使用され、外部からアクセスする必要がないため。

#### Drop 実装

```rust
impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.lib.mfx_close(self.session);
        self.lib.mfx_unload(self.loader);
    }
}
```

- `MFXClose` のエラーは `let _ =` で破棄する（Drop 内パニックを避けるため、現行の `CloseGuard` と同一方針）
- `MFXUnload` は戻り値がないため単に呼び出す
- エンコーダ/デコーダのクローズ（`MFXVideoENCODE_Close` / `MFXVideoDECODE_Close`）は `Session` の責務外とする。これらは `Encoder` / `Decoder` の `Drop` で `Session` がドロップされる前に明示的に呼ぶ

### 2. `VplLibrary::create_session` の戻り値を `Session` に変更する

#### 変更前

```rust
pub(crate) fn create_session(
    &self,
    adapter: AdapterSelector,
) -> Result<(sys::mfxLoader, sys::mfxSession), Error>
```

#### 変更後

```rust
pub(crate) fn create_session(
    &self,
    adapter: AdapterSelector,
) -> Result<Session, Error>
```

#### ドキュメントコメント更新

`create_session` の doc comment の戻り値説明を「成功時はローダーとセッションのペアを返す。ローダーはセッションが有効な間は保持する必要がある」から「成功時は Session を返す。Session の Drop で MFXClose + MFXUnload が自動実行される」に更新する。

#### エラーパスの挙動

`create_session` 内部では、`Session` の構築は MFXCreateSession 成功後に行う。それ以前の段階でエラーが発生した場合は、従来通り各エラー分岐で明示的に `MFXUnload(loader)` を呼んでから `Err` を返す（`Session` が構築されていないため Drop による自動解放は働かない）。

`Session` 構築後は、呼び出し元が受け取った `Session` の Drop で `MFXClose` + `MFXUnload` が自動実行される。

### 3. `CloseGuard` を削除する

- `src/vpl.rs` から `CloseGuard` 構造体 (`pub(crate) struct CloseGuard`)、`impl CloseGuard`、`impl Drop for CloseGuard` をすべて削除する
- `src/encode.rs` の `use crate::vpl::{CloseGuard, ...}` から `CloseGuard` を除去する
- `src/decode.rs` の `use crate::vpl::{CloseGuard, ...}` から `CloseGuard` を除去する

#### Encoder::new のエラーパス

```rust
let lib = VplLibrary::load()?;
let session = lib.create_session(config.adapter)?;

// --- Init 前のエラー → session が Drop されて MFXClose + MFXUnload が自動実行される ---

lib.mfx_video_encode_init(session.as_ptr(), &mut video_param)?;

// --- Init 成功後は以降のエラーで MFXVideoENCODE_Close が必要 ---
// Init の戻り値を確認した直後に lib の Copy と session のポインタを取得する
let lib_copy = session.lib();
let session_ptr = session.as_ptr();

lib_copy.mfx_video_encode_get_video_param(session_ptr, &mut video_param)?;
// 拡張バッファなど他の設定続行...

let (worker_tx, worker_rx) = mpsc::channel();
let session_handle = session_ptr as usize;
let worker_handle = thread::Builder::new()
    .name("vpl-encoder-sync".to_owned())
    .spawn(move || {
        run_sync_worker(lib_copy, session_handle, worker_rx, handler);
    })
    .map_err(|error| {
        // スレッド生成失敗時は MFXVideoENCODE_Close を呼んでから session を Drop させる
        let _ = lib_copy.mfx_video_encode_close(session_ptr);
        Error::new_custom_owned(
            "Encoder::new",
            format!("failed to spawn sync worker thread: {error}"),
        )
    })?;

// すべて成功 → Session の所有権を Encoder に移す
Ok(Encoder {
    session,
    // ...
})
```

ポイント:
- `mfx_video_encode_init` 成功後に `session.lib()` と `session.as_ptr()` を取得する（`VplLibrary` は `Copy` のため値コピーされる）
- 以降のエラーポイントでは、`Err` を返す前に `lib_copy.mfx_video_encode_close(session_ptr)` を呼ぶ
- 正常系では `session` を `Encoder` に move する
- `map_err` 内で close を呼ぶパターンを使うことで、エラーハンドリングの局所性を保つ

#### Decoder::new のエラーパス

`Decoder::new` では Init（`DecodeHeader` → `Init`）が `decode()` の初回呼び出し時に lazy 実行される。したがって `Decoder::new` 内で Session 作成後のエラー（`thread::spawn` 失敗等）が発生しても、Init は未完了のため `MFXVideoDECODE_Close` は不要。`session` の Drop に任せるだけでよい。

```rust
let lib = VplLibrary::load()?;
let session = lib.create_session(config.adapter)?;

let (worker_tx, worker_rx) = mpsc::channel();
let lib_copy = session.lib();
let session_handle = session.as_ptr() as usize;
let worker_handle = thread::Builder::new()
    .name("vpl-decoder-sync".to_owned())
    .spawn(move || {
        run_sync_worker(lib_copy, session_handle, worker_rx, handler);
    })
    .map_err(|error| {
        // Init 前のため MFXVideoDECODE_Close は不要。session の Drop に任せる
        Error::new_custom_owned(
            "Decoder::new",
            format!("failed to spawn sync worker thread: {error}"),
        )
    })?;

Ok(Decoder {
    session,
    // ...
})
```

#### VplLibrary::load() について

`VplLibrary::load()` は `Ok(Self)` を返すだけの unit struct 構築であり、`session.lib()` で得られる値と同一。Session 導入後も `VplLibrary::load()?` を呼んでから `create_session` を呼ぶ今のパターンを維持する（`Session` が構築されるのは `create_session` 内部であるため）。`Encoder::new` / `Decoder::new` 内の `let lib = VplLibrary::load()?;` はそのまま残す。

### 4. `Encoder` / `Decoder` のフィールドを `Session` に置き換える

#### Encoder

変更前:
```rust
pub struct Encoder<H: EncodeHandler> {
    lib: VplLibrary,
    loader: sys::mfxLoader,
    session: sys::mfxSession,
    // ... その他のフィールド ...
}
```

変更後:
```rust
pub struct Encoder<H: EncodeHandler> {
    session: Session,
    // ... その他のフィールド ...
}
```

`Encoder::new` の構築箇所:
```rust
// 変更前
let (loader, session) = lib.create_session(config.adapter)?;
// ...
Ok(Encoder {
    lib,
    loader,
    session,
    // ...
})

// 変更後
let session = lib.create_session(config.adapter)?;
// ...
Ok(Encoder {
    session,
    // ...
})
```

#### Decoder

変更前:
```rust
pub struct Decoder<H: DecodeHandler> {
    lib: VplLibrary,
    loader: sys::mfxLoader,
    session: sys::mfxSession,
    // ... その他のフィールド ...
}
```

変更後:
```rust
pub struct Decoder<H: DecodeHandler> {
    session: Session,
    // ... その他のフィールド ...
}
```

同様に `Decoder::new` の構築箇所も変更する。

### 5. `Encoder::Drop` / `Decoder::Drop` の変更

#### Encoder::Drop

変更前:
```rust
fn drop(&mut self) {
    self.stop_worker();
    let _ = self.lib.mfx_video_encode_close(self.session);
    let _ = self.lib.mfx_close(self.session);
    self.lib.mfx_unload(self.loader);
}
```

変更後:
```rust
fn drop(&mut self) {
    self.stop_worker();
    let _ = self.session.lib().mfx_video_encode_close(self.session.as_ptr());
    // self.session が続けて Drop され、MFXClose + MFXUnload が実行される
}
```

#### Decoder::Drop

変更前:
```rust
fn drop(&mut self) {
    self.stop_worker();
    if self.initialized {
        let _ = self.lib.mfx_video_decode_close(self.session);
    }
    let _ = self.lib.mfx_close(self.session);
    self.lib.mfx_unload(self.loader);
}
```

変更後:
```rust
fn drop(&mut self) {
    self.stop_worker();
    if self.initialized {
        let _ = self.session.lib().mfx_video_decode_close(self.session.as_ptr());
    }
    // self.session が続けて Drop され、MFXClose + MFXUnload が実行される
}
```

### 6. 既存の `self.lib` / `self.session` / `self.loader` 参照の置き換え

`Encoder` / `Decoder` 内の全メソッドで、以下の書き換えを行う:

| 変更前 | 変更後 |
|---|---|
| `self.lib` | `self.session.lib()` |
| `self.session`（mfxSession として） | `self.session.as_ptr()` |
| `lib`（ローカル変数。FFI 呼び出し用のコピー） | `session.lib()` |
| `session`（mfxSession としてのローカル変数） | `session.as_ptr()` |
| `self.loader` | 削除（`Session::Drop` 内の `MFXUnload` に移管） |

`FrameSurface` 型は `VplLibrary`（`Copy`）の値コピーを保持しているため、`Session` 導入の影響を受けない。`FrameSurface::new(self.lib, ...)` → `FrameSurface::new(self.session.lib(), ...)` の機械的書換で対応する。

#### ワーカースレッドへのハンドル受け渡し

変更前:
```rust
let session_handle = session as usize;
```

変更後:
```rust
let session_handle = session.as_ptr() as usize;
```

ワーカースレッド側の `session_handle as sys::mfxSession` のキャストは変更不要。

#### `VplLibrary` 値のワーカースレッドへの受け渡し

変更前:
```rust
let lib = VplLibrary;  // Copy
// ...
thread::spawn(move || {
    run_sync_worker(lib, session_handle, ...);
});
```

変更後: `VplLibrary` は `session.lib()` で取得する。`Session` は `Send` のため、スレッド spawn 時の move クロージャへのキャプチャに問題はない。

```rust
let lib = session.lib();
let session_handle = session.as_ptr() as usize;
thread::spawn(move || {
    run_sync_worker(lib, session_handle, ...);
});
```

### 7. `src/lib.rs` の import 更新

現在 `lib.rs` には `pub(crate) use vpl::CloseGuard;` は存在しない（`encode.rs` / `decode.rs` は `use crate::vpl::{CloseGuard, ...}` で直接 import している）。`pub use vpl::{frame_type, gop_opt_flag};` のみが存在するため、lib.rs の public 再エクスポートは変更不要。

`Encoder` / `Decoder` からの Session import は直接 `use crate::vpl::Session;` で行う（re-export を介さず、`crate::vpl` モジュールから直接 import する既存方針に従う）。

## テスト戦略

### PBT / fuzzing

`Session` 型は FFI ハンドル（`sys::mfxLoader`、`sys::mfxSession`）を内包し、任意の値でテストできないため PBT は適用不可。`Session::Drop` 内の処理は決定論的であり入力依存の分岐がないため fuzzing も不要。

### 単体テスト（`src/vpl.rs` の `#[cfg(test)] mod tests`）

既存の `CloseGuard` 用テストは存在しないため、削除のみで新規追加は不要。

`Session` の Drop が正しく呼ばれることのテストは、Drop 内の副作用（FFI 呼び出し）を検証する必要があるため実機依存となる。`list_adapters()` が空でない場合に限り、`create_session` で `Session` を作成し、スコープを抜けたときにクラッシュしないことを確認する簡易テストを追加する。

#### `frame_surface_gpu_required` テストの更新

以下の変更を行う:

1. 戻り値受け取り: `let (loader, session) = match lib.create_session(adapter) {` → `let session = match lib.create_session(adapter) {`
2. `lib.mfx_memory_get_surface_for_encode(session, ...)` → `lib.mfx_memory_get_surface_for_encode(session.as_ptr(), ...)`
3. エラーパスの明示的クリーンアップ（`let _ = lib.mfx_close(session); lib.mfx_unload(loader); return;`）→ 削除（`Session` の Drop に任せる）
4. テスト末尾の `let _ = lib.mfx_close(session); lib.mfx_unload(loader);` → 削除

### 既存テスト

- `tests/test_roundtrip.rs` と `tests/test_adapter.rs` は `Encoder` / `Decoder` の公開 API のみを使用しており、内部の `lib`/`loader`/`session` フィールドには直接依存していない。今回の変更後も全テストが通過すること
- `src/encode.rs` の `#[cfg(test)]` 内テスト（`run_sync_worker` を `VplLibrary` unit struct 値で直接呼ぶテスト）は、`run_sync_worker` のシグネチャが不変（`lib: VplLibrary, session_handle: usize, ...`）であるため影響を受けない

## 影響範囲

| ファイル | 操作 | 内容 |
|---|---|---|
| `src/vpl.rs` | 編集 | `Session` 型と `impl` ブロックを追加、`CloseGuard` を削除、`create_session` の戻り値型を変更と doc comment 更新、`frame_surface_gpu_required` テストを更新 |
| `src/lib.rs` | 編集不要 | `pub(crate) use vpl::CloseGuard;` は存在しないため削除不要。`pub use vpl::{frame_type, gop_opt_flag};` のみで public 再エクスポートに変更なし |
| `src/encode.rs` | 編集 | `use crate::vpl::{CloseGuard, ...}` から `CloseGuard` を除去し `Session` を追加。`lib`, `loader`, `session` フィールドを `session: Session` に置換。`self.lib` → `self.session.lib()`、`self.session` → `self.session.as_ptr()`、`CloseGuard` 使用箇所の削除、`Encoder::new` のエラーパス書き換え、`Encoder::Drop` の変更 |
| `src/decode.rs` | 編集 | 同上（`CloseGuard` import 除去・`Session` 追加、フィールド置換、`self.lib`/`self.session` 参照更新、`Decoder::new` のエラーパス簡略化、`Decoder::Drop` の変更） |
| `src/codec_info.rs` | 編集不要 | `VplLibrary` を直接使用するが `create_session` を呼ばず、独自の `MFXLoad` / `MFXUnload` フローを持つため影響なし |

`FrameSurface` 型は `VplLibrary`（`Copy`）の値コピーを保持しているため変更不要。`FrameSurface::new(self.lib, ...)` → `FrameSurface::new(self.session.lib(), ...)` の機械的書換のみ。

## 後方互換性

- `CloseGuard` は `pub(crate)` の内部型のため、クレート外部への影響はない
- `Encoder` / `Decoder` の公開 API シグネチャは変更なし
- `Session` 型は `pub(crate)` のため外部クレートからは不可視
- `Encoder` / `Decoder` の `unsafe impl Send` は維持される

## CHANGES.md

`## develop` セクションに以下を追記する:

```
- [CHANGE] Session 型を新設し、Encoder/Decoder の lib/loader/session フィールドを統合する
  - CloseGuard を廃止し、Session の Drop による RAII 解放に移行する
  - @実装者の GitHub ユーザー名
```

内部型の変更だが、破壊的変更の精神から `[CHANGE]` を採用する。

## 完了条件

- [ ] `src/vpl.rs` に `Session` 型を新設し、Drop で `MFXClose` + `MFXUnload` を呼ぶようにした
- [ ] `VplLibrary::create_session` の戻り値を `Result<Session, Error>` に変更し、doc comment を更新した
- [ ] `Encoder` の `lib`, `loader`, `session` フィールドを `session: Session` に置き換えた
- [ ] `Decoder` の `lib`, `loader`, `session` フィールドを `session: Session` に置き換えた
- [ ] `Encoder` / `Decoder` 内の全 `self.lib` / `self.session` / `lib` / `session` 参照を更新した
- [ ] `Encoder::new` / `Decoder::new` の `CloseGuard` 使用箇所を削除し、`Session` のライフタイムで置き換えた（Init 後エラーパスでの明示的 close 呼び出しを含む）
- [ ] `Encoder::Drop` と `Decoder::Drop` を `Session` を使う形に変更した
- [ ] ワーカースレッドへのハンドル受け渡し（`session as usize` → `session.as_ptr() as usize`）を更新した
- [ ] `use crate::vpl::{CloseGuard, ...}` から `CloseGuard` を除去し `Session` を追加した（`encode.rs` / `decode.rs`）
- [ ] `src/vpl.rs` の既存テスト（`frame_surface_gpu_required`）を `Session` 型に合わせて更新した
- [ ] `CloseGuard` 構造体とその `impl` ブロックを `src/vpl.rs` から削除した
- [ ] `CHANGES.md` の `## develop` に `[CHANGE]` エントリを追記した
- [ ] `cargo test` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --check` が全て通過すること
- [ ] 既存の `tests/test_roundtrip.rs` および `tests/test_adapter.rs` が変更後も全て通過すること
- [ ] issue ファイルに `Completed: YYYY-MM-DD` を追記し、`issues/closed/` へ `git mv` すること

## 解決方法

issue に記載された要件に従い、以下の実装を行った:

1. `src/vpl.rs`:
   - `Session` 型 (`lib: VplLibrary, loader: sys::mfxLoader, session: sys::mfxSession`) を新設
   - `unsafe impl Send for Session {}` を付与
   - `Session::lib()` と `Session::as_ptr()` メソッドを実装
   - `Session::Drop` で `MFXClose` + `MFXUnload` を実行
   - `CloseGuard` 構造体とその `impl` ブロックを削除
   - `VplLibrary::create_session` の戻り値を `Result<Session, Error>` に変更し、doc comment を更新
   - `frame_surface_gpu_required` テストを `Session` 型に対応するよう更新（明示的クリーンアップを削除し Drop に任せる）

2. `src/encode.rs`:
   - `use crate::vpl::{CloseGuard, ...}` から `CloseGuard` を除去し `Session` を追加
   - `Encoder` の `lib`, `loader`, `session` フィールドを `session: Session` に置換
   - `Encoder::new`: `CloseGuard` を削除し、Init 後のエラーパスでは明示的に `MFXVideoENCODE_Close` を呼ぶように変更
   - 全メソッドの `self.lib` → `self.session.lib()`、`self.session` (mfxSession パラメータ) → `self.session.as_ptr()` を置換
   - `Encoder::Drop`: `MFXVideoENCODE_Close` のみ呼び、`MFXClose`/`MFXUnload` は `Session::Drop` に任せる

3. `src/decode.rs`:
   - `Decoder` のフィールドを同様に `session: Session` に置換
   - `Decoder::new`: `CloseGuard` を削除し、Init 前のためスレッド生成失敗時も `MFXVideoDECODE_Close` は不要
   - `Decoder::Drop`: 同様に `MFXVideoDECODE_Close` のみ呼ぶ

4. `CHANGES.md`: `## develop` に `[CHANGE]` エントリを追記

全テスト (`cargo test`)、clippy (`cargo clippy --workspace --all-targets -- -D warnings`)、fmt (`cargo fmt --check`) が通過することを確認した。
