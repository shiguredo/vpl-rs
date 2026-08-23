# docs/INTEL_VPL.md が実装から乖離しているため削除する

- Priority: Medium
- Created: 2026-07-01
- Model: Opus 4.7
- Branch: feature/update-remove-outdated-intel-vpl-md
- Polished: 2026-08-02

## 目的

`docs/INTEL_VPL.md` は 2026.3.0 の破壊的リファクタ（ハンドラー方式化 / Session RAII / async_depth デフォルト 4 / DEVICE_BUSY リトライ 30 / CloseGuard 廃止 / next_frame 廃止）を **一切反映していない**。冒頭で「エンコーダのみを対象とする」と宣言しておきながらデコーダも実装済み、`MFXInitialize` を使うと書きながら実装は `MFXLoad + MFXCreateSession`、`encoder.next_frame()` を紹介しながら実装から削除済みなど、10 箇所以上の陳腐化がある。`skills/shiguredo-vpl/SKILL.md` が 2026.3.0 相当まで追随済みなので、`docs/INTEL_VPL.md` は削除して SKILL.md に一本化する。

## 優先度根拠

Medium。以下による。

- **誤情報の温床**: `docs/INTEL_VPL.md` を参照して実装ガイドを組み立てると、削除済み API を呼んだり、廃止されたパターンを再現しようとしたりして時間を無駄にする。
- **crate publish に含まれない**: `Cargo.toml` の `include` に `docs/` が含まれないため、crate publish 時に含まれず docs.rs にも出ない。リポジトリを直接読む開発者を誤誘導する。
- **修正コストが低い**: 削除 or 全面書き直しの 2 択で、SKILL.md が代替として存在するので削除を推奨。
- **Priority は Medium**: 直接的なコード破損はなく、開発者の情報源としての位置付け。誤情報の温床ではあるが、publish 対象外で影響範囲が限定的であることから High ではなく Medium とする（Low ではないのは、誤誘導された開発者の時間損失と、陳腐化を放置すると「正」であるべき SKILL.md との二重管理が恒久化するため）。

## 現状

### 陳腐化の代表例（これ以外にも陳腐化あり）

- 「エンコーダのみを対象とする。」 → デコーダも実装済み（`src/decode.rs` の `Decoder<H>`）
- 入力フォーマット表に `I420 / YV12 / P010` を列挙 → 実装は `FrameFormat` の `Nv12 / Yuy2 / Bgra` の 3 種のみ
- 「`MFXInitialize` / `VplLibrary::mfx_initialize`」 → 実装は `VplLibrary::create_session` の `MFXLoad + MFXCreateSession`。`mfx_initialize` は存在しない
- ファイル構成に `CloseGuard`（廃止済）、`vpl.rs` / `decode.rs` / `adapter.rs` / `codec_info.rs` の記述なし
- ファイル構成とビルド構成に `src/bindings.rs` と「bindgen で生成し `src/bindings.rs` に書き込む」 → 実際は `build.rs` が `OUT_DIR/bindings.rs` に生成し、`src/sys.rs` が include する
- ファイル構成の `lib.rs - VplLibrary` → `VplLibrary` は `src/vpl.rs` に移動済み
- 「`encoder.next_frame()` — 内部キューからエンコード済みフレームを取り出す」 → 削除済み（ハンドラー方式化により `next_frame` は廃止。`src/encode.rs` に `next_frame` は存在しない）
- 「`AsyncDepth = 1` で同期的に動作する」 → デフォルト 4（`async_depth` の `unwrap_or(4)`）。さらに「未実装」セクションの「`AsyncDepth > 1`」も実装済みであり誤り
- 「デバイスビジー時は最大 10 回リトライ」 → 実装は `DEVICE_BUSY_MAX_RETRIES` の 30 回
- 「`CloseGuard` でエラー時のリソースリークを防止する」 → 2026.3.0 で廃止（CHANGES.md の 2026.3.0 エントリ「`CloseGuard` を廃止し、`Session` の Drop による RAII 解放に移行する」）

### SKILL.md との重複

`skills/shiguredo-vpl/SKILL.md` は同等以上の内容を 2026.3.0 に追随した形で持っている。加えて Handler 方式や user_data の対応付け、`AdapterSelector` の使い方など、`docs/INTEL_VPL.md` にはない情報も網羅している。

### crate publish の対象外

`Cargo.toml` の `include = ["/build.rs", "/src/**", "LICENSE", "README.md"]` に `docs/` は含まれない。`docs/INTEL_VPL.md` は crate publish に含まれず、docs.rs にも出ない。リポジトリを直接見る開発者だけが誤誘導される。

## 設計方針

### 案 A: 削除する（推奨）

`docs/INTEL_VPL.md` を丸ごと削除する。SKILL.md が代替として機能する。`docs/` ディレクトリが空になるならディレクトリごと削除。

- 長所: 修正コスト最小。SKILL.md との二重管理を回避できる。
- 短所: 一部のユーザーが git history 経由で古いバージョンを参照する可能性はあるが、影響は限定的。
- **SKILL.md 側への変更は不要**（既に 2026.3.0 相当に追随済みのため。目的の「削除して SKILL.md に一本化する」は「SKILL.md を正として一本化する」の意であり、SKILL.md の書き換え作業を含まない）。

### 案 B: 全面書き直し

`docs/INTEL_VPL.md` を SKILL.md の内容に合わせて全面書き直す。

- 長所: `docs/` に何か置きたい場合に対応できる。
- 短所: SKILL.md と重複するため二重管理になる。

推奨は **案 A**。SKILL.md が既に十分な役割を果たしている以上、`docs/INTEL_VPL.md` を残す積極的な理由がない。

なお、`skills/shiguredo-vpl/SKILL.md` は LLM エージェント向けのリファレンスである。削除後、人間の開発者が詳細ドキュメントへたどり着く導線（README から SKILL.md への言及等）が README には元々存在しない状態になる。README から SKILL.md への導線の追加は本 issue のスコープ外とし、別途検討する。

## 完了条件

以下すべてを満たす。

1. `docs/INTEL_VPL.md` を削除する。
2. `docs/` ディレクトリが空になるならディレクトリごと削除する。
3. リポジトリ全体（`issues/` を除く）で `docs/INTEL_VPL.md` への参照が残らないことを確認する（現状 `rg -n "INTEL_VPL"` の結果、`docs/INTEL_VPL.md` 自身以外に同ファイルへの参照はない。`INTEL_VPL=1` 環境変数の使用箇所（build.rs / ci.yml / tests）は無関係）。
4. `Cargo.toml` の `include` に変更は不要（既に `docs/` は含まれない）。
5. `CHANGES.md` には追記しない（`.md` ファイルの変更は変更履歴に反映しない、という `shiguredo-changelog` 規約による）。

## 影響範囲

- `docs/INTEL_VPL.md`（削除）
- `docs/`（空なら削除）

## 参考

- 代替: `skills/shiguredo-vpl/SKILL.md`
- CHANGES.md の 2026.3.0 エントリ（リファクタ内容）
- 関連 issue: 0017（README.md の入力フォーマット記述の齟齬修正。0017 は「docs/INTEL_VPL.md は別 issue（0016）で削除する」と本 issue を参照している。適用順序の競合はなく独立して適用可能）
