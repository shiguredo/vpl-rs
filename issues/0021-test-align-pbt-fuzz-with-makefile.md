# Makefile / SKILL.md が pbt / fuzz を宣言しているが実体がないので方針を確定させる

- Priority: Medium
- Created: 2026-07-01
- Completed: {YYYY-MM-DD}
- Model: Opus 4.7
- Branch: feature/add-pbt-fuzz-infrastructure
- Polished: {YYYY-MM-DD}

## 目的

`Makefile` の `pbt` / `pbt-with-cover` / `fuzzing` / `fuzzing-list` ターゲットと SKILL.md L376 の「PBT / Fuzzing は `pbt/tests/prop_<module>.rs` に置く規約」が「実体は未配置」の状態で並立している。`make pbt` / `make fuzzing` を実行すると `cargo test -p pbt` / `cargo fuzz list` が失敗する。方針を確定させ、実装するか Makefile ターゲットを削除するかを決める。

## 優先度根拠

Medium。以下による。

- **規約と実装の齟齬**: shiguredo-rust 規約「PBT で実現できるものは PBT で書く」に対し、実体がない状態。規約準拠を宣言しつつ実装が伴っていない。
- **Makefile の死にターゲット**: 実行すると失敗する Makefile ターゲットは信頼を失わせる。
- **カバレッジ穴**: `FrameFormat::frame_size` / `align_up` / `Error::Display` / `picture_type_from_frame_type` / `AdapterSelector::validate` など PBT で書ける対象が多数ある。
- **Priority は High ではない**: 現状の CI では通常テストが機能しており、直接的なバグはない。

## 現状

### Makefile の死にターゲット

`Makefile:11-28`:

```makefile
# PBT を実行する
pbt:
	cargo test -p pbt

# PBT をカバレッジ付きで実行する
pbt-with-cover:
	cargo llvm-cov -p pbt --tests

# Fuzzing を全ターゲットで 30 秒ずつ実行する
fuzzing:
	@for target in $$(cargo fuzz list); do \
		echo "=== Fuzzing $$target ==="; \
		cargo +nightly fuzz run $$target -- -max_total_time=30 || exit 1; \
	done

# Fuzzing ターゲット一覧を表示する
fuzzing-list:
	cargo fuzz list
```

`Cargo.toml` は単一クレート構成（`[workspace]` セクションなし）で、`pbt/` / `fuzz/` パッケージも存在しない。`make pbt` は `error: package ID specification 'pbt' did not match any packages` で失敗。`make fuzzing` は cargo-fuzz 未インストールで失敗（インストール済みでも `fuzz/` が無ければ空ループ）。

### `.PHONY` の存在しない名

`Makefile:1`:

```makefile
.PHONY: test cover pbt pbt-cover fuzz fuzzing fuzzing-list check clippy fmt clean container-build
```

`pbt-cover` / `fuzz` はターゲット定義自体が Makefile に存在しない（実装名は `pbt-with-cover` で、`fuzz` は `fuzzing`）。二重の死に情報。

### SKILL.md の記述

`skills/shiguredo-vpl/SKILL.md:373-378`:

