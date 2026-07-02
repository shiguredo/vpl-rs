# docs/INTEL_VPL.md が実装から乖離しているため削除して SKILL.md に寄せる

- Priority: Medium
- Created: 2026-07-01
- Model: Opus 4.7
- Branch: feature/doc-remove-outdated-intel-vpl-md
- Polished: 2026-07-02

## 目的

`docs/INTEL_VPL.md` は 2026.3.0 の破壊的リファクタ（ハンドラー方式化 / Session RAII / async_depth デフォルト 4 / DEVICE_BUSY リトライ 30 / CloseGuard 廃止 / next_frame 廃止）を **一切反映していない**。冒頭で「エンコーダのみを対象とする」と宣言しておきながらデコーダも実装済み、`MFXInitialize` を使うと書きながら実装は `MFXLoad + MFXCreateSession`、`encoder.next_frame()` を紹介しながら実装から削除済みなど、8 箇所以上の陳腐化がある。`skills/shiguredo-vpl/SKILL.md` が 2026.3.0 相当まで追随済みなので、`docs/INTEL_VPL.md` は削除して SKILL.md に寄せる。

## 優先度根拠

Medium。以下による。

- **誤情報の温床**: `docs/INTEL_VPL.md` を参照して実装ガイドを組み立てると、削除済み API を呼んだり、廃止されたパターンを再現しようとしたりして時間を無駄にする。
- **`Cargo.toml:12` の `include` に含まれない**: `include = ["/build.rs", "/src/**", "LICENSE", "README.md"]` なので crate publish 時に含まれず、docs.rs にも出ない。crate 利用者への影響は限定的だが、リポジトリを直接読む開発者を誤誘導する。
- **修正コストが低い**: 削除 or 全面書き直しの 2 択で、SKILL.md が代替として存在するので削除を推奨。
- **Priority は High ではなく Medium**: 直接的なコード破損はなく、開発者の情報源としての位置付けなので Medium とする。

## 現状

### 8 箇所以上の陳腐化

- `docs/INTEL_VPL.md:6`「エンコーダのみを対象とする。」 → デコーダも実装済み（`src/decode.rs`）
- `docs/INTEL_VPL.md:20-25` の入力フォーマット表に `I420 / YV12 / P010` を列挙 → 実装は `Nv12 / Yuy2 / Bgra` の 3 種のみ（`src/encode.rs:104-112`）
- `docs/INTEL_VPL.md:46`「`MFXInitialize` / `VplLibrary::mfx_initialize`」 → 実装は `MFXLoad + MFXCreateSession`（`src/vpl.rs:47-124`）
- `docs/INTEL_VPL.md:150` のファイル構成に `CloseGuard`（廃止済）、`vpl.rs` / `decode.rs` / `adapter.rs` / `codec_info.rs` の記述なし
- `docs/INTEL_VPL.md:166`「`encoder.next_frame()` — 内部キューからエンコード済みフレームを取り出す」 → 削除済み（CHANGES.md L34）
- `docs/INTEL_VPL.md:172`「`AsyncDepth = 1` で同期的に動作する」 → デフォルト 4（`src/encode.rs:771`, `src/decode.rs:262`）
- `docs/INTEL_VPL.md:176`「デバイスビジー時は最大 10 回リトライ」 → 実装は 30 回（`src/encode.rs:549`）
- `docs/INTEL_VPL.md:177`「`CloseGuard` でエラー時のリソースリークを防止する」 → 2026.3.0 で廃止（CHANGES.md L21-26）

### SKILL.md との重複

`skills/shiguredo-vpl/SKILL.md` は同等以上の内容を 2026.3.0 に追随した形で持っている。加えて Handler 方式や user_data の対応付け、`AdapterSelector` の使い方など、`docs/INTEL_VPL.md` にはない情報も網羅している。

### `Cargo.toml` の include 外

`Cargo.toml:12`:

```toml
include = ["/build.rs", "/src/**", "LICENSE", "README.md"]
```

`docs/INTEL_VPL.md` は crate publish に含まれず、docs.rs にも出ない。リポジトリを直接見る開発者だけが誤誘導される。

## 設計方針

### 案 A: 削除する（推奨）

`docs/INTEL_VPL.md` を丸ごと削除する。SKILL.md が代替として機能する。`docs/` ディレクトリが空になるならディレクトリごと削除。

- 長所: 修正コスト最小。SKILL.md との二重管理を回避できる。
- 短所: 一部のユーザーが git history 経由で古いバージョンを参照する可能性はあるが、影響は限定的。

### 案 B: 全面書き直し

`docs/INTEL_VPL.md` を SKILL.md の内容に合わせて全面書き直す。

- 長所: `docs/` に何か置きたい場合に対応できる。
- 短所: SKILL.md と重複するため二重管理になる。

推奨は **案 A**。SKILL.md が既に十分な役割を果たしている以上、`docs/INTEL_VPL.md` を残す積極的な理由がない。

## 完了条件

以下すべてを満たす。

1. `docs/INTEL_VPL.md` を削除する。
2. `docs/` ディレクトリが空になるならディレクトリごと削除する。
3. `README.md` / `SKILL.md` / `AGENTS.md` / `CHANGES.md` に `docs/INTEL_VPL.md` への参照がある場合は削除する（現状 grep で確認する限り参照なし）。
4. `Cargo.toml:12` の `include` に変更は不要（既に `docs/` は含まれない）。
5. `CHANGES.md` の `## develop` に `[UPDATE]` として追記する（ドキュメント整理）。

## 影響範囲

- `docs/INTEL_VPL.md`（削除）
- `docs/`（空なら削除）
- `CHANGES.md`

## 参考

- 代替: `skills/shiguredo-vpl/SKILL.md`
- CHANGES.md `## 2026.3.0` のリファクタ内容
