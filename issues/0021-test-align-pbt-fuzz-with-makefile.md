# Makefile / SKILL.md が pbt を宣言しているが実体がないので PBT 基盤を整備する

- Priority: Medium
- Created: 2026-07-01
- Model: Opus 4.7
- Branch: feature/add-pbt-infrastructure
- Polished: 2026-08-02

## 目的

`Makefile` の `pbt` / `pbt-with-cover` ターゲットと `skills/shiguredo-vpl/SKILL.md` の「PBT (proptest): `pbt/tests/prop_<module>.rs` に置く規約」が「実体は未配置」の状態で並立している。`make pbt` を実行すると `cargo test -p pbt` が `error: package ID specification 'pbt' did not match any packages` で失敗する。PBT 基盤を実装し、宣言と実体を一致させる。

なお、`Makefile` の `fuzzing` / `fuzzing-list` ターゲットと `.PHONY` の `fuzz` は fuzz 基盤の整備（`fuzz/` ディレクトリと cargo-fuzz のセットアップが必要）であり、本 issue のスコープ外とする（分離候補。別 issue で対応）。

## 優先度根拠

Medium。以下による。

- **規約と実装の齟齬**: shiguredo-rust 規約「PBT で実現できるものは PBT で書く」に対し、実体がない状態。規約準拠を宣言しつつ実装が伴っていない。
- **Makefile の死にターゲット**: 実行すると失敗する Makefile ターゲットは信頼を失わせる。
- **カバレッジ穴**: `FrameFormat::frame_size`（公開 API）など PBT で書ける対象がある（非公開 API の関数は shiguredo-rust 規約により PBT の対象外。詳細は「PBT の対象」参照）。
- **Priority は High ではない**: 現状の通常テスト（実機 GPU ランナー依存）は機能しており、直接的なバグはない。

## 現状

### Makefile の死にターゲット

`Makefile` の `pbt` / `pbt-with-cover` ターゲット:

```makefile
# PBT を実行する
pbt:
	cargo test -p pbt

# PBT をカバレッジ付きで実行する
pbt-with-cover:
	cargo llvm-cov -p pbt --tests
```

`Cargo.toml` は単一クレート構成（`[workspace]` セクションなし）で、`pbt/` パッケージも存在しない。`make pbt` は `error: package ID specification 'pbt' did not match any packages` で失敗する。

### `.PHONY` の存在しない名

`Makefile` の先頭:

```makefile
.PHONY: test cover pbt pbt-cover fuzz fuzzing fuzzing-list check clippy fmt clean container-build
```

`pbt-cover` はターゲット定義自体が存在しない（実装名は `pbt-with-cover`）。`fuzz` は fuzz 基盤の整備（本 issue のスコープ外）とセットで扱う。

### Makefile の silent pass ターゲット

`Makefile` の `fuzzing` ターゲットは fuzz 基盤が未整備のため、`$(cargo fuzz list)` のコマンド置換が失敗しても for ループが 0 回で抜け、**exit 0 で無音成功する**（エラーメッセージは stderr にのみ出力される）。「実行すると失敗する」のではなく「何もせず成功して見える」状態であり、これは fuzz 基盤の整備（本 issue のスコープ外）で解消する。なお `fuzzing-list` ターゲットは `cargo fuzz list` を直接実行するため、fuzz 基盤未整備なら失敗する。

### SKILL.md の記述

`skills/shiguredo-vpl/SKILL.md` のテスト配置規約:

