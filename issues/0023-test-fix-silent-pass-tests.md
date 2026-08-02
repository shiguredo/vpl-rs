# silent early-return / 空 Vec 自動パスによるテスト偽陽性を排除する

- Priority: High
- Created: 2026-07-01
- Model: Opus 4.7
- Branch: feature/fix-silent-pass-tests
- Polished: 2026-08-02

## 目的

以下 2 件のテストは、実機 GPU なし / アダプタ未検出 / API 失敗のいずれかで **silent に早期 return または空 `Vec` 自動パス** し、テスト成功扱いになる。テストとして機能しておらず、リグレッションを検知できない。実機必須テストと GPU 非依存テストを整理し、silent skip を排除する。

- `src/vpl.rs` の `frame_surface_gpu_required`
- `tests/test_adapter.rs` の `test_list_adapters_sorted_and_deduped`

## 優先度根拠

High。以下による。

- **テストが機能していない**: リグレッションを検知できない状態は「テストがある」以上に危険。「pass しているから安心」の錯覚を生む。
- **偽陽性の温床**: issue 0022 で `ci` ジョブに `cargo test --lib` を追加すると、`frame_surface_gpu_required` が GPU なし Ubuntu で自動 pass しつつ「テストを走らせた」実績を作ってしまう。
- **CLAUDE.md 規約違反**: 「一切妥協しない」観点で、silent skip は妥協そのもの。
- **修正コスト小**: cfg ガードの追加と assert の書き足し。

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

1. `#[cfg(intel_vpl)]` ガードなしでビルドされる（GPU なし環境でも実行される）
2. `list_adapters` Err / 空 / `create_session` Err / `mfx_memory_get_surface_for_encode` Err のいずれでも silent return
3. 5 個の検証シナリオを 1 個の `#[test]` に詰めており、途中でスキップされると残りが実行されない
4. どこで止まったか / なぜ止まったかがログに残らない

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

以下いずれか。

#### 案 A: `#[cfg(intel_vpl)]` を付与し 5 シナリオに分割（推奨）

- `frame_surface_gpu_required` は `src/vpl.rs` 内に留める（`VplLibrary` / `FrameSurface` / `Session` は `pub(crate)` であり `tests/` からはアクセス不能）。
- 5 個の分割後テスト関数 **それぞれ** に `#[cfg(intel_vpl)]` を付けて、`INTEL_VPL=1` のときのみコンパイル/実行されるようにする（`mod tests` 全体に付けると GPU 不要の `frame_surface_new_rejects_null` まで巻き込むため、関数単位で付与する）。共通セットアップヘルパーにも `#[cfg(intel_vpl)]` を付与する（無ガードだと GPU なしビルドで未使用になり `dead_code` 警告 → pre-commit の clippy `-D warnings` が失敗する。またヘルパーが `crate::list_adapters()` を呼ぶため、issue 0015 の E0425 解消の前提とも整合しない）。
- 元の `frame_surface_gpu_required` 関数は削除し、5 個の検証シナリオを個別の `#[test]` 関数に分割する（各シナリオ独立）。
- silent early-return を全て削除し、前提が満たされないなら `assert!` で明示失敗させる。分割後も `test-intel-vpl` ジョブが `--test-threads=1` で実行する事実は変わらないが、各テストが独立した `Session` を張るため直列実行に依存しない（共通セットアップヘルパーはアダプタ列挙 → 非空 assert → `Session` 作成までを行い、`Session` を返す形にする。`mfx_memory_get_surface_for_encode` による surface 取得は各テスト側で行う。`FrameSurface` は `Session` への参照を持たないため、ヘルパーが `FrameSurface` だけ返すと dangling になる。ヘルパー内の非空 assert のメッセージは日本語でアダプタ一覧を含める（`format_adapters` は `tests/test_adapter.rs` 内のヘルパーであり `src/vpl.rs` の `mod tests` からは使用不能なため、vpl.rs 側に同様のフォーマッタを置く））。
- 既存の英語 assert メッセージ（`"second map_write should fail"` 等）は AGENTS.md の「テストのログメッセージは全て日本語にすること」に従い、分割時に日本語化する。

分割後のテスト関数:
1. `frame_surface_map_write_unmap_drop_succeeds` — 正常系
2. `frame_surface_double_map_write_fails` — 二重 map_write
3. `frame_surface_double_map_read_fails` — 二重 map_read
4. `frame_surface_double_unmap_fails` — 二重 unmap
5. `frame_surface_unmapped_unmap_fails` — unmapped での unmap

