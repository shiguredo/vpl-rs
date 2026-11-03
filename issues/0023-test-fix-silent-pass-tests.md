# silent early-return / 空 Vec 自動パスによるテスト偽陽性を排除する

- Priority: High
- Created: 2026-07-01
- Model: Opus 4.7
- Branch: feature/fix-silent-pass-tests
- Polished: 2026-08-02

## 目的

以下 2 件のテストは、実機 GPU なし / アダプタ未検出 / API 失敗のいずれかで **silent に早期 return または空 `Vec` 自動パス** し、テスト成功扱いになる。テストとして機能しておらず、リグレッションを検知できない。silent skip を排除する。GPU 無し環境での pass は目的としない（GPU が無ければエラーでよい）。

- `src/vpl.rs` の `frame_surface_gpu_required`
- `tests/test_adapter.rs` の `test_list_adapters_sorted_and_deduped`

## 優先度根拠

High。以下による。

- **テストが機能していない**: リグレッションを検知できない状態は「テストがある」以上に危険。「pass しているから安心」の錯覚を生む。
- **silent skip は方針に反する**: GPU が無ければエラーでよい方針なので、GPU なしで成功扱いになる early return は不要。
- **CLAUDE.md 規約違反**: 「一切妥協しない」観点で、silent skip は妥協そのもの。
- **修正コスト小**: silent return の削除と assert の書き足し。

## 現状

### `frame_surface_gpu_required` の silent early-return

`src/vpl.rs` の `frame_surface_gpu_required`:

```rust
#[test]
fn frame_surface_gpu_required() {
    let adapters = match crate::list_adapters() {
        Ok(a) => a,
        Err(_) => return,
    };
    if adapters.is_empty() {
        return;
    }

    let lib = VplLibrary;
    let adapter = AdapterSelector::DrmRenderNode(adapters[0].drm_render_node);
    let session = match lib.create_session(adapter) {
        Ok(v) => v,
        Err(_) => return,
    };

    // 正常系: new → map_write → unmap → drop
    {
        let mut surface: *mut sys::mfxFrameSurface1 = std::ptr::null_mut();
        let status = lib.mfx_memory_get_surface_for_encode(session.as_ptr(), &mut surface);
        if status != sys::mfxStatus_MFX_ERR_NONE {
            return;
        }
        // ... expect 3 個 ...
    }

    // map_write の二重呼び出し
    // map_read の二重呼び出し
    // unmap の二重呼び出し
    // unmapped 状態での unmap
    // ... 4 ブロック続く ...
}
```

問題:

1. `list_adapters` Err / 空 / `create_session` Err / `mfx_memory_get_surface_for_encode` Err のいずれでも silent return し、テスト成功扱いになる
2. どの前提が満たされなかったのかがログに残らない
3. `MFXMemory_GetSurfaceForEncode` はエンコーダー初期化後のセッションでのみサーフェスを返すため、`create_session()` だけの現在のセットアップでは最初の surface 取得で `MFX_ERR_NOT_FOUND` になり、silent return していた（実機検証で確認）

### `test_list_adapters_sorted_and_deduped` の空 `Vec` 自動パス

`tests/test_adapter.rs` の `test_list_adapters_sorted_and_deduped`:

```rust
#[test]
fn test_list_adapters_sorted_and_deduped() {
    let adapters = list_adapters().expect("list_adapters に失敗");

    let mut nodes: Vec<u32> = adapters.iter().map(|a| a.drm_render_node).collect();
    let original = nodes.clone();
    nodes.sort();
    assert_eq!(nodes, original, "drm_render_node が昇順になっていない");

    let mut deduped = nodes.clone();
    deduped.dedup();
    assert_eq!(deduped, nodes, "drm_render_node が重複している");
}
```

問題:

1. `adapters` が空だと `nodes` も `original` も `deduped` も空
2. `assert_eq!(nodes, original)` は空 Vec 同士の比較で自動成功
3. `assert_eq!(deduped, nodes)` も同様
4. 「昇順」「重複なし」プロパティは 1 要素以上の環境でしか検証されない

## 設計方針

### `frame_surface_gpu_required` の修正

#### 案 A: 1 テストのまま assert に置き換える（推奨）

- `frame_surface_gpu_required` は `src/vpl.rs` 内に留める（`VplLibrary` / `FrameSurface` / `Session` は `pub(crate)` であり `tests/` からはアクセス不能）。
- `MFXMemory_GetSurfaceForEncode` はエンコーダー初期化後のセッションでのみサーフェスを返すため、テスト用の `Encoder`（H.264 / 320x240 / NV12 / CQP）を作成し、`Encoder::session_for_test()` で `Session` を借りる。`Encoder` はテストの間ずっと保持する。
- `src/encode.rs` の `Encoder` に `#[cfg(test)] pub(crate) fn session_for_test(&self) -> &Session` を追加する。テスト専用のアクセサーとする。
- silent early-return を全て削除し、`list_adapters()` の Err / 空 Vec、`Encoder::new()` の Err、`mfx_memory_get_surface_for_encode()` の失敗を `expect` / `unwrap_or_else(panic!)` / `assert!` で明示失敗させる。
- テストは 1 個のまま分割しない。5 シナリオはコメントで区切り、assert / expect メッセージにシナリオ名を含める（正常系、二重 map_write、二重 map_read、二重 unmap、未 map 状態の unmap）。アダプタ一覧や status もメッセージに含める。
- 既存の英語 assert メッセージは日本語化する。
- `#[cfg(intel_vpl)]` は付けない。GPU 実機でのみ実行する方針のため、GPU 無し環境で実行されれば失敗するのが正しい。