```markdown
リポジトリ自身のテスト配置 (CLAUDE.md / AGENTS.md 準拠):

- 単体テスト: `tests/test_<module>.rs` (例: `tests/test_adapter.rs`, `tests/test_roundtrip.rs`)
- PBT (proptest): `pbt/tests/prop_<module>.rs` に置く規約。 vpl-rs では現状未配置だが、増やすときはこの規約で。
- `#[ignore]` は使わない。
- PBT で書けるものは単体テストに書かない。 単体テストはエラーパス・境界値・「PBT で実現できないケース」専用。
```

「現状未配置だが、増やすときはこの規約で」と自認している。なお、SKILL.md に Fuzzing の配置規約は存在しない（fuzz を宣言しているのは Makefile のみ）。

### PBT の対象

shiguredo-rust 規約は「`tests/`・`pbt/`・`fuzz/` のテストは公開 API に対してだけ書くこと」と定めている。`pbt/` は別クレートになるため、非公開 API（private 関数や `pub(crate)` の型）は PBT の対象外である。

PBT で書ける対象（公開 API）:

- `FrameFormat::frame_size` のオーバーフロー / 単調性 / フォーマット別バイト数プロパティ（`src/encode.rs` の `FrameFormat`）

**`Encoder::coded_size()` 経由の 16 ピクセルアライン等は GPU セッションを要するため PBT の対象外**（GPU なしの CI では実行できない）。追加する場合は GPU を要しない公開 API 経由で到達可能なものを選ぶ（例: `DecoderConfig::new` の設定プロパティの検証）。

非公開 API（`align_up` / `picture_type_from_frame_type` / `AdapterSelector::validate` / `Error` のコンストラクタ等）は PBT の対象外（規約により `pbt/` からアクセスできないため）。

## 設計方針

### 案 A: Makefile ターゲットと SKILL.md 記述を削除する

- Makefile の `pbt` / `pbt-with-cover` ターゲットを削除し、`.PHONY` から該当ターゲット名を除去
- SKILL.md から PBT 規約を削除するか、「今後の TODO」として明記

長所: 実装コスト極小。実態と規約が一致する。

短所: shiguredo-rust 規約「PBT で書けるものは PBT で書く」との整合が取れない。

### 案 B: PBT 基盤を実装する（推奨）

以下のステップで PBT 基盤を段階的に整備する:

1. まず Makefile の `.PHONY` 誤記（`pbt-cover` → `pbt-with-cover`）を修正する。`fuzz` の `.PHONY` は fuzz 基盤の分離候補（別 issue）で扱う。
2. `pbt/` サブディレクトリと最小限のテスト（`FrameFormat::frame_size` の PBT）を追加する。
   - `pbt/Cargo.toml` は `[package].name = "pbt"` / `publish = false` / `edition = "2024"` / `rust-version = "1.93"` とし、`proptest` 依存と `[dev-dependencies]` への root クレート（`shiguredo_vpl`）の path 依存を追加する。
   - `pbt/` には明示的な lib target として空の `pbt/src/lib.rs` を置く（tests/ のみのクレートでも cargo は動作するが、明示的な lib target として置く）。
3. `Cargo.toml` に `[workspace] members = ["pbt"]` を追加し workspace 構成にする。
   - **workspace 化の副作用**: `make test` (`cargo test --workspace`) / `make check` / `make cover` / `make clippy` / prek フック（`cargo test --workspace` は pre-push で、`cargo clippy --workspace --all-targets` はコミット時に Linux x86_64 ターゲットに対して実行） / CI の `cargo build --workspace` / `cargo clippy --workspace` が `pbt/` も対象化する。proptest はデフォルトで 256 ケース生成するため、**push 時（pre-push）のテスト時間**が少し伸びることに留意する。
4. `make pbt` が成功すること、および既存の `make test` が workspace 化後も成功することを確認する。**検証は Linux x86_64 上で行う**（`src/lib.rs` の `compile_error!` により macOS ではビルド不能。GPU なし Linux では `tests/test_roundtrip.rs` の実機アダプタ依存テストが panic するため、`make test` の確認は実機 GPU 付き Linux で行う）。
5. CI への組込みは行わない（**issue 0022 への依頼事項として明記する**: 0022 の現行完了条件には `cargo test -p pbt` の組込みが含まれていないため、0022 の完了条件に PBT の CI 実行を追加する旨を依頼する。0022 適用前は PBT は実機ジョブ `test-intel-vpl` の `cargo test --workspace` でのみ実行される）。
6. `fuzz/` は本 issue のスコープ外とする（分離候補。別 issue で対応。将来 `fuzz/` を追加する際は workspace から `exclude` する必要がある）。

推奨は **案 B**。段階的に規約準拠を進める。

## 完了条件

以下すべてを満たす。

1. Makefile の `.PHONY` 誤記を修正する（`pbt-cover` → `pbt-with-cover`。`fuzz` の `.PHONY` は fuzz 基盤の分離候補（別 issue）で扱うため、本 issue では触らない）。
2. `pbt/` サブディレクトリと `pbt/Cargo.toml`（`[package].name = "pbt"` / `publish = false` / `edition = "2024"` / `rust-version = "1.93"` / `proptest` 依存 / `[dev-dependencies]` に root クレートの path 依存）と、明示的な lib target としての空の `pbt/src/lib.rs` を作成する。
3. `Cargo.toml` に `[workspace] members = ["pbt"]` を追加する。
4. `pbt/tests/prop_encode.rs` に `FrameFormat::frame_size` の PBT を最低 1 個追加する（shiguredo-rust 規約の「`pbt/tests/prop_<module>.rs` は `src/<module>.rs` に対応させる」に従い、`FrameFormat` が属する `encode` モジュールに対応させる）。プロパティは「任意入力でパニックしない」だけでなく、フォーマット別の期待バイト数やオーバーフロー挙動など検証可能な性質を指定する（Nv12 の奇数画素での切り捨て（`pixels.checked_mul(3).map(|v| v/2)`）に注意し、戦略の制約を設計する）。
5. `make pbt` が成功し、かつ `make test`（workspace 全体の `cargo test`）も pass することを **実機 GPU 付き Linux x86_64 上で**確認する。
6. SKILL.md の PBT 規約記述を「配置済み」に更新する。
7. `CHANGES.md` の `## develop` の `### misc` サブセクションに `[ADD]` として追記する（PBT 基盤の追加。ライブラリ機能に直接影響しないため `### misc` に記載する）。
8. CI への組込みは行わない。**issue 0022 への依頼事項として明記する**: 0022 が未完了（open）の場合は、0022 の完了条件に `cargo test -p pbt` の CI 実行を追加する旨を明記する。0022 が既に完了済み（closed）の場合は、PBT の CI 実行を新規 issue として作成する（実装順序に依存しないよう分岐を定義する）。

## 影響範囲

- `Makefile`
- `Cargo.toml`（`[workspace]` 追加）
- `pbt/`（新規）
- `skills/shiguredo-vpl/SKILL.md`
- `CHANGES.md`

## 参考

- shiguredo-rust スキル規約（PBT は proptest、`pbt/tests/prop_<module>.rs`、公開 API に対してのみ）
- 関連 issue: 0022（ci ジョブが cargo test を実行しない。**PBT の CI 組込みは 0022 への依頼事項**）、0023（silent pass テスト）
- 分離候補: fuzz 基盤の追加（`fuzz/` ディレクトリと cargo-fuzz のセットアップ、Makefile の `fuzzing` / `fuzzing-list` ターゲットの機能化。`make fuzzing` の silent pass もこれで解消）