#### 案 B: 現状維持 + assert 追加

silent return を全て assert に置き換える。

- 短所: GPU なし環境で必ず失敗するため、GPU なし Linux（container 等）で失敗しっぱなしになる（macOS は `src/lib.rs` の `compile_error!` によりビルド自体が不能）。cfg ガードで分離するのが正しい。

推奨は **案 A**。

### `test_list_adapters_sorted_and_deduped` の修正

以下いずれか。

#### 案 A: 実機テストとして分離（推奨）

- `#[cfg(intel_vpl)]` を付ける
- 空 Vec なら `assert!(!adapters.is_empty(), "Intel HW アダプタが列挙されない: {listing}")` で明示失敗させる（メッセージは既存の `test_real_adapter_session` と同形式）

#### 案 B: PBT で「昇順 / 重複なし」プロパティを検証する（実装不能のため不採用）

- `AdapterInfo` は `#[non_exhaustive]` のため、外部クレート（`pbt/` を含む）から構造体リテラルで構築できず、架空の `Vec<AdapterInfo>` を生成できない
- 仮に生成できたとしても、検証対象は Rust 標準の `sort` / `dedup` の自明な性質であり、`list_adapters` 内部の実装（`sort_by_key` と重複スキップ）を一切テストしない
- shiguredo-rust 規約は PBT を公開 API に対してのみ書くことと定めており、この案は対象外の領域になる

推奨は **案 A**。テストの意図（実 GPU で列挙結果が昇順・重複なし）を維持しつつ silent skip を排除する。

## 完了条件

以下すべてを満たす。

1. `src/vpl.rs::frame_surface_gpu_required` を 5 個の個別 `#[test]` 関数に分割し、**それぞれに** `#[cfg(intel_vpl)]` を付与する（元の関数は削除。共通セットアップヘルパーにも `#[cfg(intel_vpl)]` を付与し、アダプタ列挙 → 非空 assert → `Session` 作成を行って `Session` を返す。返り値の `Session` はテスト関数側の変数で保持する（Drop による `MFXClose` を防ぐ）。`mfx_memory_get_surface_for_encode` による surface 取得は各テスト側で行う）。
2. silent early-return を全て削除し、前提が満たされない場合は `assert!` で原因メッセージ付きで明示失敗させる。assert メッセージは日本語にする（既存の英語メッセージも分割時に日本語化する）。
3. `tests/test_adapter.rs::test_list_adapters_sorted_and_deduped` に `#[cfg(intel_vpl)]` を付与し、既存の `format_adapters` ヘルパーで一覧化した `listing` 変数を用意したうえで `assert!(!adapters.is_empty(), "Intel HW アダプタが列挙されない: {listing}")` を追加して空 Vec での自動パスを防止する（設計方針の案 A に一致。メッセージ形式は既存の `test_real_adapter_session` と同形式）。
4. `CHANGES.md` の `## develop` の `### misc` サブセクションに `[ADD]` として追記する（テストの修正はライブラリ機能に直接影響しないため）。
5. 実装後に `test-intel-vpl` ジョブ（`INTEL_VPL=1`）で全テストが pass することを確認する。加えて、`INTEL_VPL` 未設定（GPU なし）環境で `cargo test --lib` と `cargo clippy --workspace --all-targets -- -D warnings` が pass することも確認する（cfg 付与による dead_code / E0425 が生じていないことの検証）。

## 影響範囲

- `src/vpl.rs`（`frame_surface_gpu_required` の分割 + cfg 付与）
- `tests/test_adapter.rs`（`test_list_adapters_sorted_and_deduped` に cfg 付与 + 空チェック assert 追加）
- `CHANGES.md`

注: `tests/test_vpl.rs` は作成不要（`VplLibrary` / `FrameSurface` / `Session` は `pub(crate)` のため `tests/` からはアクセス不能。詳細は「設計方針」参照）。

## 参考

- 関連 issue: 0022（ci ジョブが cargo test 未実行。本 issue の `#[cfg(intel_vpl)]` 付与を完了条件の前提としている）、0014（`FrameSurface::Drop` のエラー報告。0023 適用後の分割テストを前提にしている）、0015（非 Linux の `compile_error!` ガード。0023 の cfg 付与で E0425 が解消される）