#### 案 B: 5 シナリオに分割する（不採用）

- シナリオごとにテスト結果が出る点は利点だが、テストごとに `Encoder` を初期化するコストとコード量が増える。
- `test-intel-vpl` は `--test-threads=1` で実行するため、分割しても並列化されず実行時間だけ増える。
- GPU が無い環境では 5 個とも同じ前提エラーで落ち、ノイズが増える。
- シナリオが増えて 1 テストが長くなった場合に改めて検討する。

### `test_list_adapters_sorted_and_deduped` の修正

#### 案 A: 空 Vec 自動パスを防ぐ（推奨）

- `#[cfg(intel_vpl)]` は付けない。
- 空 Vec なら `assert!(!adapters.is_empty(), "Intel HW アダプタが列挙されない: {listing}")` で明示失敗させる（メッセージは既存の `test_real_adapter_session` と同形式）。
- `listing` は既存の `format_adapters` ヘルパーで作成する。`format_adapters` は現在 `#[cfg(intel_vpl)]` 付きなので、このテストから使えるよう cfg を外す。

#### 案 B: PBT で「昇順 / 重複なし」プロパティを検証する（実装不能のため不採用）

- `AdapterInfo` は `#[non_exhaustive]` のため、外部クレート（`pbt/` を含む）から構造体リテラルで構築できず、架空の `Vec<AdapterInfo>` を生成できない
- 仮に生成できたとしても、検証対象は Rust 標準の `sort` / `dedup` の自明な性質であり、`list_adapters` 内部の実装（`sort_by_key` と重複スキップ）を一切テストしない
- shiguredo-rust 規約は PBT を公開 API に対してのみ書くことと定めており、この案は対象外の領域になる

推奨は **案 A**。テストの意図（実 GPU で列挙結果が昇順・重複なし）を維持しつつ silent skip を排除する。

## 完了条件

以下すべてを満たす。

1. `src/encode.rs` の `Encoder` に `#[cfg(test)] pub(crate) fn session_for_test(&self) -> &Session` を追加する。`src/vpl.rs::frame_surface_gpu_required` ではテスト用の `Encoder`（H.264 / 320x240 / NV12 / CQP）を作成し、そのセッションで surface を取得する（`Encoder` はテストの間ずっと保持する）。`list_adapters()` の Err / 空 Vec、`Encoder::new()` の Err、`mfx_memory_get_surface_for_encode()` の失敗を `expect` / `unwrap_or_else(panic!)` / `assert!` で明示失敗させる。テストは 1 個のまま分割せず、5 シナリオはコメントで区切る。`#[cfg(intel_vpl)]` は付けない。
2. assert / expect / panic メッセージは日本語にし、シナリオ名（正常系、二重 map_write、二重 map_read、二重 unmap、未 map 状態の unmap）を含める。既存の英語メッセージも日本語化する。
3. `tests/test_adapter.rs::test_list_adapters_sorted_and_deduped` に `format_adapters` で一覧化した `listing` を用意し、`assert!(!adapters.is_empty(), "Intel HW アダプタが列挙されない: {listing}")` を追加して空 Vec での自動パスを防止する。`format_adapters` の `#[cfg(intel_vpl)]` は外す。
4. `CHANGES.md` の `## develop` の `### misc` サブセクションに `[ADD]` として追記する（テストの修正はライブラリ機能に直接影響しないため）。
5. `test-intel-vpl` ジョブで全テストが pass し、`cargo clippy --workspace --all-targets -- -D warnings` も pass することを確認する。GPU なし環境で `cargo test --lib` が pass することは確認しない（方針上不要）。

## 影響範囲

- `src/vpl.rs`（`frame_surface_gpu_required` の silent return 削除 + assert 追加 + テスト用 `Encoder` の作成）
- `src/encode.rs`（`#[cfg(test)]` の `session_for_test` 追加）
- `tests/test_adapter.rs`（`test_list_adapters_sorted_and_deduped` の空チェック assert 追加 + `format_adapters` の cfg 解除）
- `CHANGES.md`

注: `tests/test_vpl.rs` は作成不要（`VplLibrary` / `FrameSurface` / `Session` は `pub(crate)` のため `tests/` からはアクセス不能。詳細は「設計方針」参照）。

## 参考

- 関連 issue: 0014（`FrameSurface::Drop` のエラー報告。0023 適用後の `frame_surface_gpu_required` を前提にしている。0014 内の `#[cfg(intel_vpl)]` 付き分割テストという記述は別途更新が必要）
- 0022 はクローズ済み（GPU 無しでテストを回す前提を採らない）
- 0015 は非 Linux の `compile_error!` ガード。cfg を付けない方針のため本 issue との依存はない