```markdown
リポジトリ自身のテスト配置 (CLAUDE.md / AGENTS.md 準拠):

- 単体テスト: `tests/test_<module>.rs` (例: `tests/test_adapter.rs`, `tests/test_roundtrip.rs`)
- PBT (proptest): `pbt/tests/prop_<module>.rs` に置く規約。 vpl-rs では現状未配置だが、増やすときはこの規約で。
- `#[ignore]` は使わない。
- PBT で書けるものは単体テストに書かない。 単体テストはエラーパス・境界値・「PBT で実現できないケース」専用。
```

「現状未配置だが、増やすときはこの規約で」と自認している。

### PBT / Fuzz の対象候補

PBT で書ける対象:

- `FrameFormat::frame_size` のオーバーフロー / 単調性 / フォーマット別バイト数プロパティ（`src/encode.rs:138-148`）
- `align_up` の冪等性 / `alignment` の倍数化 / `u32::MAX` 近傍でのオーバーフロー（`src/encode.rs:1515-1520`）
- `Error::Display` のフォーマット組み合わせ（`status_code` / `status_name` / `status_message` の 8 通り）（`src/error.rs:107-132`）
- `picture_type_from_frame_type` の frame_type ビットに対するマッピング仕様（`src/encode.rs:1451-1463`）
- `AdapterSelector::validate` の全 render node 値（0 のみ Err、それ以外 Ok）

Fuzz で書ける対象:

- `Decoder::decode` に任意バイト列を入れてクラッシュしないこと（`src/decode.rs:318-341`）
- `Error::from_mfx` に任意 `i32` を入れて panic しないこと

## 設計方針

以下いずれかを選択する。

### 案 A: PBT / Fuzz を実装する（推奨）

- `pbt/` サブディレクトリを作り、`Cargo.toml` に `[workspace] members = ["pbt"]` を追加
- `pbt/Cargo.toml` に `proptest` 依存を追加、`pbt/tests/prop_frame_format.rs` / `pbt/tests/prop_align_up.rs` などを配置
- `fuzz/` サブディレクトリを作り、`cargo fuzz init` 相当のセットアップ
- Makefile の `pbt` / `fuzzing` ターゲットが機能するようにする
- `.PHONY` の存在しない名を修正（`pbt-cover` → `pbt-with-cover`、`fuzz` を追加または削除）

長所: 規約との整合が取れる。カバレッジ穴を PBT で埋められる。

短所: 実装コストが大きい。CI にも組み込みが必要。

### 案 B: Makefile ターゲットと SKILL.md 記述を削除する

- Makefile の `pbt` / `pbt-with-cover` / `fuzzing` / `fuzzing-list` を削除
- `.PHONY` から該当ターゲット名を除去
- SKILL.md L373-378 から PBT / Fuzz 規約を削除するか、「今後の TODO」として明記

長所: 実装コスト極小。実態と規約が一致する。

短所: shiguredo-rust 規約「PBT で書けるものは PBT で書く」との整合が取れない。

### 案 C: 段階的に案 A を進める（推奨）

1. まず Makefile の `.PHONY` 誤記（`pbt-cover` → `pbt-with-cover`）を修正
2. `pbt/` サブディレクトリと最小限のテスト（`FrameFormat::frame_size` の PBT）を追加
3. `[workspace]` セクションを追加して Cargo.toml を workspace 構成にする
4. CI に `make pbt` を組み込む
5. 追加の PBT を随時足す
6. `fuzz/` は後回し（`cargo fuzz` は nightly 依存で導入コスト高）

推奨は **案 C**。段階的に規約準拠を進める。

## 完了条件

以下すべてを満たす（案 C の場合）。

1. Makefile の `.PHONY` 誤記を修正する。
2. `pbt/` サブディレクトリと `pbt/Cargo.toml` を作成する。
3. `Cargo.toml` に `[workspace] members = ["pbt"]` を追加する。
4. `pbt/tests/prop_frame_format.rs` に `FrameFormat::frame_size` の PBT を最低 1 個追加する。
5. `make pbt` が成功する。
6. `.github/workflows/ci.yml` の `ci` ジョブに `cargo test -p pbt` を追加する（issue 0022 と統合）。
7. `fuzz/` は本 issue のスコープ外とする（別 issue で対応）。
8. SKILL.md の PBT 規約記述を「配置済み」に更新する。
9. `CHANGES.md` の `## develop` に `[ADD]` として追記する。

## 影響範囲

- `Makefile`
- `Cargo.toml`（`[workspace]` 追加）
- `pbt/`（新規）
- `.github/workflows/ci.yml`（issue 0022 と統合）
- `skills/shiguredo-vpl/SKILL.md`
- `CHANGES.md`

## 参考

- `/review-code` の致命的指摘 F14 と削除候補（大）の 3 番目
- shiguredo-rust スキル規約
- 関連 issue: 0022（ci ジョブが cargo test を実行しない）、0023（silent pass テスト）
