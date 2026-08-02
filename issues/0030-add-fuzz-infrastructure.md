# Makefile / SKILL.md が fuzz を宣言しているが実体がないので fuzz 基盤を整備する

- Priority: Medium
- Created: 2026-08-02
- Completed: {YYYY-MM-DD}
- Model: DeepSeek V4 Flash
- Branch: feature/add-fuzz-infrastructure
- Polished: {YYYY-MM-DD}

## 目的

`Makefile` の `fuzzing` / `fuzzing-list` ターゲットと `.PHONY` の `fuzz` が「実体は未配置」の状態で並立しているのを解消し、fuzz 基盤を整備する。`make fuzzing` は fuzz 基盤が未整備のため、`$(cargo fuzz list)` のコマンド置換が失敗しても for ループが 0 回で抜け、**exit 0 で無音成功する**（silent pass）。issue 0021 で「分離候補。別 issue で対応」として切り出されたものである。

## 現状

`Makefile` のターゲット:

```makefile
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

- `fuzzing` ターゲットは `cargo fuzz list` のコマンド置換が失敗しても for ループが 0 回で抜け、**exit 0 で無音成功する**（エラーメッセージは stderr にのみ出力される）。
- `fuzzing-list` ターゲットは `cargo fuzz list` を直接実行するため、fuzz 基盤未整備なら失敗する。
- `.PHONY` に `fuzz` が宣言されているが、`fuzz` ターゲットの定義自体が存在しない。
- `prek.toml` の `exclude` には `fuzz/target/**` が既に指定されている。
- `skills/shiguredo-vpl/SKILL.md` に Fuzzing の配置規約は存在しない（fuzz を宣言しているのは Makefile のみ）。
- shiguredo-rust 規約は「`tests/`・`pbt/`・`fuzz/` のテストは公開 API に対してだけ書くこと」と定めている。

## 設計方針

1. `fuzz/` ディレクトリを作成し、cargo-fuzz のセットアップを行う（`fuzz/Cargo.toml`、`fuzz/fuzz_targets/`、cargo-fuzz の `#[fuzz]` ターゲット）。
2. ワークスペース構成（issue 0021 適用後）では `fuzz/` を workspace から `exclude` する。
3. fuzz ターゲットは公開 API に対してのみ書く（shiguredo-rust 規約）。対象の選定は公開 API で到達可能なパーサ・変換処理等から行う。
4. `Makefile` の `fuzzing` ターゲットの silent pass を解消する（ターゲットが存在しない場合は失敗するようにする）。
5. `.PHONY` の `fuzz` の扱いを整理する（ターゲット定義を追加するか、`.PHONY` から削除する）。

## 完了条件

以下すべてを満たす。

1. `fuzz/` ディレクトリと cargo-fuzz のセットアップが完了し、fuzz ターゲットが少なくとも 1 つ存在する。
2. `make fuzzing` が実際に fuzz ターゲットを実行する（silent pass が解消される）。
3. `make fuzzing-list` が fuzz ターゲット一覧を表示する。
4. `.PHONY` の `fuzz` とターゲット定義の整合が取れている。
5. `CHANGES.md` の `## develop` の `### misc` サブセクションに `[ADD]` として追記する（fuzz 基盤の追加。ライブラリ機能に直接影響しないため）。

## 解決方法

- `fuzz/` ディレクトリを作成し、cargo-fuzz の設定と公開 API に対する fuzz ターゲットを追加する。
- `Makefile` の `fuzzing` / `fuzzing-list` ターゲットを実際の fuzz 基盤に合わせて修正する。
- ワークスペース構成（0021 適用後）では `Cargo.toml` の `[workspace]` に `exclude = ["fuzz"]` を設定する。

## 参考

- 関連 issue: 0021（PBT 基盤の整備。本 issue を分離候補として切り出した。workspace 構成の前提）
- 前提: issue 0021 が完了していること（ワークスペース構成に合わせて `fuzz/` を exclude するため）
