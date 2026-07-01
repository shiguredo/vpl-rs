# silent early-return / 空 Vec 自動パスによるテスト偽陽性を排除する

- Priority: High
- Created: 2026-07-01
- Completed: {YYYY-MM-DD}
- Model: Opus 4.7
- Branch: feature/fix-silent-pass-tests
- Polished: {YYYY-MM-DD}

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

#### 案 A: `tests/test_vpl.rs` に移動 + `#[cfg(intel_vpl)]`（推奨）

- `src/vpl.rs:440-538` の `#[cfg(test)] mod tests` から `frame_surface_gpu_required` を切り出し、`tests/test_vpl.rs` を新設して移動する
- `#[cfg(intel_vpl)]` を付けて、`INTEL_VPL=1` のときのみコンパイル / 実行される
- 5 個の検証シナリオを個別の `#[test]` 関数に分割する（各シナリオ独立）
- silent early-return は削除し、実機テストなので前提が満たされないなら `panic!("...")` で明示失敗させる（あるいは `#[cfg(intel_vpl)]` 適用済みなので前提は必ず満たされる）

`frame_surface_new_rejects_null` は GPU 非依存なので `src/vpl.rs` に残す。

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

1. `src/vpl.rs::frame_surface_gpu_required` を `tests/test_vpl.rs` に移動し、`#[cfg(intel_vpl)]` を付ける。
2. 移動時に 5 シナリオを個別の `#[test]` に分割する。
3. silent early-return を全て削除し、失敗時は `panic!` する（`#[cfg(intel_vpl)]` により前提が満たされる）。
4. `tests/test_adapter.rs::test_list_adapters_sorted_and_deduped` を `#[cfg(intel_vpl)]` に移動するか、空 Vec で失敗する assert を追加する。
5. `ci` ジョブ (`cargo test --lib`) で `frame_surface_gpu_required` が対象外になることを確認する（issue 0022 と併せて）。
6. `test-intel-vpl` ジョブ（実機、`INTEL_VPL=1`）でこれらのテストが実行されることを確認する。
7. `CHANGES.md` の `## develop` に `[UPDATE]` として追記する。

## 影響範囲

- `src/vpl.rs`（`frame_surface_gpu_required` 削除）
- `tests/test_vpl.rs`（新規、シナリオ 5 個の分割テスト）
- `tests/test_adapter.rs`（`test_list_adapters_sorted_and_deduped` の cfg 変更）
- `.github/workflows/ci.yml`（issue 0022 と統合）
- `CHANGES.md`

## 参考

- `/review-code` の致命的指摘 F16
- 関連 issue: 0022（ci ジョブが cargo test 未実行）、0021（PBT/Fuzz と workspace 構成）
