# silent early-return / 空 Vec 自動パスによるテスト偽陽性を排除する

- Priority: High
- Created: 2026-07-01
- Model: Opus 4.7
- Branch: feature/fix-silent-pass-tests
- Polished: 2026-07-01

## 目的

以下 2 件のテストは、実機 GPU なし / アダプタ未検出 / API 失敗のいずれかで **silent に早期 return または空 `Vec` 自動パス** し、テスト成功扱いになる。テストとして機能しておらず、リグレッションを検知できない。実機必須テストと GPU 非依存テストを整理し、silent skip を排除する。

- `src/vpl.rs:451-537` の `frame_surface_gpu_required`
- `tests/test_adapter.rs:36-48` の `test_list_adapters_sorted_and_deduped`

## 優先度根拠

High。以下による。

- **テストが機能していない**: リグレッションを検知できない状態は「テストがある」以上に危険。「pass しているから安心」の錯覚を生む。
- **偽陽性の温床**: issue 0022 で `ci` ジョブに `cargo test --lib` を追加すると、`frame_surface_gpu_required` が GPU なし Ubuntu で自動 pass しつつ「テストを走らせた」実績を作ってしまう。
- **CLAUDE.md 規約違反**: 「一切妥協しない」観点で、silent skip は妥協そのもの。
- **修正コスト小**: cfg ガードの追加と assert の書き足し。

## 現状

### `frame_surface_gpu_required` の silent early-return

`src/vpl.rs:451-537`:

```rust
#[test]
fn frame_surface_gpu_required() {
    let adapters = match crate::list_adapters() {
        Ok(a) => a,
        Err(_) => return,  // silent skip
    };
    if adapters.is_empty() {
        return;  // silent skip
    }

    let lib = VplLibrary;
    let adapter = AdapterSelector::DrmRenderNode(adapters[0].drm_render_node);
    let session = match lib.create_session(adapter) {
        Ok(v) => v,
        Err(_) => return,  // silent skip
    };

    // 正常系: new → map_write → unmap → drop
    {
        let mut surface: *mut sys::mfxFrameSurface1 = std::ptr::null_mut();
        let status = lib.mfx_memory_get_surface_for_encode(session.as_ptr(), &mut surface);
        if status != sys::mfxStatus_MFX_ERR_NONE {
            return;  // silent skip
        }
        // ... assert 5 個 ...
    }

    // map_write の二重呼び出し / map_read の二重呼び出し / unmap の二重呼び出し / unmapped 状態での unmap
    // ... 4 ブロック続く ...
}
```

問題:

1. `#[cfg(intel_vpl)]` ガードなしでビルドされる
2. `list_adapters` Err / 空 / `create_session` Err / `mfx_memory_get_surface_for_encode` Err のいずれでも silent return
3. 5 個の検証シナリオを 1 個の `#[test]` に詰めており、途中でスキップされると残りが実行されない
4. どこで止まったか / なぜ止まったかがログに残らない

### `test_list_adapters_sorted_and_deduped` の空 `Vec` 自動パス

`tests/test_adapter.rs:36-48`:

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

以下いずれか。

#### 案 A: `#[cfg(intel_vpl)]` を付与し 5 シナリオに分割（推奨）

- `frame_surface_gpu_required` は `src/vpl.rs` 内に留める（`VplLibrary` / `FrameSurface` / `Session` は `pub(crate)` であり `tests/` からはアクセス不能）。
- `#[cfg(intel_vpl)]` を付けて、`INTEL_VPL=1` のときのみコンパイル/実行されるようにする。
- 5 個の検証シナリオを個別の `#[test]` 関数に分割する（各シナリオ独立）。
- silent early-return は全て削除し、前提が満たされないなら `expect("reason")` で明示失敗させる。各テストは `cargo test -- --test-threads=1` の制約下で動作するよう、共通セットアップヘルパーを使う。

分割後のテスト関数:
1. `frame_surface_map_write_unmap_drop_succeeds` — 正常系
2. `frame_surface_double_map_write_fails` — 二重 map_write
3. `frame_surface_double_map_read_fails` — 二重 map_read
4. `frame_surface_double_unmap_fails` — 二重 unmap
5. `frame_surface_unmapped_unmap_fails` — unmapped での unmap

#### 案 B: 現状維持 + assert 追加

silent return を全て assert に置き換える。

- 短所: GPU なし環境で必ず失敗するため、`ci` ジョブや macOS 開発で失敗しっぱなしになる。cfg ガードで分離するのが正しい。

推奨は **案 A**。

### `test_list_adapters_sorted_and_deduped` の修正

以下いずれか。

#### 案 A: 実機テストとして分離（推奨）

- `#[cfg(intel_vpl)]` を付ける
- 空 Vec なら `assert!(!adapters.is_empty(), "実機 GPU に Intel HW アダプタが列挙されない");` で明示失敗させる

#### 案 B: PBT で「昇順 / 重複なし」プロパティを検証する

- 架空の `Vec<AdapterInfo>` を PBT で生成し、`sort_by_key` の結果に依存せず順序制約を検証する
- 実データではなくアルゴリズム検証に切り替える

推奨は **案 A**。テストの意図（実 GPU で列挙結果が昇順・重複なし）を維持しつつ silent skip を排除する。

## 完了条件

以下すべてを満たす。

1. `src/vpl.rs::frame_surface_gpu_required` に `#[cfg(intel_vpl)]` を付与し、5 シナリオを個別の `#[test]` 関数に分割する（共通セットアップヘルパーを使用）。
2. silent early-return を全て削除し、失敗時は `expect("reason")` で原因を残す（`panic!` ではなく）。
3. `tests/test_adapter.rs::test_list_adapters_sorted_and_deduped` に `assert!(!adapters.is_empty(), "Intel HW アダプタが列挙されない")` を追加し、空 Vec での自動パスを防止する。
4. `CHANGES.md` の `## develop` に `[UPDATE]` として追記する。
5. 実装後に `test-intel-vpl` ジョブ（`INTEL_VPL=1`）で全テストが pass することを確認する。

## 影響範囲

- `src/vpl.rs`（`frame_surface_gpu_required` の cfg 追加 + シナリオ分割）
- `tests/test_adapter.rs`（`test_list_adapters_sorted_and_deduped` に空チェック assert 追加）
- `CHANGES.md`

注: `tests/test_vpl.rs` は作成不要。`VplLibrary` / `FrameSurface` / `Session` は `pub(crate)` のため `tests/` からアクセスできない。

## 参考

- 関連 issue: 0022（ci ジョブが cargo test 未実行）、0021（PBT/Fuzz と workspace 構成）
